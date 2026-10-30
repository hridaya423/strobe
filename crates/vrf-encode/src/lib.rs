use anyhow::Result;
use serde::{Deserialize, Serialize};

#[cfg(target_os = "macos")]
pub mod audiotoolbox;
#[cfg(target_os = "macos")]
mod avcc;
#[cfg(any(windows, test))]
mod bitstream;
#[cfg(windows)]
pub mod nvenc;
#[cfg(target_os = "macos")]
pub mod turbo;
#[cfg(target_os = "macos")]
pub mod videotoolbox;
#[cfg(target_os = "macos")]
pub mod x264;

pub const AAC_FRAME_SAMPLES: u32 = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AacConfig {
    pub sample_rate: u32,
    pub channels: u32,
    pub bit_rate: u32,
}

/// An AAC-LC elementary stream. `packets` hold `AAC_FRAME_SAMPLES` samples each. The decoder
/// outputs `priming_frames` samples of encoder delay first, then `frames` samples of the input,
/// then padding up to the packet boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AacStream {
    pub config: AacConfig,
    /// The two-byte `AudioSpecificConfig` that goes inside the `esds` box.
    pub audio_specific_config: Vec<u8>,
    pub packets: Vec<Vec<u8>>,
    pub priming_frames: u32,
    pub frames: u64,
    pub max_packet_bytes: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rational {
    pub num: i64,
    pub den: i64,
}

impl Rational {
    pub const fn new(num: i64, den: i64) -> Self {
        Rational { num, den }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Codec {
    H264,
    Hevc,
    Av1,
    ProRes422,
}

/// Rate control presets. Each value is a VideoToolbox `Quality` setting. Encoding Remotion's own
/// frames of `rm-4-0-trailer`, `rm-github-unwrapped` and `rm-transitions-video`, `Balanced` scores
/// within 10% of Remotion's MP4 on every FLIP metric; 0.70 did not on the trailer's mean. The
/// libx264 CRF of `Balanced` passes the same bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Quality {
    /// `Quality` 0.50, 3 to 5 Mb/s at 1080p.
    Draft,
    /// `Quality` 0.85.
    #[default]
    Balanced,
    /// `Quality` 0.90. About 13% slower than `Balanced` on the Mac mini's H.264 ceiling.
    High,
}

impl Quality {
    pub fn value(self) -> f32 {
        match self {
            Quality::Draft => 0.50,
            Quality::Balanced => 0.85,
            Quality::High => 0.90,
        }
    }

    /// The libx264 `veryfast` CRF for the CPU lane of a turbo encode, so that its chunks pass
    /// the same bar as VideoToolbox's.
    pub fn x264_crf(self) -> u32 {
        match self {
            Quality::Draft => 25,
            Quality::Balanced => 16,
            Quality::High => 12,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EncoderConfig {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    pub frame_rate: Rational,
    pub keyframe_interval: u32,
    pub quality: Quality,
    /// 10-bit from P010 input: HEVC Main10, or AV1 on NVENC. Rejected for other codecs.
    pub ten_bit: bool,
}

impl EncoderConfig {
    pub fn new(codec: Codec, width: u32, height: u32, frame_rate: Rational) -> Self {
        EncoderConfig {
            codec,
            width,
            height,
            frame_rate,
            keyframe_interval: 240,
            quality: Quality::default(),
            ten_bit: false,
        }
    }
}

/// One compressed frame. `data` is length-prefixed (AVCC or HVCC) sample data
/// for H.264 and HEVC, never Annex B, and a run of size-delimited OBUs without
/// the temporal delimiter for AV1. Times are in seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct Packet {
    pub data: Vec<u8>,
    pub pts: Rational,
    pub dts: Rational,
    pub keyframe: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StreamInfo {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    /// `1 / frame_rate.num`, so frame `i` lands on a whole tick.
    pub timebase: Rational,
    /// The payload of the `avcC` (H.264), `hvcC` (HEVC) or `av1C` (AV1) box,
    /// with the parameter sets or sequence header. Empty for ProRes.
    pub config: Vec<u8>,
}

pub enum EncoderFrame {
    #[cfg(target_os = "macos")]
    PixelBuffer(videotoolbox::pool::PoolBuffer),
    #[cfg(windows)]
    D3d12(nvenc::PoolFrame),
}

/// Packets queue without bound until `drain`; the pixel-buffer pool bounds frames in flight.
pub trait Encoder {
    fn submit(&mut self, frame: EncoderFrame, pts: Rational) -> Result<()>;

    fn drain(&mut self) -> Result<Vec<Packet>>;

    /// Waits for every submitted frame and returns the packets not yet drained.
    fn flush(&mut self) -> Result<Vec<Packet>>;

    /// The stream parameters, known once the first packet has been drained.
    fn stream_info(&self) -> Result<StreamInfo>;

    /// Fails if packets are still undrained, so none are dropped silently.
    fn finish(self) -> Result<StreamInfo>;
}
