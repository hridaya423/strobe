use std::ffi::{CString, c_char, c_int, c_void};
use std::path::PathBuf;
use std::ptr::{null, null_mut};

use anyhow::{Context, Result, bail, ensure};
use libloading::Library;
use objc2_io_surface::IOSurfaceLockOptions;

use crate::avcc::AvcRecord;
use crate::{Codec, Encoder, EncoderConfig, EncoderFrame, Packet, Rational, StreamInfo};

const CSP_NV12: c_int = 4;
const TYPE_AUTO: c_int = 0;
const TYPE_IDR: c_int = 1;
const BUILD: u32 = 165;
const PARAM_BYTES: usize = 8192;
const PARAM_WIDTH: usize = 28;
const PARAM_HEIGHT: usize = 32;
const PARAM_CSP: usize = 36;
const PARAM_LOG_LEVEL: usize = 528;
const LOG_NONE: i32 = -1;

#[repr(C, align(16))]
struct Param([u8; PARAM_BYTES]);

#[repr(C)]
struct Nal {
    _counts: [c_int; 6],
    payload: *mut u8,
    _padding: c_int,
}

#[repr(C)]
struct Picture {
    kind: c_int,
    _qpplus1_and_structure: [c_int; 2],
    keyframe: c_int,
    pts: i64,
    dts: i64,
    _param: *mut c_void,
    csp: c_int,
    planes: c_int,
    stride: [c_int; 4],
    plane: [*const u8; 4],
    _rest: [u8; 144],
}

type ParamDefaultPreset = unsafe extern "C" fn(*mut Param, *const c_char, *const c_char) -> c_int;
type ParamApplyProfile = unsafe extern "C" fn(*mut Param, *const c_char) -> c_int;
type ParamParse = unsafe extern "C" fn(*mut Param, *const c_char, *const c_char) -> c_int;
type EncoderOpen = unsafe extern "C" fn(*mut Param) -> *mut c_void;
type EncoderHeaders = unsafe extern "C" fn(*mut c_void, *mut *mut Nal, *mut c_int) -> c_int;
type EncoderEncode = unsafe extern "C" fn(
    *mut c_void,
    *mut *mut Nal,
    *mut c_int,
    *mut Picture,
    *mut Picture,
) -> c_int;
type EncoderDelayed = unsafe extern "C" fn(*mut c_void) -> c_int;
type EncoderClose = unsafe extern "C" fn(*mut c_void);
type PictureInit = unsafe extern "C" fn(*mut Picture);

struct Api {
    _library: Library,
    param_parse: ParamParse,
    encoder_open: EncoderOpen,
    encoder_headers: EncoderHeaders,
    encoder_encode: EncoderEncode,
    encoder_delayed: EncoderDelayed,
    encoder_close: EncoderClose,
    picture_init: PictureInit,
    param_default_preset: ParamDefaultPreset,
    param_apply_profile: ParamApplyProfile,
}

fn candidates() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::env::var_os("VRF_LIBX264")
        .map(PathBuf::from)
        .into_iter()
        .collect();
    paths.extend(
        [
            "/opt/homebrew/lib/libx264.dylib",
            "/usr/local/lib/libx264.dylib",
            "libx264.dylib",
        ]
        .map(PathBuf::from),
    );
    paths
}

impl Api {
    fn load() -> Result<Api> {
        let library = candidates()
            .into_iter()
            .find_map(|path| unsafe { Library::new(path) }.ok())
            .context("libx264 was not found; install x264 or set VRF_LIBX264 to its .dylib")?;
        macro_rules! symbol {
            ($name:expr) => {
                *unsafe { library.get($name) }
                    .with_context(|| format!("libx264 has no {}", String::from_utf8_lossy($name)))?
            };
        }
        let encoder_open = *unsafe { library.get::<EncoderOpen>(b"x264_encoder_open_165\0") }
            .with_context(|| format!("libx264 is not build {BUILD}"))?;
        Ok(Api {
            param_parse: symbol!(b"x264_param_parse\0"),
            encoder_headers: symbol!(b"x264_encoder_headers\0"),
            encoder_encode: symbol!(b"x264_encoder_encode\0"),
            encoder_delayed: symbol!(b"x264_encoder_delayed_frames\0"),
            encoder_close: symbol!(b"x264_encoder_close\0"),
            picture_init: symbol!(b"x264_picture_init\0"),
            param_default_preset: symbol!(b"x264_param_default_preset\0"),
            param_apply_profile: symbol!(b"x264_param_apply_profile\0"),
            encoder_open,
            _library: library,
        })
    }
}

