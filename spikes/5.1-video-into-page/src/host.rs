use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::process::exit;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use cef::{args::Args, rc::Rc as _, *};
use objc2_core_foundation::CFRetained;
use objc2_io_surface::IOSurfaceRef;

use crate::{bundle, mac};

const FLAGS: [&str; 13] = [
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
const LOAD_TIMEOUT: Duration = Duration::from_secs(30);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(10);
const EVALUATE_TIMEOUT: Duration = Duration::from_secs(120);
const SETTLE_DELIVERY: Duration = Duration::from_millis(250);
const SETTLE_QUIET: Duration = Duration::from_millis(50);
const SETTLE_ROUNDS: usize = 10;
const SETTLE_CLEAN: usize = 2;
const SCHEME: &str = "frm";
const HOST: &str = "f";
const SHARED_MESSAGE: &str = "frame";

fn apply_flags(command_line: &mut CommandLine, extra: &[String]) {
    for flag in FLAGS.iter().copied().chain(extra.iter().map(String::as_str)) {
        match flag.split_once('=') {
            Some((key, value)) => {
                command_line.append_switch_with_value(Some(&key.into()), Some(&value.into()))
            }
            None => command_line.append_switch(Some(&flag.into())),
        }
    }
}

fn extra_flags() -> Vec<String> {
    std::env::var("VSP_FLAGS")
        .map(|flags| flags.split_whitespace().map(str::to_owned).collect())
        .unwrap_or_default()
}

pub struct Blob {
    parts: Vec<(usize, usize)>,
    total: usize,
    _keep: Box<dyn std::any::Any + Send + Sync>,
}

impl Blob {
    /// # Safety
    /// Every part must stay valid and unchanged for as long as `keep` lives.
    pub unsafe fn new(
        parts: Vec<(*const u8, usize)>,
        keep: Box<dyn std::any::Any + Send + Sync>,
    ) -> Arc<Blob> {
        let total = parts.iter().map(|(_, len)| len).sum();
        Arc::new(Blob {
            parts: parts.into_iter().map(|(ptr, len)| (ptr as usize, len)).collect(),
            total,
            _keep: keep,
        })
    }
}

static BLOBS: OnceLock<Mutex<HashMap<String, Arc<Blob>>>> = OnceLock::new();

fn blobs() -> &'static Mutex<HashMap<String, Arc<Blob>>> {
    BLOBS.get_or_init(Default::default)
}

pub fn publish(name: &str, blob: Arc<Blob>) {
    blobs().lock().unwrap().insert(name.to_owned(), blob);
}

pub fn retract(name: &str) {
    blobs().lock().unwrap().remove(name);
}

struct Cursor {
    blob: Arc<Blob>,
    part: usize,
    offset: usize,
}

wrap_resource_handler! {
    struct Responder {
        name: String,
        cursor: Arc<Mutex<Option<Cursor>>>,
    }

    impl ResourceHandler {
        fn open(
            &self,
            _request: Option<&mut Request>,
            handle_request: Option<&mut ::std::os::raw::c_int>,
            _callback: Option<&mut Callback>,
        ) -> ::std::os::raw::c_int {
            if let Some(handle_request) = handle_request {
                *handle_request = 1;
            }
            match blobs().lock().unwrap().get(&self.name) {
                Some(blob) => {
                    *self.cursor.lock().unwrap() = Some(Cursor { blob: blob.clone(), part: 0, offset: 0 });
                    1
                }
                None => 0,
            }
        }

        fn response_headers(
            &self,
            response: Option<&mut Response>,
            response_length: Option<&mut i64>,
            _redirect_url: Option<&mut CefString>,
        ) {
            let (Some(response), Some(response_length)) = (response, response_length) else {
                return;
            };
            let total = self.cursor.lock().unwrap().as_ref().map_or(0, |cursor| cursor.blob.total);
            response.set_status(200);
            response.set_status_text(Some(&"OK".into()));
            response.set_mime_type(Some(&"application/octet-stream".into()));
            for (name, value) in [("Access-Control-Allow-Origin", "*"), ("Cache-Control", "no-store")] {
                response.set_header_by_name(Some(&name.into()), Some(&value.into()), 1);
            }
            *response_length = total as i64;
        }

        #[allow(clippy::not_unsafe_ptr_arg_deref)]
        fn read(
            &self,
            data_out: *mut u8,
            bytes_to_read: ::std::os::raw::c_int,
            bytes_read: Option<&mut ::std::os::raw::c_int>,
            _callback: Option<&mut ResourceReadCallback>,
        ) -> ::std::os::raw::c_int {
            let (Some(bytes_read), true) = (bytes_read, bytes_to_read > 0) else {
                return 0;
            };
            *bytes_read = 0;
            let mut guard = self.cursor.lock().unwrap();
            let Some(cursor) = guard.as_mut() else {
                return 0;
            };
            let mut written = 0usize;
            let wanted = bytes_to_read as usize;
            while written < wanted && cursor.part < cursor.blob.parts.len() {
                let (ptr, len) = cursor.blob.parts[cursor.part];
                let take = (len - cursor.offset).min(wanted - written);
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        (ptr + cursor.offset) as *const u8,
                        data_out.add(written),
                        take,
                    );
                }
                written += take;
                cursor.offset += take;
                if cursor.offset == len {
                    cursor.part += 1;
                    cursor.offset = 0;
                }
            }
            *bytes_read = written as _;
            (written > 0) as _
        }
    }
}

