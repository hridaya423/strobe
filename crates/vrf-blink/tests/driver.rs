#[allow(dead_code)]
mod support;

use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Result, ensure};
use support::{HEIGHT, TOLERANCE, WIDTH, centre_colours, expected_colour, load_average, page_url};
use vrf_blink::driver::{Capture, Config, Pipeline, Stats, Summary, render};
use vrf_blink::{Ui, View};
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
    let config = Config::new(WIDTH, HEIGHT, FRAME_RATE, Codec::H264, frames);
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

fn main() -> ExitCode {
    support::run(&[
        ("driver_colour_per_frame_300", colour_per_frame_300),
        ("driver_import_delay_1ms_2000", import_delay_1ms_2000),
        ("driver_import_delay_50ms_300", import_delay_50ms_300),
        ("driver_no_damage_repeats", no_damage_repeats),
    ])
}
