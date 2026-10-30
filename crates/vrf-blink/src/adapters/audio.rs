use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

use anyhow::{Result, anyhow};
use vrf::audio::{
    Clip, Predecoder, RemotionAsset, SAMPLE_RATE, Volume, mix_range, remotion_clip_states,
    remotion_start, saturate,
};
#[cfg(target_os = "macos")]
use vrf_encode::audiotoolbox::encode_aac_chunks;
#[cfg(windows)]
use vrf_encode::mediafoundation::encode_aac_chunks;
use vrf_encode::{AacConfig, AacStream};

const ROUND_FRAMES: usize = 30;

type Resolve = Arc<dyn Fn(&str) -> Result<PathBuf> + Send + Sync>;

#[derive(Default)]
struct State {
    frames: Vec<Option<Vec<RemotionAsset>>>,
    prefix: usize,
    stop: bool,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

struct Stopper(Arc<Shared>);

impl Stopper {
    fn stop(&self) {
        self.0.state.lock().unwrap().stop = true;
        self.0.changed.notify_one();
    }
}

impl Drop for Stopper {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Spec {
    clip: Clip,
    end: u64,
}

struct Fed {
    to: u64,
    specs: Vec<Spec>,
    silent: bool,
    chunks: Option<Sender<Vec<f32>>>,
    encoder: Option<JoinHandle<Result<AacStream>>>,
}

impl Fed {
    fn send(&mut self, mut pcm: Vec<f32>, bit_rate: u32) {
        saturate(&mut pcm);
        self.silent &= pcm.iter().all(|sample| *sample == 0.0);
        if self.chunks.is_none() {
            let (sender, receiver) = mpsc::channel();
            let config = AacConfig {
                sample_rate: SAMPLE_RATE,
                channels: 2,
                bit_rate,
            };
            self.encoder = Some(thread::spawn(move || {
                encode_aac_chunks(receiver.into_iter(), config)
            }));
            self.chunks = Some(sender);
        }
        let _ = self.chunks.as_ref().unwrap().send(pcm);
    }
}

/// Mixes and encodes a Remotion composition's audio while its frames render. Views report each
/// frame's assets as they finish it. Once frames `0..p` are all in, every clip that has ended is
/// final, and a clip still running has the parameters it will end with. Their sources are decoded
/// at once, and the stretch of the timeline that nothing later can touch is mixed and handed to an
/// AAC encoder thread, which takes samples as they come. What is left for the end is the
/// remainder of the mix.
///
/// A running clip's volume is taken as it stands, so a clip whose volume changes later would have
/// been mixed wrongly. [`LiveAudio::finish`] checks every clip it mixed early against the final
/// clips and falls back to mixing everything again when any differs, so the stream is always the
/// one a single mix at the end would give.
pub(crate) struct LiveAudio {
    shared: Stopper,
    worker: JoinHandle<Fed>,
    decoder: Predecoder,
    samples: u64,
    bit_rate: u32,
}

impl LiveAudio {
    pub(crate) fn start(
        frames: usize,
        fps: f64,
        samples: u64,
        bit_rate: u32,
        resolve: impl Fn(&str) -> Result<PathBuf> + Send + Sync + 'static,
    ) -> LiveAudio {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                frames: vec![None; frames],
                ..State::default()
            }),
            changed: Condvar::new(),
        });
        let decoder = Predecoder::default();
        let resolve: Resolve = Arc::new(resolve);
        let worker = {
            let (shared, decoder) = (shared.clone(), decoder.clone());
            thread::spawn(move || run(&shared, &decoder, &resolve, fps, samples, bit_rate))
        };
        LiveAudio {
            shared: Stopper(shared),
            worker,
            decoder,
            samples,
            bit_rate,
        }
    }

    pub(crate) fn report(&self, index: usize, assets: Vec<RemotionAsset>) {
        let mut state = self.shared.0.state.lock().unwrap();
        state.frames[index] = Some(assets);
        let before = state.prefix;
        while state.prefix < state.frames.len() && state.frames[state.prefix].is_some() {
            state.prefix += 1;
        }
        if state.prefix - before > 0 && (state.prefix / ROUND_FRAMES != before / ROUND_FRAMES) {
            self.shared.0.changed.notify_one();
        }
    }

    /// The encoded track for `clips`, the final clips of the composition, or `None` for silence.
    pub(crate) fn finish(self, clips: &[Clip]) -> Result<Option<AacStream>> {
        self.shared.stop();
        let mut fed = self
            .worker
            .join()
            .map_err(|_| anyhow!("the audio worker panicked"))?;
        for clip in clips {
            self.decoder.prefetch(clip);
        }
        if !consistent(&fed, clips) {
            drop(fed.chunks.take());
            return full_mix(&self.decoder, clips, self.samples, self.bit_rate);
        }
        if fed.to < self.samples {
            let rest = mix_range(&self.decoder, clips, fed.to, self.samples)?;
            fed.send(rest, self.bit_rate);
        }
        drop(fed.chunks.take());
        let Some(encoder) = fed.encoder.take() else {
            return Ok(None);
        };
        let stream = encoder
            .join()
            .map_err(|_| anyhow!("the AAC encoder panicked"))??;
        Ok((!fed.silent).then_some(stream))
    }
}

