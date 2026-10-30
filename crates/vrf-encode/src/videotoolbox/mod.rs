use std::ffi::c_void;
use std::ptr::{NonNull, null, null_mut};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context, Result, bail, ensure};
use objc2_core_foundation::{
    CFArray, CFBoolean, CFData, CFDictionary, CFNumber, CFRetained, CFString, CFType,
};
use objc2_core_media::{
    CMSampleBuffer, CMTime, CMTimeFlags,
    kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms, kCMSampleAttachmentKey_NotSync,
    kCMTimeInvalid, kCMVideoCodecType_AV1, kCMVideoCodecType_AppleProRes422,
    kCMVideoCodecType_H264, kCMVideoCodecType_HEVC,
};
use objc2_core_video::{
    kCVImageBufferColorPrimaries_ITU_R_709_2, kCVImageBufferTransferFunction_ITU_R_709_2,
    kCVImageBufferYCbCrMatrix_ITU_R_709_2,
};
use objc2_video_toolbox::{
    VTCompressionSession, VTEncodeInfoFlags, VTSessionCopyProperty, VTSessionSetProperty,
    kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AllowOpenGOP,
    kVTCompressionPropertyKey_ColorPrimaries, kVTCompressionPropertyKey_ExpectedFrameRate,
    kVTCompressionPropertyKey_MaxKeyFrameInterval,
    kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality,
    kVTCompressionPropertyKey_ProfileLevel, kVTCompressionPropertyKey_Quality,
    kVTCompressionPropertyKey_RealTime, kVTCompressionPropertyKey_RecommendedParallelizationLimit,
    kVTCompressionPropertyKey_RecommendedParallelizedSubdivisionMinimumFrameCount,
    kVTCompressionPropertyKey_TransferFunction, kVTCompressionPropertyKey_YCbCrMatrix,
    kVTEncodeFrameOptionKey_ForceKeyFrame, kVTProfileLevel_H264_High_AutoLevel,
    kVTProfileLevel_HEVC_Main_AutoLevel, kVTProfileLevel_HEVC_Main10_AutoLevel,
    kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
};

use crate::{Codec, Encoder, EncoderConfig, EncoderFrame, Packet, Rational, StreamInfo};

pub mod pool;

use pool::{PixelFormat, PixelPool};

static SHARED_ENGINE_SESSIONS: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub parallelization_limit: usize,
    pub min_subdivision_frames: usize,
}

struct Shared {
    codec: Codec,
    sender: Sender<Packet>,
    config: OnceLock<Vec<u8>>,
    error: OnceLock<String>,
}

struct Slot;

