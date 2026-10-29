mod gpu;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use cef::{args::Args, *};
use gpu::{Gpu, RawTimings};

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Lockstep,
    Burst,
}

#[derive(Clone, Copy, PartialEq)]
enum Import {
    None,
    Cef,
    Raw,
    Deferred,
}

#[derive(Clone)]
struct Cfg {
    mode: Mode,
    import: Import,
    frames: u32,
    seconds: f64,
    readback: bool,
    animated: bool,
    pace_ms: u64,
    timeout_ms: u64,
    width: u32,
    height: u32,
    adapter: Option<String>,
    spin: bool,
    unchanged: bool,
    heavy: bool,
}

impl Cfg {
    fn parse() -> Self {
        let mut c = Cfg {
            mode: Mode::Lockstep,
            import: Import::Raw,
            frames: 2000,
            seconds: 5.0,
            readback: false,
            animated: false,
            pace_ms: 0,
            timeout_ms: 250,
            width: 1920,
            height: 1080,
            adapter: None,
            spin: false,
            unchanged: false,
            heavy: false,
        };
        let mut it = std::env::args().skip(1);
        while let Some(a) = it.next() {
            let mut val = || it.next().unwrap_or_default();
            match a.as_str() {
                "--mode" => {
                    c.mode = if val() == "burst" {
                        Mode::Burst
                    } else {
                        Mode::Lockstep
                    }
                }
                "--import" => {
                    c.import = match val().as_str() {
                        "none" => Import::None,
                        "cef" => Import::Cef,
                        "deferred" => Import::Deferred,
                        _ => Import::Raw,
                    }
                }
                "--frames" => c.frames = val().parse().unwrap(),
                "--seconds" => c.seconds = val().parse().unwrap(),
                "--readback" => c.readback = true,
                "--animated" => c.animated = true,
                "--pace-ms" => c.pace_ms = val().parse().unwrap(),
                "--timeout-ms" => c.timeout_ms = val().parse().unwrap(),
                "--width" => c.width = val().parse().unwrap(),
                "--height" => c.height = val().parse().unwrap(),
                "--spin" => c.spin = true,
                "--unchanged" => c.unchanged = true,
                "--heavy" => c.heavy = true,
                "--adapter" => c.adapter = Some(val()),
                _ => {}
            }
        }
        c
    }
}

struct Paint {
    at: Instant,
    callback_us: f64,
    capture_counter: Option<u64>,
    texture: Option<wgpu::Texture>,
    raw: Option<RawTimings>,
    duplicate: Option<windows::Win32::Foundation::HANDLE>,
    error: Option<String>,
}

struct Shared {
    cfg: Cfg,
    gpu: Gpu,
    loaded: Cell<bool>,
    acks: RefCell<Vec<(i32, bool, String)>>,
    paints: RefCell<Vec<Paint>>,
}

type Ctx = Rc<Shared>;

#[derive(Clone)]
struct AppState(Rc<Cell<bool>>);

wrap_app! {
    struct AppBuilder {
        state: AppState,
    }

    impl App {
        fn on_before_command_line_processing(
            &self,
            _process_type: Option<&CefStringUtf16>,
            command_line: Option<&mut CommandLine>,
        ) {
            let Some(cl) = command_line else { return };
            for s in [
                "run-all-compositor-stages-before-draw",
                "disable-threaded-animation",
                "disable-checker-imaging",
                "disable-image-animation-resync",
                "disable-new-content-rendering-timeout",
                "disable-lcd-text",
                "disable-background-timer-throttling",
                "disable-renderer-backgrounding",
                "disable-backgrounding-occluded-windows",
                "noerrdialogs",
            ] {
                cl.append_switch(Some(&s.into()));
            }
            cl.append_switch_with_value(
                Some(&"force-device-scale-factor".into()),
                Some(&"1".into()),
            );
            cl.append_switch_with_value(Some(&"force-color-profile".into()), Some(&"srgb".into()));
        }

        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            Some(ProcessHandlerBuilder::new(ProcessHandler(self.state.0.clone())))
        }
    }
}

#[derive(Clone)]
struct ProcessHandler(Rc<Cell<bool>>);

