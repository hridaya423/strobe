use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::thread::JoinHandle;

use anyhow::{Context, Result, bail, ensure};
use nvidia_video_codec_sdk::sys::nvEncodeAPI as nv;
use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;
use windows::Win32::Graphics::Direct3D12::{
    D3D12_FENCE_FLAG_NONE, D3D12_HEAP_FLAG_NONE, D3D12_HEAP_PROPERTIES, D3D12_HEAP_TYPE_READBACK,
    D3D12_RESOURCE_DESC, D3D12_RESOURCE_DIMENSION_BUFFER, D3D12_RESOURCE_FLAG_NONE,
    D3D12_RESOURCE_STATE_COPY_DEST, D3D12_TEXTURE_LAYOUT_ROW_MAJOR, D3D12CreateDevice,
    ID3D12Device, ID3D12Fence, ID3D12Resource,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
use windows::core::Interface;

use crate::bitstream;
use crate::{Codec, Encoder, EncoderConfig, EncoderFrame, Packet, Quality, Rational, StreamInfo};

mod api;
#[allow(dead_code)]
mod guid;
mod pool;
mod sessions;
#[allow(dead_code)]
mod version;

use api::{Event, Session, zeroed};

pub use pool::{InputPool, POOL_SIZE, PixelFormat, PoolFrame, Ready};
pub use sessions::NvencSessions;

const NVIDIA: u32 = 0x10DE;

static SETUP: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    P1,
    P2,
    P3,
    P4,
    P5,
    P6,
    P7,
}