impl Slot {
    fn take(limit: usize) -> Result<Slot> {
        let open = SHARED_ENGINE_SESSIONS.fetch_add(1, Ordering::SeqCst);
        if open >= limit {
            SHARED_ENGINE_SESSIONS.fetch_sub(1, Ordering::SeqCst);
            bail!(
                "{open} H.264/HEVC sessions are open, which is the parallelization limit ({limit})"
            );
        }
        Ok(Slot)
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        SHARED_ENGINE_SESSIONS.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct VideoToolboxEncoder {
    config: EncoderConfig,
    capabilities: Capabilities,
    session: CFRetained<VTCompressionSession>,
    shared: Arc<Shared>,
    packets: Option<Receiver<Packet>>,
    pool: PixelPool,
    frame_duration: Rational,
    _slot: Option<Slot>,
}

// VideoToolbox sessions may be used from any thread; the encoder is only moved, never shared.
unsafe impl Send for VideoToolboxEncoder {}

/// The pixel format the encoder takes for `config`.
pub fn pixel_format(config: &EncoderConfig) -> PixelFormat {
    if config.ten_bit {
        PixelFormat::P010
    } else {
        PixelFormat::Nv12
    }
}

fn codec_type(codec: Codec) -> u32 {
    match codec {
        Codec::H264 => kCMVideoCodecType_H264,
        Codec::Hevc => kCMVideoCodecType_HEVC,
        Codec::Av1 => kCMVideoCodecType_AV1,
        Codec::ProRes422 => kCMVideoCodecType_AppleProRes422,
    }
}

fn status(what: &str, status: i32) -> Result<()> {
    ensure!(status == 0, "{what}: OSStatus {status}");
    Ok(())
}

fn set(session: &VTCompressionSession, name: &str, key: &CFString, value: &CFType) -> Result<()> {
    status(&format!("setting {name}"), unsafe {
        VTSessionSetProperty(session.as_ref(), key, Some(value))
    })
}

fn read_usize(session: &VTCompressionSession, key: &CFString) -> Option<usize> {
    let mut out: *const CFType = null();
    let status = unsafe {
        VTSessionCopyProperty(
            session.as_ref(),
            key,
            None,
            (&mut out as *mut *const CFType).cast(),
        )
    };
    let value = unsafe { CFRetained::from_raw(NonNull::new(out as *mut CFType)?) };
    if status != 0 {
        return None;
    }
    usize::try_from(value.downcast_ref::<CFNumber>()?.as_i64()?).ok()
}

fn read_capabilities(session: &VTCompressionSession) -> Result<Capabilities> {
    Ok(Capabilities {
        parallelization_limit: read_usize(session, unsafe {
            kVTCompressionPropertyKey_RecommendedParallelizationLimit
        })
        .context("RecommendedParallelizationLimit is not readable")?,
        min_subdivision_frames: read_usize(session, unsafe {
            kVTCompressionPropertyKey_RecommendedParallelizedSubdivisionMinimumFrameCount
        })
        .context("RecommendedParallelizedSubdivisionMinimumFrameCount is not readable")?,
    })
}

fn create_session(
    codec: Codec,
    width: u32,
    height: u32,
    shared: Option<&Arc<Shared>>,
) -> Result<CFRetained<VTCompressionSession>> {
    let hardware = CFBoolean::new(true);
    let specification = unsafe {
        CFDictionary::<CFString, CFType>::from_slices(
            &[kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder],
            &[hardware.as_ref()],
        )
    };
    let (callback, refcon): (_, *mut c_void) = match shared {
        Some(shared) => (Some(on_output as _), Arc::as_ptr(shared) as *mut c_void),
        None => (None, null_mut()),
    };
    let mut raw = null_mut();
    let result = unsafe {
        VTCompressionSession::create(
            None,
            width as i32,
            height as i32,
            codec_type(codec),
            Some(specification.as_opaque()),
            None,
            None,
            callback,
            refcon,
            NonNull::from(&mut raw),
        )
    };
    status(&format!("creating a hardware {codec:?} session"), result)?;
    Ok(unsafe { CFRetained::from_raw(NonNull::new(raw).context("no session returned")?) })
}

pub fn capabilities(codec: Codec) -> Result<Capabilities> {
    static KNOWN: Mutex<Vec<(Codec, Capabilities)>> = Mutex::new(Vec::new());
    let mut known = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, capabilities)) = known.iter().find(|(c, _)| *c == codec) {
        return Ok(*capabilities);
    }
    let session = create_session(codec, 1920, 1080, None)?;
    let capabilities = read_capabilities(&session);
    unsafe { session.invalidate() };
    let capabilities = capabilities?;
    known.push((codec, capabilities));
    Ok(capabilities)
}

fn configure(session: &VTCompressionSession, config: &EncoderConfig) -> Result<()> {
    let yes = CFBoolean::new(true);
    let no = CFBoolean::new(false);
    let h26x = config.codec != Codec::ProRes422;
    unsafe {
        set(
            session,
            "RealTime",
            kVTCompressionPropertyKey_RealTime,
            no.as_ref(),
        )?;
        if h26x {
            set(
                session,
                "PrioritizeEncodingSpeedOverQuality",
                kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality,
                yes.as_ref(),
            )?;
            set(
                session,
                "AllowFrameReordering",
                kVTCompressionPropertyKey_AllowFrameReordering,
                no.as_ref(),
            )?;
            set(
                session,
                "AllowOpenGOP",
                kVTCompressionPropertyKey_AllowOpenGOP,
                no.as_ref(),
            )?;
            let profile = match (config.codec, config.ten_bit) {
                (Codec::H264, _) => kVTProfileLevel_H264_High_AutoLevel,
                (_, false) => kVTProfileLevel_HEVC_Main_AutoLevel,
                (_, true) => kVTProfileLevel_HEVC_Main10_AutoLevel,
            };
            set(
                session,
                "ProfileLevel",
                kVTCompressionPropertyKey_ProfileLevel,
                profile.as_ref(),
            )?;
            set(
                session,
                "Quality",
                kVTCompressionPropertyKey_Quality,
                CFNumber::new_f32(config.quality.value()).as_ref(),
            )?;
            set(
                session,
                "MaxKeyFrameInterval",
                kVTCompressionPropertyKey_MaxKeyFrameInterval,
                CFNumber::new_i32(config.keyframe_interval as i32).as_ref(),
            )?;
        }
        let fps = config.frame_rate.num as f64 / config.frame_rate.den as f64;
        set(
            session,
            "ExpectedFrameRate",
            kVTCompressionPropertyKey_ExpectedFrameRate,
            CFNumber::new_f64(fps).as_ref(),
        )?;
        set(
            session,
            "ColorPrimaries",
            kVTCompressionPropertyKey_ColorPrimaries,
            kCVImageBufferColorPrimaries_ITU_R_709_2.as_ref(),
        )?;
        set(
            session,
            "TransferFunction",
            kVTCompressionPropertyKey_TransferFunction,
            kCVImageBufferTransferFunction_ITU_R_709_2.as_ref(),
        )?;
        set(
            session,
            "YCbCrMatrix",
            kVTCompressionPropertyKey_YCbCrMatrix,
            kCVImageBufferYCbCrMatrix_ITU_R_709_2.as_ref(),
        )?;
    }
    Ok(())
}

