mod clips;
#[allow(dead_code)]
mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::SystemTime;

use anyhow::{Context, Result, ensure};
use vrf_bench::corpus::bench_dir;
use vrf_bench::score::{self, Sampling};
use vrf_blink::adapters::hyperframes::{self, Options};
use vrf_blink::driver::{Plan, Warm};
use vrf_blink::{Mode, Ui};
use vrf_encode::Codec;

const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;
const ID_BITS: u32 = 16;
const ID_CELL: u32 = 16;

fn work_dir() -> PathBuf {
    std::env::temp_dir().join(format!("vrf-hyperframes-tests-{}", std::process::id()))
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
    sized_fixture(name, WIDTH, HEIGHT, seconds, body, script)
}

fn sized_fixture(name: &str, width: u32, height: u32, seconds: u32, body: &str, script: &str) -> Result<PathBuf> {
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
             #root{{position:relative;width:{width}px;height:{height}px;overflow:hidden;background:#123}}\
             </style></head><body>\n\
             <div id=\"root\" data-composition-id=\"main\" data-width=\"{width}\" data-height=\"{height}\" \
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
        plan: Plan::single(),
        audio: true,
        native_video: cfg!(target_os = "macos"),
        skip: false,
    }
}

fn render(
    ui: &Ui,
    options: &Options,
    frames: Option<std::ops::RangeInclusive<u64>>,
    codec: Codec,
    out: &Path,
) -> Result<hyperframes::Report> {
    let job = hyperframes::RenderJob {
        options: options.clone(),
        frames,
        codec,
        output: out.to_owned(),
    };
    let mut warm = Warm::new()?;
    hyperframes::render(ui, &job, &mut warm, SystemTime::now(), &mut |_, _| Ok(()))
}