/// Whether a usable libx264 can be loaded on this machine.
pub fn available() -> Result<()> {
    Api::load().map(drop)
}

/// libx264 build 165, loaded at run time so that the GPL library is never linked into this
/// crate. The few `x264_param_t` fields that have no `x264_param_parse` name are written at the
/// offsets of that build's header.
///
/// Produces closed GOPs with no B-frames, `veryfast` by default, `stitchable`, with its SPS and
/// PPS ids set to 1 so that they can sit next to VideoToolbox's (id 0) in one `avcC`. Frames are
/// read straight from the pixel buffers' NV12 planes. Presentation times must be whole frames.
pub struct X264Encoder {
    api: Api,
    handle: *mut c_void,
    config: EncoderConfig,
    record: AvcRecord,
    packets: Vec<Packet>,
}

// A libx264 handle may move between threads as long as one thread uses it at a time.
unsafe impl Send for X264Encoder {}

fn headers(api: &Api, handle: *mut c_void) -> Result<AvcRecord> {
    let (mut nals, mut count) = (null_mut(), 0);
    let size = unsafe { (api.encoder_headers)(handle, &mut nals, &mut count) };
    ensure!(size > 0, "x264_encoder_headers failed");
    let mut rest = unsafe { std::slice::from_raw_parts((*nals).payload, size as usize) };
    let (mut sps, mut pps) = (Vec::new(), Vec::new());
    while !rest.is_empty() {
        ensure!(rest.len() > 4, "truncated header NAL");
        let len = u32::from_be_bytes(rest[..4].try_into()?) as usize;
        let nal = rest.get(4..4 + len).context("truncated header NAL")?;
        match nal[0] & 0x1f {
            7 => sps.push(nal.to_vec()),
            8 => pps.push(nal.to_vec()),
            _ => {}
        }
        rest = &rest[4 + len..];
    }
    AvcRecord::from_sets(sps, pps)
}

impl X264Encoder {
    /// `threads` of 0 lets libx264 choose.
    pub fn new(config: EncoderConfig, threads: u32) -> Result<X264Encoder> {
        ensure!(config.codec == Codec::H264, "libx264 encodes only H.264");
        ensure!(!config.ten_bit, "the libx264 lane is 8-bit");
        ensure!(
            config.width.is_multiple_of(2) && config.height.is_multiple_of(2),
            "libx264 needs even dimensions"
        );
        ensure!(
            config.frame_rate.num > 0 && config.frame_rate.den > 0,
            "frame rate must be positive"
        );
        let api = Api::load()?;
        let mut param = Param([0; PARAM_BYTES]);
        let preset = CString::new("veryfast")?;
        ensure!(
            unsafe { (api.param_default_preset)(&mut param, preset.as_ptr(), null()) } == 0,
            "x264_param_default_preset failed"
        );
        let crf = config.quality.x264_crf().to_string();
        let options = [
            (
                "fps",
                format!("{}/{}", config.frame_rate.num, config.frame_rate.den),
            ),
            ("threads", threads.to_string()),
            ("keyint", config.keyframe_interval.to_string()),
            ("bframes", "0".to_string()),
            ("open-gop", "0".to_string()),
            ("stitchable", "1".to_string()),
            ("repeat-headers", "0".to_string()),
            ("annexb", "0".to_string()),
            ("sps-id", "1".to_string()),
            ("colorprim", "bt709".to_string()),
            ("transfer", "bt709".to_string()),
            ("colormatrix", "bt709".to_string()),
            ("chromaloc", "0".to_string()),
            ("crf", crf),
        ];
        for (name, value) in options {
            let (key, value) = (CString::new(name)?, CString::new(value)?);
            let result = unsafe { (api.param_parse)(&mut param, key.as_ptr(), value.as_ptr()) };
            ensure!(result == 0, "libx264 rejected {name}");
        }
        for (offset, value) in [
            (PARAM_WIDTH, config.width as i32),
            (PARAM_HEIGHT, config.height as i32),
            (PARAM_CSP, CSP_NV12),
            (PARAM_LOG_LEVEL, LOG_NONE),
        ] {
            param.0[offset..offset + 4].copy_from_slice(&value.to_ne_bytes());
        }
        let profile = CString::new("high")?;
        ensure!(
            unsafe { (api.param_apply_profile)(&mut param, profile.as_ptr()) } == 0,
            "libx264 rejected the high profile"
        );
        let handle = unsafe { (api.encoder_open)(&mut param) };
        ensure!(!handle.is_null(), "x264_encoder_open failed");
        let record = headers(&api, handle)?;
        Ok(X264Encoder {
            api,
            handle,
            config,
            record,
            packets: Vec::new(),
        })
    }

