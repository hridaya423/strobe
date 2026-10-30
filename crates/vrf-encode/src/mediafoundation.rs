use anyhow::{Context, Result, bail, ensure};
use windows::Win32::Media::MediaFoundation::{
    AACMFTEncoder, IMFMediaBuffer, IMFMediaType, IMFSample, IMFTransform,
    MF_E_TRANSFORM_NEED_MORE_INPUT, MF_MT_AAC_PAYLOAD_TYPE, MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
    MF_MT_AUDIO_BITS_PER_SAMPLE, MF_MT_AUDIO_BLOCK_ALIGNMENT, MF_MT_AUDIO_NUM_CHANNELS,
    MF_MT_AUDIO_SAMPLES_PER_SECOND, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_VERSION, MFAudioFormat_PCM,
    MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Audio, MFSTARTUP_FULL,
    MFShutdown, MFStartup, MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
};

pub use crate::{AAC_FRAME_SAMPLES, AacConfig, AacStream};

const SAMPLE_RATES: [u32; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350,
];
const AAC_LC: u32 = 2;
const PRIMING_FRAMES: u32 = AAC_FRAME_SAMPLES;
const TAIL_FRAMES: usize = 2 * AAC_FRAME_SAMPLES as usize;
const INPUT_CHUNK_FRAMES: usize = 4096;
const HUNDRED_NS: i64 = 10_000_000;

struct Media;

impl Media {
    fn start() -> Result<Media> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_FULL).context("starting Media Foundation")?;
        }
        Ok(Media)
    }
}

impl Drop for Media {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
        }
    }
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

fn pcm_type(config: AacConfig) -> Result<IMFMediaType> {
    let ty = unsafe { MFCreateMediaType()? };
    let align = config.channels * 2;
    unsafe {
        ty.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        ty.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
        ty.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
        ty.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, config.sample_rate)?;
        ty.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, config.channels)?;
        ty.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, align)?;
        ty.SetUINT32(
            &MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
            config.sample_rate * align,
        )?;
    }
    Ok(ty)
}

fn output_type(transform: &IMFTransform, config: AacConfig) -> Result<IMFMediaType> {
    let wanted = config.bit_rate / 8;
    let mut best: Option<(u32, IMFMediaType)> = None;
    for index in 0.. {
        let Ok(ty) = (unsafe { transform.GetOutputAvailableType(0, index) }) else {
            break;
        };
        let get = |key| unsafe { ty.GetUINT32(key) }.unwrap_or(0);
        if get(&MF_MT_AUDIO_SAMPLES_PER_SECOND) != config.sample_rate
            || get(&MF_MT_AUDIO_NUM_CHANNELS) != config.channels
            || get(&MF_MT_AAC_PAYLOAD_TYPE) != 0
        {
            continue;
        }
        let distance = get(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND).abs_diff(wanted);
        if best.as_ref().is_none_or(|(d, _)| distance < *d) {
            best = Some((distance, ty));
        }
    }
    best.map(|(_, ty)| ty).with_context(|| {
        format!(
            "the AAC encoder has no raw AAC output at {} Hz and {} channels",
            config.sample_rate, config.channels
        )
    })
}

fn memory_sample(bytes: &[u8], time: i64, duration: i64) -> Result<IMFSample> {
    unsafe {
        let buffer: IMFMediaBuffer = MFCreateMemoryBuffer(bytes.len() as u32)?;
        let mut at = std::ptr::null_mut();
        buffer.Lock(&mut at, None, None)?;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), at, bytes.len());
        buffer.Unlock()?;
        buffer.SetCurrentLength(bytes.len() as u32)?;
        let sample = MFCreateSample()?;
        sample.AddBuffer(&buffer)?;
        sample.SetSampleTime(time)?;
        sample.SetSampleDuration(duration)?;
        Ok(sample)
    }
}

fn pull(transform: &IMFTransform, capacity: u32, packets: &mut Vec<Vec<u8>>) -> Result<()> {
    loop {
        let buffer = unsafe { MFCreateMemoryBuffer(capacity)? };
        let sample = unsafe { MFCreateSample()? };
        unsafe { sample.AddBuffer(&buffer)? };
        let mut out = [MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: std::mem::ManuallyDrop::new(Some(sample)),
            dwStatus: 0,
            pEvents: std::mem::ManuallyDrop::new(None),
        }];
        let mut status = 0u32;
        let result = unsafe { transform.ProcessOutput(0, &mut out, &mut status) };
        let [slot] = &mut out;
        let sample = unsafe { std::mem::ManuallyDrop::take(&mut slot.pSample) };
        unsafe { std::mem::ManuallyDrop::drop(&mut slot.pEvents) };
        match result {
            Ok(()) => {
                let sample = sample.context("the AAC encoder returned no sample")?;
                let joined = unsafe { sample.ConvertToContiguousBuffer()? };
                let (mut at, mut length) = (std::ptr::null_mut(), 0u32);
                unsafe {
                    joined.Lock(&mut at, None, Some(&mut length))?;
                    packets.push(std::slice::from_raw_parts(at, length as usize).to_vec());
                    joined.Unlock()?;
                }
            }
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
            Err(e) => return Err(e).context("pulling AAC from the encoder"),
        }
    }
}

/// Encodes interleaved `f32` samples with the Media Foundation AAC-LC encoder. `samples.len()`
/// must be a multiple of `config.channels`.
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
    let _media = Media::start()?;
    let transform: IMFTransform =
        unsafe { CoCreateInstance(&AACMFTEncoder, None, CLSCTX_INPROC_SERVER)? };
    let channels = config.channels as usize;
    unsafe {
        transform.SetOutputType(0, &output_type(&transform, config)?, 0)?;
        transform.SetInputType(0, &pcm_type(config)?, 0)?;
        transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
        transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
    }
    let capacity = unsafe { transform.GetOutputStreamInfo(0)? }
        .cbSize
        .max(4096);

    let padded: Vec<f32> = std::iter::repeat_n(0.0, PRIMING_FRAMES as usize * channels)
        .chain(samples.iter().copied())
        .chain(std::iter::repeat_n(0.0, TAIL_FRAMES * channels))
        .collect();
    let mut packets = Vec::new();
    let mut at = 0usize;
    for chunk in padded.chunks(INPUT_CHUNK_FRAMES * channels) {
        let bytes: Vec<u8> = chunk
            .iter()
            .flat_map(|&v| ((v * 32768.0).round().clamp(-32768.0, 32767.0) as i16).to_le_bytes())
            .collect();
        let frames = (chunk.len() / channels) as i64;
        let time = at as i64 * HUNDRED_NS / i64::from(config.sample_rate);
        let duration = frames * HUNDRED_NS / i64::from(config.sample_rate);
        let sample = memory_sample(&bytes, time, duration)?;
        unsafe { transform.ProcessInput(0, &sample, 0)? };
        pull(&transform, capacity, &mut packets)?;
        at += frames as usize;
    }
    unsafe {
        transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)?;
        transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)?;
    }
    pull(&transform, capacity, &mut packets)?;
    ensure!(!packets.is_empty(), "the AAC encoder produced no packets");

    Ok(AacStream {
        config,
        audio_specific_config,
        priming_frames: PRIMING_FRAMES,
        frames: (samples.len() / channels) as u64,
        max_packet_bytes: packets.iter().map(Vec::len).max().unwrap_or(0) as u32,
        packets,
    })
}
