use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::{NonNull, null_mut};

use anyhow::{Result, bail, ensure};
use objc2_audio_toolbox::{
    AudioConverterDispose, AudioConverterFillComplexBuffer, AudioConverterGetProperty,
    AudioConverterNew, AudioConverterPrimeInfo, AudioConverterRef, AudioConverterSetProperty,
    kAudioConverterCodecQuality, kAudioConverterEncodeBitRate, kAudioConverterPrimeInfo,
    kAudioConverterPropertyMaximumOutputPacketSize, kAudioConverterQuality_Max,
};
use objc2_core_audio_types::{
    AudioBuffer, AudioBufferList, AudioStreamBasicDescription, AudioStreamPacketDescription,
    kAudioFormatFlagIsFloat, kAudioFormatFlagIsPacked, kAudioFormatLinearPCM, kAudioFormatMPEG4AAC,
};

pub const AAC_FRAME_SAMPLES: u32 = 1024;

const AAC_LC: u32 = 2;
const BIT_RATE_CONTROL_MODE: u32 = u32::from_be_bytes(*b"acbf");
const BIT_RATE_CONSTANT: u32 = 0;
const SAMPLE_RATES: [u32; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350,
];

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

struct Input<'a> {
    samples: &'a [f32],
    channels: u32,
    cursor: usize,
}

unsafe extern "C-unwind" fn feed(
    _converter: AudioConverterRef,
    packets: NonNull<u32>,
    data: NonNull<AudioBufferList>,
    _descriptions: *mut *mut AudioStreamPacketDescription,
    user: *mut c_void,
) -> i32 {
    let input = unsafe { &mut *(user as *mut Input) };
    let channels = input.channels as usize;
    let remaining = (input.samples.len() - input.cursor) / channels;
    let count = (unsafe { *packets.as_ptr() } as usize).min(remaining);
    unsafe {
        *packets.as_ptr() = count as u32;
        let buffer = &mut (*data.as_ptr()).mBuffers[0];
        buffer.mNumberChannels = input.channels;
        buffer.mDataByteSize = (count * channels * size_of::<f32>()) as u32;
        buffer.mData = input.samples.as_ptr().add(input.cursor) as *mut c_void;
    }
    input.cursor += count * channels;
    0
}

struct Converter(AudioConverterRef);

impl Drop for Converter {
    fn drop(&mut self) {
        unsafe { AudioConverterDispose(self.0) };
    }
}

fn status(what: &str, code: i32) -> Result<()> {
    ensure!(
        code == 0,
        "{what} failed: OSStatus {code} ({})",
        fourcc(code)
    );
    Ok(())
}

fn fourcc(code: i32) -> String {
    let bytes = code.to_be_bytes();
    if bytes.iter().all(|b| b.is_ascii_graphic()) {
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        "-".into()
    }
}

fn set_u32(converter: &Converter, property: u32, value: u32, what: &str) -> Result<()> {
    let mut value = value;
    status(what, unsafe {
        AudioConverterSetProperty(
            converter.0,
            property,
            size_of::<u32>() as u32,
            NonNull::from(&mut value).cast(),
        )
    })
}

fn audio_specific_config(config: AacConfig) -> Result<Vec<u8>> {
    let Some(rate) = SAMPLE_RATES.iter().position(|&r| r == config.sample_rate) else {
        bail!(
            "{} Hz has no AAC sampling frequency index",
            config.sample_rate
        );
    };
    let bits = (AAC_LC << 11) | ((rate as u32) << 7) | (config.channels << 3);
    Ok((bits as u16).to_be_bytes().to_vec())
}

