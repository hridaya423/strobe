#![cfg(target_os = "macos")]

mod turbo_support;

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, MutexGuard};

use turbo_support::*;
use vrf::mux::AudioTrack;
use vrf_encode::turbo::LaneKind;

static ENGINE: Mutex<()> = Mutex::new(());

fn engine() -> MutexGuard<'static, ()> {
    ENGINE.lock().unwrap_or_else(|e| e.into_inner())
}

fn count(haystack: &[u8], needle: &[u8; 4]) -> usize {
    haystack.windows(4).filter(|w| w == needle).count()
}

fn ffmpeg_md5s(file: &Path) -> Vec<String> {
    run(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-i",
            file.to_str().unwrap(),
            "-an",
            "-pix_fmt",
            "nv12",
            "-f",
            "framemd5",
            "-",
        ],
    )
    .lines()
    .filter(|l| !l.starts_with('#'))
    .map(|l| l.rsplit(", ").next().unwrap().to_string())
    .collect()
}

fn avfoundation_md5s(file: &Path, dir: &Dir) -> Vec<String> {
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/research/2026-10-29/encode-bench/avdecode.swift");
    let raw = dir.file("av.nv12");
    let log = run(
        "swift",
        &[
            script.to_str().unwrap(),
            file.to_str().unwrap(),
            raw.to_str().unwrap(),
        ],
    );
    eprintln!("{log}");
    let size = format!("{WIDTH}x{HEIGHT}");
    run(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "nv12",
            "-s",
            &size,
            "-i",
            raw.to_str().unwrap(),
            "-f",
            "framemd5",
            "-",
        ],
    )
    .lines()
    .filter(|l| !l.starts_with('#'))
    .map(|l| l.rsplit(", ").next().unwrap().to_string())
    .collect()
}

fn decodes_cleanly(file: &Path) {
    let out = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-i",
            file.to_str().unwrap(),
            "-f",
            "null",
            "-",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && stderr.is_empty(), "{stderr}");
}

#[test]
fn an_1800_frame_turbo_file_is_one_continuous_stream_that_both_decoders_agree_on() {
    let _engine = engine();
    let dir = Dir::new("1800");
    let out = dir.file("turbo.mp4");
    let stats = encode_turbo(&out, 1800, options(60), None).unwrap();
    eprintln!("lanes: {stats:?}");
    assert!(
        stats.iter().all(|lane| lane.chunks > 0),
        "both lanes must have encoded chunks: {stats:?}"
    );
    let x264 = stats.iter().find(|l| l.kind == LaneKind::X264).unwrap();
    assert!(x264.frames > 0);

    let streams = probe(&out, &["-show_entries", "stream=codec_type,nb_streams"]);
    assert_eq!(streams.len(), 1, "{streams:?}");
    let video = probe(
        &out,
        &[
            "-count_frames",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name,profile,width,height,time_base,avg_frame_rate,nb_read_frames,\
             color_range,color_space,color_transfer,color_primaries,duration",
        ],
    )
    .remove(0);
    assert_eq!(video["codec_name"], "h264");
    assert_eq!(video["nb_read_frames"], "1800");
    assert_eq!(video["avg_frame_rate"], "30/1");
    assert_eq!(video["time_base"], "1/30");
    assert_eq!(video["duration"].parse::<f64>().unwrap(), 60.0);
    assert_eq!(video["color_space"], "bt709");
    assert_eq!(video["color_range"], "tv");

    let pts: Vec<i64> = probe(
        &out,
        &["-select_streams", "v:0", "-show_entries", "frame=pts"],
    )
    .iter()
    .map(|f| f["pts"].parse().unwrap())
    .collect();
    assert_eq!(pts, (0..1800).collect::<Vec<_>>());

    let keyframes: Vec<i64> = probe(
        &out,
        &[
            "-select_streams",
            "v:0",
            "-show_entries",
            "frame=pts,key_frame",
        ],
    )
    .iter()
    .filter(|f| f["key_frame"] == "1")
    .map(|f| f["pts"].parse().unwrap())
    .collect();
    for chunk in 0..30 {
        assert!(
            keyframes.contains(&(chunk * 60)),
            "chunk {chunk} does not start on a keyframe"
        );
    }

    let bytes = fs::read(&out).unwrap();
    assert_eq!(count(&bytes, b"avcC"), 1, "one sample entry");
    decodes_cleanly(&out);

    let ffmpeg = ffmpeg_md5s(&out);
    assert_eq!(ffmpeg.len(), 1800);
    let av = avfoundation_md5s(&out, &dir);
    assert_eq!(av.len(), 1800);
    let differing: Vec<usize> = (0..1800).filter(|&i| ffmpeg[i] != av[i]).collect();
    assert!(
        differing.is_empty(),
        "{} frames differ between ffmpeg and AVFoundation, first {:?}",
        differing.len(),
        &differing[..differing.len().min(10)]
    );
}

