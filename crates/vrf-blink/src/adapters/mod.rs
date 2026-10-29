use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Context, Result};
use ring::digest;
use serde::Serialize;
use vrf::audio::SAMPLE_RATE;
use vrf::mux::{AudioTrack, Muxer};
use vrf_encode::audiotoolbox::encode_aac;
use vrf_encode::{AacConfig, Rational};

use crate::driver::Summary;

pub mod hyperframes;
pub mod remotion;

const NOT_HASHED: [&str; 6] = [
    "node_modules",
    ".git",
    ".vrf",
    ".cache",
    ".hyperframes",
    "out",
];

#[derive(Debug, Serialize)]
pub struct StageStats {
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

impl StageStats {
    pub(crate) fn of(samples: &[Duration]) -> StageStats {
        let summary = Summary::of(samples);
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        StageStats {
            mean_ms: ms(summary.mean),
            p50_ms: ms(summary.p50),
            p99_ms: ms(summary.p99),
            max_ms: ms(samples.iter().copied().max().unwrap_or_default()),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct PerFrame {
    pub seek: StageStats,
    pub begin_to_paint: StageStats,
    pub convert: StageStats,
    pub encode: StageStats,
}

pub(crate) fn cache_dir() -> PathBuf {
    std::env::var_os("VRF_CACHE_DIR")
        .map_or_else(|| std::env::temp_dir().join("vrf-cache"), PathBuf::from)
}

pub(crate) fn hash_tree(context: &mut digest::Context, root: &Path, dir: &Path) -> Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .with_context(|| format!("read {}", dir.display()))?
        .collect::<std::io::Result<_>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if NOT_HASHED
            .iter()
            .any(|name| entry.file_name() == OsStr::new(name))
        {
            continue;
        }
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            hash_tree(context, root, &path)?;
            continue;
        }
        let metadata = fs::metadata(&path)?;
        let modified = metadata.modified()?.duration_since(UNIX_EPOCH)?.as_nanos();
        context.update(path.strip_prefix(root)?.as_os_str().as_encoded_bytes());
        context.update(&metadata.len().to_le_bytes());
        context.update(&modified.to_le_bytes());
    }
    Ok(())
}

pub(crate) fn short_hex(context: digest::Context) -> String {
    context.finish().as_ref()[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Timeline samples for `frames` frames at `frame_rate`.
pub(crate) fn audio_samples(frames: u64, frame_rate: Rational) -> u64 {
    let rate = u128::from(SAMPLE_RATE) * frames as u128 * frame_rate.den as u128;
    ((rate + frame_rate.num as u128 / 2) / frame_rate.num as u128) as u64
}

/// Encodes interleaved stereo `pcm` as AAC at `bit_rate` and adds the track to `muxer`. A mix
/// that is silent everywhere adds no track, so a project with no audio stays video-only.
pub(crate) fn add_audio(muxer: &mut Muxer, pcm: &[f32], bit_rate: u32) -> Result<()> {
    if pcm.iter().all(|sample| *sample == 0.0) {
        return Ok(());
    }
    let stream = encode_aac(
        pcm,
        AacConfig {
            sample_rate: SAMPLE_RATE,
            channels: 2,
            bit_rate,
        },
    )?;
    muxer.set_audio(AudioTrack::from(&stream))
}