fn full_mix(
    decoder: &Predecoder,
    clips: &[Clip],
    samples: u64,
    bit_rate: u32,
) -> Result<Option<AacStream>> {
    let mut pcm = mix_range(decoder, clips, 0, samples)?;
    saturate(&mut pcm);
    if pcm.iter().all(|sample| *sample == 0.0) {
        return Ok(None);
    }
    let config = AacConfig {
        sample_rate: SAMPLE_RATE,
        channels: 2,
        bit_rate,
    };
    encode_aac_chunks(std::iter::once(pcm), config).map(Some)
}

fn same_source(a: &Clip, b: &Clip) -> bool {
    a.src == b.src
        && a.start == b.start
        && a.source_start.to_bits() == b.source_start.to_bits()
        && a.playback_rate.to_bits() == b.playback_rate.to_bits()
        && a.looped == b.looped
        && a.mono_copy == b.mono_copy
}

fn same_volume(early: &Volume, last: &Volume) -> bool {
    match (early, last) {
        (Volume::Constant(a), Volume::Constant(b)) => a == b,
        (Volume::Frames { fps, values }, Volume::Frames { fps: f, values: v }) => {
            fps == f && v.starts_with(values)
        }
        _ => false,
    }
}

/// Whether the samples already fed are the ones the final clips give: every clip that reaches
/// into them was mixed, as the same source at the same place with the same volume over the part
/// that was used.
fn consistent(fed: &Fed, clips: &[Clip]) -> bool {
    let used = fed.specs.iter().all(|spec| {
        clips.iter().any(|clip| {
            same_source(clip, &spec.clip)
                && clip.start + clip.length >= spec.end
                && same_volume(&spec.clip.volume, &clip.volume)
        })
    });
    let none_missing = clips
        .iter()
        .filter(|clip| clip.start < fed.to)
        .all(|clip| fed.specs.iter().any(|spec| same_source(&spec.clip, clip)));
    used && none_missing
}

