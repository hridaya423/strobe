#[allow(dead_code)]
mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, ensure};
use vrf::gpu::Gpu;
use vrf_bench::corpus::bench_dir;
use vrf_bench::score::{self, Rgb8};
use vrf_blink::adapters::hyperframes::{self, CPU_RASTER, Options};
use vrf_blink::driver::{Capture, Plan, Warm};
use vrf_blink::{Mode, Ui, View};
use vrf_encode::Codec;

const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;
const FPS: u64 = 30;
const FRAMES: u64 = 90;

fn work_dir() -> PathBuf {
    std::env::temp_dir().join(format!("vrf-hf-raster-{}", std::process::id()))
}

fn hyperframes_package() -> PathBuf {
    bench_dir().join("engines/node_modules/hyperframes")
}

const STYLE: &str = "\
    #win{position:absolute;left:30px;top:20px;width:580px;height:320px;background:#ece8dd;\
    border-radius:16px;overflow:hidden;will-change:transform,opacity}\
    #flash{position:absolute;inset:0;z-index:1;background:#2bd07a;opacity:0}\
    #burst{position:absolute;inset:0;z-index:2;pointer-events:none}\
    .icon{position:absolute;left:50%;top:50%;width:20px;height:12px;border-radius:3px;background:#30d158;\
    opacity:0;will-change:transform,opacity}\
    #stage{position:absolute;inset:0;z-index:3;display:flex;flex-direction:column;align-items:center;\
    justify-content:center;gap:9px}\
    #label{font:600 14px Menlo,monospace;letter-spacing:.22em;color:#9a9488}\
    #amt{font:800 110px 'Helvetica Neue',Arial,sans-serif;letter-spacing:-.03em;color:#161410;\
    line-height:.9;font-variant-numeric:tabular-nums;text-shadow:0 4px 14px rgba(0,0,0,.08)}\
    #cap{position:absolute;left:40px;right:40px;bottom:12px;z-index:4;text-align:center;opacity:0;\
    will-change:transform,opacity;font:500 15px Arial,sans-serif}";

const BODY: &str = "<div id=\"win\"><div id=\"flash\"></div>\
    <div id=\"burst\"><div class=\"icon\"></div><div class=\"icon\"></div><div class=\"icon\"></div></div>\
    <div id=\"stage\"><div id=\"label\">SPACEX IMPLIED IPO VALUATION</div><div id=\"amt\">$1.0T</div></div>\
    <div id=\"cap\">caption</div></div>";

const SCRIPT: &str = "const tl = gsap.timeline({paused: true});\
    tl.from('#win', {y: 40, opacity: 0, duration: .6, ease: 'power3.out'}, 0);\
    tl.from('#amt', {opacity: 0, y: 18, scale: .985, duration: .45, ease: 'power2.out'}, .35);\
    tl.to('#amt', {color: '#1f9d57', duration: .18}, 1.2);\
    tl.to('#amt', {scale: 1.06, duration: .16, ease: 'back.out(2.2)'}, 1.2);\
    tl.to('#amt', {scale: 1, duration: .24, ease: 'power2.out'}, 1.36);\
    tl.to('#flash', {opacity: .26, duration: .08, ease: 'none'}, 1.2);\
    tl.to('#flash', {opacity: 0, duration: .5, ease: 'power2.out'}, 1.28);\
    window.__timelines = {main: tl};";

fn project() -> Result<PathBuf> {
    let dir = work_dir().join("amount");
    if dir.join("index.html").exists() {
        return Ok(dir);
    }
    fs::create_dir_all(&dir)?;
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
             {STYLE}</style></head><body>\n\
             <div id=\"root\" data-composition-id=\"main\" data-width=\"{WIDTH}\" data-height=\"{HEIGHT}\" \
             data-duration=\"{}\" data-start=\"0\" data-fps=\"{FPS}\">{BODY}</div>\n\
             <script src=\"gsap.min.js\"></script>\n<script>{SCRIPT}</script></body></html>\n",
            FRAMES / FPS
        ),
    )?;
    Ok(dir)
}

fn options(project: &Path, plan: Plan) -> Options {
    Options {
        project: project.to_owned(),
        hyperframes: Some(hyperframes_package()),
        fps: FPS as u32,
        mode: Mode::Offline,
        plan,
        audio: false,
        native_video: false,
        skip: false,
    }
}

