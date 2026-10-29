use crate::gpu::Gpu;
use cef::{rc::Rc as _, *};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
    time::{Duration, Instant},
};

pub struct Shared {
    pub gpu: Gpu,
    pub width: i32,
    pub height: i32,
    pub on_paint: RefCell<Option<Box<dyn FnMut(&AcceleratedPaintInfo)>>>,
    pub on_bitmap: RefCell<Option<Box<dyn FnMut(&[u8], i32, i32)>>>,
    pub paints: Cell<u64>,
    pub bitmaps: Cell<u64>,
    pub loaded: Cell<bool>,
    pub responses: RefCell<VecDeque<serde_json::Value>>,
}

pub fn raster_mode() -> String {
    std::env::var("HARNESS_RASTER").unwrap_or_else(|_| "gpu".into())
}

pub fn command_line_flags() -> Vec<String> {
    let mut flags: Vec<String> = [
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
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    match raster_mode().as_str() {
        "gpu" => {}
        "cpu" => flags.push("disable-gpu-rasterization".into()),
        "swcomp" => flags.push("disable-gpu-compositing".into()),
        other => panic!("HARNESS_RASTER must be gpu, cpu or swcomp, got {other}"),
    }
    if let Ok(extra) = std::env::var("HARNESS_FLAGS") {
        flags.extend(extra.split_whitespace().map(|f| f.trim_start_matches("--").to_string()));
    }
    flags
}

fn apply_flags(command_line: &mut CommandLine) {
    for flag in command_line_flags() {
        match flag.split_once('=') {
            Some((k, v)) => command_line
                .append_switch_with_value(Some(&k.into()), Some(&v.into())),
            None => command_line.append_switch(Some(&flag.as_str().into())),
        }
    }
}

wrap_app! {
    pub struct HarnessApp;

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

        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            Some(HarnessBrowserProcessHandler::new())
        }
    }
}

wrap_browser_process_handler! {
    struct HarnessBrowserProcessHandler;

    impl BrowserProcessHandler {
        fn on_before_child_process_launch(&self, command_line: Option<&mut CommandLine>) {
            if let Some(command_line) = command_line {
                apply_flags(command_line);
            }
        }
    }
}

wrap_render_handler! {
    struct HarnessRenderHandler {
        shared: Rc<Shared>,
    }

    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            if let Some(rect) = rect {
                rect.x = 0;
                rect.y = 0;
                rect.width = self.shared.width;
                rect.height = self.shared.height;
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
            let Some(info) = info else { return };
            if type_ != PaintElementType::default() {
                return;
            }
            if let Some(handler) = self.shared.on_paint.borrow_mut().as_mut() {
                handler(info);
            }
            self.shared.paints.set(self.shared.paints.get() + 1);
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
            if type_ != PaintElementType::default() || buffer.is_null() {
                return;
            }
            let bytes = unsafe { std::slice::from_raw_parts(buffer, (width * height * 4) as usize) };
            if let Some(handler) = self.shared.on_bitmap.borrow_mut().as_mut() {
                handler(bytes, width, height);
            }
            self.shared.bitmaps.set(self.shared.bitmaps.get() + 1);
        }
    }
}

wrap_load_handler! {
    struct HarnessLoadHandler {
        shared: Rc<Shared>,
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
                self.shared.loaded.set(true);
            }
        }
    }
}

wrap_client! {
    struct HarnessClient {
        render_handler: RenderHandler,
        load_handler: LoadHandler,
    }

    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.render_handler.clone())
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            Some(self.load_handler.clone())
        }
    }
}

wrap_dev_tools_message_observer! {
    struct HarnessObserver {
        shared: Rc<Shared>,
    }

    impl DevToolsMessageObserver {
        fn on_dev_tools_message(
            &self,
            _browser: Option<&mut Browser>,
            message: Option<&[u8]>,
        ) -> ::std::os::raw::c_int {
            if let Some(message) = message
                && let Ok(value) = serde_json::from_slice::<serde_json::Value>(message)
                && value.get("id").is_some()
            {
                self.shared.responses.borrow_mut().push_back(value);
            }
            0
        }
    }
}