impl Preset {
    fn guid(self) -> nv::GUID {
        match self {
            Preset::P1 => guid::NV_ENC_PRESET_P1_GUID,
            Preset::P2 => guid::NV_ENC_PRESET_P2_GUID,
            Preset::P3 => guid::NV_ENC_PRESET_P3_GUID,
            Preset::P4 => guid::NV_ENC_PRESET_P4_GUID,
            Preset::P5 => guid::NV_ENC_PRESET_P5_GUID,
            Preset::P6 => guid::NV_ENC_PRESET_P6_GUID,
            Preset::P7 => guid::NV_ENC_PRESET_P7_GUID,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tuning {
    HighQuality,
    LowLatency,
    UltraLowLatency,
}

impl Tuning {
    fn info(self) -> nv::NV_ENC_TUNING_INFO {
        match self {
            Tuning::HighQuality => nv::NV_ENC_TUNING_INFO::NV_ENC_TUNING_INFO_HIGH_QUALITY,
            Tuning::LowLatency => nv::NV_ENC_TUNING_INFO::NV_ENC_TUNING_INFO_LOW_LATENCY,
            Tuning::UltraLowLatency => nv::NV_ENC_TUNING_INFO::NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateControl {
    /// Constant bitrate in bits per second.
    Cbr(u32),
    /// Variable bitrate with this average and maximum, in bits per second.
    Vbr(u32),
    /// Variable bitrate at a constant quality level, 1 (best) to 51.
    ConstantQuality(u8),
}

/// How the NVENC session is set up, beyond what [`EncoderConfig`] says.
///
/// `From<Quality>` picks the high-quality tuning and constant quality for every level: in spike
/// 6.7 that tuning ran at least as fast as low latency for all three codecs, and HEVC low latency
/// ran slower because its auto-selected reference count is larger. `Balanced` is calibrated
/// against the delivered-file rule in `docs/spikes/6.9-nvenc-quality.md`; `Draft` and `High` are
/// not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub preset: Preset,
    pub tuning: Tuning,
    pub rate_control: RateControl,
    /// Consecutive B-frames. Zero keeps output order equal to input order.
    pub b_frames: u32,
}

impl From<Quality> for Options {
    fn from(quality: Quality) -> Options {
        let (preset, level) = match quality {
            Quality::Draft => (Preset::P2, 33),
            Quality::Balanced => (Preset::P1, 17),
            Quality::High => (Preset::P7, 15),
        };
        Options {
            preset,
            tuning: Tuning::HighQuality,
            rate_control: RateControl::ConstantQuality(level),
            b_frames: 0,
        }
    }
}

fn codec_guid(codec: Codec) -> Result<nv::GUID> {
    match codec {
        Codec::H264 => Ok(guid::NV_ENC_CODEC_H264_GUID),
        Codec::Hevc => Ok(guid::NV_ENC_CODEC_HEVC_GUID),
        Codec::Av1 => Ok(guid::NV_ENC_CODEC_AV1_GUID),
        Codec::ProRes422 => bail!("NVENC has no ProRes encoder"),
    }
}

/// The first NVIDIA adapter, as a D3D12 device.
pub fn nvidia_device() -> Result<ID3D12Device> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
    let mut index = 0;
    while let Ok(adapter) = unsafe { factory.EnumAdapters1(index) } {
        index += 1;
        if unsafe { adapter.GetDesc1()? }.VendorId == NVIDIA {
            let mut device = None;
            unsafe { D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut device)? };
            return device.context("D3D12CreateDevice returned no device");
        }
    }
    bail!("no NVIDIA adapter");
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodecCapabilities {
    pub codec: Codec,
    pub max_width: u32,
    pub max_height: u32,
    pub max_b_frames: u32,
    pub ten_bit: bool,
    pub lookahead: bool,
    pub engines: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    /// The newest NVENC API the driver supports, as (major, minor).
    pub api_version: (u32, u32),
    pub codecs: Vec<CodecCapabilities>,
}

pub fn capabilities(device: &ID3D12Device) -> Result<Capabilities> {
    let session = Session::open(device)?;
    let mut codecs = Vec::new();
    for codec in [Codec::H264, Codec::Hevc, Codec::Av1] {
        let id = codec_guid(codec)?;
        if !session.guids()?.contains(&id) {
            continue;
        }
        let cap = |cap| session.cap(id, cap);
        codecs.push(CodecCapabilities {
            codec,
            max_width: cap(nv::NV_ENC_CAPS::NV_ENC_CAPS_WIDTH_MAX)? as u32,
            max_height: cap(nv::NV_ENC_CAPS::NV_ENC_CAPS_HEIGHT_MAX)? as u32,
            max_b_frames: cap(nv::NV_ENC_CAPS::NV_ENC_CAPS_NUM_MAX_BFRAMES)? as u32,
            ten_bit: cap(nv::NV_ENC_CAPS::NV_ENC_CAPS_SUPPORT_10BIT_ENCODE)? != 0,
            lookahead: cap(nv::NV_ENC_CAPS::NV_ENC_CAPS_SUPPORT_LOOKAHEAD)? != 0,
            engines: cap(nv::NV_ENC_CAPS::NV_ENC_CAPS_NUM_ENCODER_ENGINES)? as u32,
        });
    }
    Ok(Capabilities {
        api_version: (session.api_version >> 4, session.api_version & 0xf),
        codecs,
    })
}

fn set_vui(vui: &mut nv::NV_ENC_CONFIG_H264_VUI_PARAMETERS) {
    vui.videoSignalTypePresentFlag = 1;
    vui.videoFormat = nv::NV_ENC_VUI_VIDEO_FORMAT::NV_ENC_VUI_VIDEO_FORMAT_UNSPECIFIED;
    vui.videoFullRangeFlag = 0;
    vui.colourDescriptionPresentFlag = 1;
    vui.colourPrimaries = nv::NV_ENC_VUI_COLOR_PRIMARIES::NV_ENC_VUI_COLOR_PRIMARIES_BT709;
    vui.transferCharacteristics =
        nv::NV_ENC_VUI_TRANSFER_CHARACTERISTIC::NV_ENC_VUI_TRANSFER_CHARACTERISTIC_BT709;
    vui.colourMatrix = nv::NV_ENC_VUI_MATRIX_COEFFS::NV_ENC_VUI_MATRIX_COEFFS_BT709;
}

fn configure(
    session: &Session,
    config: &EncoderConfig,
    options: &Options,
) -> Result<nv::NV_ENC_CONFIG> {
    let codec = codec_guid(config.codec)?;
    let mut preset: nv::NV_ENC_PRESET_CONFIG = unsafe { zeroed() };
    preset.version = version::NV_ENC_PRESET_CONFIG_VER;
    preset.presetCfg.version = version::NV_ENC_CONFIG_VER;
    {
        let _guard = session.guard();
        session.check(
            unsafe {
                (session.functions.nvEncGetEncodePresetConfigEx.unwrap())(
                    session.handle,
                    codec,
                    options.preset.guid(),
                    options.tuning.info(),
                    &mut preset,
                )
            },
            "reading the preset configuration",
        )?;
    }
    let mut cfg = preset.presetCfg;
    cfg.gopLength = config.keyframe_interval;
    cfg.frameIntervalP = options.b_frames as i32 + 1;
    let rc = &mut cfg.rcParams;
    match options.rate_control {
        RateControl::Cbr(bits) => {
            rc.rateControlMode = nv::NV_ENC_PARAMS_RC_MODE::NV_ENC_PARAMS_RC_CBR;
            rc.averageBitRate = bits;
            rc.maxBitRate = bits;
        }
        RateControl::Vbr(bits) => {
            rc.rateControlMode = nv::NV_ENC_PARAMS_RC_MODE::NV_ENC_PARAMS_RC_VBR;
            rc.averageBitRate = bits;
            rc.maxBitRate = bits;
        }
        RateControl::ConstantQuality(level) => {
            rc.rateControlMode = nv::NV_ENC_PARAMS_RC_MODE::NV_ENC_PARAMS_RC_VBR;
            rc.averageBitRate = 0;
            rc.maxBitRate = 0;
            rc.targetQuality = level;
        }
    }
    let depth = if config.ten_bit { 2 } else { 0 };
    unsafe {
        match config.codec {
            Codec::H264 => {
                let h264 = &mut cfg.encodeCodecConfig.h264Config;
                h264.idrPeriod = config.keyframe_interval;
                h264.chromaFormatIDC = 1;
                h264.set_repeatSPSPPS(1);
                h264.set_outputAUD(0);
                set_vui(&mut h264.h264VUIParameters);
            }
            Codec::Hevc => {
                let hevc = &mut cfg.encodeCodecConfig.hevcConfig;
                hevc.idrPeriod = config.keyframe_interval;
                hevc.set_chromaFormatIDC(1);
                hevc.set_pixelBitDepthMinus8(depth);
                hevc.set_repeatSPSPPS(1);
                hevc.set_outputAUD(0);
                set_vui(&mut hevc.hevcVUIParameters);
                if config.ten_bit {
                    cfg.profileGUID = guid::NV_ENC_HEVC_PROFILE_MAIN10_GUID;
                }
            }
            Codec::Av1 => {
                let av1 = &mut cfg.encodeCodecConfig.av1Config;
                av1.idrPeriod = config.keyframe_interval;
                av1.set_chromaFormatIDC(1);
                av1.set_pixelBitDepthMinus8(depth);
                av1.set_inputPixelBitDepthMinus8(depth);
                av1.set_repeatSeqHdr(1);
                av1.set_outputAnnexBFormat(0);
                av1.set_enableTimingInfo(0);
                av1.colorPrimaries =
                    nv::NV_ENC_VUI_COLOR_PRIMARIES::NV_ENC_VUI_COLOR_PRIMARIES_BT709;
                av1.transferCharacteristics =
                    nv::NV_ENC_VUI_TRANSFER_CHARACTERISTIC::NV_ENC_VUI_TRANSFER_CHARACTERISTIC_BT709;
                av1.matrixCoefficients =
                    nv::NV_ENC_VUI_MATRIX_COEFFS::NV_ENC_VUI_MATRIX_COEFFS_BT709;
                av1.colorRange = 0;
            }
            Codec::ProRes422 => unreachable!("rejected before the session opens"),
        }
    }
    Ok(cfg)
}

fn buffer_desc(size: u64) -> D3D12_RESOURCE_DESC {
    D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Width: size,
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: DXGI_FORMAT_UNKNOWN,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        Flags: D3D12_RESOURCE_FLAG_NONE,
        ..Default::default()
    }
}

struct Output {
    _resource: ID3D12Resource,
    registered: *mut c_void,
    mapped: *mut c_void,
    descriptor: *mut nv::NV_ENC_OUTPUT_RESOURCE_D3D12,
}

struct Job {
    slot: usize,
    value: u64,
}

struct Flight {
    _frame: PoolFrame,
    pts: Rational,
}

#[derive(Default)]
struct State {
    free_outputs: Vec<usize>,
    jobs: VecDeque<Job>,
    flights: HashMap<u64, Flight>,
    pts: VecDeque<Rational>,
    collected: u64,
    error: Option<String>,
    closing: bool,
}

struct Shared {
    session: Session,
    codec: Codec,
    ten_bit: bool,
    fence: ID3D12Fence,
    outputs: Vec<Output>,
    state: Mutex<State>,
    changed: Condvar,
    sender: Sender<Packet>,
    config: OnceLock<Vec<u8>>,
    frame_duration: Rational,
    delay: i64,
}

unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

fn behind(time: Rational, frames: i64, duration: Rational) -> Rational {
    if frames == 0 {
        return time;
    }
    Rational::new(
        time.num * duration.den - frames * duration.num * time.den,
        time.den * duration.den,
    )
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn wait<'a>(&self, state: MutexGuard<'a, State>) -> MutexGuard<'a, State> {
        self.changed
            .wait(state)
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn check(&self) -> Result<()> {
        match &self.state().error {
            Some(error) => bail!("{error}"),
            None => Ok(()),
        }
    }

    fn fail(&self, error: anyhow::Error) {
        self.state().error.get_or_insert(format!("{error:#}"));
        self.changed.notify_all();
    }

    fn take_output(&self) -> Result<usize> {
        let mut state = self.state();
        loop {
            if let Some(error) = &state.error {
                bail!("{error}");
            }
            if let Some(slot) = state.free_outputs.pop() {
                return Ok(slot);
            }
            state = self.wait(state);
        }
    }

    fn wait_collected(&self, count: u64) -> Result<()> {
        let mut state = self.state();
        loop {
            if let Some(error) = &state.error {
                bail!("{error}");
            }
            if state.collected >= count {
                return Ok(());
            }
            state = self.wait(state);
        }
    }

    fn next_job(&self) -> Option<Job> {
        let mut state = self.state();
        loop {
            if let Some(job) = state.jobs.pop_front() {
                return Some(job);
            }
            if state.closing {
                return None;
            }
            state = self.wait(state);
        }
    }

    fn collect(&self, event: &Event, job: Job) -> Result<()> {
        let output = &self.outputs[job.slot];
        event.wait(&self.fence, job.value)?;
        let (bytes, index) = {
            let _guard = self.session.guard();
            let mut lock: nv::NV_ENC_LOCK_BITSTREAM = unsafe { zeroed() };
            lock.version = version::NV_ENC_LOCK_BITSTREAM_VER;
            lock.outputBitstream = output.descriptor.cast();
            self.session.check(
                unsafe {
                    (self.session.functions.nvEncLockBitstream.unwrap())(
                        self.session.handle,
                        &mut lock,
                    )
                },
                "locking a bitstream",
            )?;
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    lock.bitstreamBufferPtr.cast::<u8>(),
                    lock.bitstreamSizeInBytes as usize,
                )
            }
            .to_vec();
            self.session.check(
                unsafe {
                    (self.session.functions.nvEncUnlockBitstream.unwrap())(
                        self.session.handle,
                        lock.outputBitstream,
                    )
                },
                "unlocking a bitstream",
            )?;
            (bytes, lock.outputTimeStamp)
        };
        let (flight, ordinal_pts) = {
            let mut state = self.state();
            state.free_outputs.push(job.slot);
            let flight = state.flights.remove(&index);
            (flight, state.pts.pop_front())
        };
        self.changed.notify_all();
        let flight = flight.context("NVENC returned a picture that was never submitted")?;
        let ordinal_pts =
            ordinal_pts.context("NVENC returned more pictures than were submitted")?;
        let access = bitstream::convert(self.codec, &bytes)?;
        if self.config.get().is_none() && !access.parameters.is_empty() {
            let record = bitstream::config_record(self.codec, self.ten_bit, &access.parameters)?;
            let _ = self.config.set(record);
        }
        let packet = Packet {
            data: access.data,
            pts: flight.pts,
            dts: behind(ordinal_pts, self.delay, self.frame_duration),
            keyframe: access.keyframe,
        };
        drop(flight);
        let _ = self.sender.send(packet);
        self.state().collected += 1;
        self.changed.notify_all();
        Ok(())
    }

