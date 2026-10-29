#[allow(dead_code)]
mod support;

use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use support::{
    HEIGHT, TOLERANCE, WIDTH, centre_colours, expected_colour, load_average, page_url, pixel,
};
use vrf_blink::driver::{Capture, Config, Pipeline, Plan, Stats, Summary, render, render_runs};
use vrf_blink::{Mode, Project, Ui, View, calibration};
use vrf_encode::{Codec, Rational};

const FRAME_RATE: Rational = Rational::new(30, 1);

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn report(name: &str, config: &Config, stats: &Stats) {
    println!(
        "{name}: {} frames in {:.3} s = {:.1} fps, {} repeats, {} retries, load {}",
        config.frames,
        stats.elapsed.as_secs_f64(),
        config.frames as f64 / stats.elapsed.as_secs_f64(),
        stats.repeats,
        stats.retries,
        load_average()
    );
    for (stage, samples) in [
        ("seek", &stats.seek),
        ("begin frame to paint", &stats.begin_to_paint),
        ("import + convert", &stats.convert),
        ("encode submit", &stats.encode),
    ] {
        let summary = Summary::of(samples);
        println!(
            "  {stage:<22} mean {:.3} ms  p50 {:.3} ms  p99 {:.3} ms",
            ms(summary.mean),
            ms(summary.p50),
            ms(summary.p99)
        );
    }
}

fn check_output(path: &Path, frames: u64, expected: impl Fn(u64) -> u64) -> Result<i32> {
    let colours = centre_colours(path)?;
    ensure!(
        colours.len() as u64 == frames,
        "{} frames decoded, {frames} rendered",
        colours.len()
    );
    let mut worst = 0;
    for (index, colour) in colours.iter().enumerate() {
        let want = expected_colour(expected(index as u64));
        let error = colour
            .iter()
            .zip(want)
            .map(|(a, b)| (a - b).abs())
            .max()
            .unwrap();
        ensure!(
            error <= TOLERANCE,
            "frame {index}: centre {colour:?}, expected {want:?} (index {})",
            expected(index as u64)
        );
        worst = worst.max(error);
    }
    Ok(worst)
}

fn render_delayed(
    view: &mut View,
    config: &Config,
    seek_index: &impl Fn(u64) -> u64,
    delay: Duration,
    out: &Path,
) -> Result<Stats> {
    let mut pipeline = Pipeline::new(config, out)?;
    let mut capture = Capture::default();
    let started = Instant::now();
    for index in 0..config.frames {
        view.evaluate(&format!("seek({})", seek_index(index)))?;
        match capture.next(view, config.watchdog)? {
            Some(paint) => {
                std::thread::sleep(delay);
                pipeline.frame(index, &paint)?;
            }
            None => pipeline.repeat(index)?,
        }
    }
    let mut stats = Stats {
        repeats: pipeline.repeats,
        retries: capture.retries,
        begin_to_paint: capture.begin_to_paint,
        convert: std::mem::take(&mut pipeline.convert),
        ..Stats::default()
    };
    let (muxer, encode) = pipeline.finish()?;
    stats.elapsed = started.elapsed();
    stats.encode = encode;
    muxer.finish()?;
    Ok(stats)
}

fn run_case(
    ui: &Ui,
    name: &str,
    frames: u64,
    import_delay: Duration,
    seek_index: impl Fn(u64) -> u64,
) -> Result<(Config, Stats)> {
    let mut view = View::open(ui, &page_url(), WIDTH as i32, HEIGHT as i32)?;
    let mut config = Config::new(WIDTH, HEIGHT, FRAME_RATE, Codec::H264, frames);
    config.watchdog = Duration::from_millis(100);
    let out = std::env::temp_dir().join(format!("vrf-blink-{name}.mp4"));
    let stats = if import_delay.is_zero() {
        render(
            &mut view,
            &config,
            |view, i| view.evaluate(&format!("seek({})", seek_index(i))).map(drop),
            &out,
        )?
    } else {
        render_delayed(&mut view, &config, &seek_index, import_delay, &out)?
    };
    view.close()?;
    let worst = check_output(&out, frames, &seek_index)?;
    println!("{name}: {frames} frames match, worst channel error {worst} (tolerance {TOLERANCE})");
    std::fs::remove_file(&out)?;
    Ok((config, stats))
}