wrap_browser_process_handler! {
    struct ProcessHandlerBuilder {
        h: ProcessHandler,
    }

    impl BrowserProcessHandler {
        fn on_context_initialized(&self) {
            self.h.0.set(true);
        }
    }
}

#[derive(Clone)]
struct Render(Ctx);

wrap_render_handler! {
    struct RenderBuilder {
        h: Render,
    }

    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            if let Some(rect) = rect {
                rect.x = 0;
                rect.y = 0;
                rect.width = self.h.0.cfg.width as _;
                rect.height = self.h.0.cfg.height as _;
            }
        }

        fn screen_info(
            &self,
            _browser: Option<&mut Browser>,
            screen_info: Option<&mut ScreenInfo>,
        ) -> ::std::os::raw::c_int {
            let Some(si) = screen_info else { return false as _ };
            si.device_scale_factor = 1.0;
            true as _
        }

        fn on_accelerated_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            info: Option<&AcceleratedPaintInfo>,
        ) {
            let t0 = Instant::now();
            let Some(info) = info else { return };
            if type_ != PaintElementType::default() {
                return;
            }
            let ctx = &self.h.0;
            let (mut texture, mut raw, mut error, mut duplicate) = (None, None, None, None);
            match ctx.cfg.import {
                Import::None => {}
                Import::Cef => match ctx.gpu.import_cef(info) {
                    Ok(t) => texture = Some(t),
                    Err(e) => error = Some(e),
                },
                Import::Raw => {
                    match ctx.gpu.import_raw(windows::Win32::Foundation::HANDLE(info.shared_texture_handle)) {
                        Ok(t) => raw = Some(t),
                        Err(e) => error = Some(e),
                    }
                }
                Import::Deferred => match ctx.gpu.duplicate(info) {
                    Ok(h) => duplicate = Some(h),
                    Err(e) => error = Some(e),
                },
            }
            let at = Instant::now();
            ctx.paints.borrow_mut().push(Paint {
                at,
                callback_us: (at - t0).as_secs_f64() * 1e6,
                capture_counter: (info.extra.has_capture_counter != 0)
                    .then_some(info.extra.capture_counter),
                texture,
                raw,
                duplicate,
                error,
            });
        }

        fn on_paint(
            &self,
            _browser: Option<&mut Browser>,
            _type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            _buffer: *const u8,
            _width: ::std::os::raw::c_int,
            _height: ::std::os::raw::c_int,
        ) {
            eprintln!("on_paint (CPU path) was called");
        }
    }
}

#[derive(Clone)]
struct Load(Ctx);

wrap_load_handler! {
    struct LoadBuilder {
        h: Load,
    }

    impl LoadHandler {
        fn on_loading_state_change(
            &self,
            _browser: Option<&mut Browser>,
            is_loading: ::std::os::raw::c_int,
            _can_go_back: ::std::os::raw::c_int,
            _can_go_forward: ::std::os::raw::c_int,
        ) {
            if is_loading == 0 {
                self.h.0.loaded.set(true);
            }
        }
    }
}

wrap_client! {
    struct ClientBuilder {
        render: RenderHandler,
        load: LoadHandler,
    }

    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.render.clone())
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            Some(self.load.clone())
        }
    }
}

#[derive(Clone)]
struct Observer(Ctx);

wrap_dev_tools_message_observer! {
    struct ObserverBuilder {
        h: Observer,
    }

    impl DevToolsMessageObserver {
        fn on_dev_tools_method_result(
            &self,
            _browser: Option<&mut Browser>,
            message_id: ::std::os::raw::c_int,
            success: ::std::os::raw::c_int,
            result: Option<&[u8]>,
        ) {
            let text = result.map(|r| String::from_utf8_lossy(r).into_owned()).unwrap_or_default();
            self.h.0.acks.borrow_mut().push((message_id, success != 0, text));
        }
    }
}