    fn unregister(&self) {
        let _guard = self.session.guard();
        for output in &self.outputs {
            unsafe {
                (self.session.functions.nvEncUnmapInputResource.unwrap())(
                    self.session.handle,
                    output.mapped,
                );
                (self.session.functions.nvEncUnregisterResource.unwrap())(
                    self.session.handle,
                    output.registered,
                );
            }
        }
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        for output in &self.outputs {
            drop(unsafe { Box::from_raw(output.descriptor) });
        }
    }
}

fn collect_loop(shared: Arc<Shared>) {
    let event = match Event::new() {
        Ok(event) => event,
        Err(error) => return shared.fail(error),
    };
    while let Some(job) = shared.next_job() {
        if let Err(error) = shared.collect(&event, job) {
            return shared.fail(error);
        }
    }
}

pub struct NvencEncoder {
    config: EncoderConfig,
    shared: Arc<Shared>,
    pool: InputPool,
    packets: Receiver<Packet>,
    collector: Option<JoinHandle<()>>,
    input: Box<nv::NV_ENC_INPUT_RESOURCE_D3D12>,
    submitted: u64,
    last_pts: Option<Rational>,
    ended: bool,
}

// The NVENC session is serialised by `Session::guard`; the encoder is only moved, never shared.
unsafe impl Send for NvencEncoder {}

