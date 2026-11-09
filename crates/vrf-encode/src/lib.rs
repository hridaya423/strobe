use anyhow::Result;
use serde::{Deserialize, Serialize};

#[cfg(target_os = "macos")]
pub mod audiotoolbox;
#[cfg(target_os = "macos")]
mod avcc;
#[cfg(any(windows, test))]
mod bitstream;
#[cfg(windows)]
pub mod d3d12ve;
#[cfg(windows)]
pub mod mediafoundation;
#[cfg(windows)]
pub mod nvenc;
#[cfg(target_os = "macos")]
pub mod turbo;
#[cfg(target_os = "macos")]
pub mod videotoolbox;
#[cfg(target_os = "macos")]
pub mod x264;

pub const AAC_FRAME_SAMPLES: u32 = 1024;

#[cfg(any(windows, target_os = "macos"))]
const AAC_LC: u32 = 2;

#[cfg(any(windows, target_os = "macos"))]
const SAMPLE_RATES: [u32; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350,
];

#[cfg(any(windows, target_os = "macos"))]
fn audio_specific_config(config: AacConfig) -> Result<Vec<u8>> {
    let Some(rate) = SAMPLE_RATES.iter().position(|&r| r == config.sample_rate) else {
        anyhow::bail!(
            "{} Hz has no AAC sampling frequency index",
            config.sample_rate
        );
    };
    let bits = (AAC_LC << 11) | ((rate as u32) << 7) | (config.channels << 3);
    Ok((bits as u16).to_be_bytes().to_vec())
}

#[cfg(any(windows, target_os = "macos", test))]
fn h264_profile_has_chroma_format(profile: u8) -> bool {
    matches!(
        profile,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134
    )
}

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

/// Rate control presets. `value` is the VideoToolbox `Quality` setting; NVENC and D3D12 Video
/// Encode map each preset themselves. Encoding Remotion's own
/// frames of `rm-4-0-trailer`, `rm-github-unwrapped` and `rm-transitions-video`, `Balanced` scores
/// within 10% of Remotion's MP4 on every FLIP metric; 0.70 did not on the trailer's mean. The
/// libx264 CRF of `Balanced` passes the same bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Quality {
    /// `Quality` 0.50, 3 to 5 Mb/s at 1080p.
    Draft,
    /// `Quality` 0.90.
    #[default]
    Balanced,
    /// `Quality` 0.90, the same VideoToolbox setting as `Balanced`. Only the libx264 lane
    /// differs: CRF 8 against 10.
    High,
}

impl Quality {
    pub fn value(self) -> f32 {
        match self {
            Quality::Draft => 0.50,
            Quality::Balanced => 0.90,
            Quality::High => 0.90,
        }
    }

    /// The libx264 `veryfast` CRF for the CPU lane of a turbo encode, so that its chunks pass
    /// the same bar as VideoToolbox's.
    pub fn x264_crf(self) -> u32 {
        match self {
            Quality::Draft => 23,
            Quality::Balanced => 10,
            Quality::High => 8,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Matrix {
    Bt601,
    #[default]
    Bt709,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Range {
    #[default]
    Limited,
    Full,
}

/// The YCbCr matrix and quantisation range of the frames an encoder takes, which it tags in the
/// stream. Primaries and transfer are always BT.709, the sRGB primaries a browser paints in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct ColourSpace {
    pub matrix: Matrix,
    pub range: Range,
}

impl ColourSpace {
    pub const BT709: ColourSpace = ColourSpace {
        matrix: Matrix::Bt709,
        range: Range::Limited,
    };
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
    pub colour: ColourSpace,
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
            colour: ColourSpace::BT709,
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
    pub colour: ColourSpace,
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

#[cfg(target_os = "macos")]
impl From<videotoolbox::pool::PoolBuffer> for EncoderFrame {
    fn from(buffer: videotoolbox::pool::PoolBuffer) -> Self {
        EncoderFrame::PixelBuffer(buffer)
    }
}

#[cfg(windows)]
impl From<nvenc::PoolFrame> for EncoderFrame {
    fn from(frame: nvenc::PoolFrame) -> Self {
        EncoderFrame::D3d12(frame)
    }
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

#[cfg(test)]
mod tests {
    use super::{AacConfig, audio_specific_config, h264_profile_has_chroma_format};

    #[test]
    fn the_audio_specific_config_packs_profile_rate_index_and_channels() {
        let config = |sample_rate, channels| AacConfig {
            sample_rate,
            channels,
            bit_rate: 128_000,
        };
        assert_eq!(audio_specific_config(config(48_000, 2)).unwrap(), [0x11, 0x90]);
        assert_eq!(audio_specific_config(config(48_000, 1)).unwrap(), [0x11, 0x88]);
        assert_eq!(audio_specific_config(config(44_100, 2)).unwrap(), [0x12, 0x10]);
        assert!(audio_specific_config(config(47_000, 2)).is_err());
    }

    #[test]
    fn high_profiles_carry_the_chroma_extension_and_baseline_main_extended_do_not() {
        for profile in [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134] {
            assert!(h264_profile_has_chroma_format(profile), "{profile}");
        }
        for profile in [66, 77, 88, 144] {
            assert!(!h264_profile_has_chroma_format(profile), "{profile}");
        }
    }
}