pub struct Harness {
    pub shared: Rc<Shared>,
    pub host: BrowserHost,
    _registration: Option<Registration>,
    next_id: Cell<i64>,
}

impl Harness {
    pub fn open(url: &str, width: i32, height: i32) -> Harness {
        let shared = Rc::new(Shared {
            gpu: Gpu::new(),
            width,
            height,
            on_paint: RefCell::new(None),
            on_bitmap: RefCell::new(None),
            paints: Cell::new(0),
            bitmaps: Cell::new(0),
            loaded: Cell::new(false),
            responses: RefCell::new(VecDeque::new()),
        });
        let window_info = WindowInfo {
            windowless_rendering_enabled: true as _,
            shared_texture_enabled: true as _,
            external_begin_frame_enabled: true as _,
            ..Default::default()
        };
        let browser_settings = BrowserSettings {
            windowless_frame_rate: 1000,
            background_color: 0xFFFFFFFF,
            ..Default::default()
        };
        let mut client = HarnessClient::new(
            HarnessRenderHandler::new(shared.clone()),
            HarnessLoadHandler::new(shared.clone()),
        );
        let browser = browser_host_create_browser_sync(
            Some(&window_info),
            Some(&mut client),
            Some(&url.into()),
            Some(&browser_settings),
            None,
            None,
        )
        .expect("browser creation failed");
        let host = browser.host().expect("browser has no host");
        let registration = host.add_dev_tools_message_observer(Some(&mut HarnessObserver::new(shared.clone())));
        let harness = Harness {
            shared,
            host,
            _registration: registration,
            next_id: Cell::new(1),
        };
        assert!(
            harness.pump_until(Duration::from_secs(30), || harness.shared.loaded.get()),
            "page did not finish loading: {url}"
        );
        harness
    }

    pub fn pump_until(&self, timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
        let end = Instant::now() + timeout;
        loop {
            if done() {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            do_message_loop_work();
            std::thread::sleep(Duration::from_micros(30));
        }
    }

    pub fn pump_for(&self, duration: Duration) {
        self.pump_until(duration, || false);
    }

    pub fn eval(&self, expression: &str) -> serde_json::Value {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        let message = serde_json::json!({
            "id": id,
            "method": "Runtime.evaluate",
            "params": {"expression": expression, "awaitPromise": true, "returnByValue": true},
        });
        self.host
            .send_dev_tools_message(Some(message.to_string().as_bytes()));
        let mut response = None;
        let got = self.pump_until(Duration::from_secs(60), || {
            let mut queue = self.shared.responses.borrow_mut();
            while let Some(value) = queue.pop_front() {
                if value["id"] == id {
                    response = Some(value);
                    return true;
                }
            }
            false
        });
        assert!(got, "no DevTools response for {expression}");
        let response = response.unwrap();
        if let Some(exception) = response["result"].get("exceptionDetails") {
            panic!("page threw for {expression}: {exception}");
        }
        response["result"]["result"]["value"].clone()
    }

    pub fn begin_frame(&self) {
        self.host.send_external_begin_frame();
    }

    pub fn set_paint(&self, handler: impl FnMut(&AcceleratedPaintInfo) + 'static) {
        *self.shared.on_paint.borrow_mut() = Some(Box::new(handler));
    }

    pub fn set_bitmap(&self, handler: impl FnMut(&[u8], i32, i32) + 'static) {
        *self.shared.on_bitmap.borrow_mut() = Some(Box::new(handler));
    }

    pub fn deliveries(&self) -> u64 {
        self.shared.paints.get() + self.shared.bitmaps.get()
    }

    pub fn close(&self) {
        self.host.close_browser(true as _);
        self.pump_for(Duration::from_millis(300));
    }
}