impl NvencEncoder {
    /// Opens an NVENC session on `device`. The device must be the one the frames are rendered
    /// on, because the pool textures are created on it.
    pub fn new(device: &ID3D12Device, config: EncoderConfig, options: Options) -> Result<Self> {
        Self::with_pool_size(device, config, options, POOL_SIZE)
    }

    /// Like [`NvencEncoder::new`], with `pool_size` input textures instead of [`POOL_SIZE`]. The
    /// output ring keeps its depth: it bounds the frames inside the encoder, not the ones waiting
    /// for it.
    pub fn with_pool_size(
        device: &ID3D12Device,
        config: EncoderConfig,
        options: Options,
        pool_size: usize,
    ) -> Result<Self> {
        ensure!(pool_size > 0, "the pool needs at least one texture");
        ensure!(
            !config.ten_bit || matches!(config.codec, Codec::Hevc | Codec::Av1),
            "ten_bit is only supported for HEVC and AV1"
        );
        ensure!(
            config.frame_rate.num > 0 && config.frame_rate.den > 0,
            "frame rate must be positive"
        );
        let id = codec_guid(config.codec)?;
        let _setup = SETUP
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let session = Session::open(device)?;
        ensure!(
            session.guids()?.contains(&id),
            "this GPU has no {:?} encoder",
            config.codec
        );
        ensure!(
            !config.ten_bit
                || session.cap(id, nv::NV_ENC_CAPS::NV_ENC_CAPS_SUPPORT_10BIT_ENCODE)? != 0,
            "this GPU cannot encode 10-bit {:?}",
            config.codec
        );
        let (max_width, max_height) = (
            session.cap(id, nv::NV_ENC_CAPS::NV_ENC_CAPS_WIDTH_MAX)? as u32,
            session.cap(id, nv::NV_ENC_CAPS::NV_ENC_CAPS_HEIGHT_MAX)? as u32,
        );
        ensure!(
            config.width <= max_width && config.height <= max_height,
            "{:?} is limited to {max_width}x{max_height} on this GPU",
            config.codec
        );
        let max_b_frames = session.cap(id, nv::NV_ENC_CAPS::NV_ENC_CAPS_NUM_MAX_BFRAMES)? as u32;
        ensure!(
            options.b_frames <= max_b_frames,
            "{:?} supports {max_b_frames} B-frames on this GPU, {} were requested",
            config.codec,
            options.b_frames
        );

        let mut cfg = configure(&session, &config, &options)?;
        let mut init: nv::NV_ENC_INITIALIZE_PARAMS = unsafe { zeroed() };
        init.version = version::NV_ENC_INITIALIZE_PARAMS_VER;
        init.encodeGUID = id;
        init.presetGUID = options.preset.guid();
        init.encodeWidth = config.width;
        init.encodeHeight = config.height;
        init.darWidth = config.width;
        init.darHeight = config.height;
        init.frameRateNum = config.frame_rate.num as u32;
        init.frameRateDen = config.frame_rate.den as u32;
        init.enableEncodeAsync = 0;
        init.enablePTD = 1;
        init.encodeConfig = &mut cfg;
        init.maxEncodeWidth = config.width;
        init.maxEncodeHeight = config.height;
        init.tuningInfo = options.tuning.info();
        let format = if config.ten_bit {
            PixelFormat::P010
        } else {
            PixelFormat::Nv12
        };
        init.bufferFormat = format.buffer_format();
        {
            let _guard = session.guard();
            session.check(
                unsafe {
                    (session.functions.nvEncInitializeEncoder.unwrap())(session.handle, &mut init)
                },
                "initializing the encoder",
            )?;
        }

        let fence: ID3D12Fence = unsafe { device.CreateFence(0, D3D12_FENCE_FLAG_NONE)? };
        let pool = InputPool::new(
            &session,
            device,
            &fence,
            config.width,
            config.height,
            format,
            pool_size,
        )?;

        let bytes = if config.ten_bit { 2 } else { 1 };
        let size = (config.width as u64 * config.height as u64 * 3 / 2 * bytes).max(1 << 20);
        let mut outputs = Vec::with_capacity(POOL_SIZE);
        for _ in 0..POOL_SIZE {
            let mut resource: Option<ID3D12Resource> = None;
            unsafe {
                device.CreateCommittedResource(
                    &D3D12_HEAP_PROPERTIES {
                        Type: D3D12_HEAP_TYPE_READBACK,
                        ..Default::default()
                    },
                    D3D12_HEAP_FLAG_NONE,
                    &buffer_desc(size),
                    D3D12_RESOURCE_STATE_COPY_DEST,
                    None,
                    &mut resource,
                )?;
            }
            let resource = resource.context("no output buffer returned")?;
            let _guard = session.guard();
            let (registered, mapped) = unsafe {
                let mut register: nv::NV_ENC_REGISTER_RESOURCE = zeroed();
                register.version = version::NV_ENC_REGISTER_RESOURCE_VER;
                register.resourceType =
                    nv::NV_ENC_INPUT_RESOURCE_TYPE::NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX;
                register.width = size as u32;
                register.height = 1;
                register.resourceToRegister = resource.as_raw();
                register.bufferFormat = nv::NV_ENC_BUFFER_FORMAT::NV_ENC_BUFFER_FORMAT_U8;
                register.bufferUsage = nv::NV_ENC_BUFFER_USAGE::NV_ENC_OUTPUT_BITSTREAM;
                session.check(
                    (session.functions.nvEncRegisterResource.unwrap())(
                        session.handle,
                        &mut register,
                    ),
                    "registering an output buffer",
                )?;
                let mut map: nv::NV_ENC_MAP_INPUT_RESOURCE = zeroed();
                map.version = version::NV_ENC_MAP_INPUT_RESOURCE_VER;
                map.registeredResource = register.registeredResource;
                session.check(
                    (session.functions.nvEncMapInputResource.unwrap())(session.handle, &mut map),
                    "mapping an output buffer",
                )?;
                (register.registeredResource, map.mappedResource)
            };
            let mut descriptor: Box<nv::NV_ENC_OUTPUT_RESOURCE_D3D12> =
                Box::new(unsafe { zeroed() });
            descriptor.version = version::NV_ENC_OUTPUT_RESOURCE_D3D12_VER;
            descriptor.pOutputBuffer = mapped;
            descriptor.outputFencePoint.version = version::NV_ENC_FENCE_POINT_D3D12_VER;
            descriptor.outputFencePoint.pFence = fence.as_raw();
            descriptor.outputFencePoint.set_bSignal(1);
            outputs.push(Output {
                _resource: resource,
                registered,
                mapped,
                descriptor: Box::into_raw(descriptor),
            });
        }

        let mut input: Box<nv::NV_ENC_INPUT_RESOURCE_D3D12> = Box::new(unsafe { zeroed() });
        input.version = version::NV_ENC_INPUT_RESOURCE_D3D12_VER;
        input.inputFencePoint.version = version::NV_ENC_FENCE_POINT_D3D12_VER;
        input.inputFencePoint.pFence = fence.as_raw();

        let (sender, packets) = channel();
        let shared = Arc::new(Shared {
            session,
            codec: config.codec,
            ten_bit: config.ten_bit,
            fence,
            state: Mutex::new(State {
                free_outputs: (0..outputs.len()).rev().collect(),
                ..State::default()
            }),
            outputs,
            changed: Condvar::new(),
            sender,
            config: OnceLock::new(),
            frame_duration: Rational::new(config.frame_rate.den, config.frame_rate.num),
            delay: if config.codec == Codec::Av1 {
                0
            } else {
                options.b_frames as i64
            },
        });
        let collector = std::thread::Builder::new()
            .name("nvenc-collect".into())
            .spawn({
                let shared = shared.clone();
                move || collect_loop(shared)
            })?;
        Ok(NvencEncoder {
            config,
            shared,
            pool,
            packets,
            collector: Some(collector),
            input,
            submitted: 0,
            last_pts: None,
            ended: false,
        })
    }

