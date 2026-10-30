use std::panic::{AssertUnwindSafe, catch_unwind};
use std::process::exit;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

use cef::{args::Args, rc::Rc as _, *};

use crate::video::VideoRenderHandler;
use crate::{bundle, mac, scheme};

const FLAGS: [&str; 14] = [
    "disable-features=LocalNetworkAccessChecks",
    "run-all-compositor-stages-before-draw",
    "disable-threaded-animation",
    "disable-checker-imaging",
    "disable-image-animation-resync",
    "disable-new-content-rendering-timeout",
    "disable-lcd-text",
    "force-device-scale-factor=1",
    "force-color-profile=srgb",
    "disable-background-timer-throttling",
    "disable-renderer-backgrounding",
    "disable-backgrounding-occluded-windows",
    "use-mock-keychain",
    "autoplay-policy=no-user-gesture-required",
];

const MAX_IDLE: Duration = Duration::from_millis(10);

static CPU_RASTER: AtomicBool = AtomicBool::new(false);
static SOFTWARE_WEBGL: AtomicBool = AtomicBool::new(false);

/// Rasterises in software, as `chrome-headless-shell` does, before [`run`] starts CEF. Frames are
/// then bit-identical to its output; GPU raster differs from it by a few levels on glyph and
/// shape edges. Has no effect once CEF is running.
pub fn set_cpu_raster(on: bool) {
    CPU_RASTER.store(on, Ordering::Relaxed);
}

/// Runs WebGL on SwiftShader, as Remotion's `chrome-headless-shell` does, before [`run`] starts
/// CEF. WebGL output is then bit-identical to its output; on Metal it differs wherever a shader
/// depends on the GPU's maths. Chromium composites in software in this mode and frames arrive
/// through `OnPaint`. Has no effect once CEF is running.
pub fn set_software_webgl(on: bool) {
    SOFTWARE_WEBGL.store(on, Ordering::Relaxed);
}

pub fn software_webgl() -> bool {
    SOFTWARE_WEBGL.load(Ordering::Relaxed)
}

pub fn cpu_raster() -> bool {
    CPU_RASTER.load(Ordering::Relaxed)
}

fn apply_flags(command_line: &mut CommandLine) {
    let extra = std::env::var("VRF_CEF_FLAGS").unwrap_or_default();
    for flag in FLAGS.into_iter().chain(extra.split_whitespace()) {
        match flag.split_once('=') {
            Some((key, value)) => {
                command_line.append_switch_with_value(Some(&key.into()), Some(&value.into()))
            }
            None => command_line.append_switch(Some(&flag.into())),
        }
    }
    if CPU_RASTER.load(Ordering::Relaxed) {
        command_line.append_switch(Some(&"disable-gpu-rasterization".into()));
    }
    if SOFTWARE_WEBGL.load(Ordering::Relaxed) {
        command_line
            .append_switch_with_value(Some(&"use-angle".into()), Some(&"swiftshader-webgl".into()));
    }
    if command_line.has_switch(Some(&"disable-gpu-rasterization".into())) != 0 {
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
        command_line.append_switch_with_value(
            Some(&"num-raster-threads".into()),
            Some(&threads.to_string().as_str().into()),
        );
    }
}

#[derive(Default)]
struct Wake {
    next: Mutex<Option<Instant>>,
    ready: Condvar,
}

impl Wake {
    fn schedule(&self, delay_ms: i64) {
        let at = Instant::now() + Duration::from_millis(delay_ms.max(0) as u64);
        let mut next = self.next.lock().unwrap();
        *next = Some(next.map_or(at, |current| current.min(at)));
        self.ready.notify_one();
    }

    fn wait(&self) {
        let mut next = self.next.lock().unwrap();
        let timeout = next.map_or(MAX_IDLE, |at| at.saturating_duration_since(Instant::now()));
        if !timeout.is_zero() {
            next = self.ready.wait_timeout(next, timeout).unwrap().0;
        }
        *next = None;
    }
}

wrap_app! {
    struct BlinkApp {
        wake: Arc<Wake>,
    }

    impl App {
        fn on_before_command_line_processing(
            &self,
            _process_type: Option<&CefString>,
            command_line: Option<&mut CommandLine>,
        ) {
            if let Some(command_line) = command_line {
                apply_flags(command_line);
            }
        }

        fn on_register_custom_schemes(&self, registrar: Option<&mut SchemeRegistrar>) {
            if let Some(registrar) = registrar {
                scheme::register_scheme(registrar);
            }
        }

        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            Some(BlinkBrowserProcessHandler::new(self.wake.clone()))
        }

        fn render_process_handler(&self) -> Option<RenderProcessHandler> {
            Some(VideoRenderHandler::new())
        }
    }
}

wrap_browser_process_handler! {
    struct BlinkBrowserProcessHandler {
        wake: Arc<Wake>,
    }

    impl BrowserProcessHandler {
        fn on_before_child_process_launch(&self, command_line: Option<&mut CommandLine>) {
            if let Some(command_line) = command_line {
                apply_flags(command_line);
            }
        }

        fn on_schedule_message_pump_work(&self, delay_ms: i64) {
            self.wake.schedule(delay_ms);
        }
    }
}

