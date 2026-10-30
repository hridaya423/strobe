use std::cell::Cell;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use cef::{rc::Rc as _, *};
use objc2_core_foundation::CFRetained;
use objc2_io_surface::IOSurfaceRef;
use vrf::colour;

use crate::app::{OnUi, Ui};
use crate::scheme::Project;
use crate::video::{self, Pusher};

const LOAD_TIMEOUT: Duration = Duration::from_secs(30);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(10);
const EVALUATE_TIMEOUT: Duration = Duration::from_secs(60);
const SETTLE_DELIVERY: [Duration; 3] = [
    Duration::from_millis(60),
    Duration::from_millis(120),
    Duration::from_millis(250),
];
const ALIVE_TIMEOUT: Duration = Duration::from_secs(1);
const SETTLE_QUIET: Duration = Duration::from_millis(20);
const BEGIN_FRAME_GAP: Duration = Duration::from_millis(1);
const SETTLE_ROUNDS: usize = 10;
const SETTLE_CLEAN: usize = 2;

const TICK_JS: &str = include_str!("../js/tick.js");

const GENERIC_FONTS: [&str; 4] = ["Times New Roman", "Arial", "Script", "Impact"];

static SERIAL: AtomicU64 = AtomicU64::new(0);

/// One `OnAcceleratedPaint`. The surface is retained, so it outlives the callback, and CEF
/// draws nothing new into it until the next begin frame.
pub struct Paint {
    pub surface: CFRetained<IOSurfaceRef>,
    pub format: wgpu::TextureFormat,
    pub visible: colour::Rect,
    pub counter: u64,
    pub at: Instant,
}

unsafe impl Send for Paint {}

#[derive(Default)]
struct Size {
    width: AtomicI32,
    height: AtomicI32,
}

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
    pages: Sender<String>,
}

fn bitmap_surface(
    buffer: *const u8,
    width: usize,
    height: usize,
) -> Option<CFRetained<IOSurfaceRef>> {
    use objc2_core_foundation::{CFDictionary, CFNumber, CFString, CFType};
    use objc2_io_surface::{
        kIOSurfaceBytesPerElement, kIOSurfaceHeight, kIOSurfacePixelFormat, kIOSurfaceWidth,
    };
    let number = |v: i64| CFNumber::new_i64(v);
    let (w, h, bpe, fmt) = (
        number(width as i64),
        number(height as i64),
        number(4),
        number(0x4247_5241),
    );
    let properties = unsafe {
        CFDictionary::<CFString, CFType>::from_slices(
            &[
                kIOSurfaceWidth,
                kIOSurfaceHeight,
                kIOSurfaceBytesPerElement,
                kIOSurfacePixelFormat,
            ],
            &[w.as_ref(), h.as_ref(), bpe.as_ref(), fmt.as_ref()],
        )
    };
    let surface = unsafe { IOSurfaceRef::new(properties.as_opaque()) }?;
    unsafe {
        surface.lock(
            objc2_io_surface::IOSurfaceLockOptions::empty(),
            std::ptr::null_mut(),
        );
        let stride = surface.bytes_per_row();
        let base = surface.base_address().as_ptr().cast::<u8>();
        for row in 0..height {
            std::ptr::copy_nonoverlapping(
                buffer.add(row * width * 4),
                base.add(row * stride),
                width * 4,
            );
        }
        surface.unlock(
            objc2_io_surface::IOSurfaceLockOptions::empty(),
            std::ptr::null_mut(),
        );
    }
    Some(surface)
}

wrap_render_handler! {
    struct BlinkRenderHandler {
        size: Arc<Size>,
        paints: Sender<Paint>,
        soft_counter: Arc<AtomicU64>,
    }

    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            if let Some(rect) = rect {
                rect.x = 0;
                rect.y = 0;
                rect.width = self.size.width.load(Ordering::Relaxed);
                rect.height = self.size.height.load(Ordering::Relaxed);
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

        fn on_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            buffer: *const u8,
            width: ::std::os::raw::c_int,
            height: ::std::os::raw::c_int,
        ) {
            let at = Instant::now();
            if type_ != PaintElementType::default() {
                return;
            }
            let (w, h) = (width as usize, height as usize);
            let Some(surface) = bitmap_surface(buffer, w, h) else {
                return;
            };
            let counter = self.soft_counter.fetch_add(1, Ordering::Relaxed);
            let _ = self.paints.send(Paint {
                surface,
                format: wgpu::TextureFormat::Bgra8Unorm,
                visible: colour::Rect { x: 0, y: 0, width: w as u32, height: h as u32 },
                counter,
                at,
            });
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
                visible: colour::Rect {
                    x: visible.x as u32,
                    y: visible.y as u32,
                    width: visible.width as u32,
                    height: visible.height as u32,
                },
                counter: info.extra.capture_counter,
                at,
            });
        }
    }
}

