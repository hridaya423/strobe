use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use vrf::audio::{
    HyperframesTrack, RemotionAsset, SAMPLE_RATE, Volume, hyperframes_clips, mix, remotion_clips,
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
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
        .collect()
}

fn onset(stereo: &[f32], threshold: f32) -> usize {
    stereo
        .chunks_exact(2)
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