const PAGE_LOCKSTEP: &str = "<!doctype html><meta charset=utf-8><style>html,body{margin:0;height:100%}</style><script>window.__setFrame=function(i){document.documentElement.style.background='rgb('+(i&255)+','+(i>>8)+','+(255-(i&255))+')';return new Promise(function(r){setTimeout(r,0)})}</script><body>";
const PAGE_HEAVY: &str = "<!doctype html><meta charset=utf-8><style>html,body{margin:0;height:100%;overflow:hidden}.d{position:absolute;top:0;width:700px;height:700px;border-radius:50%;filter:blur(30px);box-shadow:0 0 80px 40px rgba(0,0,0,.5);transform:translate(var(--t,0px),var(--u,0px))}</style><script>for(var k=0;k<48;k++){var d=document.createElement('div');d.className='d';d.style.left=(k%2?1150+(k%5)*20:-600+(k%5)*20)+'px';d.style.top=((k*37)%900-200)+'px';d.style.background='hsl('+(k*37%360)+',80%,55%)';document.addEventListener('DOMContentLoaded',function(){document.body.appendChild(d)})}window.__setFrame=function(i){var r=document.documentElement.style;r.background='rgb('+(i&255)+','+(i>>8)+','+(255-(i&255))+')';r.setProperty('--t',(i%13)+'px');r.setProperty('--u',(i%7)+'px');return new Promise(function(r){setTimeout(r,0)})}</script><body>";
const PAGE_ANIMATED: &str = "<!doctype html><meta charset=utf-8><style>html,body{margin:0;height:100%}</style><script>var n=0;(function f(){document.documentElement.style.background='rgb('+(n&255)+','+((n>>8)&255)+',0)';n++;requestAnimationFrame(f)})()</script><body>";
const PAGE_STATIC: &str = "<!doctype html><meta charset=utf-8><style>html,body{margin:0;height:100%;background:#336699}</style><body>";

fn pump_until(deadline: Duration, mut done: impl FnMut() -> bool) -> bool {
    let t = Instant::now();
    while t.elapsed() < deadline {
        do_message_loop_work();
        if done() {
            return true;
        }
        std::thread::yield_now();
    }
    false
}

fn pump_for(d: Duration) {
    pump_until(d, || false);
}

fn eval(host: &BrowserHost, id: i32, expression: &str) {
    let mut d = dictionary_value_create().unwrap();
    d.set_string(Some(&"expression".into()), Some(&expression.into()));
    d.set_bool(Some(&"awaitPromise".into()), 1);
    d.set_bool(Some(&"returnByValue".into()), 1);
    host.execute_dev_tools_method(id, Some(&"Runtime.evaluate".into()), Some(&mut d));
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    sorted[(((sorted.len() - 1) as f64) * p).round() as usize]
}

fn summarize(name: &str, mut v: Vec<f64>) {
    v.sort_by(|a, b| a.total_cmp(b));
    let mean = v.iter().sum::<f64>() / v.len().max(1) as f64;
    println!(
        "{name}: n={} mean={:.1} p50={:.1} p95={:.1} p99={:.1} max={:.1}",
        v.len(),
        mean,
        percentile(&v, 0.5),
        percentile(&v, 0.95),
        percentile(&v, 0.99),
        percentile(&v, 1.0)
    );
}

#[derive(Default)]
struct CaptureCounters {
    last: Option<u64>,
    seen: u64,
    gaps: u64,
    missing: u64,
    non_increasing: u64,
    absent: u64,
}

impl CaptureCounters {
    fn observe(&mut self, c: Option<u64>) {
        let Some(c) = c else {
            self.absent += 1;
            return;
        };
        self.seen += 1;
        if let Some(prev) = self.last {
            if c > prev + 1 {
                self.gaps += 1;
                self.missing += c - prev - 1;
            } else if c <= prev {
                self.non_increasing += 1;
            }
        }
        self.last = Some(c);
    }

    fn print(&self) {
        println!(
            "capture_counter: observed={} absent={} gaps={} missing_values={} non_increasing={}",
            self.seen, self.absent, self.gaps, self.missing, self.non_increasing
        );
    }
}

fn decode(px: [u8; 4]) -> (u32, u8) {
    let (b, g, r) = (px[0], px[1], px[2]);
    (r as u32 | (g as u32) << 8, b)
}

