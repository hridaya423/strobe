use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

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
    mix_with(&Predecoder::default(), clips, length)
}

/// [`mix`] with sources decoded by `decoder`, which may already hold some of them. The sources
/// still missing are decoded in parallel, and the sum runs in clip order, so the result does not
/// depend on which were ready.
pub fn mix_with(decoder: &Predecoder, clips: &[Clip], length: u64) -> Result<Vec<f32>> {
    for clip in clips {
        decoder.prefetch(clip);
    }
    mix_range(decoder, clips, 0, length)
}

/// The timeline samples `from..to` of the sum of `clips`, as [`mix`] would hold them.
pub fn mix_range(decoder: &Predecoder, clips: &[Clip], from: u64, to: u64) -> Result<Vec<f32>> {
    let mut out = vec![0.0f32; (to - from) as usize * 2];
    for clip in clips {
        if clip.start >= to || clip.start + clip.length <= from {
            continue;
        }
        let pcm = decoder
            .pcm(clip)
            .with_context(|| format!("decoding {:?}", clip.src))?;
        let frames = pcm.as_chunks::<2>().0;
        let first = from.saturating_sub(clip.start) as usize;
        for (n, frame) in frames
            .iter()
            .enumerate()
            .take(clip.length as usize)
            .skip(first)
        {
            let at = clip.start + n as u64;
            if at >= to {
                break;
            }
            let at = (at - from) as usize;
            let gain = clip.volume.at(n as u64);
            out[at * 2] += frame[0] * gain;
            out[at * 2 + 1] += frame[1] * gain;
        }
    }
    Ok(out)
}

const DECODES_AT_ONCE: usize = 3;

#[derive(Clone, PartialEq)]
struct Source {
    src: PathBuf,
    source_start: u64,
    playback_rate: u64,
    looped: bool,
    mono_copy: bool,
}

impl Source {
    fn of(clip: &Clip) -> Source {
        Source {
            src: clip.src.clone(),
            source_start: clip.source_start.to_bits(),
            playback_rate: clip.playback_rate.to_bits(),
            looped: clip.looped,
            mono_copy: clip.mono_copy,
        }
    }
}

type Decoded = Option<std::result::Result<Arc<Vec<f32>>, String>>;

struct Slot {
    source: Source,
    length: u64,
    pcm: Mutex<Decoded>,
    ready: Condvar,
}

#[derive(Default)]
struct Pending {
    slots: Vec<Arc<Slot>>,
    running: usize,
}

#[derive(Default)]
struct Shared {
    pending: Mutex<Pending>,
    freed: Condvar,
}

/// Decodes sources on background threads, a few at a time, before the mix needs them. A decode is
/// shared by every clip that reads the same source the same way, whatever its place on the
/// timeline. At tempo 1 a decode that is longer than a clip's length serves it too: the filters
/// before the trim work sample by sample, so the first `length` samples do not depend on where
/// the decode ends.
#[derive(Default, Clone)]
pub struct Predecoder {
    shared: Arc<Shared>,
}

impl Predecoder {
    fn usable(slot: &Slot, clip: &Clip) -> bool {
        slot.source == Source::of(clip)
            && (slot.length == clip.length
                || (slot.length > clip.length && clip.playback_rate == 1.0 && !clip.looped))
    }

    /// Starts decoding `clip` unless a decode that can serve it exists.
    pub fn prefetch(&self, clip: &Clip) {
        let slot = {
            let mut pending = self.shared.pending.lock().unwrap();
            if pending.slots.iter().any(|slot| Self::usable(slot, clip)) {
                return;
            }
            let slot = Arc::new(Slot {
                source: Source::of(clip),
                length: clip.length,
                pcm: Mutex::new(None),
                ready: Condvar::new(),
            });
            pending.slots.push(slot.clone());
            slot
        };
        let shared = self.shared.clone();
        let clip = clip.clone();
        thread::spawn(move || {
            {
                let mut pending = shared.pending.lock().unwrap();
                while pending.running >= DECODES_AT_ONCE {
                    pending = shared.freed.wait(pending).unwrap();
                }
                pending.running += 1;
            }
            let result = decode(&clip)
                .map(Arc::new)
                .map_err(|error| format!("{error:#}"));
            *slot.pcm.lock().unwrap() = Some(result);
            slot.ready.notify_all();
            shared.pending.lock().unwrap().running -= 1;
            shared.freed.notify_one();
        });
    }

