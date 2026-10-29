#![cfg(target_os = "macos")]

mod turbo_support;

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, MutexGuard};

use turbo_support::*;
use vrf::mux::AudioTrack;
use vrf_encode::Quality;
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

fn has_libvmaf() -> bool {
    run("ffmpeg", &["-hide_banner", "-filters"]).contains("libvmaf")
}

fn per_frame_scores(
    distorted: &Path,
    source: &Path,
    size: &str,
    dir: &Dir,
    vmaf: bool,
) -> Vec<f64> {
    let log = dir.file("scores.txt");
    let decoded = dir.file("decoded.nv12");
    run(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-y",
            "-i",
            distorted.to_str().unwrap(),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "nv12",
            decoded.to_str().unwrap(),
        ],
    );
    let filter = if vmaf {
        format!(
            "[0:v]format=yuv420p[d];[1:v]format=yuv420p[r];[d][r]libvmaf=log_fmt=json:log_path={}:n_threads=8",
            log.display()
        )
    } else {
        format!(
            "[0:v]format=yuv420p[d];[1:v]format=yuv420p[r];[d][r]ssim=stats_file={}",
            log.display()
        )
    };
    let mut args = vec![
        "-v", "error", "-f", "rawvideo", "-pix_fmt", "nv12", "-s", size, "-r", "30",
    ];
    args.extend(["-i", decoded.to_str().unwrap()]);
    args.extend(["-f", "rawvideo", "-pix_fmt", "nv12", "-s", size, "-r", "30"]);
    args.extend([
        "-i",
        source.to_str().unwrap(),
        "-lavfi",
        &filter,
        "-f",
        "null",
        "-",
    ]);
    run("ffmpeg", &args);
    let text = fs::read_to_string(&log).unwrap();
    if vmaf {
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        json["frames"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["metrics"]["vmaf"].as_f64().unwrap())
            .collect()
    } else {
        text.lines()
            .map(|line| {
                let all = line.split("All:").nth(1).unwrap();
                all.split_whitespace().next().unwrap().parse().unwrap()
            })
            .collect()
    }
}

fn per_chunk(scores: &[f64], chunk: usize) -> Vec<f64> {
    scores
        .chunks(chunk)
        .map(|c| c.iter().sum::<f64>() / c.len() as f64)
        .collect()
}

fn quality_parity(quality: Quality) {
    let _engine = engine();
    let (width, height, frames, chunk) = (960, 540, 480, 60);
    let dir = Dir::new("quality");
    let content = Content::new(width, height, 60);
    let source = dir.file("source.nv12");
    let mut raw = Vec::new();
    for index in 0..frames {
        let (luma, chroma) = content.frame(index);
        raw.extend_from_slice(luma);
        raw.extend_from_slice(chroma);
    }
    fs::write(&source, &raw).unwrap();
    drop(raw);
    let size = format!("{width}x{height}");

    let single = dir.file("single.mp4");
    encode_single(&single, &content, frames, quality).unwrap();
    let turbo = dir.file("turbo.mp4");
    let stats = encode_content(
        &turbo,
        &content,
        frames,
        options(chunk as u32),
        None,
        quality,
    )
    .unwrap();
    eprintln!("lanes: {stats:?}");
    assert!(stats.iter().all(|l| l.chunks > 0), "{stats:?}");
    let lane = dir.file("x264-lane.mp4");
    encode_x264(&lane, &content, frames, quality).unwrap();
    let reference = dir.file("x264-crf18.mp4");
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
            "-r",
            "30",
            "-i",
            source.to_str().unwrap(),
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-crf",
            "18",
            "-bf",
            "0",
            reference.to_str().unwrap(),
        ],
    );

    for name in ["single", "turbo", "x264-lane", "x264-crf18"] {
        let bytes = fs::metadata(dir.file(&format!("{name}.mp4")))
            .unwrap()
            .len();
        eprintln!(
            "{name}: {:.2} Mb/s",
            bytes as f64 * 8.0 / (frames as f64 / 30.0) / 1e6
        );
    }
    let vmaf = has_libvmaf();
    let metric = if vmaf { "VMAF" } else { "SSIM" };
    let allowed = if vmaf { 1.0 } else { 0.01 };
    let score = |file: &Path| per_chunk(&per_frame_scores(file, &source, &size, &dir, vmaf), chunk);
    let (single, turbo, reference, lane) = (
        score(&single),
        score(&turbo),
        score(&reference),
        score(&lane),
    );
    assert_eq!(turbo.len(), frames / chunk);
    eprintln!("{metric} per chunk (allowed loss {allowed}):");
    for c in 0..turbo.len() {
        eprintln!(
            "chunk {c}: single VideoToolbox {:.3}, turbo {:.3}, x264 lane alone {:.3}, libx264 crf 18 {:.3}",
            single[c], turbo[c], lane[c], reference[c]
        );
        assert!(
            turbo[c] >= single[c] - allowed,
            "chunk {c}: turbo {:.3} is more than {allowed} below the single encoder's {:.3}",
            turbo[c],
            single[c]
        );
    }
}

#[test]
fn each_turbo_chunk_scores_within_one_point_of_the_single_encoder_at_draft_quality() {
    quality_parity(Quality::Draft);
}

#[test]
fn each_turbo_chunk_scores_within_one_point_of_the_single_encoder_at_balanced_quality() {
    quality_parity(Quality::Balanced);
}

#[test]
fn each_turbo_chunk_scores_within_one_point_of_the_single_encoder_at_high_quality() {
    quality_parity(Quality::High);
}
