use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::Value;
use vrf_blink::adapters::remotion::{self, Offthread, Options, Page, VideoDecoder};
use vrf_blink::driver::{Capture, Config, Pipeline, Plan, Warm};
use vrf_blink::{Mode, Project, Ui};
use vrf_encode::Codec;

const SHIM: &str = r#"(() => {
  const now = () => new Event('x').timeStamp;
  const p = (globalThis.__prof = { fetches: [] });
  const setFrame = window.remotion_setFrame;
  window.remotion_setFrame = (...a) => {
    p.setFrameStart = now();
    const r = setFrame(...a);
    p.setFrameEnd = now();
    return r;
  };
  const d = Object.getOwnPropertyDescriptor(window, 'remotion_renderReady');
  Object.defineProperty(window, 'remotion_renderReady', {
    configurable: true,
    enumerable: true,
    get: d.get,
    set(v) {
      d.set.call(window, v);
      if (v === true) p.readyAt.push(now());
    },
  });
  const collect = window.remotion_collectAssets;
  window.remotion_collectAssets = () => {
    p.collectStart = now();
    const r = collect();
    p.collectEnd = now();
    return r;
  };
  const fetch0 = globalThis.fetch;
  globalThis.fetch = (input, init) => {
    const t = now();
    const url = String(input);
    return fetch0(input, init).then((res) => {
      const t1 = now();
      const entry = { frame: p.frame, url: url.slice(0, 200), headers: t1 - t };
      p.fetches.push(entry);
      (globalThis.__fetchlog ??= []).push(entry);
      res.clone().arrayBuffer().then((b) => {
        entry.body = now() - t;
        entry.bytes = b.byteLength;
      });
      return res;
    });
  };
  const ga = Document.prototype.getAnimations;
  p.gaCalls = 0;
  if (__PROF_NOANIM__) {
    Document.prototype.getAnimations = function () {
      p.gaCalls++;
      return [];
    };
  } else {
    Document.prototype.getAnimations = function () {
      p.gaCalls++;
      return ga.call(this);
    };
  }
  const seek = __vrf.rm.seek;
  __vrf.rm.seek = async (...a) => {
    p.readyAt = [];
    p.fetches = [];
    p.setFrameStart = p.setFrameEnd = p.collectStart = p.collectEnd = null;
    p.t0 = now();
    p.gaCalls = 0;
    p.frame = a[0];
    const r = await seek(...a);
    p.t1 = now();
    return { ...r, prof: { ...p, assets: undefined } };
  };
})()"#;