type Job = Box<dyn FnOnce() + Send>;

wrap_end_tracing_callback! {
    struct TraceDone {
        done: mpsc::Sender<()>,
    }

    impl EndTracingCallback {
        fn on_end_tracing_complete(&self, _tracing_file: Option<&CefString>) {
            let _ = self.done.send(());
        }
    }
}

fn begin_trace() -> Option<String> {
    let categories = std::env::var("VRF_TRACE").ok()?;
    let file = std::env::var("VRF_TRACE_FILE").ok()?;
    begin_tracing(Some(&categories.as_str().into()), None);
    Some(file)
}

fn end_trace(file: &str) {
    let (done, finished) = mpsc::channel();
    let mut callback = TraceDone::new(done);
    end_tracing(Some(&file.into()), Some(&mut callback));
    for _ in 0..6000 {
        do_message_loop_work();
        if finished.recv_timeout(Duration::from_millis(10)).is_ok() {
            return;
        }
    }
}

wrap_task! {
    struct UiTask {
        job: Arc<Mutex<Option<Job>>>,
    }

    impl Task {
        fn execute(&self) {
            if let Some(job) = self.job.lock().unwrap().take() {
                job();
            }
        }
    }
}

/// Proof that CEF is initialised, and the way to reach its UI thread, which is the process main
/// thread on macOS. CEF objects may only be touched from jobs run through it.
#[derive(Clone)]
pub struct Ui(());

impl Ui {
    pub fn post(&self, job: impl FnOnce() + Send + 'static) {
        let mut task = UiTask::new(Arc::new(Mutex::new(Some(Box::new(job)))));
        assert_eq!(
            post_task(ThreadId::UI, Some(&mut task)),
            1,
            "CefPostTask failed"
        );
    }

    pub fn call<R: Send + 'static>(&self, job: impl FnOnce() -> R + Send + 'static) -> R {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.post(move || {
            let _ = sender.send(job());
        });
        receiver.recv().expect("the UI thread dropped a job")
    }
}

/// Holds a CEF object that may only be used on the UI thread. It is `Send` so that handles to
/// views can live on a worker thread, and it is only dereferenced inside [`Ui`] jobs.
pub struct OnUi<T>(pub T);

unsafe impl<T> Send for OnUi<T> {}
unsafe impl<T> Sync for OnUi<T> {}

impl<T: Clone> Clone for OnUi<T> {
    fn clone(&self) -> Self {
        OnUi(self.0.clone())
    }
}

/// Runs `work` on a worker thread while the calling thread, which must be the process main
/// thread, serves CEF as its UI thread. Helper processes never return from this call.
pub fn run<R: Send + 'static>(work: impl FnOnce(Ui) -> R + Send + 'static) -> R {
    let exe = std::env::current_exe().expect("no executable path");
    if !bundle::is_bundled(&exe) {
        let bundled = bundle::build(&exe).expect("cannot build the CEF app bundle");
        let status = std::process::Command::new(bundled)
            .args(std::env::args_os().skip(1))
            .status()
            .expect("cannot launch the bundled executable");
        exit(status.code().unwrap_or(1));
    }

    let is_helper = std::env::args().any(|arg| arg.starts_with("--type="));
    let loader = library_loader::LibraryLoader::new(&exe, is_helper);
    assert!(loader.load(), "cannot load the CEF framework");
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
    if !is_helper {
        mac::setup_application();
    }

    let wake = Arc::new(Wake::default());
    let args = Args::new();
    let mut app = BlinkApp::new(wake.clone());
    let code = execute_process(
        Some(args.as_main_args()),
        Some(&mut app),
        std::ptr::null_mut(),
    );
    if is_helper {
        exit(code.max(0));
    }
    assert_eq!(code, -1, "browser process expected");

    let cache = std::env::temp_dir().join(format!("vrf-blink-{}", std::process::id()));
    let settings = Settings {
        no_sandbox: true as _,
        windowless_rendering_enabled: true as _,
        external_message_pump: true as _,
        root_cache_path: cache.to_str().unwrap().into(),
        log_severity: LogSeverity::from(sys::cef_log_severity_t::LOGSEVERITY_WARNING),
        log_file: cache.join("cef.log").to_str().unwrap().into(),
        ..Default::default()
    };
    assert_eq!(
        initialize(
            Some(args.as_main_args()),
            Some(&settings),
            Some(&mut app),
            std::ptr::null_mut()
        ),
        1,
        "CEF initialisation failed"
    );

    let trace = begin_trace();
    let done = Arc::new(AtomicBool::new(false));
    let worker = {
        let (done, wake) = (done.clone(), wake.clone());
        std::thread::spawn(move || {
            let result = catch_unwind(AssertUnwindSafe(|| work(Ui(()))));
            done.store(true, Ordering::Release);
            wake.schedule(0);
            result
        })
    };
    while !done.load(Ordering::Acquire) {
        do_message_loop_work();
        wake.wait();
    }
    let result = worker.join().expect("worker thread cannot be joined");
    if let Some(file) = trace {
        end_trace(&file);
    }
    let Ok(result) = result else {
        exit(101);
    };
    shutdown();
    let _ = std::fs::remove_dir_all(&cache);
    result
}