wrap_load_handler! {
    struct BlinkLoadHandler {
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
    struct BlinkLifeSpanHandler {
        closed: Sender<()>,
    }

    impl LifeSpanHandler {
        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            let _ = self.closed.send(());
        }
    }
}

wrap_client! {
    struct BlinkClient {
        render_handler: RenderHandler,
        load_handler: LoadHandler,
        life_span_handler: LifeSpanHandler,
        request_handler: Option<RequestHandler>,
        pages: Sender<String>,
    }

    impl Client {
        fn on_process_message_received(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> ::std::os::raw::c_int {
            let Some(message) = message else { return 0 };
            if CefString::from(&message.name()).to_string() != video::PAGE {
                return 0;
            }
            let text = message
                .argument_list()
                .map(|list| CefString::from(&list.string(0)).to_string());
            let _ = self.pages.send(text.unwrap_or_default());
            1
        }

        fn request_handler(&self) -> Option<RequestHandler> {
            self.request_handler.clone()
        }

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
    struct BlinkObserver {
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

/// An off-screen browser at a fixed size, driven by external begin frames. Every method may be
/// called from any thread except the UI thread.
pub struct View {
    ui: Ui,
    host: OnUi<BrowserHost>,
    observer: OnUi<Option<Registration>>,
    paints: Receiver<Paint>,
    methods: Receiver<MethodResult>,
    closed: Receiver<()>,
    pages: Option<Receiver<String>>,
    project: Option<Project>,
    size: Arc<Size>,
    next_id: i32,
    serial: u64,
    ticking: bool,
    last_begin: Cell<Option<Instant>>,
    closing: bool,
}

impl View {
    pub fn open(ui: &Ui, url: &str, width: i32, height: i32) -> Result<View> {
        let view = View::launch(ui, url, width, height, None, None)?;
        view.settle()?;
        Ok(view)
    }

    /// Opens `entry` from `project` at `vrf://project/`. `init_script` runs in every document
    /// before any of the page's own scripts, and with it the view repaints a one-pixel layer on
    /// every begin frame, so each begin frame delivers even when the page did not change. Fails if
    /// the page asked for anything the project refused, such as an unpinned URL in offline mode.
    pub fn open_project(
        ui: &Ui,
        project: &Project,
        entry: &str,
        width: i32,
        height: i32,
        init_script: Option<&str>,
    ) -> Result<View> {
        let view = View::launch_project(ui, project, entry, width, height, init_script)?;
        view.settle()?;
        Ok(view)
    }

    /// Like [`View::open_project`], but returns before begin frames are settled, so the caller
    /// can [`View::resize`] first. Call [`View::settle`] before the first capture.
    pub fn launch_project(
        ui: &Ui,
        project: &Project,
        entry: &str,
        width: i32,
        height: i32,
        init_script: Option<&str>,
    ) -> Result<View> {
        let view = View::launch(
            ui,
            &project.url(entry),
            width,
            height,
            Some(project.clone()),
            init_script,
        )?;
        view.check_project()?;
        Ok(view)
    }

    /// Fails if the project refused or could not serve a request since it was opened.
    pub fn check_project(&self) -> Result<()> {
        let failures = self
            .project
            .as_ref()
            .map(Project::failures)
            .unwrap_or_default();
        ensure!(failures.is_empty(), "{}", failures.join("; "));
        Ok(())
    }

    fn launch(
        ui: &Ui,
        url: &str,
        width: i32,
        height: i32,
        project: Option<Project>,
        init_script: Option<&str>,
    ) -> Result<View> {
        let (paints_tx, paints) = mpsc::channel();
        let (methods_tx, methods) = mpsc::channel();
        let (loaded_tx, loaded) = mpsc::channel();
        let (closed_tx, closed) = mpsc::channel();
        let (pages_tx, pages) = mpsc::channel();
        let channels = Channels {
            paints: paints_tx,
            methods: methods_tx,
            loaded: loaded_tx,
            closed: closed_tx,
            pages: pages_tx,
        };
        let target = url.to_owned();
        let init_script = init_script.map(|script| format!("{TICK_JS}\n{script}"));
        let url = if init_script.is_some() {
            "about:blank".to_owned()
        } else {
            target.clone()
        };
        let size = Arc::new(Size {
            width: AtomicI32::new(width),
            height: AtomicI32::new(height),
        });
        let bound = project.clone();
        let render_size = size.clone();
        let (host, observer) = ui.call(move || -> Result<_> {
            let (mut context, request_handler) =
                bound.map(|project| project.bind()).transpose()?.unzip();
            let window_info = WindowInfo {
                windowless_rendering_enabled: true as _,
                shared_texture_enabled: true as _,
                external_begin_frame_enabled: true as _,
                ..Default::default()
            };
            let browser_settings = BrowserSettings {
                windowless_frame_rate: 1000,
                background_color: 0xFFFFFFFF,
                standard_font_family: GENERIC_FONTS[0].into(),
                serif_font_family: GENERIC_FONTS[0].into(),
                sans_serif_font_family: GENERIC_FONTS[1].into(),
                cursive_font_family: GENERIC_FONTS[2].into(),
                fantasy_font_family: GENERIC_FONTS[3].into(),
                ..Default::default()
            };
            let mut client = BlinkClient::new(
                BlinkRenderHandler::new(render_size, channels.paints, Arc::new(AtomicU64::new(0))),
                BlinkLoadHandler::new(channels.loaded),
                BlinkLifeSpanHandler::new(channels.closed),
                request_handler,
                channels.pages,
            );
            let browser = browser_host_create_browser_sync(
                Some(&window_info),
                Some(&mut client),
                Some(&url.as_str().into()),
                Some(&browser_settings),
                None,
                context.as_mut(),
            )
            .context("CEF did not create the browser")?;
            let host = browser.host().context("browser has no host")?;
            let observer = host
                .add_dev_tools_message_observer(Some(&mut BlinkObserver::new(channels.methods)));
            Ok((OnUi(host), OnUi(observer)))
        })?;
        loaded
            .recv_timeout(LOAD_TIMEOUT)
            .context("the page did not finish loading")?;
        let mut view = View {
            ui: ui.clone(),
            host,
            observer,
            paints,
            methods,
            closed,
            pages: Some(pages),
            project,
            size,
            next_id: 1,
            serial: SERIAL.fetch_add(1, Ordering::Relaxed),
            ticking: init_script.is_some(),
            last_begin: Cell::new(None),
            closing: false,
        };
        if let Some(source) = init_script.as_deref() {
            view.execute("Page.enable", serde_json::json!({}))?;
            view.execute(
                "Page.addScriptToEvaluateOnNewDocument",
                serde_json::json!({"source": source}),
            )?;
            view.execute("Page.navigate", serde_json::json!({"url": target}))?;
            while view.evaluate("location.href")? != target.as_str() {
                loaded
                    .recv_timeout(LOAD_TIMEOUT)
                    .context("the page did not finish loading")?;
            }
            view.evaluate(
                "document.readyState === 'complete' || new Promise((resolve) => addEventListener('load', () => resolve(true), { once: true }))",
            )?;
        }
        Ok(view)
    }

    /// The first begin frames after a load are unreliable. The first is usually lost, and a late
    /// delivery can follow one that came on time. This sends begin frames until `SETTLE_CLEAN`
    /// in a row give exactly one delivery at the view's size. The wait for a delivery starts
    /// short and doubles, so a lost frame costs tens of milliseconds, not a fixed quarter second.
    pub fn settle(&self) -> Result<()> {
        let (width, height) = (
            self.size.width.load(Ordering::Relaxed) as u32,
            self.size.height.load(Ordering::Relaxed) as u32,
        );
        let mut clean = 0;
        for round in 0..SETTLE_ROUNDS {
            if !self.ticking {
                self.invalidate();
            }
            self.begin_frame();
            let wait = SETTLE_DELIVERY[round.min(SETTLE_DELIVERY.len() - 1)];
            let delivered = self.recv_paint(wait).is_some_and(|paint| {
                (paint.visible.width, paint.visible.height) == (width, height)
            });
            let extra = self.recv_paint(SETTLE_QUIET).is_some();
            clean = if delivered && !extra { clean + 1 } else { 0 };
            if clean == SETTLE_CLEAN {
                return Ok(());
            }
        }
        bail!("begin frames never settled after {SETTLE_ROUNDS} rounds")
    }

    /// Changes the size of the view. Call it before the page has painted anything that matters,
    /// then [`View::settle`], which waits for a delivery at the new size: CEF shows the old
    /// size until the first frame at the new one. A resize without the screen info notice leaves
    /// every later delivery one begin frame behind the page.
    pub fn resize(&self, width: i32, height: i32) {
        self.size.width.store(width, Ordering::Relaxed);
        self.size.height.store(height, Ordering::Relaxed);
        let host = self.host.clone();
        self.ui.post(move || {
            host.0.was_resized();
            host.0.notify_screen_info_changed();
            host.0.was_resized();
        });
    }

    /// The messages the page sent with `__vrf_video_send`. Can be taken once.
    pub(crate) fn take_pages(&mut self) -> Option<Receiver<String>> {
        self.pages.take()
    }

    pub(crate) fn pusher(&self) -> Pusher {
        Pusher::new(self.ui.clone(), self.host.clone())
    }

    /// Distinguishes views, so a driver can tell that the page it was capturing was replaced.
    pub fn serial(&self) -> u64 {
        self.serial
    }

    /// Whether every begin frame is guaranteed a delivery, because the view repaints a one-pixel
    /// layer each time. A missing delivery on such a view is a failure, not an unchanged page.
    pub fn ticking(&self) -> bool {
        self.ticking
    }

    /// Sends a begin frame, at least `BEGIN_FRAME_GAP` after the last one. Chromium's frame
    /// capturer drops a frame that damages the page less than its 1 ms minimum capture period
    /// after the previous capture, and the drop is silent.
    pub fn begin_frame(&self) -> Instant {
        if let Some(last) = self.last_begin.get() {
            thread::sleep(BEGIN_FRAME_GAP.saturating_sub(last.elapsed()));
        }
        let host = self.host.clone();
        let sent = Instant::now();
        self.last_begin.set(Some(sent));
        self.ui.post(move || host.0.send_external_begin_frame());
        sent
    }

    pub fn invalidate(&self) {
        let host = self.host.clone();
        self.ui
            .post(move || host.0.invalidate(PaintElementType::default()));
    }

    pub fn recv_paint(&self, timeout: Duration) -> Option<Paint> {
        self.paints.recv_timeout(timeout).ok()
    }

    /// Runs a DevTools method and returns its result object. Fails if the method fails.
    pub fn execute(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.execute_within(EVALUATE_TIMEOUT, method, params)
    }

    fn execute_within(
        &mut self,
        timeout: Duration,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let id = self.next_id;
        self.next_id += 1;
        let host = self.host.clone();
        let (method_name, params) = (method.to_owned(), params.to_string());
        let submitted = self.ui.call(move || {
            let mut params =
                parse_json(Some(&params.as_str().into()), JsonParserOptions::default())
                    .and_then(|value| value.dictionary())
                    .expect("DevTools params are not a JSON object");
            host.0.execute_dev_tools_method(
                id,
                Some(&method_name.as_str().into()),
                Some(&mut params),
            )
        });
        ensure!(submitted == id, "DevTools rejected {method}");
        let result = loop {
            let result = self
                .methods
                .recv_timeout(timeout)
                .with_context(|| format!("no DevTools response to {method}"))?;
            if result.id == id {
                break result;
            }
        };
        let body: serde_json::Value = serde_json::from_slice(&result.result)?;
        ensure!(result.success, "{method} failed: {body}");
        Ok(body)
    }

    /// Runs `expression` in the page, waits for its promise if it returns one, and returns its
    /// value. Fails if the page throws.
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

    /// Whether the page's process still answers, which a view that sat idle may not.
    pub fn is_alive(&mut self) -> bool {
        self.execute_within(
            ALIVE_TIMEOUT,
            "Runtime.evaluate",
            serde_json::json!({"expression": "1"}),
        )
        .is_ok()
    }

    fn shutdown(&mut self) {
        if self.closing {
            return;
        }
        self.closing = true;
        let host = self.host.clone();
        let observer = OnUi(self.observer.0.take());
        self.ui.post(move || {
            drop(observer);
            host.0.close_browser(true as _);
        });
    }

    pub fn close(mut self) -> Result<()> {
        self.shutdown();
        self.closed
            .recv_timeout(CLOSE_TIMEOUT)
            .context("the browser did not close")
    }
}

impl Drop for View {
    fn drop(&mut self) {
        self.shutdown();
    }
}