wrap_scheme_handler_factory! {
    struct FrameFactory;

    impl SchemeHandlerFactory {
        fn create(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _scheme_name: Option<&CefString>,
            request: Option<&mut Request>,
        ) -> Option<ResourceHandler> {
            let url = CefString::from(&request?.url()).to_string();
            let name = url.rsplit('/').next()?.split('?').next()?.to_owned();
            Some(Responder::new(name, Arc::default()))
        }
    }
}

type Region = Arc<Mutex<Option<SharedMemoryRegion>>>;

wrap_v8_handler! {
    struct TakeHandler {
        region: Region,
    }

    impl V8Handler {
        fn execute(
            &self,
            _name: Option<&CefString>,
            _object: Option<&mut V8Value>,
            _arguments: Option<&[Option<V8Value>]>,
            retval: Option<&mut Option<V8Value>>,
            _exception: Option<&mut CefString>,
        ) -> ::std::os::raw::c_int {
            let region = self.region.lock().unwrap().take();
            if let (Some(region), Some(retval)) = (region, retval) {
                *retval = v8_value_create_array_buffer_with_copy(region.memory().cast(), region.size());
            }
            1
        }
    }
}

wrap_render_process_handler! {
    struct SpikeRenderHandler {
        region: Region,
    }

    impl RenderProcessHandler {
        fn on_context_created(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            context: Option<&mut V8Context>,
        ) {
            let Some(global) = context.and_then(|context| context.global()) else {
                return;
            };
            let mut handler = TakeHandler::new(self.region.clone());
            let Some(mut function) = v8_value_create_function(Some(&"take".into()), Some(&mut handler)) else {
                return;
            };
            global.set_value_bykey(Some(&"__take".into()), Some(&mut function), V8Propertyattribute::default());
        }

        fn on_process_message_received(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> ::std::os::raw::c_int {
            let Some(message) = message else { return 0 };
            if CefString::from(&message.name()).to_string() != SHARED_MESSAGE {
                return 0;
            }
            *self.region.lock().unwrap() = message.shared_memory_region();
            1
        }
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
    struct SpikeApp {
        wake: Arc<Wake>,
        region: Region,
    }

    impl App {
        fn on_before_command_line_processing(
            &self,
            _process_type: Option<&CefString>,
            command_line: Option<&mut CommandLine>,
        ) {
            if let Some(command_line) = command_line {
                apply_flags(command_line, &extra_flags());
            }
        }

        fn on_register_custom_schemes(&self, registrar: Option<&mut SchemeRegistrar>) {
            if let Some(registrar) = registrar {
                let options = [
                    sys::cef_scheme_options_t::CEF_SCHEME_OPTION_STANDARD,
                    sys::cef_scheme_options_t::CEF_SCHEME_OPTION_SECURE,
                    sys::cef_scheme_options_t::CEF_SCHEME_OPTION_CORS_ENABLED,
                    sys::cef_scheme_options_t::CEF_SCHEME_OPTION_FETCH_ENABLED,
                ]
                .iter()
                .fold(0, |bits, option| bits | *option as i32);
                registrar.add_custom_scheme(Some(&SCHEME.into()), options);
            }
        }

        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            Some(SpikeBrowserHandler::new(self.wake.clone()))
        }

        fn render_process_handler(&self) -> Option<RenderProcessHandler> {
            Some(SpikeRenderHandler::new(self.region.clone()))
        }
    }
}

wrap_browser_process_handler! {
    struct SpikeBrowserHandler {
        wake: Arc<Wake>,
    }

    impl BrowserProcessHandler {
        fn on_before_child_process_launch(&self, command_line: Option<&mut CommandLine>) {
            if let Some(command_line) = command_line {
                apply_flags(command_line, &extra_flags());
            }
        }

        fn on_schedule_message_pump_work(&self, delay_ms: i64) {
            self.wake.schedule(delay_ms);
        }
    }
}

type Job = Box<dyn FnOnce() + Send>;

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

#[derive(Clone)]
pub struct Ui(());

impl Ui {
    pub fn post(&self, job: impl FnOnce() + Send + 'static) {
        let mut task = UiTask::new(Arc::new(Mutex::new(Some(Box::new(job)))));
        assert_eq!(post_task(ThreadId::UI, Some(&mut task)), 1, "CefPostTask failed");
    }

    pub fn call<R: Send + 'static>(&self, job: impl FnOnce() -> R + Send + 'static) -> R {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.post(move || {
            let _ = sender.send(job());
        });
        receiver.recv().expect("the UI thread dropped a job")
    }
}

