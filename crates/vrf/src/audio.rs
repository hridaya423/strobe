use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

pub const SAMPLE_RATE: u32 = 48_000;

const MAX_HYPERFRAMES_VOLUME: f64 = 3.98;

/// Source sample `n` of a clip lands on timeline sample `start + n`, scaled by [`Volume`].
/// Mixing is a plain sum with no normalisation (contract item 5).
#[derive(Debug, Clone, PartialEq)]
pub struct Clip {
    pub src: PathBuf,
    /// Timeline sample where the clip begins.
    pub start: u64,
    /// Seconds into the source, before tempo change.
    pub source_start: f64,
    /// Timeline samples the clip may fill. A shorter source leaves the rest silent.
    pub length: u64,
    pub playback_rate: f64,
    pub volume: Volume,
    /// Repeat the source from `source_start` until `length` is full.
    pub looped: bool,
    /// Copy a mono source to both channels at full level (HyperFrames). Otherwise a mono source
    /// is down-weighted by 1/sqrt(2), which is what `ffmpeg -ac 2` does (Remotion).
    pub mono_copy: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Volume {
    Constant(f64),
    /// `values[i]` holds from half a video frame before frame `i` to half a frame after it, counted
    /// from the clip's first sample. The last value holds beyond the end.
    Frames {
        fps: f64,
        values: Vec<f64>,
    },
}

impl Volume {
    /// The gain at `offset` samples after the clip's first sample.
    pub fn at(&self, offset: u64) -> f32 {
        match self {
            Volume::Constant(v) => *v as f32,
            Volume::Frames { fps, values } => {
                let frame = (offset as f64 / f64::from(SAMPLE_RATE) * fps + 0.5).floor() as usize;
                values[frame.min(values.len() - 1)] as f32
            }
        }
    }
}

/// Sums `clips` into `length` timeline samples of interleaved stereo `f32`.
///
/// Sources are decoded, trimmed, tempo-changed and resampled to 48 kHz by an `ffmpeg` subprocess.
/// Both frameworks do those steps in ffmpeg too, so their rematrixing, resampling and `atempo`
/// are reproduced exactly; reimplementing them is where a mix would drift from REF. Delay,
/// volume and the sum are done here.
pub fn mix(clips: &[Clip], length: u64) -> Result<Vec<f32>> {
    let mut out = vec![0.0f32; length as usize * 2];
    for clip in clips {
        let pcm = decode(clip).with_context(|| format!("decoding {:?}", clip.src))?;
        for (n, frame) in pcm.as_chunks::<2>().0.iter().enumerate() {
            let at = clip.start as usize + n;
            if at >= length as usize {
                break;
            }
            let gain = clip.volume.at(n as u64);
            out[at * 2] += frame[0] * gain;
            out[at * 2 + 1] += frame[1] * gain;
        }
    }
    Ok(out)
}

fn channels(src: &Path) -> Result<u32> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "a:0"])
        .args(["-show_entries", "stream=channels", "-of", "csv=p=0"])
        .arg(src)
        .output()
        .context("running ffprobe")?;
    ensure!(
        out.status.success(),
        "ffprobe: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text.trim().parse().unwrap_or(0))
}

fn tempo_stages(rate: f64) -> Vec<String> {
    let mut remaining = rate;
    let mut stages = Vec::new();
    while remaining < 0.5 - 1e-9 {
        stages.push(0.5);
        remaining /= 0.5;
    }
    while remaining > 2.0 + 1e-9 {
        stages.push(2.0);
        remaining /= 2.0;
    }
    if (remaining - 1.0).abs() >= 1e-9 {
        stages.push(remaining);
    }
    stages.iter().map(|s| format!("atempo={s:.6}")).collect()
}