    /// The textures to write frames into. Clone it to share it with a render thread.
    pub fn pool(&self) -> &InputPool {
        &self.pool
    }

    fn end_stream(&mut self) -> Result<()> {
        if self.ended {
            return Ok(());
        }
        self.ended = true;
        let session = &self.shared.session;
        let mut eos: nv::NV_ENC_PIC_PARAMS = unsafe { zeroed() };
        eos.version = version::NV_ENC_PIC_PARAMS_VER;
        eos.encodePicFlags = nv::NV_ENC_PIC_FLAGS::NV_ENC_PIC_FLAG_EOS as u32;
        let _guard = session.guard();
        session.check(
            unsafe { (session.functions.nvEncEncodePicture.unwrap())(session.handle, &mut eos) },
            "ending the stream",
        )
    }
}

impl NvencEncoder {
    /// Submits a frame, as an IDR frame if `keyframe` is set.
    pub fn submit_keyframe(
        &mut self,
        frame: EncoderFrame,
        pts: Rational,
        keyframe: bool,
    ) -> Result<()> {
        let EncoderFrame::D3d12(frame) = frame;
        ensure!(
            frame.pool_is(&self.pool),
            "the frame comes from another encoder's pool"
        );
        ensure!(!self.ended, "submit after flush: flushing ends the stream");
        ensure!(
            self.last_pts
                .is_none_or(|last| i128::from(pts.num) * i128::from(last.den)
                    > i128::from(last.num) * i128::from(pts.den)),
            "pts must increase with every frame"
        );
        let shared = &self.shared;
        let slot = shared.take_output()?;
        let index = self.submitted;
        let output = &shared.outputs[slot];
        unsafe { (*output.descriptor).outputFencePoint.signalValue = index + 1 };
        let point = &mut self.input.inputFencePoint;
        match &frame.ready {
            Some(ready) => {
                point.pFence = ready.fence.as_raw();
                point.waitValue = ready.value;
                point.set_bWait(1);
            }
            None => point.set_bWait(0),
        }
        self.input.pInputBuffer = frame.mapped();
        {
            let mut state = shared.state();
            state.flights.insert(index, Flight { _frame: frame, pts });
            state.pts.push_back(pts);
        }
        let session = &shared.session;
        let mut pic: nv::NV_ENC_PIC_PARAMS = unsafe { zeroed() };
        pic.version = version::NV_ENC_PIC_PARAMS_VER;
        pic.inputWidth = self.config.width;
        pic.inputHeight = self.config.height;
        pic.inputBuffer = (&mut *self.input as *mut nv::NV_ENC_INPUT_RESOURCE_D3D12).cast();
        pic.outputBitstream = output.descriptor.cast();
        pic.bufferFmt = self.pool.format().buffer_format();
        pic.pictureStruct = nv::NV_ENC_PIC_STRUCT::NV_ENC_PIC_STRUCT_FRAME;
        pic.inputTimeStamp = index;
        if keyframe {
            pic.encodePicFlags = nv::NV_ENC_PIC_FLAGS::NV_ENC_PIC_FLAG_FORCEIDR as u32;
        }
        let status = {
            let _guard = session.guard();
            unsafe { (session.functions.nvEncEncodePicture.unwrap())(session.handle, &mut pic) }
        };
        if status != nv::NVENCSTATUS::NV_ENC_ERR_NEED_MORE_INPUT {
            if let Err(error) = session.check(status, "encoding a frame") {
                let mut state = shared.state();
                state.flights.remove(&index);
                state.pts.pop_back();
                state.free_outputs.push(slot);
                return Err(error);
            }
        }
        shared.state().jobs.push_back(Job {
            slot,
            value: index + 1,
        });
        shared.changed.notify_all();
        self.submitted += 1;
        self.last_pts = Some(pts);
        Ok(())
    }
}

impl Encoder for NvencEncoder {
    fn submit(&mut self, frame: EncoderFrame, pts: Rational) -> Result<()> {
        self.submit_keyframe(frame, pts, false)
    }

    fn drain(&mut self) -> Result<Vec<Packet>> {
        self.shared.check()?;
        Ok(self.packets.try_iter().collect())
    }

    fn flush(&mut self) -> Result<Vec<Packet>> {
        self.end_stream()?;
        self.shared.wait_collected(self.submitted)?;
        self.drain()
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

impl Drop for NvencEncoder {
    fn drop(&mut self) {
        let _ = self.end_stream();
        let _ = self.shared.wait_collected(self.submitted);
        self.shared.state().closing = true;
        self.shared.changed.notify_all();
        if let Some(collector) = self.collector.take() {
            let _ = collector.join();
        }
        self.pool.unregister(&self.shared.session);
        self.shared.unregister();
    }
}