#[test]
fn every_chunk_of_a_turbo_file_decodes_to_the_frames_its_lane_produced() {
    let _engine = engine();
    let dir = Dir::new("lanes");
    let out = dir.file("turbo.mp4");
    encode_turbo(&out, 240, options(60), None).unwrap();
    let whole = ffmpeg_md5s(&out);
    for chunk in 0..4 {
        let part = dir.file(&format!("part{chunk}.mp4"));
        run(
            "ffmpeg",
            &[
                "-v",
                "error",
                "-y",
                "-i",
                out.to_str().unwrap(),
                "-ss",
                &format!("{}", chunk * 2),
                "-t",
                "2",
                "-c",
                "copy",
                "-avoid_negative_ts",
                "make_zero",
                part.to_str().unwrap(),
            ],
        );
        assert_eq!(
            ffmpeg_md5s(&part),
            whole[chunk * 60..(chunk + 1) * 60],
            "chunk {chunk} decodes differently on its own"
        );
    }
}

const AUDIO_RATE: u32 = 48_000;
const CLICK_FRAMES: [usize; 4] = [30, 60, 90, 120];

fn click_track(seconds: usize) -> Vec<f32> {
    let mut samples = vec![0.0f32; seconds * AUDIO_RATE as usize * 2];
    for video_frame in CLICK_FRAMES {
        let start = video_frame * (AUDIO_RATE as usize / 30);
        for i in 0..240 {
            let envelope = (std::f32::consts::PI * i as f32 / 240.0).sin().powi(2);
            let v = 0.8
                * envelope
                * (i as f32 * 2_000.0 * std::f32::consts::TAU / AUDIO_RATE as f32).sin();
            samples[(start + i) * 2] = v;
            samples[(start + i) * 2 + 1] = v;
        }
    }
    samples
}

fn decode_audio(file: &Path) -> Vec<f32> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i", file.to_str().unwrap()])
        .args([
            "-map", "0:a:0", "-f", "f32le", "-ar", "48000", "-ac", "2", "-",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    out.stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect()
}

fn centroid(stereo: &[f32], around: usize, half: usize) -> f64 {
    let (mut weight, mut moment) = (0.0, 0.0);
    for frame in around.saturating_sub(half)..(around + half).min(stereo.len() / 2) {
        let e = f64::from(stereo[frame * 2]).powi(2);
        weight += e;
        moment += e * frame as f64;
    }
    moment / weight
}

#[test]
fn audio_still_muxes_with_a_turbo_video_track() {
    let _engine = engine();
    let dir = Dir::new("audio");
    let out = dir.file("turbo-audio.mp4");
    let input = click_track(5);
    let stream = vrf_encode::audiotoolbox::encode_aac(
        &input,
        vrf_encode::AacConfig {
            sample_rate: AUDIO_RATE,
            channels: 2,
            bit_rate: 128_000,
        },
    )
    .unwrap();
    let stats = encode_turbo(&out, 150, options(30), Some(AudioTrack::from(&stream))).unwrap();
    assert!(stats.iter().all(|l| l.chunks > 0), "{stats:?}");

    let audio = probe(
        &out,
        &[
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=codec_name,sample_rate,channels,start_time",
        ],
    )
    .remove(0);
    assert_eq!(audio["codec_name"], "aac");
    assert_eq!(audio["start_time"].parse::<f64>().unwrap(), 0.0);
    let video = probe(
        &out,
        &[
            "-count_frames",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=nb_read_frames,start_time",
        ],
    )
    .remove(0);
    assert_eq!(video["nb_read_frames"], "150");
    assert_eq!(video["start_time"].parse::<f64>().unwrap(), 0.0);
    decodes_cleanly(&out);

    let decoded = decode_audio(&out);
    assert_eq!(decoded.len(), input.len());
    for frame in CLICK_FRAMES {
        let expected = frame * (AUDIO_RATE as usize / 30) + 120;
        let found = centroid(&decoded, expected, 2_048);
        assert!(
            (found - expected as f64).abs() < 64.0,
            "click at video frame {frame} is {} samples off",
            found - expected as f64
        );
    }
    assert_eq!(ffmpeg_md5s(&out).len(), 150);
}