fn rational_of(time: CMTime) -> Option<Rational> {
    (time.flags.contains(CMTimeFlags::Valid) && time.timescale > 0)
        .then(|| Rational::new(time.value, time.timescale as i64))
}

unsafe fn config_of(sample: &CMSampleBuffer, codec: Codec) -> Vec<u8> {
    let atom = match codec {
        Codec::H264 => "avcC",
        Codec::Hevc => "hvcC",
        Codec::Av1 => "av1C",
        Codec::ProRes422 => return Vec::new(),
    };
    let atoms = unsafe {
        sample.format_description().and_then(|description| {
            description.extension(kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms)
        })
    };
    atoms
        .and_then(|atoms| atoms.downcast::<CFDictionary>().ok())
        .and_then(|atoms| {
            let atoms = unsafe { atoms.cast_unchecked::<CFString, CFType>() };
            atoms.get(&CFString::from_str(atom))
        })
        .and_then(|data| data.downcast::<CFData>().ok())
        .map(|data| data.to_vec())
        .unwrap_or_default()
}

unsafe fn is_keyframe(sample: &CMSampleBuffer) -> bool {
    let Some(attachments) = (unsafe { sample.sample_attachments_array(false) }) else {
        return true;
    };
    let attachments = unsafe {
        CFRetained::cast_unchecked::<CFArray<CFDictionary<CFString, CFType>>>(attachments)
    };
    let Some(first) = attachments.get(0) else {
        return true;
    };
    !first.contains_key(unsafe { kCMSampleAttachmentKey_NotSync })
}

unsafe fn packet_of(sample: &CMSampleBuffer) -> Option<Packet> {
    let block = unsafe { sample.data_buffer() }?;
    let length = unsafe { block.data_length() };
    let mut data = vec![0u8; length];
    let copied =
        unsafe { block.copy_data_bytes(0, length, NonNull::new(data.as_mut_ptr().cast())?) };
    if copied != 0 {
        return None;
    }
    let pts = rational_of(unsafe { sample.presentation_time_stamp() })?;
    let dts = rational_of(unsafe { sample.decode_time_stamp() }).unwrap_or(pts);
    Some(Packet {
        data,
        pts,
        dts,
        keyframe: unsafe { is_keyframe(sample) },
    })
}

unsafe extern "C-unwind" fn on_output(
    refcon: *mut c_void,
    _frame_refcon: *mut c_void,
    result: i32,
    _flags: VTEncodeInfoFlags,
    sample: *mut CMSampleBuffer,
) {
    let shared = unsafe { &*(refcon as *const Shared) };
    if result != 0 {
        let _ = shared
            .error
            .set(format!("VideoToolbox failed a frame: OSStatus {result}"));
        return;
    }
    let Some(sample) = NonNull::new(sample) else {
        let _ = shared.error.set("VideoToolbox dropped a frame".to_string());
        return;
    };
    let sample = unsafe { sample.as_ref() };
    let Some(packet) = (unsafe { packet_of(sample) }) else {
        let _ = shared
            .error
            .set("VideoToolbox returned an unreadable sample".to_string());
        return;
    };
    shared
        .config
        .get_or_init(|| unsafe { config_of(sample, shared.codec) });
    let _ = shared.sender.send(packet);
}

impl VideoToolboxEncoder {
    pub fn new(config: EncoderConfig) -> Result<Self> {
        let pool = PixelPool::new(
            config.width as usize,
            config.height as usize,
            pixel_format(&config),
        )?;
        Self::with_pool(config, pool, None)
    }