fn colour_per_frame_300(ui: &Ui) -> Result<()> {
    let (config, stats) = run_case(
        ui,
        "driver_colour_per_frame_300",
        300,
        Duration::ZERO,
        |i| i,
    )?;
    ensure!(
        stats.repeats == 0,
        "{} repeats on a page that changes every frame",
        stats.repeats
    );
    report("driver_colour_per_frame_300", &config, &stats);
    Ok(())
}

fn import_delay_1ms_2000(ui: &Ui) -> Result<()> {
    let (config, stats) = run_case(
        ui,
        "driver_import_delay_1ms_2000",
        2000,
        Duration::from_millis(1),
        |i| i,
    )?;
    report("driver_import_delay_1ms_2000", &config, &stats);
    Ok(())
}

fn import_delay_50ms_300(ui: &Ui) -> Result<()> {
    let (config, stats) = run_case(
        ui,
        "driver_import_delay_50ms_300",
        300,
        Duration::from_millis(50),
        |i| i,
    )?;
    report("driver_import_delay_50ms_300", &config, &stats);
    Ok(())
}

fn no_damage_repeats(ui: &Ui) -> Result<()> {
    let (config, stats) = run_case(ui, "driver_no_damage_repeats", 60, Duration::ZERO, |i| {
        i / 10
    })?;
    ensure!(
        stats.repeats == 54,
        "{} repeats, expected 54",
        stats.repeats
    );
    report("driver_no_damage_repeats", &config, &stats);
    Ok(())
}

fn pages() -> Result<Project> {
    Project::open(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/pages"),
        Mode::Offline,
    )
}

fn ticking_view_delivers_every_begin_frame(ui: &Ui) -> Result<()> {
    let project = pages()?;
    let mut view = View::open_project(
        ui,
        &project,
        "colour.html",
        WIDTH as i32,
        HEIGHT as i32,
        Some(""),
    )?;
    ensure!(
        view.ticking(),
        "a project view with an init script is not ticking"
    );
    let mut capture = Capture::default();
    let watchdog = Duration::from_millis(100);
    for index in 0..60 {
        view.evaluate(&format!("seek({})", index / 10))?;
        let paint = capture.next(&view, watchdog)?;
        ensure!(paint.is_some(), "frame {index} did not deliver");
    }
    ensure!(capture.retries == 0, "{} retries", capture.retries);
    let layout = view.evaluate(
        "[innerWidth, innerHeight, document.documentElement.scrollWidth, document.documentElement.scrollHeight, document.body.children.length, document.elementFromPoint(0, 0).tagName].join()",
    )?;
    ensure!(
        layout == format!("{WIDTH},{HEIGHT},{WIDTH},{HEIGHT},1,BODY"),
        "layout changed: {layout}"
    );
    view.close()
}

fn ticking_view_leaves_every_level_untouched(ui: &Ui) -> Result<()> {
    let project = pages()?;
    let mut view = View::open_project(
        ui,
        &project,
        "colour.html",
        WIDTH as i32,
        HEIGHT as i32,
        Some(""),
    )?;
    let mut capture = Capture::default();
    let watchdog = Duration::from_millis(100);
    for level in [0u8, 1, 2, 63, 127, 128, 129, 191, 253, 254, 255, 0, 255] {
        let want = [level, 255 - level, level.wrapping_mul(7)];
        view.evaluate(&format!(
            "document.documentElement.style.background = 'rgb({}, {}, {})'",
            want[0], want[1], want[2]
        ))?;
        for _ in 0..3 {
            let paint = capture.next(&view, watchdog)?.context("no delivery")?;
            for (x, y) in [
                (0, 0),
                (1, 1),
                (WIDTH as usize - 1, 0),
                (WIDTH as usize / 2, HEIGHT as usize / 2),
            ] {
                let got = pixel(&paint, x, y)?;
                ensure!(
                    got == want,
                    "level {level}: pixel ({x}, {y}) is {got:?}, expected {want:?}"
                );
            }
        }
    }
    view.close()
}

#[cfg(target_os = "macos")]
const DETERMINISTIC: (Codec, &str) = (Codec::ProRes422, "mov");
#[cfg(windows)]
const DETERMINISTIC: (Codec, &str) = (Codec::H264, "mp4");