fn decode(clip: &Clip) -> Result<Vec<f32>> {
    let source_channels = channels(&clip.src)?;
    if source_channels == 0 {
        return Ok(Vec::new());
    }
    ensure!(
        !clip.looped || clip.playback_rate == 1.0,
        "a looped clip cannot change tempo"
    );
    let source_seconds =
        clip.length as f64 / f64::from(SAMPLE_RATE) * clip.playback_rate + clip.source_start;
    let mut filters = vec![format!(
        "aformat=sample_fmts=fltp:sample_rates={SAMPLE_RATE}"
    )];
    if clip.looped {
        filters.push(format!("atrim=start={:.6}", clip.source_start));
    } else {
        filters.push(format!(
            "atrim=start={:.6}:end={source_seconds:.6}",
            clip.source_start
        ));
    }
    filters.extend(tempo_stages(clip.playback_rate));
    if clip.mono_copy && source_channels == 1 {
        filters.push("pan=stereo|FL=FL+FC|FR=FR+FC".into());
    }
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(&clip.src)
        .args(["-map", "0:a:0", "-vn", "-af", &filters.join(",")])
        .args(["-ac", "2", "-f", "f32le", "pipe:1"])
        .stderr(Stdio::piped())
        .output()
        .context("running ffmpeg")?;
    ensure!(
        out.status.success(),
        "ffmpeg: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut pcm: Vec<f32> = out
        .stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    if clip.looped && !pcm.is_empty() {
        let cycle = pcm.clone();
        while pcm.len() < clip.length as usize * 2 {
            pcm.extend_from_slice(&cycle);
        }
    }
    pcm.truncate(clip.length as usize * 2);
    Ok(pcm)
}

/// One entry of `window.remotion_collectAssets()` as `crates/vrf-blink/js/remotion.js` returns it
/// for a frame. Fields that older Remotion versions do not emit default to zero or absent.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotionAsset {
    #[serde(rename = "type")]
    pub kind: String,
    /// A URL, or `same-as-<id>-<frame>` for a repeat of a source of 400 characters or more.
    pub src: String,
    pub id: String,
    pub frame: u32,
    pub volume: f64,
    pub media_frame: f64,
    pub playback_rate: f64,
    #[serde(default)]
    pub tone_frequency: Option<f64>,
    #[serde(default)]
    pub audio_start_frame: f64,
}

struct Run<'a> {
    src: &'a str,
    start: u32,
    media_frame: f64,
    audio_start_frame: f64,
    playback_rate: f64,
    volumes: Vec<f64>,
}

fn round_volume(v: f64) -> f64 {
    let rounded = (v * 97.0).round() / 97.0;
    (rounded * 1000.0).round() / 1000.0
}

