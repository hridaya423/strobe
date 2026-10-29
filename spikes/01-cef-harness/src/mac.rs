use cef::application_mac::{CefAppProtocol, CrAppControlProtocol, CrAppProtocol};
use objc2::{
    ClassType, DefinedClass, MainThreadMarker, define_class, extern_methods, msg_send, rc::Retained,
    runtime::{Bool, NSObjectProtocol},
};
use objc2_app_kit::{NSApp, NSApplication, NSEvent};
use std::cell::Cell;

#[derive(Default)]
pub struct Ivars {
    handling_send_event: Cell<Bool>,
}

define_class!(
    #[unsafe(super(NSApplication))]
    #[ivars = Ivars]
    pub struct HarnessApplication;

    impl HarnessApplication {
        #[unsafe(method(sendEvent:))]
        unsafe fn send_event(&self, event: &NSEvent) {
            let was_sending = self.ivars().handling_send_event.get().as_bool();
            if !was_sending {
                self.ivars().handling_send_event.set(Bool::YES);
            }
            let _: () = msg_send![super(self), sendEvent: event];
            if !was_sending {
                self.ivars().handling_send_event.set(Bool::NO);
            }
        }
    }

    unsafe impl CrAppControlProtocol for HarnessApplication {
        #[unsafe(method(setHandlingSendEvent:))]
        unsafe fn _set_handling_send_event(&self, handling: Bool) {
            self.ivars().handling_send_event.set(handling);
        }
    }

    unsafe impl CrAppProtocol for HarnessApplication {
        #[unsafe(method(isHandlingSendEvent))]
        unsafe fn _is_handling_send_event(&self) -> Bool {
            self.ivars().handling_send_event.get()
        }
    }

    unsafe impl CefAppProtocol for HarnessApplication {}
);

impl HarnessApplication {
    extern_methods! {
        #[unsafe(method(sharedApplication))]
        fn shared_application() -> Retained<Self>;
    }
}

pub fn setup_application() {
    let _ = HarnessApplication::shared_application();
    let mtm = MainThreadMarker::new().expect("CEF needs the main thread");
    assert!(NSApp(mtm).isKindOfClass(HarnessApplication::class()));
}