fn headless_frames(project: &Path, workers: u32) -> Result<PathBuf> {
    let out = work_dir().join(format!("headless-{workers}"));
    if out.exists() {
        return Ok(out);
    }
    let output = Command::new(hyperframes_package().join("../.bin/hyperframes"))
        .arg("render")
        .arg(project)
        .args(["--format", "png-sequence", "--workers", &workers.to_string(), "--quiet", "-o"])
        .arg(&out)
        .current_dir(bench_dir().join("engines"))
        .env("HYPERFRAMES_NO_TELEMETRY", "1")
        .env("DO_NOT_TRACK", "1")
        .output()?;
    ensure!(
        output.status.success(),
        "hyperframes render:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(out)
}

fn headless_frame(dir: &Path, frame: u64) -> Result<Rgb8> {
    let frames = score::png_frames(dir)?;
    let (_, path) = frames
        .iter()
        .find(|(n, _)| u64::from(*n) == frame + 1)
        .with_context(|| format!("no frame {frame} in {}", dir.display()))?;
    Rgb8::load_png(path)
}

fn distance(a: &Rgb8, b: &Rgb8) -> Result<(f64, u8)> {
    let (score, _) = score::compare_with_map(a, b)?;
    Ok((score.flip_mean, score.max_delta))
}

struct Ours<'a> {
    ui: &'a Ui,
    page: hyperframes::Page,
    gpu: Gpu,
}

impl<'a> Ours<'a> {
    fn new(ui: &'a Ui, project: &Path) -> Result<Ours<'a>> {
        let options = options(project, Plan::single());
        let prepare = hyperframes::prepare_for(&options)?;
        Ok(Ours {
            ui,
            page: hyperframes::Page::new(prepare, &options)?,
            gpu: Gpu::new()?,
        })
    }

    fn paint(&self, view: &mut View, capture: &mut Capture, frame: u64) -> Result<Rgb8> {
        hyperframes::Page::seek(view, frame, None, false)?;
        let paint = capture
            .next(view, Duration::from_millis(700))?
            .with_context(|| format!("no delivery for frame {frame}"))?;
        let texture = self.gpu.import_iosurface(&paint.surface, 0, paint.format)?;
        Ok(Rgb8 {
            width: WIDTH,
            height: HEIGHT,
            data: self
                .gpu
                .read_texture(&texture)?
                .chunks_exact(4)
                .flat_map(|bgra| [bgra[2], bgra[1], bgra[0]])
                .collect(),
        })
    }

    fn run(&self, from: u64, to: u64, keep: &[u64]) -> Result<Vec<(u64, Rgb8)>> {
        let mut view = self.page.open(self.ui, WIDTH as i32, HEIGHT as i32)?.0;
        let mut capture = Capture::default();
        let mut kept = Vec::new();
        for frame in from..=to {
            let rgb = self.paint(&mut view, &mut capture, frame)?;
            if keep.contains(&frame) {
                kept.push((frame, rgb));
            }
        }
        view.close()?;
        Ok(kept)
    }
}

struct Worst {
    flip: f64,
    delta: u8,
    frame: u64,
}

impl Worst {
    fn new() -> Worst {
        Worst { flip: 0.0, delta: 0, frame: 0 }
    }