/// Groups per-frame assets into clips the way `calculateAssetPositions` does (Remotion 4.0.539):
/// an id present on consecutive frames is one clip. The delay is `startInVideo / fps` rounded to
/// a whole millisecond (`adelay=<ms>`), not to a sample, as every installed Remotion version does
/// for `<Audio>`. `resolve` maps an asset URL to a file.
pub fn remotion_clips(
    frames: &[Vec<RemotionAsset>],
    fps: f64,
    resolve: impl Fn(&str) -> Result<PathBuf>,
) -> Result<Vec<Clip>> {
    let mut sources: HashMap<String, &str> = HashMap::new();
    for asset in frames.iter().flatten() {
        if !asset.src.starts_with("same-as") {
            sources
                .entry(format!("same-as-{}-{}", asset.id, asset.frame))
                .or_insert(&asset.src);
        }
    }

    let mut open: HashMap<&str, Run> = HashMap::new();
    let mut done: Vec<(Run, u32)> = Vec::new();
    for (index, assets) in frames.iter().enumerate() {
        let frame = index as u32;
        let mut current: Vec<&RemotionAsset> = Vec::new();
        for asset in assets
            .iter()
            .filter(|a| a.kind == "audio" || a.kind == "video")
        {
            if !current.iter().any(|c| c.id == asset.id) {
                current.push(asset);
            }
        }
        let ended: Vec<&str> = open
            .keys()
            .copied()
            .filter(|id| !current.iter().any(|a| a.id == *id))
            .collect();
        for id in ended {
            let run = open.remove(id).unwrap();
            let duration = frame - run.start;
            done.push((run, duration));
        }
        for asset in current {
            ensure!(
                asset.tone_frequency.is_none_or(|t| t == 1.0),
                "toneFrequency is not supported (asset {})",
                asset.id
            );
            let src = sources
                .get(asset.src.as_str())
                .copied()
                .unwrap_or(&asset.src);
            open.entry(&asset.id)
                .or_insert_with(|| Run {
                    src,
                    start: frame,
                    media_frame: asset.media_frame,
                    audio_start_frame: asset.audio_start_frame,
                    playback_rate: asset.playback_rate,
                    volumes: Vec::new(),
                })
                .volumes
                .push(asset.volume);
        }
    }
    let end = frames.len() as u32;
    for (_, run) in open {
        let duration = end - run.start;
        done.push((run, duration));
    }
    done.sort_by(|a, b| a.0.start.cmp(&b.0.start).then(a.0.src.cmp(b.0.src)));

    done.into_iter()
        .map(|(run, duration)| {
            let rate = run.playback_rate;
            let tempo_start = run.audio_start_frame / fps / rate
                + (run.media_frame - run.audio_start_frame) / fps;
            let delay_ms = (f64::from(run.start) / fps * 1000.0 + 0.5).floor() as u64;
            let volume = if run.volumes.iter().all(|v| *v == run.volumes[0]) {
                Volume::Constant(run.volumes[0])
            } else {
                Volume::Frames {
                    fps,
                    values: run.volumes.iter().map(|v| round_volume(*v)).collect(),
                }
            };
            Ok(Clip {
                src: resolve(run.src)?,
                start: delay_ms * u64::from(SAMPLE_RATE) / 1000,
                source_start: tempo_start * rate,
                length: (f64::from(duration) / fps * f64::from(SAMPLE_RATE)).round() as u64,
                playback_rate: rate,
                volume,
                looped: false,
                mono_copy: false,
            })
        })
        .collect()
}

/// One track as HyperFrames' `parseAudioElements` returns it for an `<audio>` element, or for a
/// `<video data-has-audio="true">` element. Times are seconds on the composition timeline.
/// Volume automation, effect chains, groups and rate ramps are not represented.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HyperframesTrack {
    pub id: String,
    pub src: String,
    pub start: f64,
    pub end: f64,
    #[serde(default)]
    pub media_start: f64,
    #[serde(default = "one")]
    pub playback_rate: f64,
    #[serde(default, rename = "loop")]
    pub looped: bool,
    #[serde(default = "one")]
    pub volume: f64,
    #[serde(default)]
    pub fade_in: Option<f64>,
    #[serde(default)]
    pub fade_out: Option<f64>,
}

fn one() -> f64 {
    1.0
}

/// Places HyperFrames tracks as `buildTrackInputFilter` does: `atrim=0:<end - start>`, then
/// `adelay=<round(start * 1000)>` milliseconds. Tracks with volume 0 are dropped.
pub fn hyperframes_clips(
    tracks: &[HyperframesTrack],
    resolve: impl Fn(&str) -> Result<PathBuf>,
) -> Result<Vec<Clip>> {
    let mut clips = Vec::new();
    for track in tracks {
        ensure!(
            track.end > track.start,
            "{} has no data-duration or data-end",
            track.id
        );
        ensure!(
            track.fade_in.is_none() && track.fade_out.is_none(),
            "{}: fades are not supported",
            track.id
        );
        let volume = track.volume.clamp(0.0, MAX_HYPERFRAMES_VOLUME);
        if volume == 0.0 {
            continue;
        }
        let delay_ms = (track.start * 1000.0 + 0.5).floor() as u64;
        clips.push(Clip {
            src: resolve(&track.src)?,
            start: delay_ms * u64::from(SAMPLE_RATE) / 1000,
            source_start: track.media_start,
            length: ((track.end - track.start) * f64::from(SAMPLE_RATE)).round() as u64,
            playback_rate: track.playback_rate,
            volume: Volume::Constant(volume),
            looped: track.looped,
            mono_copy: true,
        });
    }
    Ok(clips)
}