pub struct OnUi<T>(pub T);

unsafe impl<T> Send for OnUi<T> {}
unsafe impl<T> Sync for OnUi<T> {}

impl<T: Clone> Clone for OnUi<T> {
    fn clone(&self) -> Self {
        OnUi(self.0.clone())
    }
}

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
    let mut app = SpikeApp::new(wake.clone(), Arc::default());
    let code = execute_process(Some(args.as_main_args()), Some(&mut app), std::ptr::null_mut());
    if is_helper {
        exit(code.max(0));
    }
    assert_eq!(code, -1, "browser process expected");

    let cache = std::env::temp_dir().join(format!("vsp-{}", std::process::id()));
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
        initialize(Some(args.as_main_args()), Some(&settings), Some(&mut app), std::ptr::null_mut()),
        1,
        "CEF initialisation failed"
    );
    let mut factory = FrameFactory::new();
    assert_eq!(
        register_scheme_handler_factory(Some(&SCHEME.into()), Some(&HOST.into()), Some(&mut factory)),
        1
    );

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
    let Ok(result) = result else {
        exit(101);
    };
    shutdown();
    let _ = std::fs::remove_dir_all(&cache);
    result
}

pub fn url(name: &str) -> String {
    format!("{SCHEME}://{HOST}/{name}")
}

pub struct Paint {
    pub surface: CFRetained<IOSurfaceRef>,
    pub format: wgpu::TextureFormat,
    pub width: u32,
    pub height: u32,
    pub at: Instant,
}

unsafe impl Send for Paint {}

struct MethodResult {
    id: i32,
    success: bool,
    result: Vec<u8>,
}

#[derive(Clone)]
struct Channels {
    paints: Sender<Paint>,
    methods: Sender<MethodResult>,
    loaded: Sender<()>,
    closed: Sender<()>,
}

