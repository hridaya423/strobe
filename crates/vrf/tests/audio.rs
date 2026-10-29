use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use vrf::audio::{
    Clip, HyperframesTrack, RemotionAsset, SAMPLE_RATE, Volume, hyperframes_clips, mix,
    remotion_clips, saturate,
};

fn bench_dir() -> PathBuf {
    match std::env::var_os("VRF_BENCH_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench"),
    }
}

fn support(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/audio_support")
        .join(name)
}

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let path = std::env::temp_dir().join(format!("vrf-audio-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Dir(path)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(program: impl AsRef<std::ffi::OsStr>, args: &[&str], cwd: &Path) -> Result<String> {
    let out = Command::new(program.as_ref())
        .args(args)
        .current_dir(cwd)
        .env("VRF_BENCH_DIR", bench_dir())
        .output()?;
    ensure!(
        out.status.success(),
        "{:?} {args:?}: {}{}",
        program.as_ref(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(String::from_utf8(out.stdout)?)
}

fn tone_wav(path: &Path) {
    run(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=44100:duration=2",
            "-af",
            "volume=4",
            "-ac",
            "2",
            path.to_str().unwrap(),
        ],
        Path::new("/"),
    )
    .unwrap();
}

fn decode(file: &Path) -> Vec<f32> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i", file.to_str().unwrap()])
        .args([
            "-map", "0:a:0", "-f", "f32le", "-ar", "48000", "-ac", "2", "-",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect()
}

fn onset(stereo: &[f32], threshold: f32) -> usize {
    stereo
        .as_chunks::<2>()
        .0
        .iter()
        .position(|f| f[0].abs() > threshold)
        .unwrap()
}

fn rms(stereo: &[f32], from: usize, frames: usize) -> f64 {
    let window = &stereo[from * 2..(from + frames) * 2];
    (window.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / window.len() as f64).sqrt()
}

fn max_diff(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max)
}

#[derive(Deserialize)]
struct Collected {
    fps: f64,
    #[serde(rename = "durationInFrames")]
    duration_in_frames: u32,
    frames: Vec<Vec<RemotionAsset>>,
}

struct RemotionFixture {
    dir: Dir,
    modules: PathBuf,
}

fn remotion_fixture(name: &str, modules: PathBuf, from: u32) -> RemotionFixture {
    let dir = Dir::new(name);
    fs::create_dir_all(dir.file("src")).unwrap();
    fs::create_dir_all(dir.file("public")).unwrap();
    fs::write(
        dir.file("package.json"),
        r#"{"name":"vrf-audio-fixture","private":true}"#,
    )
    .unwrap();
    symlink(&modules, dir.file("node_modules")).unwrap();
    tone_wav(&dir.file("public/tone.wav"));
    fs::write(
        dir.file("src/index.ts"),
        "import {registerRoot} from 'remotion';\nimport {Root} from './Root';\nregisterRoot(Root);\n",
    )
    .unwrap();
    fs::write(
        dir.file("src/Root.tsx"),
        format!(
            "import React from 'react';\n\
             import {{Audio, Composition, Sequence, staticFile, AbsoluteFill}} from 'remotion';\n\
             const Tone: React.FC = () => (\n\
               <AbsoluteFill style={{{{background: '#123'}}}}>\n\
                 <Sequence from={{{from}}}><Audio src={{staticFile('tone.wav')}} volume={{0.5}} /></Sequence>\n\
               </AbsoluteFill>\n\
             );\n\
             export const Root: React.FC = () => (\n\
               <Composition id=\"Tone\" component={{Tone}} durationInFrames={{90}} fps={{30}} width={{320}} height={{180}} />\n\
             );\n"
        ),
    )
    .unwrap();
    RemotionFixture { dir, modules }
}

fn headless_shell() -> PathBuf {
    bench_dir().join(
        "engines/browsers/chrome-headless-shell/mac_arm-156.0.8078.73/\
         chrome-headless-shell-mac-arm64/chrome-headless-shell",
    )
}

fn remotion_ref_wav(project: &Path, composition: &str, out: &Path) -> Result<()> {
    run(
        project.join("node_modules/.bin/remotion"),
        &[
            "render",
            "src/index.ts",
            composition,
            out.to_str().unwrap(),
            "--codec=wav",
            "--browser-executable",
            headless_shell().to_str().unwrap(),
        ],
        project,
    )?;
    Ok(())
}

fn remotion_assets(project: &Path, entry: &str, composition: &str) -> Result<Collected> {
    let json = run(
        "node",
        &[
            support("remotion-assets.mjs").to_str().unwrap(),
            project.to_str().unwrap(),
            entry,
            composition,
        ],
        project,
    )?;
    Ok(serde_json::from_str(&json)?)
}

fn public_file(project: &Path) -> impl Fn(&str) -> Result<PathBuf> + '_ {
    move |url: &str| {
        let path = url
            .split_once("//")
            .and_then(|(_, rest)| rest.split_once('/'))
            .context("url")?
            .1;
        Ok(project.join(path))
    }
}

fn remotion_tone_matches_ref(version: &str, modules: PathBuf) {
    let fixture = remotion_fixture(&format!("rm-{version}"), modules, 7);
    let project = fixture.dir.0.clone();
    let reference = fixture.dir.file("ref.wav");
    remotion_ref_wav(&project, "Tone", &reference).unwrap();
    let expected = decode(&reference);

    let collected = remotion_assets(&project, "src/index.ts", "Tone").unwrap();
    let clips = remotion_clips(&collected.frames, collected.fps, public_file(&project)).unwrap();
    assert_eq!(clips.len(), 1);
    let length =
        u64::from(collected.duration_in_frames) * u64::from(SAMPLE_RATE) / collected.fps as u64;
    let ours = mix(&clips, length).unwrap();

    let (a, b) = (onset(&ours, 1e-3), onset(&expected, 1e-3));
    eprintln!(
        "{version}: onset ours {a}, REF {b}, ideal {}",
        7.0 / 30.0 * 48000.0
    );
    assert!(a.abs_diff(b) <= 1, "onset {a} vs REF {b}");
    let (ra, rb) = (rms(&ours, b + 100, 90_000), rms(&expected, b + 100, 90_000));
    eprintln!("{version}: rms ours {ra:.6}, REF {rb:.6}");
    assert!(((ra - rb) / rb).abs() < 1e-3);
    let common = ours.len().min(expected.len());
    let worst = max_diff(&ours[..common], &expected[..common]);
    eprintln!(
        "{version}: REF has {} frames, ours {}, worst sample difference {worst}",
        expected.len() / 2,
        ours.len() / 2
    );
    assert!(worst <= 2.0 / 32768.0, "worst sample difference {worst}");
    drop(fixture.modules);
}

#[test]
fn remotion_tone_in_a_sequence_matches_ref_on_4_0_60() {
    let modules = bench_dir().join("corpus/external/transitions-video/node_modules");
    remotion_tone_matches_ref("4.0.60", modules);
}

#[test]
fn remotion_tone_in_a_sequence_matches_ref_on_4_0_539() {
    remotion_tone_matches_ref("4.0.539", bench_dir().join("engines/node_modules"));
}

fn hyperframes_dir() -> PathBuf {
    bench_dir().join("engines/node_modules/hyperframes")
}

fn hyperframes_fixture(name: &str, start: &str) -> Dir {
    let dir = Dir::new(name);
    fs::create_dir_all(dir.file("assets")).unwrap();
    tone_wav(&dir.file("assets/tone.wav"));
    fs::write(
        dir.file("hyperframes.json"),
        r#"{"paths":{"blocks":"compositions","components":"compositions/components","assets":"assets"}}"#,
    )
    .unwrap();
    fs::write(
        dir.file("index.html"),
        format!(
            "<!DOCTYPE html>\n<html lang=\"en\"><head><meta charset=\"UTF-8\">\
             <meta name=\"viewport\" content=\"width=320, height=180\">\
             <style>html,body{{margin:0;width:320px;height:180px;background:#123}}</style></head>\n\
             <body><div id=\"root\" data-composition-id=\"root\" data-start=\"0\" data-width=\"320\" \
             data-height=\"180\" data-duration=\"3\">\n\
             <audio id=\"tone\" src=\"assets/tone.wav\" data-start=\"{start}\" data-duration=\"2\" \
             data-track-index=\"1\" data-volume=\"0.5\"></audio></div>\n\
             <script>window.__timelines = window.__timelines || {{}}; window.__timelines[\"root\"] = \
             {{ duration: () => 3, time: () => 0, seek() {{}}, pause() {{}}, play() {{}}, \
             totalTime() {{ return 0; }}, paused() {{ return true; }}, progress() {{ return 0; }} }};</script>\n\
             </body></html>\n"
        ),
    )
    .unwrap();
    dir
}

fn hyperframes_tracks(project: &Path) -> Vec<HyperframesTrack> {
    let json = run(
        "node",
        &[
            support("hyperframes-tracks.mjs").to_str().unwrap(),
            hyperframes_dir().to_str().unwrap(),
            project.join("index.html").to_str().unwrap(),
        ],
        project,
    )
    .unwrap();
    serde_json::from_str(&json).unwrap()
}

#[test]
fn hyperframes_tone_at_an_offset_matches_ref_within_one_millisecond() {
    let dir = hyperframes_fixture("hf", "0.2335");
    let reference = dir.file("ref.mp4");
    run(
        hyperframes_dir().join("../.bin/hyperframes"),
        &[
            "render",
            dir.0.to_str().unwrap(),
            "-o",
            reference.to_str().unwrap(),
            "--quiet",
        ],
        &bench_dir().join("engines"),
    )
    .unwrap();
    let expected = decode(&reference);

    let tracks = hyperframes_tracks(&dir.0);
    assert_eq!(tracks.len(), 1);
    let clips = hyperframes_clips(&tracks, |src| Ok(dir.file(src))).unwrap();
    let ours = mix(&clips, 3 * u64::from(SAMPLE_RATE)).unwrap();

    let (a, b) = (onset(&ours, 0.02), onset(&expected, 0.02));
    eprintln!("onset ours {a}, REF {b}, ideal {}", 0.2335 * 48000.0);
    assert!(a.abs_diff(b) <= 48, "onset {a} vs REF {b}");
    let (ra, rb) = (
        rms(&ours, b + 2_000, 90_000),
        rms(&expected, b + 2_000, 90_000),
    );
    eprintln!("rms ours {ra:.6}, REF {rb:.6}");
    assert!(((ra - rb) / rb).abs() < 0.01);
    assert_eq!(ours.len(), expected.len());
}

#[test]
fn volume_frames_hold_from_half_a_frame_before_to_half_a_frame_after() {
    let volume = Volume::Frames {
        fps: 30.0,
        values: vec![1.0, 0.5, 0.25],
    };
    let samples_per_frame = 1600;
    assert_eq!(volume.at(0), 1.0);
    assert_eq!(volume.at(samples_per_frame / 2 - 1), 1.0);
    assert_eq!(volume.at(samples_per_frame / 2), 0.5);
    assert_eq!(volume.at(samples_per_frame + 799), 0.5);
    assert_eq!(volume.at(samples_per_frame * 2), 0.25);
    assert_eq!(volume.at(samples_per_frame * 9), 0.25);
}

struct Entry {
    id: &'static str,
    project: &'static str,
    entry: &'static str,
    composition: &'static str,
    fps: u32,
    frames: u32,
}

const REMOTION: [Entry; 3] = [
    Entry {
        id: "rm-transitions-video",
        project: "transitions-video",
        entry: "src/index.ts",
        composition: "Main",
        fps: 30,
        frames: 840,
    },
    Entry {
        id: "rm-4-0-trailer",
        project: "4-0-trailer",
        entry: "src/index.ts",
        composition: "Intro",
        fps: 30,
        frames: 1900,
    },
    Entry {
        id: "rm-github-unwrapped",
        project: "github-unwrapped",
        entry: "remotion/index.ts",
        composition: "Main",
        fps: 30,
        frames: 1692,
    },
];

const HYPERFRAMES: [Entry; 4] = [
    Entry {
        id: "hf-figma-launch",
        project: "figma-launch",
        entry: "",
        composition: "",
        fps: 30,
        frames: 1922,
    },
    Entry {
        id: "hf-heygen-stripe",
        project: "HF-heygen-stripe",
        entry: "",
        composition: "",
        fps: 30,
        frames: 1161,
    },
    Entry {
        id: "hf-hyperframes-launch",
        project: "hyperframes-launch",
        entry: "",
        composition: "",
        fps: 30,
        frames: 1494,
    },
    Entry {
        id: "hf-spacex-launch",
        project: "spacex-launch",
        entry: "",
        composition: "",
        fps: 30,
        frames: 1185,
    },
];

const SPAN_SECONDS: u64 = 10;

fn loudest_span(clips: &[Clip], total_seconds: u64) -> u64 {
    let rate = u64::from(SAMPLE_RATE);
    let weight = |second: u64| -> f64 {
        let (from, to) = (second * rate, (second + SPAN_SECONDS) * rate);
        clips
            .iter()
            .map(|c| {
                let overlap = (c.start + c.length)
                    .min(to)
                    .saturating_sub(c.start.max(from));
                let gain = match &c.volume {
                    Volume::Constant(v) => *v,
                    Volume::Frames { values, .. } => {
                        values.iter().sum::<f64>() / values.len() as f64
                    }
                };
                overlap as f64 * gain
            })
            .sum()
    };
    (0..=total_seconds.saturating_sub(SPAN_SECONDS))
        .max_by(|a, b| weight(*a).total_cmp(&weight(*b)))
        .unwrap_or(0)
}

struct Row {
    id: &'static str,
    clips: usize,
    span_start: u64,
    onset_ours: Option<usize>,
    onset_ref: Option<usize>,
    rms_ours: f64,
    rms_ref: f64,
    error: f64,
    lag: i64,
    worst: f32,
    worst_at: usize,
}

fn first_sound(stereo: &[f32], threshold: f32) -> Option<usize> {
    stereo
        .as_chunks::<2>()
        .0
        .iter()
        .position(|f| f[0].abs() > threshold)
}

fn best_lag(ours: &[f32], reference: &[f32], reach: i64) -> i64 {
    let rate = SAMPLE_RATE as usize;
    let frames = ours.len() / 2;
    let energy = |from: usize| -> f64 {
        ours[from * 2..(from + rate) * 2]
            .iter()
            .map(|v| f64::from(*v).powi(2))
            .sum()
    };
    let from = (0..frames.saturating_sub(rate + reach as usize))
        .step_by(rate / 4)
        .skip(1)
        .max_by(|a, b| energy(*a).total_cmp(&energy(*b)))
        .unwrap_or(reach as usize);
    (-reach..=reach)
        .max_by(|a, b| {
            let score = |lag: i64| -> f64 {
                (from..from + rate)
                    .map(|i| {
                        f64::from(ours[i * 2])
                            * f64::from(reference[((i as i64 + lag) as usize) * 2])
                    })
                    .sum()
            };
            score(*a).total_cmp(&score(*b))
        })
        .unwrap()
}

fn compare(
    id: &'static str,
    clips: usize,
    span_start: u64,
    ours: &[f32],
    reference: &[f32],
) -> Row {
    let frames = (ours.len() / 2).min(reference.len() / 2);
    let (ours, reference) = (&ours[..frames * 2], &reference[..frames * 2]);
    let diff: Vec<f32> = ours.iter().zip(reference).map(|(a, b)| a - b).collect();
    let reach = 4_800;
    Row {
        id,
        clips,
        span_start,
        onset_ours: first_sound(ours, 1e-3),
        onset_ref: first_sound(reference, 1e-3),
        rms_ours: rms(ours, 0, frames),
        rms_ref: rms(reference, 0, frames),
        error: rms(&diff, 0, frames),
        lag: best_lag(
            &ours[reach as usize * 2..],
            &reference[reach as usize * 2..],
            reach,
        ),
        worst: diff.iter().fold(0.0, |m, v| m.max(v.abs())),
        worst_at: diff
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .map_or(0, |(i, _)| i / 2),
    }
}

fn print_row(row: &Row) {
    eprintln!(
        "{:<24} clips {:>2}  span {:>2}-{:>2}s  onset ours/REF {:?}/{:?}  rms ours {:.5} REF {:.5}  error {:.5} ({:+.1} dB)  lag {:+}  worst {:.5} at {:.3}s",
        row.id,
        row.clips,
        row.span_start,
        row.span_start + SPAN_SECONDS,
        row.onset_ours,
        row.onset_ref,
        row.rms_ours,
        row.rms_ref,
        row.error,
        20.0 * (row.error / row.rms_ref.max(1e-12)).log10(),
        row.lag,
        row.worst,
        row.worst_at as f64 / f64::from(SAMPLE_RATE)
    );
}

fn remotion_corpus_row(entry: &Entry) -> Row {
    let project = bench_dir().join("corpus/external").join(entry.project);
    let dir = Dir::new(entry.id);
    let collected = remotion_assets(&project, entry.entry, entry.composition).unwrap();
    let total = u64::from(entry.frames) / u64::from(entry.fps);
    let probe = remotion_clips(&collected.frames, collected.fps, public_file(&project)).unwrap();
    let start = loudest_span(&probe, total);
    let first = (start * u64::from(entry.fps)) as usize;
    let last = first + (SPAN_SECONDS * u64::from(entry.fps)) as usize;
    let span: Vec<Vec<RemotionAsset>> = collected.frames[first..last].to_vec();
    let clips = remotion_clips(&span, collected.fps, public_file(&project)).unwrap();
    let mut ours = mix(&clips, SPAN_SECONDS * u64::from(SAMPLE_RATE)).unwrap();
    saturate(&mut ours);
    let reference = dir.file("ref.wav");
    run(
        project.join("node_modules/.bin/remotion"),
        &[
            "render",
            entry.entry,
            entry.composition,
            reference.to_str().unwrap(),
            "--codec=wav",
            &format!("--frames={first}-{}", last - 1),
            "--browser-executable",
            headless_shell().to_str().unwrap(),
        ],
        &project,
    )
    .unwrap();
    let expected = decode(&reference);
    if let Some(dump) = std::env::var_os("VRF_DUMP") {
        let bytes = |v: &[f32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        fs::write(
            Path::new(&dump).join(format!("{}.ours", entry.id)),
            bytes(&ours),
        )
        .unwrap();
        fs::write(
            Path::new(&dump).join(format!("{}.ref", entry.id)),
            bytes(&expected),
        )
        .unwrap();
    }
    compare(entry.id, clips.len(), start, &ours, &expected)
}

fn hyperframes_corpus_row(entry: &Entry) -> Row {
    let project = bench_dir()
        .join("corpus/external/hyperframes-launches")
        .join(entry.project);
    let json = run(
        "node",
        &[
            support("hyperframes-tracks.mjs").to_str().unwrap(),
            hyperframes_dir().to_str().unwrap(),
            project.to_str().unwrap(),
        ],
        &project,
    )
    .unwrap();
    let tracks: Vec<HyperframesTrack> = serde_json::from_str(&json).unwrap();
    let all = hyperframes_clips(&tracks, |src| Ok(project.join(src))).unwrap();
    let total = u64::from(entry.frames) / u64::from(entry.fps);
    let start = loudest_span(&all, total);
    let full = mix(
        &all,
        (u64::from(entry.frames) * u64::from(SAMPLE_RATE)) / u64::from(entry.fps),
    )
    .unwrap();
    let from = (start * u64::from(SAMPLE_RATE)) as usize * 2;
    let ours = &full[from..from + (SPAN_SECONDS * u64::from(SAMPLE_RATE)) as usize * 2];

    let dir = Dir::new(entry.id);
    let reference = dir.file("ref.mp4");
    run(
        hyperframes_dir().join("../.bin/hyperframes"),
        &[
            "render",
            project.to_str().unwrap(),
            "-o",
            reference.to_str().unwrap(),
            "--fps",
            "1",
            "--quiet",
        ],
        &bench_dir().join("engines"),
    )
    .unwrap();
    let decoded = decode(&reference);
    compare(
        entry.id,
        all.len(),
        start,
        ours,
        &decoded[from..from + ours.len()],
    )
}

#[test]
#[ignore = "renders every corpus composition's audio with its own framework; run with --ignored --nocapture"]
fn corpus_audio_matches_each_frameworks_own_mix_over_ten_seconds() {
    let mut rows = Vec::new();
    for entry in REMOTION
        .iter()
        .filter(|e| std::env::var("VRF_ONLY").map_or(true, |o| o == e.id))
    {
        rows.push(remotion_corpus_row(entry));
        print_row(rows.last().unwrap());
    }
    for entry in HYPERFRAMES
        .iter()
        .filter(|e| std::env::var("VRF_ONLY").map_or(true, |o| o == e.id))
    {
        rows.push(hyperframes_corpus_row(entry));
        print_row(rows.last().unwrap());
    }
}