fn digest_of(paint: &vrf_blink::Paint) -> String {
    use objc2_io_surface::IOSurfaceLockOptions;
    let surface = &paint.surface;
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    unsafe {
        surface.lock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut());
        let base = surface.base_address().as_ptr() as *const u8;
        let stride = surface.bytes_per_row();
        let visible = paint.visible;
        for row in visible.y..visible.y + visible.height {
            let line = std::slice::from_raw_parts(
                base.add(row as usize * stride + visible.x as usize * 4),
                visible.width as usize * 4,
            );
            context.update(line);
        }
        surface.unlock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut());
    }
    context.finish().as_ref()[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn run(ui: Ui) -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let project = PathBuf::from(&args[1]).canonicalize()?;
    let composition = args[2].clone();
    let viewloop = args[3] == "viewloop";
    let first: u64 = args[3].parse().unwrap_or(0);
    let last: u64 = args[4].parse().unwrap_or(0);
    let csv = PathBuf::from(&args[5]);
    let shim = args.get(6).is_none_or(|s| s != "noshim");
    let started = Instant::now();
    let options = Options {
        project: project.clone(),
        entry: None,
        composition,
        props: None,
        mode: Mode::Online,
        timeout: Duration::from_secs(30),
        plan: Plan::single(),
        audio: false,
        video: VideoDecoder::Remotion,
        skip: false,
    };
    let step = Instant::now();
    let bundle = remotion::bundle(&project, None)?;
    let bundle_s = step.elapsed();
    let step = Instant::now();
    let offthread = Offthread::start(&project, &bundle.dir)?;
    let server_s = step.elapsed();
    let page = Page::new(
        Project::open(&bundle.dir, Mode::Online)?,
        &options,
        Some(&offthread),
    )?;
    if viewloop {
        let n: usize = args[4].parse()?;
        let mut times = Vec::new();
        let mut failures = 0;
        for _ in 0..n {
            let t = Instant::now();
            match page.open(&ui, 1080, 1080) {
                Ok((view, _)) => {
                    times.push(ms(t.elapsed()));
                    view.close()?;
                }
                Err(error) => {
                    failures += 1;
                    eprintln!("open failed: {error:#}");
                }
            }
        }
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!(
            "viewloop n={n} failures={failures} mean={:.0} p50={:.0} max={:.0}",
            times.iter().sum::<f64>() / times.len().max(1) as f64,
            times.get(times.len() / 2).copied().unwrap_or(0.0),
            times.last().copied().unwrap_or(0.0)
        );
        return Ok(());
    }
    let step = Instant::now();
    let (probe, composition) = page.open(&ui, 640, 360)?;
    probe.close()?;
    let probe_s = step.elapsed();
    let step = Instant::now();
    let (mut view, _) = page.open(&ui, composition.width as i32, composition.height as i32)?;
    let open_s = step.elapsed();
    if shim {
        let noanim = std::env::var_os("VRF_NOANIM").is_some();
        view.evaluate(&SHIM.replace("__PROF_NOANIM__", if noanim { "true" } else { "false" }))?;
    }
    let step = Instant::now();
    for n in 0..200 {
        let _ = n;
        view.evaluate("1")?;
    }
    let null_rt = ms(step.elapsed()) / 200.0;
    let step = Instant::now();
    for _ in 0..200 {
        view.evaluate("(async()=>1)()")?;
    }
    let null_async_rt = ms(step.elapsed()) / 200.0;

    let span = last - first + 1;
    let rounds: u64 = std::env::var("VRF_ROUNDS")
        .ok()
        .and_then(|r| r.parse().ok())
        .unwrap_or(1);
    let count = span * rounds;
    let config = Config::new(
        composition.width,
        composition.height,
        composition.frame_rate()?,
        Codec::H264,
        count,
    );
    let out = std::env::temp_dir().join("vrf-profile.mp4");
    let mut warm = Warm::new()?;
    let mut pipeline = Pipeline::new(&config, &out, &mut warm)?;
    let mut capture = Capture::default();
    let mut rows = String::from(
        "hash,frame,ga_calls,seek_rt,page_total,set_frame_sync,ready_after_set,tail,collect,fetch_hdr_max,fetch_body_max,fetches,bytes,watchdog,b2p,pipeline,total\n",
    );
    for before in first.saturating_sub(10)..first {
        Page::seek(&mut view, before, 1)?;
    }
    let cpu_profile = std::env::var_os("VRF_CPUPROFILE");
    if cpu_profile.is_some() {
        view.execute("Profiler.enable", serde_json::json!({}))?;
        view.execute(
            "Profiler.setSamplingInterval",
            serde_json::json!({"interval": 200}),
        )?;
        view.execute("Profiler.start", serde_json::json!({}))?;
    }
    view.evaluate("console.timeStamp('vrf-render-begin')")?;
    let mut stale = 0u64;
    let hashing = std::env::var_os("VRF_HASH").is_some();
    let render_started = Instant::now();
    for index in 0..count {
        let frame = first + index % span;
        let loop_start = Instant::now();
        let t = Instant::now();
        let result = view.evaluate(&format!("__vrf.rm.seek({frame}, {{ attempt: 1 }})"))?;
        let seek_rt = ms(t.elapsed());
        let p = &result["prof"];
        let num = |v: &Value| v.as_f64().unwrap_or(f64::NAN);
        let (t0, t1) = (num(&p["t0"]), num(&p["t1"]));
        let ready_last = p["readyAt"]
            .as_array()
            .and_then(|a| a.last())
            .map_or(f64::NAN, num);
        let fetches = p["fetches"].as_array().cloned().unwrap_or_default();
        let max_of = |key: &str| {
            fetches
                .iter()
                .map(|f| f[key].as_f64().unwrap_or(0.0))
                .fold(0.0, f64::max)
        };
        let bytes: f64 = fetches
            .iter()
            .map(|f| f["bytes"].as_f64().unwrap_or(0.0))
            .sum();
        let before_capture = Instant::now();
        let b2p_before = capture.begin_to_paint.len();
        let paint = loop {
            match capture.next(&view, config.watchdog) {
                Ok(paint) => break paint,
                Err(error) if error.to_string().contains("delivery from before") => stale += 1,
                Err(error) => return Err(error),
            }
        };
        let capture_wall = ms(before_capture.elapsed());
        let (watchdog, b2p) = match capture.begin_to_paint.get(b2p_before) {
            Some(d) => (capture_wall - ms(*d), ms(*d)),
            None => (capture_wall, f64::NAN),
        };
        let hash = match (&paint, hashing) {
            (Some(paint), true) => digest_of(paint),
            _ => String::new(),
        };
        let step = Instant::now();
        match paint {
            Some(paint) => pipeline.frame(index, &paint)?,
            None => pipeline.repeat(index)?,
        }
        let pipeline_ms = ms(step.elapsed());
        writeln!(
            rows,
            "{hash},{frame},{},{seek_rt:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{},{bytes},{watchdog:.3},{b2p:.3},{pipeline_ms:.3},{:.3}",
            num(&p["gaCalls"]),
            t1 - t0,
            num(&p["setFrameEnd"]) - num(&p["setFrameStart"]),
            ready_last - num(&p["setFrameEnd"]),
            t1 - ready_last,
            num(&p["collectEnd"]) - num(&p["collectStart"]),
            max_of("headers"),
            max_of("body"),
            fetches.len(),
            ms(loop_start.elapsed()),
        )?;
    }
    let render_s = render_started.elapsed();
    view.evaluate("console.timeStamp('vrf-render-end')")?;
    if let Some(path) = cpu_profile {
        let stopped = view.execute("Profiler.stop", serde_json::json!({}))?;
        std::fs::write(path, stopped["profile"].to_string())?;
    }
    let fetch_log = view.evaluate("JSON.stringify(globalThis.__fetchlog ?? [])")?;
    if let Some(text) = fetch_log.as_str().filter(|t| t.len() > 2) {
        std::fs::write(csv.with_extension("fetches.json"), text)?;
    }
    let repeats = pipeline.repeats;
    let convert = std::mem::take(&mut pipeline.convert);
    let (muxer, _) = pipeline.finish()?;
    let drain_s = render_started.elapsed() - render_s;
    view.close()?;
    muxer.finish()?;
    std::fs::write(&csv, rows)?;
    let mean_convert = convert.iter().map(|d| ms(*d)).sum::<f64>() / convert.len().max(1) as f64;
    println!(
        "{{\"frames\":{count},\"repeats\":{repeats},\"stale\":{stale},\"bundle_s\":{:.3},\"server_s\":{:.3},\"probe_s\":{:.3},\"open_s\":{:.3},\"render_s\":{:.3},\"drain_s\":{:.3},\"total_in_process_s\":{:.3},\"null_rt_ms\":{null_rt:.3},\"null_async_rt_ms\":{null_async_rt:.3},\"convert_mean_ms\":{mean_convert:.3}}}",
        bundle_s.as_secs_f64(),
        server_s.as_secs_f64(),
        probe_s.as_secs_f64(),
        open_s.as_secs_f64(),
        render_s.as_secs_f64(),
        drain_s.as_secs_f64(),
        started.elapsed().as_secs_f64(),
    );
    Ok(())
}

fn main() {
    let is_helper = std::env::args().any(|a| a.starts_with("--type="));
    if std::env::args().any(|a| a == "--cpu") {
        vrf_blink::set_cpu_raster(true);
    }
    let _ = is_helper;
    let result = vrf_blink::run(|ui| run(ui).context("profile run"));
    if let Err(error) = result {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}