    /// Encodes frames from `pool`, which must hold buffers of the config's size and format.
    /// `session_limit` replaces the machine's `RecommendedParallelizationLimit` for the count of
    /// open H.264 and HEVC sessions.
    pub fn with_pool(
        config: EncoderConfig,
        pool: PixelPool,
        session_limit: Option<usize>,
    ) -> Result<Self> {
        ensure!(
            !config.ten_bit || config.codec == Codec::Hevc,
            "ten_bit is only supported for HEVC"
        );
        ensure!(
            config.frame_rate.num > 0 && config.frame_rate.den > 0,
            "frame rate must be positive"
        );
        let (sender, receiver) = channel();
        let shared = Arc::new(Shared {
            codec: config.codec,
            sender,
            config: OnceLock::new(),
            error: OnceLock::new(),
        });
        let session = create_session(config.codec, config.width, config.height, Some(&shared))?;
        let capabilities = read_capabilities(&session).unwrap_or(Capabilities {
            parallelization_limit: 1,
            min_subdivision_frames: 0,
        });
        let slot = match config.codec {
            Codec::ProRes422 => None,
            _ => Some(
                Slot::take(session_limit.unwrap_or(capabilities.parallelization_limit))
                    .inspect_err(|_| unsafe { session.invalidate() })?,
            ),
        };
        configure(&session, &config)?;
        status("preparing the session", unsafe {
            session.prepare_to_encode_frames()
        })?;
        let frame_duration = Rational::new(config.frame_rate.den, config.frame_rate.num);
        Ok(VideoToolboxEncoder {
            config,
            capabilities,
            session,
            shared,
            packets: Some(receiver),
            pool,
            frame_duration,
            _slot: slot,
        })
    }

    pub fn pool(&self) -> &PixelPool {
        &self.pool
    }

    pub fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    fn check(&self) -> Result<()> {
        match self.shared.error.get() {
            Some(error) => bail!("{error}"),
            None => Ok(()),
        }
    }
}

fn cm_time(time: Rational) -> Result<CMTime> {
    ensure!(
        time.den > 0 && time.den <= i32::MAX as i64,
        "timestamp denominator {} does not fit a CMTime timescale",
        time.den
    );
    Ok(unsafe { CMTime::new(time.num, time.den as i32) })
}

impl VideoToolboxEncoder {
    /// Like [`Encoder::submit`], and starts a new closed GOP at this frame when `keyframe` is
    /// set, whatever the frames before it were.
    pub fn submit_keyframe(
        &mut self,
        frame: EncoderFrame,
        pts: Rational,
        keyframe: bool,
    ) -> Result<()> {
        self.check()?;
        let EncoderFrame::PixelBuffer(buffer) = frame;
        let force = keyframe.then(|| {
            CFDictionary::<CFString, CFType>::from_slices(
                &[unsafe { kVTEncodeFrameOptionKey_ForceKeyFrame }],
                &[CFBoolean::new(true).as_ref()],
            )
        });
        let result = unsafe {
            self.session.encode_frame(
                buffer.pixel_buffer(),
                cm_time(pts)?,
                cm_time(self.frame_duration)?,
                force.as_ref().map(|force| force.as_opaque()),
                null_mut(),
                null_mut(),
            )
        };
        status("encoding a frame", result)
    }
}

impl Encoder for VideoToolboxEncoder {
    fn submit(&mut self, frame: EncoderFrame, pts: Rational) -> Result<()> {
        self.submit_keyframe(frame, pts, false)
    }

    fn drain(&mut self) -> Result<Vec<Packet>> {
        self.check()?;
        Ok(self
            .packets
            .as_ref()
            .map(|packets| packets.try_iter().collect())
            .unwrap_or_default())
    }

    fn flush(&mut self) -> Result<Vec<Packet>> {
        let mut out = self.drain()?;
        status("completing frames", unsafe {
            self.session.complete_frames(kCMTimeInvalid)
        })?;
        out.extend(self.drain()?);
        Ok(out)
    }

    fn stream_info(&self) -> Result<StreamInfo> {
        let config = self
            .shared
            .config
            .get()
            .context("no frame was encoded")?
            .clone();
        Ok(StreamInfo {
            codec: self.config.codec,
            width: self.config.width,
            height: self.config.height,
            timebase: Rational::new(1, self.config.frame_rate.num),
            config,
        })
    }

    fn finish(mut self) -> Result<StreamInfo> {
        let undrained = self.flush()?.len();
        ensure!(
            undrained == 0,
            "finish found {undrained} undrained packets; drain after flush"
        );
        self.stream_info()
    }
}

impl Drop for VideoToolboxEncoder {
    fn drop(&mut self) {
        self.packets = None;
        unsafe {
            self.session.complete_frames(kCMTimeInvalid);
            self.session.invalidate();
        }
    }
}