fn run(
    shared: &Shared,
    decoder: &Predecoder,
    resolve: &Resolve,
    fps: f64,
    samples: u64,
    bit_rate: u32,
) -> Fed {
    let mut fed = Fed {
        to: 0,
        specs: Vec::new(),
        silent: true,
        chunks: None,
        encoder: None,
    };
    let frame_samples = (f64::from(SAMPLE_RATE) / fps).ceil() as u64;
    let mut seen = 0;
    loop {
        let known: Vec<Vec<RemotionAsset>> = {
            let mut state = shared.state.lock().unwrap();
            while !state.stop && state.prefix / ROUND_FRAMES == seen / ROUND_FRAMES {
                state = shared.changed.wait(state).unwrap();
            }
            if state.stop {
                return fed;
            }
            seen = state.prefix;
            state.frames[..seen]
                .iter()
                .map(|frame| frame.clone().unwrap_or_default())
                .collect()
        };
        let Ok(states) = remotion_clip_states(&known, fps, |url| resolve(url)) else {
            continue;
        };
        let mut limit = remotion_start(seen as u64, fps);
        let mut clips = Vec::with_capacity(states.len());
        for (clip, open) in &states {
            let clip = clip.clone();
            if *open {
                if clip.playback_rate == 1.0 && !clip.looped {
                    let mut longest = clip.clone();
                    longest.length = samples.saturating_sub(clip.start);
                    decoder.prefetch(&longest);
                    limit = limit.min((clip.start + clip.length).saturating_sub(frame_samples));
                } else {
                    limit = limit.min(clip.start);
                }
            } else {
                decoder.prefetch(&clip);
            }
            clips.push((clip, *open));
        }
        let limit = limit.min(samples);
        if limit <= fed.to {
            continue;
        }
        let used: Vec<Clip> = clips.iter().map(|(clip, _)| clip.clone()).collect();
        let Ok(pcm) = mix_range(decoder, &used, fed.to, limit) else {
            continue;
        };
        for (clip, _) in clips
            .iter()
            .filter(|(clip, _)| clip.start < limit && clip.start + clip.length > fed.to)
        {
            fed.specs.push(Spec {
                clip: clip.clone(),
                end: (clip.start + clip.length).min(limit),
            });
        }
        fed.send(pcm, bit_rate);
        fed.to = limit;
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use serde_json::json;
    use vrf::audio::remotion_clips;

    use super::*;

    const FPS: f64 = 30.0;
    const FRAMES: usize = 240;

    fn tone(dir: &std::path::Path) -> PathBuf {
        let path = dir.join("tone.wav");
        let status = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=330:duration=9",
            ])
            .args(["-ac", "2"])
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        path
    }

    fn asset(id: &str, frame: usize, start: usize, volume: f64) -> RemotionAsset {
        serde_json::from_value(json!({
            "type": "audio",
            "src": "tone.wav",
            "id": id,
            "frame": frame,
            "volume": volume,
            "mediaFrame": frame - start,
            "playbackRate": 1.0,
            "audioStartFrame": 0.0,
        }))
        .unwrap()
    }

    fn frames(volume_of_c: impl Fn(usize) -> f64) -> Vec<Vec<RemotionAsset>> {
        (0..FRAMES)
            .map(|frame| {
                let mut assets = Vec::new();
                if (10..70).contains(&frame) {
                    assets.push(asset("a", frame, 10, 1.0));
                }
                if frame >= 30 {
                    assets.push(asset("b", frame, 30, 0.5));
                }
                if (100..150).contains(&frame) {
                    assets.push(asset("c", frame, 100, volume_of_c(frame)));
                }
                assets
            })
            .collect()
    }

    fn run(
        volume_of_c: impl Fn(usize) -> f64,
        order: &[usize],
    ) -> (Option<AacStream>, Option<AacStream>) {
        let dir = std::env::temp_dir().join(format!("vrf-live-audio-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav = tone(&dir);
        let resolve = move |_: &str| Ok(wav.clone());
        let all = frames(volume_of_c);
        let samples = vrf::audio::SAMPLE_RATE as u64 * FRAMES as u64 / 30;
        let live = LiveAudio::start(FRAMES, FPS, samples, 128_000, resolve.clone());
        for (n, &index) in order.iter().enumerate() {
            live.report(index, all[index].clone());
            if n % ROUND_FRAMES == ROUND_FRAMES - 1 {
                std::thread::sleep(std::time::Duration::from_millis(400));
            }
        }
        let clips = remotion_clips(&all, FPS, resolve).unwrap();
        let early = live.finish(&clips).unwrap();
        let late = full_mix(&Predecoder::default(), &clips, samples, 128_000).unwrap();
        (early, late)
    }

    #[test]
    fn live_mixing_gives_the_stream_of_one_mix_at_the_end() {
        let in_order: Vec<usize> = (0..FRAMES).collect();
        let mut shuffled = in_order.clone();
        for chunk in shuffled.chunks_mut(40) {
            chunk.reverse();
        }
        for order in [&in_order, &shuffled] {
            let (early, late) = run(|_| 0.8, order);
            assert!(early.is_some());
            assert_eq!(early, late);
        }
    }

    #[test]
    fn a_volume_that_changes_after_it_was_mixed_falls_back_to_the_full_mix() {
        let in_order: Vec<usize> = (0..FRAMES).collect();
        let (early, late) = run(|frame| if frame < 125 { 0.8 } else { 0.4 }, &in_order);
        assert_eq!(early, late);
    }
}