fn run_lockstep(ctx: &Ctx, host: &BrowserHost) {
    let cfg = &ctx.cfg;
    let mut next_id = 100;
    let mut cc = CaptureCounters::default();
    let (mut bf_sent, mut accepted, mut timeouts, mut strays) = (0u64, 0u64, 0u64, 0u64);
    let (mut stale, mut other, mut blue_bad, mut import_errors, mut ack_failures) =
        (0u64, 0u64, 0u64, 0u64, 0u64);
    let (mut latency_us, mut callback_us, mut open_us, mut copy_us) =
        (vec![], vec![], vec![], vec![]);
    let (mut submit_us, mut wait_us, mut release_us) = (vec![], vec![], vec![]);
    let mut deferred_us = vec![];
    let mut handles = std::collections::BTreeMap::<usize, u64>::new();
    let mut examples: Vec<String> = vec![];
    let started = Instant::now();

    for i in 0..cfg.frames {
        ctx.acks.borrow_mut().clear();
        let id = next_id;
        next_id += 1;
        eval(
            host,
            id,
            &format!("window.__setFrame({})", if cfg.unchanged { 0 } else { i }),
        );
        let acked = pump_until(Duration::from_secs(5), || {
            ctx.acks.borrow().iter().any(|a| a.0 == id)
        });
        if !acked {
            println!("frame {i}: no devtools ack within 5 s; aborting");
            break;
        }
        if let Some(a) = ctx.acks.borrow().iter().find(|a| a.0 == id)
            && !a.1
        {
            ack_failures += 1;
        }
        for p in ctx.paints.borrow_mut().drain(..) {
            strays += 1;
            cc.observe(p.capture_counter);
        }

        let sent = Instant::now();
        host.send_external_begin_frame();
        bf_sent += 1;
        let got = pump_until(Duration::from_millis(cfg.timeout_ms), || {
            !ctx.paints.borrow().is_empty()
        });
        if !got {
            timeouts += 1;
            if examples.len() < 5 {
                examples.push(format!(
                    "frame {i}: no callback within {} ms",
                    cfg.timeout_ms
                ));
            }
            continue;
        }
        let mut paints: Vec<Paint> = ctx.paints.borrow_mut().drain(..).collect();
        let mut first = paints.remove(0);
        if let Some(h) = first.duplicate.take() {
            let t = Instant::now();
            match ctx.gpu.import_raw(h) {
                Ok(r) => first.raw = Some(r),
                Err(e) => first.error = Some(e),
            }
            ctx.gpu.close(h);
            deferred_us.push(t.elapsed().as_secs_f64() * 1e6);
        }
        accepted += 1;
        strays += paints.len() as u64;
        cc.observe(first.capture_counter);
        for p in &paints {
            cc.observe(p.capture_counter);
        }
        latency_us.push((first.at - sent).as_secs_f64() * 1e6);
        callback_us.push(first.callback_us);
        if let Some(e) = &first.error {
            import_errors += 1;
            if examples.len() < 5 {
                examples.push(format!("frame {i}: import error {e}"));
            }
        }
        if let Some(r) = &first.raw {
            open_us.push(r.open_us);
            submit_us.push(r.submit_us);
            wait_us.push(r.wait_us);
            release_us.push(r.release_us);
            copy_us.push(r.submit_us + r.wait_us);
            *handles.entry(r.handle).or_default() += 1;
        }
        if cfg.readback && first.error.is_none() {
            let px = match (&first.texture, cfg.import) {
                (Some(t), Import::Cef) => Some(ctx.gpu.centre_wgpu(t)),
                (None, Import::Raw | Import::Deferred) => Some(ctx.gpu.centre_raw()),
                _ => None,
            };
            if let Some(px) = px {
                let (idx, blue) = decode(px);
                if idx != i {
                    if idx < i {
                        stale += 1;
                    } else {
                        other += 1;
                    }
                    if examples.len() < 5 {
                        examples.push(format!(
                            "frame {i}: centre pixel decodes to frame {idx} (bgra {px:?})"
                        ));
                    }
                } else if blue != 255 - (i & 255) as u8 {
                    blue_bad += 1;
                }
            }
        }
        if cfg.pace_ms > 0 {
            pump_for(Duration::from_millis(cfg.pace_ms));
        }
    }
    pump_for(Duration::from_millis(500));
    for p in ctx.paints.borrow_mut().drain(..) {
        strays += 1;
        cc.observe(p.capture_counter);
    }
    let elapsed = started.elapsed().as_secs_f64();
    println!("--- lockstep result ---");
    println!(
        "frames_requested={} begin_frames_sent={bf_sent} callbacks_accepted={accepted} timeouts_no_callback={timeouts} stray_callbacks={strays} devtools_ack_failures={ack_failures}",
        cfg.frames
    );
    println!(
        "elapsed={elapsed:.2}s  frames_per_s={:.1}  callbacks_per_s={:.1}",
        accepted as f64 / elapsed,
        (accepted + strays) as f64 / elapsed
    );
    if cfg.readback {
        println!(
            "readback: mismatches_stale={stale} mismatches_future_or_other={other} blue_channel_bad={blue_bad} import_errors={import_errors}"
        );
    } else {
        println!("readback: off (import_errors={import_errors})");
    }
    cc.print();
    summarize("begin_frame_to_callback_us", latency_us);
    let buckets: Vec<String> = callback_us
        .chunks(250)
        .map(|c| format!("{:.0}", c.iter().sum::<f64>() / c.len() as f64))
        .collect();
    println!(
        "callback_body_us mean per 250 frames: {}",
        buckets.join(" ")
    );
    summarize("callback_body_us (entry to GPU completion)", callback_us);
    if !open_us.is_empty() {
        for (name, v) in [
            ("open", &open_us),
            ("fence_wait", &wait_us),
            ("release", &release_us),
        ] {
            let b: Vec<String> = v
                .chunks(250)
                .map(|c| format!("{:.0}", c.iter().sum::<f64>() / c.len() as f64))
                .collect();
            println!("  {name}_us mean per 250 frames: {}", b.join(" "));
        }
        summarize("  OpenSharedHandle_us", open_us);
        summarize("  CopyResource_record_submit_us", submit_us);
        summarize("  fence_wait_us", wait_us);
        summarize("  CopyResource+fence_us", copy_us);
        summarize("  resource_release_us", release_us);
        println!(
            "distinct shared handle values over the run: {}",
            handles.len()
        );
    }
    if !deferred_us.is_empty() {
        summarize(
            "deferred_import_outside_callback_us (dup handle -> open+copy+wait+release)",
            deferred_us,
        );
    }
    for e in examples {
        println!("example: {e}");
    }
}