    fn index(&self, pts: Rational) -> Result<i64> {
        let rate = self.config.frame_rate;
        let scaled = i128::from(pts.num) * i128::from(rate.num);
        let step = i128::from(pts.den) * i128::from(rate.den);
        ensure!(
            scaled % step == 0,
            "{}/{} s is not a whole frame at {}/{} fps",
            pts.num,
            pts.den,
            rate.num,
            rate.den
        );
        i64::try_from(scaled / step).context("frame index out of range")
    }

    fn time(&self, index: i64) -> Rational {
        let rate = self.config.frame_rate;
        Rational::new(index * rate.den, rate.num)
    }

    fn encode(&mut self, input: Option<&mut Picture>) -> Result<()> {
        let (mut nals, mut count) = (null_mut(), 0);
        let mut out = unsafe { std::mem::zeroed::<Picture>() };
        let input = input.map_or(null_mut(), |p| p as *mut Picture);
        let size = unsafe {
            (self.api.encoder_encode)(self.handle, &mut nals, &mut count, input, &mut out)
        };
        ensure!(size >= 0, "x264_encoder_encode failed");
        if size > 0 {
            let data =
                unsafe { std::slice::from_raw_parts((*nals).payload, size as usize) }.to_vec();
            self.packets.push(Packet {
                data,
                pts: self.time(out.pts),
                dts: self.time(out.dts),
                keyframe: out.keyframe != 0,
            });
        }
        Ok(())
    }

    /// Like [`Encoder::submit`], and starts a new closed GOP at this frame when `keyframe` is
    /// set.
    pub fn submit_keyframe(
        &mut self,
        frame: EncoderFrame,
        pts: Rational,
        keyframe: bool,
    ) -> Result<()> {
        let EncoderFrame::PixelBuffer(buffer) = frame;
        let surface = buffer.surface();
        unsafe { surface.lock(IOSurfaceLockOptions::ReadOnly, null_mut()) };
        let plane = |index| {
            (
                surface.base_address_of_plane(index).as_ptr().cast::<u8>(),
                surface.bytes_per_row_of_plane(index) as c_int,
            )
        };
        let ((luma, luma_stride), (chroma, chroma_stride)) = (plane(0), plane(1));
        let mut picture = unsafe { std::mem::zeroed::<Picture>() };
        unsafe { (self.api.picture_init)(&mut picture) };
        picture.kind = if keyframe { TYPE_IDR } else { TYPE_AUTO };
        picture.pts = self.index(pts)?;
        picture.csp = CSP_NV12;
        picture.planes = 2;
        picture.stride = [luma_stride, chroma_stride, 0, 0];
        picture.plane = [luma, chroma, null(), null()];
        let result = self.encode(Some(&mut picture));
        unsafe { surface.unlock(IOSurfaceLockOptions::ReadOnly, null_mut()) };
        result
    }
}

impl Encoder for X264Encoder {
    fn submit(&mut self, frame: EncoderFrame, pts: Rational) -> Result<()> {
        self.submit_keyframe(frame, pts, false)
    }

    fn drain(&mut self) -> Result<Vec<Packet>> {
        Ok(std::mem::take(&mut self.packets))
    }

    fn flush(&mut self) -> Result<Vec<Packet>> {
        while unsafe { (self.api.encoder_delayed)(self.handle) } > 0 {
            self.encode(None)?;
        }
        self.drain()
    }

    fn stream_info(&self) -> Result<StreamInfo> {
        Ok(StreamInfo {
            codec: Codec::H264,
            width: self.config.width,
            height: self.config.height,
            timebase: Rational::new(1, self.config.frame_rate.num),
            config: self.record.to_bytes(),
        })
    }

    fn finish(mut self) -> Result<StreamInfo> {
        let undrained = self.flush()?.len();
        if undrained != 0 {
            bail!("finish found {undrained} undrained packets; drain after flush");
        }
        self.stream_info()
    }
}

impl Drop for X264Encoder {
    fn drop(&mut self) {
        unsafe { (self.api.encoder_close)(self.handle) };
    }
}