wrap_render_handler! {
    struct SpikeRenderHandlerOsr {
        width: i32,
        height: i32,
        paints: Sender<Paint>,
    }

    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            if let Some(rect) = rect {
                rect.x = 0;
                rect.y = 0;
                rect.width = self.width;
                rect.height = self.height;
            }
        }

        fn screen_info(
            &self,
            _browser: Option<&mut Browser>,
            screen_info: Option<&mut ScreenInfo>,
        ) -> ::std::os::raw::c_int {
            match screen_info {
                Some(info) => {
                    info.device_scale_factor = 1.0;
                    true as _
                }
                None => false as _,
            }
        }

        fn on_accelerated_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            info: Option<&AcceleratedPaintInfo>,
        ) {
            let at = Instant::now();
            let Some(info) = info else { return };
            if type_ != PaintElementType::default() {
                return;
            }
            let Some(surface) = NonNull::new(info.shared_texture_io_surface.cast::<IOSurfaceRef>()) else {
                return;
            };
            let raw: sys::cef_color_type_t = *info.format.as_ref();
            let format = match raw {
                sys::cef_color_type_t::CEF_COLOR_TYPE_BGRA_8888 => wgpu::TextureFormat::Bgra8Unorm,
                sys::cef_color_type_t::CEF_COLOR_TYPE_RGBA_8888 => wgpu::TextureFormat::Rgba8Unorm,
                other => panic!("unsupported CEF colour type {other:?}"),
            };
            let visible = &info.extra.visible_rect;
            let _ = self.paints.send(Paint {
                surface: unsafe { CFRetained::retain(surface) },
                format,
                width: visible.width as u32,
                height: visible.height as u32,
                at,
            });
        }
    }
}

wrap_load_handler! {
    struct SpikeLoadHandler {
        loaded: Sender<()>,
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
                let _ = self.loaded.send(());
            }
        }
    }
}

wrap_life_span_handler! {
    struct SpikeLifeSpanHandler {
        closed: Sender<()>,
    }

    impl LifeSpanHandler {
        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            let _ = self.closed.send(());
        }
    }
}

wrap_client! {
    struct SpikeClient {
        render_handler: RenderHandler,
        load_handler: LoadHandler,
        life_span_handler: LifeSpanHandler,
    }

    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.render_handler.clone())
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            Some(self.load_handler.clone())
        }

        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(self.life_span_handler.clone())
        }
    }
}

wrap_dev_tools_message_observer! {
    struct SpikeObserver {
        methods: Sender<MethodResult>,
    }

    impl DevToolsMessageObserver {
        fn on_dev_tools_method_result(
            &self,
            _browser: Option<&mut Browser>,
            message_id: ::std::os::raw::c_int,
            success: ::std::os::raw::c_int,
            result: Option<&[u8]>,
        ) {
            let _ = self.methods.send(MethodResult {
                id: message_id,
                success: success != 0,
                result: result.map(<[u8]>::to_vec).unwrap_or_default(),
            });
        }
    }
}

pub struct View {
    ui: Ui,
    browser: OnUi<Browser>,
    host: OnUi<BrowserHost>,
    observer: OnUi<Option<Registration>>,
    paints: Receiver<Paint>,
    methods: Receiver<MethodResult>,
    closed: Receiver<()>,
    next_id: i32,
}

impl View {
    pub fn open(ui: &Ui, url: &str, width: i32, height: i32, transparent: bool) -> Result<View> {
        let (paints_tx, paints) = mpsc::channel();
        let (methods_tx, methods) = mpsc::channel();
        let (loaded_tx, loaded) = mpsc::channel();
        let (closed_tx, closed) = mpsc::channel();
        let channels = Channels { paints: paints_tx, methods: methods_tx, loaded: loaded_tx, closed: closed_tx };
        let url = url.to_owned();
        let (browser, host, observer) = ui.call(move || -> Result<_> {
            let window_info = WindowInfo {
                windowless_rendering_enabled: true as _,
                shared_texture_enabled: true as _,
                external_begin_frame_enabled: true as _,
                ..Default::default()
            };
            let browser_settings = BrowserSettings {
                windowless_frame_rate: 1000,
                background_color: if transparent { 0x00000000 } else { 0xFFFFFFFF },
                ..Default::default()
            };
            let mut client = SpikeClient::new(
                SpikeRenderHandlerOsr::new(width, height, channels.paints),
                SpikeLoadHandler::new(channels.loaded),
                SpikeLifeSpanHandler::new(channels.closed),
            );
            let browser = browser_host_create_browser_sync(
                Some(&window_info),
                Some(&mut client),
                Some(&url.as_str().into()),
                Some(&browser_settings),
                None,
                None,
            )
            .context("CEF did not create the browser")?;
            let host = browser.host().context("browser has no host")?;
            let observer = host.add_dev_tools_message_observer(Some(&mut SpikeObserver::new(channels.methods)));
            Ok((OnUi(browser), OnUi(host), OnUi(observer)))
        })?;
        loaded.recv_timeout(LOAD_TIMEOUT).context("the page did not finish loading")?;
        let view = View { ui: ui.clone(), browser, host, observer, paints, methods, closed, next_id: 1 };
        view.settle()?;
        Ok(view)
    }

