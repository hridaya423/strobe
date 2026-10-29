use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{Value, json};
use vrf::colour::Format;
use vrf::gpu::Gpu;
use vrf::gpu::convert::Converter;
use vrf::gpu::metal::PoolTargets;
use vrf_blink::adapters::remotion::{self, Offthread, Options, Page};
use vrf_blink::{Mode, Project, Ui, View};
use vrf_encode::videotoolbox::pool::{PixelFormat, PixelPool};

const COLOUR: &str = concat!(
    "file://",
    env!("CARGO_MANIFEST_DIR"),
    "/../../crates/vrf-blink/tests/pages/colour.html"
);
const LEAD_IN: u64 = 20;

fn corpus(name: &str) -> (PathBuf, &'static str, &'static str) {
    let dir = PathBuf::from(std::env::var("VRF_BENCH_DIR").unwrap()).join("corpus/external");
    match name {
        "trans" => (dir.join("transitions-video"), "Main", "src/index.ts"),
        "github" => (dir.join("github-unwrapped"), "Main", "remotion/index.ts"),
        "trailer" => (dir.join("4-0-trailer"), "Intro", "src/index.ts"),
        other => panic!("unknown page {other}"),
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn mean(v: &[Duration]) -> f64 {
    v.iter().map(|d| ms(*d)).sum::<f64>() / v.len().max(1) as f64
}

fn cpu_seconds(text: &str) -> f64 {
    text.split(':')
        .fold(0.0, |acc, part| acc * 60.0 + part.parse::<f64>().unwrap_or(0.0))
}

fn descendants_cpu(root: u32) -> HashMap<&'static str, f64> {
    let out = Command::new("ps")
        .args(["-axo", "pid=,ppid=,cputime=,command="])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let rows: Vec<(u32, u32, f64, String)> = text
        .lines()
        .filter_map(|line| {
            let mut it = line.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let ppid = it.next()?.parse().ok()?;
            let cpu = cpu_seconds(it.next()?);
            Some((pid, ppid, cpu, it.collect::<Vec<_>>().join(" ")))
        })
        .collect();
    let mut inside = vec![root];
    let mut grew = true;
    while grew {
        grew = false;
        for (pid, ppid, _, _) in &rows {
            if inside.contains(ppid) && !inside.contains(pid) {
                inside.push(*pid);
                grew = true;
            }
        }
    }
    let mut classes: HashMap<&'static str, f64> = HashMap::new();
    for (pid, _, cpu, command) in &rows {
        if !inside.contains(pid) {
            continue;
        }
        let class = if *pid == root {
            "browser"
        } else if command.contains("--type=gpu-process") {
            "gpu"
        } else if command.contains("--type=renderer") {
            "renderer"
        } else {
            "other"
        };
        *classes.entry(class).or_default() += cpu;
    }
    classes
}

struct Setup {
    page: String,
    frames: u64,
    convert: bool,
    base: u64,
    bundle: Option<(Arc<Page>, remotion::Composition, Arc<Offthread>)>,
}

fn open_view(ui: &Ui, setup: &Setup) -> Result<(View, i32, i32)> {
    match &setup.bundle {
        None => Ok((View::open(ui, COLOUR, 1920, 1080)?, 1920, 1080)),
        Some((page, composition, _)) => {
            let (w, h) = (composition.width as i32, composition.height as i32);
            Ok((page.open(ui, w, h)?.0, w, h))
        }
    }
}

fn seek(setup: &Setup, view: &mut View, frame: u64) -> Result<()> {
    match &setup.bundle {
        None => view.evaluate(&format!("seek({frame})")).map(drop),
        Some(_) => Page::seek(view, frame, 1).map(drop),
    }
}

struct Converter1 {
    gpu: Gpu,
    converter: Converter,
    targets: PoolTargets,
    pool: PixelPool,
}

fn run_view(
    ui: &Ui,
    setup: &Setup,
    index: u64,
    barrier: &Barrier,
    finished: &Barrier,
) -> Result<(Duration, Vec<Duration>, Vec<Duration>, Vec<Duration>, u64)> {
    let (mut view, w, h) = open_view(ui, setup)?;
    let mut conv = if setup.convert {
        let gpu = Gpu::new()?;
        let converter = Converter::new(&gpu, Format::Nv12);
        let pool = PixelPool::new(w as usize, h as usize, PixelFormat::Nv12)?;
        Some(Converter1 { gpu, converter, targets: PoolTargets::default(), pool })
    } else {
        None
    };
    let first = setup.base + index * setup.frames;
    for before in first.saturating_sub(LEAD_IN)..first {
        seek(setup, &mut view, before)?;
    }
    barrier.wait();
    let started = Instant::now();
    let (mut seeks, mut b2p, mut convs) = (vec![], vec![], vec![]);
    let mut missing = 0;
    for frame in first..first + setup.frames {
        let t = Instant::now();
        seek(setup, &mut view, frame)?;
        seeks.push(t.elapsed());
        let sent = view.begin_frame();
        let Some(paint) = view.recv_paint(Duration::from_millis(2000)) else {
            missing += 1;
            continue;
        };
        b2p.push(paint.at - sent);
        if let Some(c) = conv.as_mut() {
            let t = Instant::now();
            let source = {
                let texture = c.gpu.import_iosurface(&paint.surface, 0, paint.format)?;
                c.converter.source(&texture.create_view(&Default::default()))
            };
            let buffer = c.pool.acquire()?;
            let target = c.targets.get(&c.gpu, &c.converter, &buffer)?;
            let mut commands = c.gpu.device.create_command_encoder(&Default::default());
            {
                let mut pass = commands.begin_compute_pass(&Default::default());
                c.converter.encode(&mut pass, &source, target, paint.visible, vrf::colour::Chroma::Swscale);
            }
            let submission = c.gpu.queue.submit([commands.finish()]);
            c.gpu.device.poll(wgpu::PollType::Wait { submission_index: Some(submission), timeout: None })?;
            convs.push(t.elapsed());
        }
    }
    let elapsed = started.elapsed();
    finished.wait();
    finished.wait();
    view.close()?;
    Ok((elapsed, seeks, b2p, convs, missing))
}

fn child_main(args: Vec<String>) -> Result<()> {
    let views: u64 = args[0].parse()?;
    let page = args[1].clone();
    let frames: u64 = args[2].parse()?;
    let convert = args[3] == "1";
    let base: u64 = args[4].parse()?;
    let sync = args[5] == "sync";
    let report = vrf_blink::run(move |ui| -> Result<Value> {
        let bundle = if page == "colour" {
            None
        } else {
            let (project, composition, entry) = corpus(&page);
            let b = remotion::bundle(&project, Some(entry.as_ref()))?;
            let off = Offthread::start(&project.canonicalize()?, &b.dir)?;
            let options = Options {
                project: project.clone(),
                entry: None,
                composition: composition.into(),
                props: None,
                mode: Mode::Online,
                timeout: Duration::from_secs(30),
                plan: vrf_blink::driver::Plan::single(),
            };
            let p = Page::new(Project::open(&b.dir, Mode::Online)?, &options, Some(&off))?;
            let (v, comp) = p.open(&ui, 640, 360)?;
            v.close()?;
            Some((Arc::new(p), comp, Arc::new(off)))
        };
        let setup = Arc::new(Setup { page: page.clone(), frames, convert, base, bundle });
        let barrier = Arc::new(Barrier::new(views as usize + 1));
        let finished = Arc::new(Barrier::new(views as usize + 1));
        let handles: Vec<_> = (0..views)
            .map(|i| {
                let (ui, setup, barrier, finished) =
                    (ui.clone(), setup.clone(), barrier.clone(), finished.clone());
                std::thread::spawn(move || run_view(&ui, &setup, i, &barrier, &finished))
            })
            .collect();
        barrier.wait();
        if sync {
            println!("ready");
            std::io::stdout().flush()?;
            let mut line = String::new();
            std::io::stdin().lock().read_line(&mut line)?;
        }
        let before = descendants_cpu(std::process::id());
        let started = Instant::now();
        finished.wait();
        let wall = started.elapsed();
        let after = descendants_cpu(std::process::id());
        finished.wait();
        let mut elapsed = Duration::ZERO;
        let (mut s, mut b, mut c, mut missing) = (vec![], vec![], vec![], 0);
        for h in handles {
            let (e, a1, a2, a3, m) = h.join().unwrap()?;
            elapsed = elapsed.max(e);
            s.extend(a1);
            b.extend(a2);
            c.extend(a3);
            missing += m;
        }
        let cpu: HashMap<_, _> = after
            .iter()
            .map(|(k, v)| (*k, (v - before.get(k).copied().unwrap_or(0.0)) / wall.as_secs_f64()))
            .collect();
        let _ = &setup.page;
        Ok(json!({
            "views": views, "frames_per_view": frames, "elapsed_s": wall.as_secs_f64(),
            "slowest_view_s": elapsed.as_secs_f64(),
            "fps": (views * frames) as f64 / wall.as_secs_f64(),
            "seek_ms": mean(&s), "begin_to_paint_ms": mean(&b), "convert_ms": mean(&c),
            "missing": missing, "cores": cpu,
        }))
    })?;
    println!("{report}");
    Ok(())
}

fn main() -> Result<()> {
    if std::env::args().any(|arg| arg.starts_with("--type=")) {
        return vrf_blink::run(|_| ()).pipe(Ok);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args[0].as_str() {
        "inproc" => child_main(args[1..].to_vec()),
        "noop" => vrf_blink::run(|_| ()).pipe(Ok),
        "multiproc" => {
            let procs: usize = args[1].parse()?;
            let (page, frames, convert) = (&args[2], &args[3], &args[4]);
            let exe = std::env::current_exe()?;
            let status = Command::new(&exe).arg("noop").status()?;
            anyhow::ensure!(status.success());
            let mut children: Vec<_> = (0..procs)
                .map(|i| {
                    let base = if i == 0 { "0".to_string() } else { (i as u64 * frames.parse::<u64>().unwrap()).to_string() };
                    Command::new(&exe)
                        .args(["inproc", "1", page, frames, convert, &base, "sync"])
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .spawn()
                        .unwrap()
                })
                .collect();
            let mut readers: Vec<_> = children
                .iter_mut()
                .map(|c| std::io::BufReader::new(c.stdout.take().unwrap()))
                .collect();
            for r in readers.iter_mut() {
                let mut line = String::new();
                r.read_line(&mut line)?;
                anyhow::ensure!(line.trim() == "ready", "child said {line:?}");
            }
            let started = Instant::now();
            for c in children.iter_mut() {
                writeln!(c.stdin.as_mut().unwrap())?;
            }
            let mut reports = Vec::new();
            for r in readers.iter_mut() {
                let mut line = String::new();
                r.read_line(&mut line)?;
                reports.push(serde_json::from_str::<Value>(&line).context(line.clone())?);
            }
            let wall = started.elapsed();
            for mut c in children {
                c.wait()?;
            }
            let frames_each: u64 = frames.parse()?;
            let mut cores: HashMap<String, f64> = HashMap::new();
            for r in &reports {
                for (k, v) in r["cores"].as_object().unwrap() {
                    *cores.entry(k.clone()).or_default() += v.as_f64().unwrap();
                }
            }
            let slowest = reports.iter().map(|r| r["elapsed_s"].as_f64().unwrap()).fold(0.0, f64::max);
            let avg = |key: &str| reports.iter().map(|r| r[key].as_f64().unwrap()).sum::<f64>() / reports.len() as f64;
            println!("{}", json!({
                "views": procs, "frames_per_view": frames_each, "elapsed_s": slowest, "wall_s": wall.as_secs_f64(),
                "fps": (procs as u64 * frames_each) as f64 / slowest,
                "seek_ms": avg("seek_ms"), "begin_to_paint_ms": avg("begin_to_paint_ms"), "convert_ms": avg("convert_ms"),
                "missing": reports.iter().map(|r| r["missing"].as_u64().unwrap()).sum::<u64>(), "cores": cores,
            }));
            Ok(())
        }
        other => panic!("unknown mode {other}"),
    }
}

trait Pipe: Sized {
    fn pipe<R>(self, f: impl FnOnce(Self) -> R) -> R {
        f(self)
    }
}
impl<T> Pipe for T {}