    fn add(&mut self, frame: u64, (flip, delta): (f64, u8)) {
        if flip > self.flip {
            self.frame = frame;
        }
        self.flip = self.flip.max(flip);
        self.delta = self.delta.max(delta);
    }
}

fn against(ours: &[(u64, Rgb8)], reference: &Path) -> Result<Worst> {
    let mut worst = Worst::new();
    for (frame, rgb) in ours {
        worst.add(*frame, distance(rgb, &headless_frame(reference, *frame)?)?);
    }
    Ok(worst)
}

fn the_scale_tween_equals_headless_shell(ui: &Ui) -> Result<()> {
    let project = project()?;
    let reference = headless_frames(&project, 1)?;
    let frames: Vec<u64> = (34..=46).collect();
    let worst = against(&Ours::new(ui, &project)?.run(0, 46, &frames)?, &reference)?;
    println!(
        "scale tween, frames 34 to 46: worst FLIP mean {:.6}, max delta {}, at frame {}",
        worst.flip, worst.delta, worst.frame
    );
    ensure!(
        worst.flip <= 0.0001 && worst.delta <= 2,
        "frame {}: FLIP mean {}, max delta {}",
        worst.frame,
        worst.flip,
        worst.delta
    );
    Ok(())
}

fn a_view_that_started_later_paints_will_change_layers_differently(ui: &Ui) -> Result<()> {
    let project = project()?;
    let ours = Ours::new(ui, &project)?;
    let frames = [34, 38, 41];
    let sequential = ours.run(0, 41, &frames)?;
    let later = ours.run(34, 41, &frames)?;
    let mut worst = 0.0f64;
    for ((frame, a), (_, b)) in sequential.iter().zip(&later) {
        let (flip, delta) = distance(a, b)?;
        println!("started at 0 against started at 34, frame {frame}: FLIP mean {flip:.6}, max delta {delta}");
        worst = worst.max(flip);
    }
    ensure!(worst >= 0.002, "the views agree (worst FLIP mean {worst}), so the fixture shows nothing");
    Ok(())
}

fn fresh_views_on_headless_ranges_equal_its_workers(ui: &Ui) -> Result<()> {
    let project = project()?;
    let reference = headless_frames(&project, 3)?;
    let ours = Ours::new(ui, &project)?;
    let mut worst = Worst::new();
    for from in (0..FRAMES).step_by(30) {
        let all: Vec<u64> = (from..from + 30).collect();
        let range = against(&ours.run(from, from + 29, &all)?, &reference)?;
        worst.add(range.frame, (range.flip, range.delta));
    }
    println!(
        "three fresh views against three HyperFrames workers: worst FLIP mean {:.6}, max delta {}",
        worst.flip, worst.delta
    );
    ensure!(worst.flip <= 0.0001 && worst.delta <= 2, "FLIP mean {}, max delta {}", worst.flip, worst.delta);
    Ok(())
}

fn render_prores(
    ui: &Ui,
    project: &Path,
    plan: Plan,
    name: &str,
    frames: Option<std::ops::RangeInclusive<u64>>,
) -> Result<PathBuf> {
    let out = work_dir().join(name);
    let count = frames.as_ref().map_or(FRAMES, |range| range.end() - range.start() + 1);
    let job = hyperframes::RenderJob {
        options: options(project, plan),
        frames,
        codec: Codec::ProRes422,
        output: out.clone(),
    };
    let mut warm = Warm::new()?;
    let report = hyperframes::render(ui, &job, &mut warm, SystemTime::now(), &mut |_, _| Ok(()))?;
    ensure!(report.frames == count, "{} frames", report.frames);
    Ok(out)
}

fn the_view_count_does_not_change_the_file(ui: &Ui) -> Result<()> {
    let project = project()?;
    let mut files = Vec::new();
    for views in [1, 2, 4] {
        let out = render_prores(ui, &project, Plan::new(views, 7), &format!("views-{views}.mov"), None)?;
        files.push((views, fs::read(&out)?));
    }
    for (views, bytes) in &files[1..] {
        ensure!(*bytes == files[0].1, "{views} views wrote different bytes from one view");
    }
    Ok(())
}

fn decoded(path: &Path) -> Result<Vec<Rgb8>> {
    score::open(path)?.frames.map(|frame| Ok(frame?.1)).collect()
}

fn a_range_of_the_file_is_a_render_that_started_there(ui: &Ui) -> Result<()> {
    let project = project()?;
    let alone = render_prores(
        ui,
        &project,
        Plan::new(1, 7).ranged(1, 30),
        "ranges-alone.mov",
        Some(30..=59),
    )?;
    let alone = decoded(&alone)?;
    ensure!(alone.len() == 30);
    let mut worst = Worst::new();
    for views in [1, 3] {
        let plan = Plan { ranges: Some(3), ..Plan::new(views, 7) };
        let all = decoded(&render_prores(ui, &project, plan, &format!("ranges-{views}.mov"), None)?)?;
        for (frame, (a, b)) in all[30..60].iter().zip(&alone).enumerate() {
            worst.add(frame as u64 + 30, distance(a, b)?);
        }
        println!(
            "{views} views, 3 ranges: frames 30 to 59 against a render that started at 30: worst FLIP mean {:.6}, max delta {}",
            worst.flip, worst.delta
        );
    }
    ensure!(worst.delta == 0, "frame {}: FLIP mean {}, max delta {}", worst.frame, worst.flip, worst.delta);
    let sequential = decoded(&render_prores(
        ui,
        &project,
        Plan::new(1, 7).ranged(1, FRAMES),
        "ranges-1.mov",
        None,
    )?)?;
    let mut apart = Worst::new();
    for (frame, (a, b)) in sequential[30..60].iter().zip(&alone).enumerate() {
        apart.add(frame as u64 + 30, distance(a, b)?);
    }
    println!("one range against a render that started at 30: worst FLIP mean {:.6}", apart.flip);
    ensure!(apart.flip >= 0.002, "a render from frame 0 equals one from frame 30: the fixture shows nothing");
    Ok(())
}

fn main() -> std::process::ExitCode {
    let cpu = std::env::var("VRF_MODE").map_or(CPU_RASTER, |mode| mode == "reference");
    vrf_blink::set_cpu_raster(cpu);
    support::run(&[
        ("hf_raster_scale_tween_equals_headless_shell", the_scale_tween_equals_headless_shell),
        (
            "hf_raster_started_later_paints_differently",
            a_view_that_started_later_paints_will_change_layers_differently,
        ),
        (
            "hf_raster_fresh_views_equal_headless_workers",
            fresh_views_on_headless_ranges_equal_its_workers,
        ),
        ("hf_raster_view_count_does_not_change_the_file", the_view_count_does_not_change_the_file),
        (
            "hf_raster_file_ranges_are_renders_that_started_there",
            a_range_of_the_file_is_a_render_that_started_there,
        ),
    ])
}
