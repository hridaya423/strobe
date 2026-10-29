#[allow(dead_code)]
mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, ensure};
use vrf_bench::corpus::bench_dir;
use vrf_bench::score::{self, Sampling};
use vrf_blink::adapters::hyperframes::{self, Options};
use vrf_blink::{Mode, Ui};
use vrf_encode::Codec;

const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;
const ID_BITS: u32 = 16;
const ID_CELL: u32 = 16;

fn work_dir() -> PathBuf {
    std::env::temp_dir().join("vrf-hyperframes-tests")
}

fn hyperframes_package() -> PathBuf {
    bench_dir().join("engines/node_modules/hyperframes")
}

fn run(program: impl AsRef<std::ffi::OsStr>, args: &[&str], cwd: &Path) -> Result<()> {
    let output = Command::new(program.as_ref())
        .args(args)
        .current_dir(cwd)
        .output()?;
    ensure!(
        output.status.success(),
        "{:?} {args:?}:\n{}{}",
        program.as_ref(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn tone_wav(path: &Path) -> Result<()> {
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
}

fn counted_clip(path: &Path) -> Result<()> {
    let region = |w: u32, h: u32| format!("lt(Y,{h})*lt(X,{w})");
    let bits = format!("16+219*mod(floor(N/pow(2,floor(X/{ID_CELL}))),2)");
    let filter = format!(
        "format=yuv420p,geq=lum='if({},{bits},lum(X,Y))':cb='if({},128,cb(X,Y))':cr='if({},128,cr(X,Y))'",
        region(ID_BITS * ID_CELL, ID_CELL),
        region(ID_BITS * ID_CELL / 2, ID_CELL / 2),
        region(ID_BITS * ID_CELL / 2, ID_CELL / 2),
    );
    run(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc2=size={WIDTH}x{HEIGHT}:rate=30:duration=2"),
            "-vf",
            &filter,
            "-c:v",
            "libx264",
            "-crf",
            "8",
            "-g",
            "15",
            "-pix_fmt",
            "yuv420p",
            "-colorspace",
            "bt709",
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
            "-color_range",
            "tv",
            path.to_str().unwrap(),
        ],
        Path::new("/"),
    )
}

fn fixture(name: &str, seconds: u32, body: &str, script: &str) -> Result<PathBuf> {
    let dir = work_dir().join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("assets"))?;
    fs::copy(
        hyperframes_package().join("dist/vendor/npm/gsap@3.15.0/dist/gsap.min.js"),
        dir.join("gsap.min.js"),
    )?;
    fs::write(
        dir.join("index.html"),
        format!(
            "<!doctype html>\n<html><head><meta charset=\"utf-8\"><style>\
             html,body{{margin:0;background:#123}}\
             #root{{position:relative;width:{WIDTH}px;height:{HEIGHT}px;overflow:hidden;background:#123}}\
             </style></head><body>\n\
             <div id=\"root\" data-composition-id=\"main\" data-width=\"{WIDTH}\" data-height=\"{HEIGHT}\" \
             data-duration=\"{seconds}\" data-start=\"0\" data-fps=\"30\">{body}</div>\n\
             <script src=\"gsap.min.js\"></script>\n<script>{script}</script></body></html>\n"
        ),
    )?;
    Ok(dir)
}

const BOX: &str = "<div id=\"box\" style=\"position:absolute;left:0;top:60px;width:60px;height:60px;background:#f84\"></div>";

fn sweep(seconds: u32) -> String {
    format!(
        "const tl = gsap.timeline({{paused: true}}); \
         tl.to('#box', {{x: 240, duration: {seconds}, ease: 'none'}}); \
         window.__timelines = {{main: tl}};"
    )
}

fn options(project: &Path) -> Options {
    Options {
        project: project.to_owned(),
        hyperframes: Some(hyperframes_package()),
        fps: 30,
        mode: Mode::Offline,
    }
}

fn hyperframes_render(project: &Path, out: &Path) -> Result<()> {
    run(
        hyperframes_package().join("../.bin/hyperframes"),
        &[
            "render",
            project.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
            "--quiet",
        ],
        &bench_dir().join("engines"),
    )
}

fn frames_of(path: &Path) -> Result<Vec<score::Rgb8>> {
    score::open(path)?
        .frames
        .map(|frame| Ok(frame?.1))
        .collect()
}

fn frame_id(frame: &score::Rgb8) -> u32 {
    (0..ID_BITS)
        .map(|bit| {
            let at = (((ID_CELL / 2) * frame.width + bit * ID_CELL + ID_CELL / 2) * 3 + 1) as usize;
            u32::from(frame.data[at] > 128) << bit
        })
        .sum()
}

fn frames_and_audio_match_hyperframes_own_render(ui: &Ui) -> Result<()> {
    let seconds = 3;
    let project = fixture(
        "tone",
        seconds,
        &format!(
            "{BOX}<audio id=\"tone\" src=\"assets/tone.wav\" data-start=\"0.2335\" data-duration=\"2\" \
             data-track-index=\"1\" data-volume=\"0.5\"></audio>"
        ),
        &sweep(seconds),
    )?;
    tone_wav(&project.join("assets/tone.wav"))?;
    let ours = work_dir().join("tone-ours.mp4");
    let theirs = work_dir().join("tone-theirs.mp4");
    let report = hyperframes::render(ui, &options(&project), None, Codec::H264, &ours)?;
    println!("{}", serde_json::to_string(&report)?);
    ensure!(report.frames == 90, "{} frames", report.frames);
    hyperframes_render(&project, &theirs)?;

    let scored_ours = score::score(&ours, &theirs, Sampling::All, None)?;
    let worst = &scored_ours.worst;
    println!(
        "tone video vs hyperframes: FLIP mean {:.5}, p99.9 {:.4}, max delta {}, over 8 {:.4}%, over 32 {:.4}%",
        worst.flip_mean.value,
        worst.flip_p999.value,
        worst.max_delta.value,
        worst.frac_over_8.value * 100.0,
        worst.frac_over_32.value * 100.0
    );
    ensure!(scored_ours.frames_compared == 90);
    ensure!(
        worst.flip_mean.value <= 0.02,
        "FLIP mean {}",
        worst.flip_mean.value
    );
    ensure!(
        worst.frac_over_32.value <= 0.001,
        "{} of pixels over 32",
        worst.frac_over_32.value
    );

    let (ours_audio, their_audio) = (
        support::decode_audio(&ours)?,
        support::decode_audio(&theirs)?,
    );
    let (a, b) = (
        support::onset(&ours_audio, 0.02).context("our audio is silent")?,
        support::onset(&their_audio, 0.02).context("their audio is silent")?,
    );
    let (ra, rb) = (
        support::rms(&ours_audio, b + 2_000, 90_000),
        support::rms(&their_audio, b + 2_000, 90_000),
    );
    println!(
        "tone audio: onset ours {a}, hyperframes {b}, ideal {}; rms ours {ra:.6}, hyperframes {rb:.6}; {} and {} frames",
        0.2335 * 48000.0,
        ours_audio.len() / 2,
        their_audio.len() / 2
    );
    ensure!(a.abs_diff(b) <= 48, "onset {a} vs hyperframes {b}");
    ensure!(((ra - rb) / rb).abs() < 0.01, "rms {ra} vs {rb}");
    ensure!(ours_audio.len() == their_audio.len());
    Ok(())
}

fn embedded_h264_shows_the_right_frame_on_every_frame(ui: &Ui) -> Result<()> {
    let seconds = 2;
    let project = fixture(
        "counted",
        seconds,
        "<video id=\"v\" src=\"assets/counted.mp4\" data-start=\"0.5\" data-media-start=\"0.2\" \
         data-duration=\"1.5\" data-track-index=\"0\" muted playsinline \
         style=\"position:absolute;left:0;top:0;width:320px;height:180px\"></video>",
        "const tl = gsap.timeline({paused: true}); tl.to('#root', {opacity: 1, duration: 2}); window.__timelines = {main: tl};",
    )?;
    counted_clip(&project.join("assets/counted.mp4"))?;
    let out = work_dir().join("counted.mp4");
    let report = hyperframes::render(ui, &options(&project), None, Codec::H264, &out)?;
    ensure!(report.frames == 60, "{} frames", report.frames);
    let ids: Vec<u32> = frames_of(&out)?.iter().map(frame_id).collect();
    let expected: Vec<u32> = (0..60u32)
        .map(|f| f.checked_sub(15).map_or(0, |n| n + 6))
        .collect();
    let wrong: Vec<(usize, u32, u32)> = ids
        .iter()
        .zip(&expected)
        .enumerate()
        .filter(|(_, (got, want))| got != want)
        .map(|(f, (got, want))| (f, *got, *want))
        .collect();
    println!(
        "counted: {} of {} frames show the expected id",
        ids.len() - wrong.len(),
        ids.len()
    );
    ensure!(wrong.is_empty(), "frame, got, want: {wrong:?}");
    Ok(())
}

fn a_project_without_audio_stays_video_only(ui: &Ui) -> Result<()> {
    let seconds = 1;
    let project = fixture("silent", seconds, BOX, &sweep(seconds))?;
    let out = work_dir().join("silent.mp4");
    hyperframes::render(ui, &options(&project), None, Codec::H264, &out)?;
    ensure!(
        !support::has_audio_stream(&out)?,
        "the file has an audio stream"
    );
    Ok(())
}

fn frame_range_renders_the_slice(ui: &Ui) -> Result<()> {
    let seconds = 2;
    let project = fixture("range", seconds, BOX, &sweep(seconds))?;
    let out = work_dir().join("range.mp4");
    let report = hyperframes::render(ui, &options(&project), Some(20..=29), Codec::H264, &out)?;
    ensure!(report.frames == 10 && report.first_frame == 20);
    let frames = frames_of(&out)?;
    ensure!(frames.len() == 10, "{} frames in the file", frames.len());
    let box_x = |frame: &score::Rgb8| {
        (0..frame.width)
            .find(|x| {
                let at = (((HEIGHT / 2) * frame.width + x) * 3) as usize;
                frame.data[at] > 200 && frame.data[at + 2] < 120
            })
            .map(|x| x as i32)
    };
    let (first, last) = (box_x(&frames[0]), box_x(&frames[9]));
    println!("box left edge at frame 20: {first:?}, at 29: {last:?}");
    ensure!(
        first.is_some_and(|x| (x - 80).abs() <= 3) && last.is_some_and(|x| (x - 116).abs() <= 3),
        "box at {first:?} and {last:?}"
    );
    Ok(())
}

fn main() -> ExitCode {
    vrf_blink::set_cpu_raster(true);
    support::run(&[
        (
            "hyperframes_frames_and_audio_match_hyperframes_own_render",
            frames_and_audio_match_hyperframes_own_render,
        ),
        (
            "hyperframes_embedded_h264_shows_exact_frame_ids",
            embedded_h264_shows_the_right_frame_on_every_frame,
        ),
        (
            "hyperframes_project_without_audio_stays_video_only",
            a_project_without_audio_stays_video_only,
        ),
        ("hyperframes_frame_range", frame_range_renders_the_slice),
    ])
}