    fn settle(&self) -> Result<()> {
        let mut clean = 0;
        for _ in 0..SETTLE_ROUNDS {
            self.invalidate();
            self.begin_frame();
            let delivered = self.recv_paint(SETTLE_DELIVERY).is_some();
            let extra = self.recv_paint(SETTLE_QUIET).is_some();
            clean = if delivered && !extra { clean + 1 } else { 0 };
            if clean == SETTLE_CLEAN {
                return Ok(());
            }
        }
        bail!("begin frames never settled after {SETTLE_ROUNDS} rounds")
    }

    pub fn begin_frame(&self) -> Instant {
        let host = self.host.clone();
        let sent = Instant::now();
        self.ui.post(move || host.0.send_external_begin_frame());
        sent
    }

    pub fn invalidate(&self) {
        let host = self.host.clone();
        self.ui.post(move || host.0.invalidate(PaintElementType::default()));
    }

    pub fn recv_paint(&self, timeout: Duration) -> Option<Paint> {
        self.paints.recv_timeout(timeout).ok()
    }

    pub fn drain_paints(&self) {
        while self.paints.try_recv().is_ok() {}
    }

    /// Copies `size` bytes into a new shared memory region with `fill`, and sends it to the
    /// renderer process of the main frame.
    pub fn send_shared(
        &self,
        size: usize,
        fill: impl FnOnce(*mut u8) + Send + 'static,
    ) -> Result<()> {
        let browser = self.browser.clone();
        self.ui.call(move || -> Result<()> {
            let builder = shared_process_message_builder_create(Some(&SHARED_MESSAGE.into()), size)
                .context("no shared message builder")?;
            fill(builder.memory().cast());
            let mut message = builder.build().context("no shared message")?;
            let frame = browser.0.main_frame().context("no main frame")?;
            frame.send_process_message(ProcessId::RENDERER, Some(&mut message));
            Ok(())
        })
    }

    pub fn execute(&mut self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        let id = self.next_id;
        self.next_id += 1;
        let host = self.host.clone();
        let (method_name, params) = (method.to_owned(), params.to_string());
        let submitted = self.ui.call(move || {
            let mut params = parse_json(Some(&params.as_str().into()), JsonParserOptions::default())
                .and_then(|value| value.dictionary())
                .expect("DevTools params are not a JSON object");
            host.0.execute_dev_tools_method(id, Some(&method_name.as_str().into()), Some(&mut params))
        });
        ensure!(submitted == id, "DevTools rejected {method}");
        let result = loop {
            let result = self
                .methods
                .recv_timeout(EVALUATE_TIMEOUT)
                .with_context(|| format!("no DevTools response to {method}"))?;
            if result.id == id {
                break result;
            }
        };
        let body: serde_json::Value = serde_json::from_slice(&result.result)?;
        ensure!(result.success, "{method} failed: {body}");
        Ok(body)
    }

    pub fn evaluate(&mut self, expression: &str) -> Result<serde_json::Value> {
        let body = self.execute(
            "Runtime.evaluate",
            serde_json::json!({"expression": expression, "awaitPromise": true, "returnByValue": true}),
        )?;
        if let Some(exception) = body.get("exceptionDetails") {
            bail!("the page threw: {exception}");
        }
        Ok(body["result"]["value"].clone())
    }

    pub fn close(self) -> Result<()> {
        let View { ui, host, observer, closed, .. } = self;
        ui.post(move || {
            drop(observer);
            host.0.close_browser(true as _);
        });
        closed.recv_timeout(CLOSE_TIMEOUT).context("the browser did not close")
    }
}