fn run_burst(ctx: &Ctx, host: &BrowserHost) {
    let cfg = &ctx.cfg;
    let mut cc = CaptureCounters::default();
    let (mut bf_sent, mut callbacks) = (0u64, 0u64);
    let mut per_second = vec![0u64; cfg.seconds.ceil() as usize + 1];
    let started = Instant::now();
    while started.elapsed().as_secs_f64() < cfg.seconds {
        host.send_external_begin_frame();
        bf_sent += 1;
        do_message_loop_work();
        let sec = started.elapsed().as_secs() as usize;
        for p in ctx.paints.borrow_mut().drain(..) {
            callbacks += 1;
            per_second[sec] += 1;
            cc.observe(p.capture_counter);
        }
    }
    let elapsed = started.elapsed().as_secs_f64();
    println!("--- burst result ---");
    println!(
        "page={} elapsed={elapsed:.2}s begin_frames_sent={bf_sent} ({:.0}/s) callbacks={callbacks} ({:.1}/s) callbacks_per_begin_frame={:.4}",
        if cfg.animated { "animated" } else { "static" },
        bf_sent as f64 / elapsed,
        callbacks as f64 / elapsed,
        callbacks as f64 / bf_sent as f64
    );
    println!(
        "callbacks per wall second: {:?}",
        &per_second[..cfg.seconds.ceil() as usize]
    );
    cc.print();
}