    /// The decoded samples of `clip`, at least `clip.length` of them unless the source is shorter.
    pub fn pcm(&self, clip: &Clip) -> Result<Arc<Vec<f32>>> {
        let slot = {
            let pending = self.shared.pending.lock().unwrap();
            pending
                .slots
                .iter()
                .find(|slot| Self::usable(slot, clip))
                .cloned()
        };
        let slot = match slot {
            Some(slot) => slot,
            None => {
                self.prefetch(clip);
                return self.pcm(clip);
            }
        };
        let mut pcm = slot.pcm.lock().unwrap();
        while pcm.is_none() {
            pcm = slot.ready.wait(pcm).unwrap();
        }
        match pcm.as_ref().unwrap() {
            Ok(pcm) => Ok(pcm.clone()),
            Err(error) => Err(anyhow::anyhow!("{error}")),
        }
    }
}

/// Clips to full scale. Remotion mixes through 16-bit files, so its sum saturates; HyperFrames
/// keeps floats and lowers the gain of the encoded track instead, so it needs no call.
pub fn saturate(samples: &mut [f32]) {
    for sample in samples {
        *sample = sample.clamp(-1.0, 1.0);
    }
}

fn channels(src: &Path) -> Result<u32> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "a:0"])
        .args([
            "-show_entries",
            "stream=channels",
            "-of",
            "default=nw=1:nk=1",
        ])
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
    Ok(remotion_clip_states(frames, fps, resolve)?
        .into_iter()
        .map(|(clip, _)| clip)
        .collect())
}

/// The timeline sample where a clip that starts at `frame` begins.
pub fn remotion_start(frame: u64, fps: f64) -> u64 {
    let delay_ms = (frame as f64 / fps * 1000.0 + 0.5).floor() as u64;
    delay_ms * u64::from(SAMPLE_RATE) / 1000
}

/// [`remotion_clips`] for the first frames of a composition: each clip comes with whether it is
/// still running at the last frame given, which means a later frame may lengthen it.
pub fn remotion_clip_states(
    frames: &[Vec<RemotionAsset>],
    fps: f64,
    resolve: impl Fn(&str) -> Result<PathBuf>,
) -> Result<Vec<(Clip, bool)>> {
    let mut sources: HashMap<String, &str> = HashMap::new();
    for asset in frames.iter().flatten() {
        if !asset.src.starts_with("same-as") {
            sources
                .entry(format!("same-as-{}-{}", asset.id, asset.frame))
                .or_insert(&asset.src);
        }
    }

    let mut open: HashMap<&str, Run> = HashMap::new();
    let mut done: Vec<(Run, u32, bool)> = Vec::new();
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
            done.push((run, duration, false));
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
        done.push((run, duration, true));
    }
    done.sort_by(|a, b| a.0.start.cmp(&b.0.start).then(a.0.src.cmp(b.0.src)));

    done.into_iter()
        .map(|(run, duration, open)| {
            let rate = run.playback_rate;
            let tempo_start = run.audio_start_frame / fps / rate
                + (run.media_frame - run.audio_start_frame) / fps;
            let volume = if run.volumes.iter().all(|v| *v == run.volumes[0]) {
                Volume::Constant(run.volumes[0])
            } else {
                Volume::Frames {
                    fps,
                    values: run.volumes.iter().map(|v| round_volume(*v)).collect(),
                }
            };
            let clip = Clip {
                src: resolve(run.src)?,
                start: remotion_start(u64::from(run.start), fps),
                source_start: tempo_start * rate,
                length: (f64::from(duration) / fps * f64::from(SAMPLE_RATE)).round() as u64,
                playback_rate: rate,
                volume,
                looped: false,
                mono_copy: false,
            };
            Ok((clip, open))
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