/// Encodes interleaved `f32` samples with Apple's AAC-LC encoder. `samples.len()` must be a
/// multiple of `config.channels`.
pub fn encode_aac(samples: &[f32], config: AacConfig) -> Result<AacStream> {
    ensure!(
        (1..=2).contains(&config.channels),
        "AAC-LC is mono or stereo here, got {} channels",
        config.channels
    );
    ensure!(
        samples.len().is_multiple_of(config.channels as usize),
        "{} samples are not whole frames of {} channels",
        samples.len(),
        config.channels
    );
    let audio_specific_config = audio_specific_config(config)?;

    let mut source = AudioStreamBasicDescription {
        mSampleRate: f64::from(config.sample_rate),
        mFormatID: kAudioFormatLinearPCM,
        mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
        mBytesPerPacket: 4 * config.channels,
        mFramesPerPacket: 1,
        mBytesPerFrame: 4 * config.channels,
        mChannelsPerFrame: config.channels,
        mBitsPerChannel: 32,
        mReserved: 0,
    };
    let mut destination = AudioStreamBasicDescription {
        mSampleRate: f64::from(config.sample_rate),
        mFormatID: kAudioFormatMPEG4AAC,
        mFormatFlags: AAC_LC,
        mBytesPerPacket: 0,
        mFramesPerPacket: AAC_FRAME_SAMPLES,
        mBytesPerFrame: 0,
        mChannelsPerFrame: config.channels,
        mBitsPerChannel: 0,
        mReserved: 0,
    };
    let mut raw: AudioConverterRef = null_mut();
    status("AudioConverterNew", unsafe {
        AudioConverterNew(
            NonNull::from(&mut source),
            NonNull::from(&mut destination),
            NonNull::from(&mut raw),
        )
    })?;
    let converter = Converter(raw);
    set_u32(
        &converter,
        kAudioConverterEncodeBitRate,
        config.bit_rate,
        "setting the bit rate",
    )?;
    set_u32(
        &converter,
        BIT_RATE_CONTROL_MODE,
        BIT_RATE_CONSTANT,
        "setting constant bit rate",
    )?;
    set_u32(
        &converter,
        kAudioConverterCodecQuality,
        kAudioConverterQuality_Max,
        "setting the codec quality",
    )?;

    let mut max_packet_bytes = 0u32;
    let mut size = size_of::<u32>() as u32;
    status("reading the maximum packet size", unsafe {
        AudioConverterGetProperty(
            converter.0,
            kAudioConverterPropertyMaximumOutputPacketSize,
            NonNull::from(&mut size),
            NonNull::from(&mut max_packet_bytes).cast(),
        )
    })?;

    let mut input = Input {
        samples,
        channels: config.channels,
        cursor: 0,
    };
    let mut packets = Vec::new();
    let mut out = vec![0u8; max_packet_bytes as usize];
    loop {
        let mut count = 1u32;
        let mut description = AudioStreamPacketDescription {
            mStartOffset: 0,
            mVariableFramesInPacket: 0,
            mDataByteSize: 0,
        };
        let mut list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [AudioBuffer {
                mNumberChannels: config.channels,
                mDataByteSize: max_packet_bytes,
                mData: out.as_mut_ptr().cast(),
            }],
        };
        status("AudioConverterFillComplexBuffer", unsafe {
            AudioConverterFillComplexBuffer(
                converter.0,
                Some(feed),
                (&mut input as *mut Input).cast(),
                NonNull::from(&mut count),
                NonNull::from(&mut list),
                &mut description,
            )
        })?;
        if count == 0 {
            break;
        }
        let start = description.mStartOffset as usize;
        packets.push(out[start..start + description.mDataByteSize as usize].to_vec());
    }

    let mut prime = AudioConverterPrimeInfo {
        leadingFrames: 0,
        trailingFrames: 0,
    };
    let mut size = size_of::<AudioConverterPrimeInfo>() as u32;
    status("reading the prime info", unsafe {
        AudioConverterGetProperty(
            converter.0,
            kAudioConverterPrimeInfo,
            NonNull::from(&mut size),
            NonNull::from(&mut prime).cast(),
        )
    })?;

    Ok(AacStream {
        config,
        audio_specific_config,
        max_packet_bytes: packets.iter().map(Vec::len).max().unwrap_or(0) as u32,
        packets,
        priming_frames: prime.leadingFrames,
        frames: (samples.len() / config.channels as usize) as u64,
    })
}
