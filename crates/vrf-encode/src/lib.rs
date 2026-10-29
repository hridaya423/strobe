use anyhow::Result;

#[cfg(target_os = "macos")]
pub mod videotoolbox;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    H264,
    Hevc,
    ProRes422,
}

/// Rate control presets. Each value is a VideoToolbox `Quality` setting probed
/// against libx264 veryfast CRF 18 with VMAF on two 1080p motion-graphics
/// clips (`docs/research/2026-10-29/02-encode.md` §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Quality {
    /// `Quality` 0.50: VMAF 90.1 and 91.9, 3 to 5 Mb/s.
    Draft,
    /// `Quality` 0.70: VMAF 96.9 and 98.3, at or above libx264 veryfast CRF 18 (95.6 and 97.1).
    #[default]
    Balanced,
    /// `Quality` 0.85: VMAF 98.3 and 99.4, 25 to 33 Mb/s.
    High,
}

impl Quality {
    pub fn value(self) -> f32 {
        match self {
            Quality::Draft => 0.50,
            Quality::Balanced => 0.70,
            Quality::High => 0.85,
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
    /// HEVC Main10 from P010 input. Rejected for other codecs.
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
/// for H.264 and HEVC, never Annex B. Times are in seconds.
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
    /// The payload of the `avcC` (H.264) or `hvcC` (HEVC) box, with the
    /// parameter sets. Empty for ProRes.
    pub config: Vec<u8>,
}

pub enum EncoderFrame {
    #[cfg(target_os = "macos")]
    PixelBuffer(videotoolbox::pool::PoolBuffer),
}

/// Packets queue without bound until `drain`; the pixel-buffer pool bounds frames in flight.
pub trait Encoder {
    fn submit(&mut self, frame: EncoderFrame, pts: Rational) -> Result<()>;

    fn drain(&mut self) -> Result<Vec<Packet>>;

    /// Waits for every submitted frame and returns the packets not yet drained.
    fn flush(&mut self) -> Result<Vec<Packet>>;

    /// Fails if packets are still undrained, so none are dropped silently.
    fn finish(self) -> Result<StreamInfo>;
}