fn parallel_views_write_the_same_bytes_as_one_view(ui: &Ui) -> Result<()> {
    let project = pages()?;
    let config = Config::new(640, 360, FRAME_RATE, DETERMINISTIC.0, 96);
    let mut files = Vec::new();
    for views in [1, 2, 4, 8] {
        let out =
            std::env::temp_dir().join(format!("vrf-blink-parallel-{views}.{}", DETERMINISTIC.1));
        let runs = render_runs(
            &config,
            Plan::new(views, 7),
            0,
            &out,
            || View::open_project(ui, &project, "colour.html", 640, 360, Some("")),
            |view, frame| view.evaluate(&format!("seek({frame})")).map(drop),
        )?;
        runs.muxer.finish()?;
        ensure!(runs.plan.views == views, "{} views ran", runs.plan.views);
        ensure!(runs.stats.repeats == 0, "{} repeats", runs.stats.repeats);
        println!(
            "parallel {views}: {} lead-in frames, {} fps",
            runs.lead_in_frames,
            96.0 / runs.stats.elapsed.as_secs_f64()
        );
        files.push((views, std::fs::read(&out)?));
        std::fs::remove_file(&out)?;
    }
    for (views, bytes) in &files[1..] {
        ensure!(
            *bytes == files[0].1,
            "{views} views wrote different bytes from one view"
        );
    }
    Ok(())
}

fn a_failing_view_stops_the_others_and_names_its_frame(ui: &Ui) -> Result<()> {
    let project = pages()?;
    let config = Config::new(640, 360, FRAME_RATE, Codec::H264, 96);
    let out = std::env::temp_dir().join("vrf-blink-parallel-failure.mp4");
    let started = Instant::now();
    let Err(error) = render_runs(
        &config,
        Plan::new(4, 7),
        0,
        &out,
        || View::open_project(ui, &project, "colour.html", 640, 360, Some("")),
        |view, frame| {
            ensure!(frame != 50, "frame 50 is broken");
            view.evaluate(&format!("seek({frame})")).map(drop)
        },
    ) else {
        anyhow::bail!("a render with a broken frame succeeded");
    };
    let text = format!("{error:#}");
    ensure!(text.contains("frame 50 is broken"), "{text}");
    ensure!(
        started.elapsed() < Duration::from_secs(60),
        "took {:?} to fail",
        started.elapsed()
    );
    Ok(())
}

fn calibration_is_measured_once_and_flags_override_it(ui: &Ui) -> Result<()> {
    let cache = std::env::temp_dir().join(format!("vrf-calibration-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&cache);
    let measured = calibration::calibrated(ui, &cache)?;
    ensure!(
        [1, 2, 4, 8].contains(&measured.views) && measured.run >= 1 && measured.fps.len() >= 2,
        "{measured:?}"
    );
    let cached = calibration::calibrated(ui, &cache)?;
    ensure!(cached == measured, "{cached:?} is not {measured:?}");
    ensure!(
        std::fs::read_to_string(&cache)?.contains(&calibration::key()),
        "the cache is not keyed by machine and CEF version"
    );
    std::fs::remove_file(&cache)?;
    let plan = calibration::plan(ui, Some(3), Some(5), 7)?;
    ensure!(
        (plan.views, plan.run, plan.lead_in) == (3, 5, 7),
        "{plan:?}"
    );
    Ok(())
}

fn main() -> ExitCode {
    support::run(&[
        ("driver_colour_per_frame_300", colour_per_frame_300),
        ("driver_import_delay_1ms_2000", import_delay_1ms_2000),
        ("driver_import_delay_50ms_300", import_delay_50ms_300),
        ("driver_no_damage_repeats", no_damage_repeats),
        (
            "driver_parallel_views_write_the_same_bytes_as_one_view",
            parallel_views_write_the_same_bytes_as_one_view,
        ),
        (
            "driver_calibration_is_cached_and_flags_override_it",
            calibration_is_measured_once_and_flags_override_it,
        ),
        (
            "driver_failing_view_stops_the_others",
            a_failing_view_stops_the_others_and_names_its_frame,
        ),
        (
            "driver_ticking_view_delivers_every_begin_frame",
            ticking_view_delivers_every_begin_frame,
        ),
        (
            "driver_ticking_view_leaves_every_level_untouched",
            ticking_view_leaves_every_level_untouched,
        ),
    ])
}
