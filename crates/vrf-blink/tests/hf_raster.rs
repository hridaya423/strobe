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

const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;
const FPS: u64 = 30;
const FRAMES: u64 = 90;
const TWEEN_FLIP: f64 = if cfg!(windows) { 0.002 } else { 0.0001 };
const WORKERS_FLIP: f64 = if cfg!(windows) { 0.05 } else { 0.0001 };

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

fn image_project() -> Result<PathBuf> {
    let dir = work_dir().join("halftone");
    if dir.join("index.html").exists() {
        return Ok(dir);
    }
    fs::create_dir_all(&dir)?;
    fs::copy(
        hyperframes_package().join("dist/vendor/npm/gsap@3.15.0/dist/gsap.min.js"),
        dir.join("gsap.min.js"),
    )?;
    let made = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", "color=c=black:s=978x550", "-vf"])
        .arg(
            "geq=r='150+100*sin(X*0.9)*sin(Y*0.9)+60*sin(X/150)':\
             g='120+90*sin(X*0.9)*sin(Y*0.9)-50*sin(Y/90)':b='110+80*sin(X*0.9)*sin(Y*0.9)+40*sin((X+Y)/200)',format=rgb24",
        )
        .args(["-frames:v", "1"])
        .arg(dir.join("bg.png"))
        .status()?;
    ensure!(made.success(), "ffmpeg could not make the image");
    fs::write(
        dir.join("index.html"),
        format!(
            "<!doctype html>\n<html><head><meta charset=\"utf-8\"><style>\
             html,body{{margin:0;background:#123}}\
             #root{{position:relative;width:{WIDTH}px;height:{HEIGHT}px;overflow:hidden;background:#fbc24a}}\
             #bg{{position:absolute;inset:0;width:100%;height:100%;object-fit:cover}}\
             #t{{position:absolute;left:50%;top:50%;transform:translate(-50%,-50%);font:700 90px Arial,sans-serif;\
             color:#0a0a0a;will-change:transform,opacity}}</style></head><body>\n\
             <div id=\"root\" data-composition-id=\"main\" data-width=\"{WIDTH}\" data-height=\"{HEIGHT}\" \
             data-duration=\"{}\" data-start=\"0\" data-fps=\"{FPS}\"><img id=\"bg\" src=\"bg.png\">\
             <div id=\"t\">Three</div></div>\n\
             <script src=\"gsap.min.js\"></script>\n\
             <script>const tl = gsap.timeline({{paused: true}}); \
             tl.from('#t', {{opacity: 0, scale: 1.35, duration: .45, ease: 'expo.out'}}, .1); \
             window.__timelines = {{main: tl}};</script></body></html>\n",
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
    let name = project.file_name().context("project has no name")?.to_string_lossy();
    let out = work_dir().join(format!("headless-{name}-{workers}"));
    if out.exists() {
        return Ok(out);
    }
    let output = Command::new(hyperframes_package().join(if cfg!(windows) {
        "../.bin/hyperframes.cmd"
    } else {
        "../.bin/hyperframes"
    }))
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
        let texture = support::import(&self.gpu, &paint)?;
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
        worst.flip <= TWEEN_FLIP && worst.delta <= 2,
        "frame {}: FLIP mean {}, max delta {}",
        worst.frame,
        worst.flip,
        worst.delta
    );
    Ok(())
}

fn a_downscaled_image_equals_headless_shell(ui: &Ui) -> Result<()> {
    let project = image_project()?;
    let reference = headless_frames(&project, 1)?;
    let frames: Vec<u64> = vec![0, 6, 12, 20];
    let worst = against(&Ours::new(ui, &project)?.run(0, 20, &frames)?, &reference)?;
    println!(
        "downscaled image: worst FLIP mean {:.6}, max delta {}, at frame {}",
        worst.flip, worst.delta, worst.frame
    );
    ensure!(
        worst.flip <= TWEEN_FLIP && worst.delta <= 2,
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
    ensure!(worst.flip <= WORKERS_FLIP && worst.delta <= 2, "FLIP mean {}, max delta {}", worst.flip, worst.delta);
    Ok(())
}

fn render_file(
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
        codec: support::DETERMINISTIC.0,
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
        let out = render_file(ui, &project, Plan::new(views, 7), &format!("views-{views}.{}", support::DETERMINISTIC.1), None)?;
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
    let alone = render_file(
        ui,
        &project,
        Plan::new(1, 7).ranged(1, 30),
        &format!("ranges-alone.{}", support::DETERMINISTIC.1),
        Some(30..=59),
    )?;
    let alone = decoded(&alone)?;
    ensure!(alone.len() == 30);
    let mut worst = Worst::new();
    let mut files = Vec::new();
    for views in [1, 3] {
        let plan = Plan { ranges: Some(3), ..Plan::new(views, 7) };
        let out = render_file(ui, &project, plan, &format!("ranges-{views}.{}", support::DETERMINISTIC.1), None)?;
        files.push(fs::read(&out)?);
        let all = decoded(&out)?;
        for (frame, (a, b)) in all[30..60].iter().zip(&alone).enumerate() {
            worst.add(frame as u64 + 30, distance(a, b)?);
        }
        println!(
            "{views} views, 3 ranges: frames 30 to 59 against a render that started at 30: worst FLIP mean {:.6}, max delta {}",
            worst.flip, worst.delta
        );
    }
    if cfg!(windows) {
        ensure!(files[0] == files[1], "1 and 3 views wrote different bytes");
        return Ok(());
    }
    ensure!(worst.delta == 0, "frame {}: FLIP mean {}, max delta {}", worst.frame, worst.flip, worst.delta);
    let sequential = decoded(&render_file(
        ui,
        &project,
        Plan::new(1, 7).ranged(1, FRAMES),
        &format!("ranges-1.{}", support::DETERMINISTIC.1),
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

const LAYERS: u64 = 400;
const LAYER_FRAME: u64 = 95;
const VIEWS: usize = 6;
const OPENS: usize = 3;

fn layers_project() -> Result<PathBuf> {
    let dir = work_dir().join("layers");
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
             html,body{{margin:0;width:1920px;height:1080px;overflow:hidden;background:#090b14}}\
             #root{{position:relative;width:1920px;height:1080px;overflow:hidden;\
             background:radial-gradient(circle at 50% 50%,#1a1f3a,#090b14 75%)}}\
             .layer{{position:absolute;left:0;top:0;border-radius:28%;will-change:transform,opacity}}\
             .shadow{{box-shadow:0 12px 36px rgba(0,0,0,.6),0 0 24px rgba(255,255,255,.15)}}\
             .blur{{filter:blur(6px)}}.glow{{filter:drop-shadow(0 0 14px rgba(255,255,255,.55))}}\
             </style></head><body>\n\
             <div id=\"root\" data-composition-id=\"main\" data-start=\"0\" data-duration=\"5\" \
             data-width=\"1920\" data-height=\"1080\"></div>\n\
             <script src=\"gsap.min.js\"></script>\n<script>\
             const root=document.getElementById('root');const tl=gsap.timeline({{paused:true}});\
             for(let i=0;i<{LAYERS};i++){{\
             const size=60+(i*37%160);const layer=document.createElement('div');\
             layer.className='layer'+(i%4===0?' shadow':'')+(i%10===1?' blur':'')+(i%10===2?' glow':'');\
             layer.style.width=layer.style.height=size+'px';\
             layer.style.background='linear-gradient('+(i*47%360)+'deg,hsl('+(i*53%360)+' 85% 60%),hsl('+(i*29%360)+' 80% 40%))';\
             root.appendChild(layer);\
             tl.fromTo(layer,{{x:i*91%1800,y:i*53%960,rotation:i*17%360,scale:.6+i%7/10,opacity:.4+i%5/10}},\
             {{x:i*131%1800,y:i*79%960,rotation:i*17%360+200,scale:.5+i%6/8,opacity:.3+i%4/8,duration:5,ease:'sine.inOut'}},0);}}\
             window.__timelines={{main:tl}};</script></body></html>\n"
        ),
    )?;
    Ok(dir)
}

fn hash(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

fn a_fresh_views_first_frame_is_the_seeked_frame(ui: &Ui) -> Result<()> {
    let project = layers_project()?;
    let options = options(&project, Plan::single());
    let prepare = hyperframes::prepare_for(&options)?;
    let page = hyperframes::Page::new(prepare, &options)?;
    let failures: Vec<Result<Vec<(String, Vec<u64>)>>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..VIEWS)
            .map(|worker| {
                let page = &page;
                scope.spawn(move || -> Result<Vec<(String, Vec<u64>)>> {
                    let gpu = Gpu::new()?;
                    let read = |paint: &vrf_blink::Paint| -> Result<Vec<u8>> {
                        gpu.read_texture(&support::import(&gpu, paint)?)
                    };
                    let mut opened = Vec::new();
                    for open in 0..OPENS {
                        let (mut view, _) = page.open(ui, 1920, 1080)?;
                        hyperframes::Page::seek(&mut view, LAYER_FRAME, None, false)?;
                        let first = Capture::default()
                            .next(&view, Duration::from_millis(700))?
                            .context("no delivery for the first frame")?;
                        let first = read(&first)?;
                        let mut settled = first.clone();
                        for _ in 0..3 {
                            view.begin_frame();
                            while let Some(paint) = view.recv_paint(Duration::from_millis(150)) {
                                settled = read(&paint)?;
                            }
                        }
                        let mut hashes = vec![hash(&first)];
                        let mut capture = Capture::default();
                        for frame in LAYER_FRAME + 1..LAYER_FRAME + 4 {
                            hyperframes::Page::seek(&mut view, frame, None, false)?;
                            let paint = capture
                                .next(&view, Duration::from_millis(700))?
                                .with_context(|| format!("no delivery for frame {frame}"))?;
                            hashes.push(hash(&read(&paint)?));
                        }
                        let mut wrong = String::new();
                        if first != settled {
                            let differing = first.iter().zip(&settled).filter(|(a, b)| a != b).count();
                            wrong = format!("view {worker}.{open}: first frame has {differing} bytes that differ from the settled frame");
                        }
                        opened.push((wrong, hashes));
                        view.close()?;
                    }
                    Ok(opened)
                })
            })
            .collect();
        handles.into_iter().map(|handle| handle.join().unwrap()).collect()
    });
    let mut wrong = Vec::new();
    let mut sequences = Vec::new();
    for failure in failures {
        for (message, hashes) in failure? {
            if !message.is_empty() {
                wrong.push(message);
            }
            sequences.push(hashes);
        }
    }
    ensure!(
        wrong.is_empty(),
        "{} of {} first frames were not the seeked frame: {wrong:?}",
        wrong.len(),
        VIEWS * OPENS
    );
    let differing = sequences.iter().filter(|hashes| **hashes != sequences[0]).count();
    ensure!(
        differing == 0 && sequences[0].windows(2).all(|pair| pair[0] != pair[1]),
        "{differing} of {} fresh views painted frames {LAYER_FRAME} to {} differently from the first",
        sequences.len(),
        LAYER_FRAME + 3
    );
    Ok(())
}

fn canvas_project() -> Result<PathBuf> {
    let dir = work_dir().join("canvas");
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
             html,body{{margin:0;width:{WIDTH}px;height:{HEIGHT}px;overflow:hidden;background:#05060d}}\
             canvas{{position:absolute;left:0;top:0}}</style></head><body>\n\
             <div id=\"root\" data-composition-id=\"main\" data-start=\"0\" data-duration=\"{}\" \
             data-width=\"{WIDTH}\" data-height=\"{HEIGHT}\">\
             <canvas id=\"field\" width=\"{WIDTH}\" height=\"{HEIGHT}\"></canvas></div>\n\
             <script src=\"gsap.min.js\"></script>\n<script>\
             const ctx=document.getElementById('field').getContext('2d');\
             const particles=[];for(let i=0;i<1500;i++)particles.push({{radius:20+(i*37%170),angle:i*2.399,\
             speed:(.3+(i*13%9)/10)*(i%2?-1:1),wobble:i*1.7,size:4+i*7%14,hue:190+i*11%140}});\
             const sprites={{}};function sprite(hue){{const key=Math.round(hue/10);if(!sprites[key]){{\
             const c=document.createElement('canvas');c.width=c.height=32;const g=c.getContext('2d');\
             const grad=g.createRadialGradient(16,16,0,16,16,16);\
             grad.addColorStop(0,'hsla('+key*10+',90%,70%,1)');grad.addColorStop(1,'hsla('+key*10+',90%,50%,0)');\
             g.fillStyle=grad;g.fillRect(0,0,32,32);sprites[key]=c;}}return sprites[key];}}\
             function draw(t){{ctx.globalCompositeOperation='source-over';ctx.fillStyle='#05060d';\
             ctx.fillRect(0,0,{WIDTH},{HEIGHT});ctx.globalCompositeOperation='lighter';\
             for(const p of particles){{const a=p.angle+p.speed*t;const r=p.radius*(1+.15*Math.sin(t*2+p.wobble));\
             const x={WIDTH}/2+Math.cos(a)*r*1.5;const y={HEIGHT}/2+Math.sin(a)*r*.9;\
             const s=p.size*(1+.4*Math.sin(t*3+p.wobble))/2;ctx.drawImage(sprite(p.hue),x-s,y-s,s*2,s*2);}}}}\
             window.addEventListener('hf-seek',(e)=>draw(e.detail.time));draw(0);\
             const tl=gsap.timeline({{paused:true}});tl.to({{}},{{duration:{}}},0);\
             window.__timelines={{main:tl}};</script></body></html>\n",
            FRAMES / FPS,
            FRAMES / FPS
        ),
    )?;
    Ok(dir)
}

fn canvas_2d_equals_headless_shell(ui: &Ui) -> Result<()> {
    let project = canvas_project()?;
    let reference = headless_frames(&project, 1)?;
    let kept: Vec<u64> = (12..=14).collect();
    let _prepared = Ours::new(ui, &project)?;
    let results: Vec<Result<Worst>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..VIEWS)
            .map(|_| {
                let (project, reference, kept) = (&project, &reference, &kept);
                scope.spawn(move || against(&Ours::new(ui, project)?.run(10, 14, kept)?, reference))
            })
            .collect();
        handles.into_iter().map(|handle| handle.join().unwrap()).collect()
    });
    let mut worst = Worst::new();
    for result in results {
        let view = result?;
        println!("canvas view: worst FLIP mean {:.6}, max delta {}", view.flip, view.delta);
        worst.add(view.frame, (view.flip, view.delta));
    }
    ensure!(
        worst.delta <= 2,
        "canvas 2D differs from headless shell: worst FLIP mean {}, max delta {}",
        worst.flip,
        worst.delta
    );
    Ok(())
}

fn main() -> std::process::ExitCode {
    let cpu = std::env::var("VRF_MODE").map_or(CPU_RASTER, |mode| mode == "reference");
    vrf_blink::set_cpu_raster(cpu);
    support::run(&[
        ("hf_raster_canvas_2d_equals_headless_shell", canvas_2d_equals_headless_shell),
        ("hf_raster_fresh_views_first_frame_is_the_seeked_frame", a_fresh_views_first_frame_is_the_seeked_frame),
        ("hf_raster_scale_tween_equals_headless_shell", the_scale_tween_equals_headless_shell),
        ("hf_raster_downscaled_image_equals_headless_shell", a_downscaled_image_equals_headless_shell),
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