fn hyperframes_render(project: &Path, out: &Path) -> Result<()> {
    run(
        hyperframes_package().join(if cfg!(windows) {
            "../.bin/hyperframes.cmd"
        } else {
            "../.bin/hyperframes"
        }),
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
    let report = render(ui, &options(&project), None, Codec::H264, &ours)?;
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
    let report = render(ui, &options(&project), None, Codec::H264, &out)?;
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

fn counter(frame: &score::Rgb8) -> u32 {
    let cell = frame.width / 48;
    (0..24)
        .map(|bit| {
            let at = (((cell / 2) * frame.width + bit * cell + cell / 2) * 3 + 1) as usize;
            u32::from(frame.data[at] > 128) << bit
        })
        .sum()
}

fn embedded_video_shows_the_frames_hyperframes_extracts(ui: &Ui) -> Result<()> {
    let cases = [
        ("cfr1080.mp4", "0.5", "0.2", true),
        ("vfr1080.mp4", "0", "0", true),
        ("off1080.mp4", "0.25", "0.4", true),
        ("cfr1080-hevc.mp4", "0", "0", true),
        ("cfr1080-vp9.webm", "0", "0", false),
        ("rate24.mp4", "0", "0.1", true),
        ("rate60.mp4", "0.1", "0", true),
    ];
    for (name, start, media_start, native) in cases {
        let project = sized_fixture(
            &format!("ids-{name}"),
            1920,
            1080,
            4,
            &format!(
                "<video id=\"v\" src=\"assets/{name}\" data-start=\"{start}\" data-media-start=\"{media_start}\" \
                 data-duration=\"3\" data-track-index=\"0\" muted playsinline \
                 style=\"position:absolute;left:0;top:0;width:1920px;height:1080px\"></video>"
            ),
            "const tl = gsap.timeline({paused: true}); tl.to('#root', {opacity: 1, duration: 4}); window.__timelines = {main: tl};",
        )?;
        match name {
            "rate24.mp4" | "rate60.mp4" => {
                let rate = &name[4..6];
                let target = project.join("assets").join(name);
                let source = clips::clip("cfr1080.mp4")?;
                run(
                    "ffmpeg",
                    &["-v", "error", "-y", "-i", source.to_str().unwrap(), "-vf", &format!("fps={rate}"), "-c:v", "libx264", "-crf", "14", "-g", "30", target.to_str().unwrap()],
                    Path::new("/"),
                )?;
            }
            _ => {
                fs::copy(clips::clip(name)?, project.join("assets").join(name))?;
            }
        }
        let mut options = options(&project);
        options.audio = false;
        let used = hyperframes::prepare_for(&options)?.native_videos().len();
        ensure!((used == 1) == native, "{name}: {used} natively decoded videos");
        let mut ids = Vec::new();
        for (label, native_video) in [("native", true), ("extraction", false)] {
            options.native_video = native_video;
            let out = work_dir().join(format!("ids-{name}-{label}.mp4"));
            render(ui, &options, None, Codec::H264, &out)?;
            ids.push(frames_of(&out)?.iter().map(counter).collect::<Vec<u32>>());
        }
        let wrong: Vec<(usize, u32, u32)> = ids[0]
            .iter()
            .zip(&ids[1])
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(frame, (a, b))| (frame, *a, *b))
            .collect();
        println!("{name}: {} of {} frames show HyperFrames' frame", ids[0].len() - wrong.len(), ids[0].len());
        ensure!(ids[0].len() == 120, "{name}: {} frames", ids[0].len());
        ensure!(wrong.is_empty(), "{name}: frame, native, extraction: {wrong:?}");
    }
    Ok(())
}

fn embedded_video_colours_equal_hyperframes_extraction(ui: &Ui) -> Result<()> {
    for name in ["cfr1080.mp4", "cfr1080-601.mp4", "cfr1080-full.mp4", "cfr1080-hevc.mp4"] {
        let project = sized_fixture(
            &format!("colour-{name}"),
            1920,
            1080,
            1,
            &format!(
                "<video id=\"v\" src=\"assets/{name}\" data-start=\"0\" data-duration=\"1\" data-track-index=\"0\" muted playsinline \
                 style=\"position:absolute;left:0;top:0;width:1920px;height:1080px\"></video>"
            ),
            "const tl = gsap.timeline({paused: true}); tl.to('#root', {opacity: 1, duration: 1}); window.__timelines = {main: tl};",
        )?;
        fs::copy(clips::clip(name)?, project.join("assets").join(name))?;
        let mut options = options(&project);
        options.audio = false;
        let mut files = Vec::new();
        for (label, native_video) in [("native", true), ("extraction", false)] {
            options.native_video = native_video;
            let out = work_dir().join(format!("colour-{name}-{label}.mp4"));
            render(ui, &options, None, Codec::H264, &out)?;
            files.push(out);
        }
        let scored = score::score(&files[0], &files[1], Sampling::All, None)?;
        let worst = &scored.worst;
        println!(
            "{name}: FLIP mean {:.5}, p99.9 {:.4}, max delta {}, over 8 {:.4}%, over 32 {:.4}%",
            worst.flip_mean.value,
            worst.flip_p999.value,
            worst.max_delta.value,
            worst.frac_over_8.value * 100.0,
            worst.frac_over_32.value * 100.0
        );
        ensure!(scored.frames_compared == 30);
        ensure!(worst.flip_mean.value <= 0.02, "{name}: FLIP mean {}", worst.flip_mean.value);
        ensure!(worst.frac_over_32.value <= 0.001, "{name}: {} of pixels over 32", worst.frac_over_32.value);
    }
    Ok(())
}

fn a_project_without_audio_stays_video_only(ui: &Ui) -> Result<()> {
    let seconds = 1;
    let project = fixture("silent", seconds, BOX, &sweep(seconds))?;
    let out = work_dir().join("silent.mp4");
    render(ui, &options(&project), None, Codec::H264, &out)?;
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
    let report = render(ui, &options(&project), Some(20..=29), Codec::H264, &out)?;
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

fn parallel_views_write_the_same_bytes_as_one_view(ui: &Ui) -> Result<()> {
    let project = fixture(
        "parallel",
        2,
        "<video id=\"v\" src=\"assets/counted.mp4\" data-start=\"0.5\" data-media-start=\"0.2\" \
         data-duration=\"1.5\" data-track-index=\"0\" muted playsinline \
         style=\"position:absolute;left:0;top:0;width:320px;height:180px\"></video>",
        "const tl = gsap.timeline({paused: true}); tl.to('#root', {opacity: 1, duration: 2}); window.__timelines = {main: tl};",
    )?;
    counted_clip(&project.join("assets/counted.mp4"))?;
    let mut files = Vec::new();
    for views in [1, 2, 4] {
        let out = work_dir().join(format!("parallel-{views}.{}", support::DETERMINISTIC.1));
        let mut options = options(&project);
        options.plan = Plan::new(views, 7);
        let report = render(ui, &options, None, support::DETERMINISTIC.0, &out)?;
        ensure!(report.frames == 60 && report.views == views && report.repeats == 0);
        files.push((views, fs::read(&out)?));
    }
    for (views, bytes) in &files[1..] {
        ensure!(
            *bytes == files[0].1,
            "{views} views wrote different bytes from one view"
        );
    }
    Ok(())
}

fn unchanged_frames_are_skipped_and_the_bytes_do_not_change(ui: &Ui) -> Result<()> {
    let project = fixture(
        "skip",
        3,
        BOX,
        "const tl = gsap.timeline({paused: true}); \
         tl.to('#box', {x: 240, duration: 1, ease: 'none'}); \
         tl.set('#box', {x: 240}, 2.9); \
         window.__timelines = {main: tl};",
    )?;
    for views in [1, 3] {
        let mut render_with = |skip: bool| -> Result<(Vec<u8>, u64)> {
            let out = work_dir().join(format!(
                "skip-{views}-{skip}.{}",
                support::DETERMINISTIC.1
            ));
            let options = Options {
                skip,
                plan: Plan::new(views, 7),
                ..options(&project)
            };
            let report = render(ui, &options, None, support::DETERMINISTIC.0, &out)?;
            ensure!(report.frames == 90 && report.views == views);
            Ok((fs::read(&out)?, report.skipped))
        };
        let (plain, none) = render_with(false)?;
        let (skipping, skipped) = render_with(true)?;
        ensure!(none == 0, "{none} frames skipped with skipping off");
        ensure!(
            skipped >= 30,
            "{views} views skipped only {skipped} of 90 frames"
        );
        ensure!(
            plain == skipping,
            "{views} views wrote different bytes with skipping on"
        );
        println!("hyperframes skip, {views} views: {skipped} of 90 frames skipped");
    }
    Ok(())
}

fn main() -> ExitCode {
    vrf_blink::set_cpu_raster(hyperframes::CPU_RASTER);
    let mut cases: Vec<support::Case> = vec![
        (
            "hyperframes_frames_and_audio_match_hyperframes_own_render",
            frames_and_audio_match_hyperframes_own_render,
        ),
        (
            "hyperframes_embedded_h264_shows_exact_frame_ids",
            embedded_h264_shows_the_right_frame_on_every_frame,
        ),
        (
            "hyperframes_native_video_shows_hyperframes_frames",
            embedded_video_shows_the_frames_hyperframes_extracts,
        ),
        (
            "hyperframes_native_video_colours_equal_extraction",
            embedded_video_colours_equal_hyperframes_extraction,
        ),
        (
            "hyperframes_project_without_audio_stays_video_only",
            a_project_without_audio_stays_video_only,
        ),
        ("hyperframes_frame_range", frame_range_renders_the_slice),
        (
            "hyperframes_parallel_views_match_one_view",
            parallel_views_write_the_same_bytes_as_one_view,
        ),
        (
            "hyperframes_skipped_frames_write_the_same_bytes",
            unchanged_frames_are_skipped_and_the_bytes_do_not_change,
        ),
    ];
    if cfg!(windows) {
        cases.retain(|(name, _)| !name.contains("native_video"));
    }
    support::run(&cases)
}