fn main() -> std::process::ExitCode {
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
    let args = Args::new();
    let cmd = args.as_cmd_line().unwrap();
    let is_browser = cmd.has_switch(Some(&"type".into())) != 1;
    let ready = Rc::new(Cell::new(false));
    let mut app = AppBuilder::new(AppState(ready.clone()));
    let ret = execute_process(
        Some(args.as_main_args()),
        Some(&mut app),
        std::ptr::null_mut(),
    );
    if !is_browser {
        return (ret.max(0) as u8).into();
    }
    assert_eq!(ret, -1, "execute_process in browser process");

    let cfg = Cfg::parse();
    let gpu = Gpu::new(cfg.width, cfg.height, cfg.adapter.as_deref(), cfg.spin);
    let ctx: Ctx = Rc::new(Shared {
        cfg: cfg.clone(),
        gpu,
        loaded: Cell::new(false),
        acks: RefCell::new(vec![]),
        paints: RefCell::new(vec![]),
    });

    let exe_dir = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let cache = exe_dir.join("cef-cache");
    let _ = std::fs::remove_dir_all(&cache);
    let page = exe_dir.join("page.html");
    std::fs::write(
        &page,
        match (cfg.mode, cfg.animated) {
            (Mode::Lockstep, _) if cfg.heavy => PAGE_HEAVY,
            (Mode::Lockstep, _) => PAGE_LOCKSTEP,
            (Mode::Burst, true) => PAGE_ANIMATED,
            (Mode::Burst, false) => PAGE_STATIC,
        },
    )
    .unwrap();
    let url = format!("file:///{}", page.display().to_string().replace('\\', "/"));

    let settings = Settings {
        no_sandbox: 1,
        windowless_rendering_enabled: 1,
        external_message_pump: 1,
        cache_path: cache.display().to_string().as_str().into(),
        root_cache_path: cache.display().to_string().as_str().into(),
        ..Default::default()
    };
    assert_eq!(
        initialize(
            Some(args.as_main_args()),
            Some(&settings),
            Some(&mut app),
            std::ptr::null_mut()
        ),
        1
    );
    assert!(
        pump_until(Duration::from_secs(30), || ready.get()),
        "CEF context did not initialise"
    );

    let window_info = WindowInfo {
        windowless_rendering_enabled: 1,
        shared_texture_enabled: 1,
        external_begin_frame_enabled: 1,
        ..Default::default()
    };
    let browser_settings = BrowserSettings {
        windowless_frame_rate: 1000,
        background_color: 0xFFFF_FFFF,
        ..Default::default()
    };
    let mut client = ClientBuilder::new(
        RenderBuilder::new(Render(ctx.clone())),
        LoadBuilder::new(Load(ctx.clone())),
    );
    let url = CefString::from(url.as_str());
    let browser = browser_host_create_browser_sync(
        Some(&window_info),
        Some(&mut client),
        Some(&url),
        Some(&browser_settings),
        None,
        None,
    )
    .expect("create browser");
    let host = browser.host().unwrap();
    let mut observer = ObserverBuilder::new(Observer(ctx.clone()));
    let _registration = host.add_dev_tools_message_observer(Some(&mut observer));

    assert!(
        pump_until(Duration::from_secs(30), || ctx.loaded.get()),
        "page did not finish loading"
    );
    pump_for(Duration::from_millis(500));
    ctx.paints.borrow_mut().clear();

    println!(
        "config: mode={} import={} frames={} {}x{} readback={} pace_ms={} timeout_ms={} spin={}",
        if cfg.mode == Mode::Burst {
            "burst"
        } else {
            "lockstep"
        },
        match cfg.import {
            Import::None => "none",
            Import::Cef => "cef",
            Import::Raw => "raw",
            Import::Deferred => "deferred",
        },
        cfg.frames,
        cfg.width,
        cfg.height,
        cfg.readback,
        cfg.pace_ms,
        cfg.timeout_ms,
        cfg.spin
    );
    match cfg.mode {
        Mode::Lockstep => run_lockstep(&ctx, &host),
        Mode::Burst => run_burst(&ctx, &host),
    }

    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(8));
        std::process::exit(0);
    });
    host.close_browser(1);
    pump_for(Duration::from_millis(500));
    shutdown();
    std::process::ExitCode::SUCCESS
}
