use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use cef::{rc::Rc as _, *};

pub(crate) const INIT: &str = "vrf.init";
static INITS: LazyLock<Mutex<HashMap<i32, String>>> = LazyLock::new(Default::default);
pub(crate) const PUSH: &str = "vrf.video.push";
pub(crate) const PAGE: &str = "vrf.video.page";
const SEND: &str = "__vrf_video_send";
const PUSHED: &str = "__vrf_video_push";

wrap_v8_handler! {
    struct SendToHost;

    impl V8Handler {
        fn execute(
            &self,
            _name: Option<&CefString>,
            _object: Option<&mut V8Value>,
            arguments: Option<&[Option<V8Value>]>,
            _retval: Option<&mut Option<V8Value>>,
            _exception: Option<&mut CefString>,
        ) -> ::std::os::raw::c_int {
            let Some(Some(text)) = arguments.and_then(<[_]>::first) else { return 0 };
            let Some(frame) = v8_context_get_current_context().and_then(|context| context.frame()) else { return 0 };
            let Some(mut message) = process_message_create(Some(&PAGE.into())) else { return 0 };
            if let Some(list) = message.argument_list() {
                list.set_string(0, Some(&CefString::from(&text.string_value())));
            }
            frame.send_process_message(ProcessId::BROWSER, Some(&mut message));
            1
        }
    }
}

wrap_render_process_handler! {
    pub(crate) struct VideoRenderHandler;

    impl RenderProcessHandler {
        fn on_browser_created(&self, browser: Option<&mut Browser>, extra_info: Option<&mut DictionaryValue>) {
            let (Some(browser), Some(extra_info)) = (browser, extra_info) else { return };
            if extra_info.has_key(Some(&INIT.into())) != 0 {
                let script = CefString::from(&extra_info.string(Some(&INIT.into()))).to_string();
                INITS.lock().unwrap().insert(browser.identifier(), script);
            }
        }

        fn on_browser_destroyed(&self, browser: Option<&mut Browser>) {
            if let Some(browser) = browser {
                INITS.lock().unwrap().remove(&browser.identifier());
            }
        }

        fn on_context_created(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            context: Option<&mut V8Context>,
        ) {
            let (Some(frame), Some(context)) = (frame, context) else { return };
            let script = browser.and_then(|browser| INITS.lock().unwrap().get(&browser.identifier()).cloned());
            if let Some(script) = script {
                let (mut retval, mut exception) = (None, None);
                context.eval(Some(&script.as_str().into()), Some(&"vrf://init".into()), 1, Some(&mut retval), Some(&mut exception));
            }
            if frame.is_main() == 0 {
                return;
            }
            let Some(global) = context.global() else { return };
            let mut function = v8_value_create_function(Some(&SEND.into()), Some(&mut SendToHost::new()));
            global.set_value_bykey(Some(&SEND.into()), function.as_mut(), V8Propertyattribute::default());
        }

        fn on_process_message_received(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> ::std::os::raw::c_int {
            let (Some(frame), Some(message)) = (frame, message) else { return 0 };
            if CefString::from(&message.name()).to_string() != PUSH {
                return 0;
            }
            let Some(region) = message.shared_memory_region() else { return 0 };
            let Some(context) = frame.v8_context() else { return 0 };
            if context.enter() == 0 {
                return 0;
            }
            if let (Some(buffer), Some(global)) = (
                v8_value_create_array_buffer_with_copy(region.memory().cast(), region.size()),
                context.global(),
            ) && let Some(callback) = global.value_bykey(Some(&PUSHED.into()))
            {
                let _ = callback.execute_function(Some(&mut global.clone()), Some(&[Some(buffer)]));
            }
            context.exit();
            1
        }
    }
}
