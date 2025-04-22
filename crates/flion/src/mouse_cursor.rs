use std::rc::Weak;

use windows::Win32::UI::WindowsAndMessaging::{
    LoadCursorW, HCURSOR, IDC_ARROW, IDC_HAND, IDC_IBEAM,
};

use crate::codec::EncodableValue;
use crate::standard_method_channel::{StandardMethodHandler, StandardMethodReply};
use crate::window::Window;

pub struct MouseCursorHandler {
    window: Weak<Window>,
}

impl MouseCursorHandler {
    pub fn new(window: Weak<Window>) -> MouseCursorHandler {
        MouseCursorHandler { window }
    }
}

impl StandardMethodHandler for MouseCursorHandler {
    fn handle(&self, method: &str, args: EncodableValue, reply: StandardMethodReply) {
        match method {
            "activateSystemCursor" => {
                let args = args.as_map().unwrap();
                let kind = args
                    .get(&EncodableValue::Str("kind"))
                    .unwrap()
                    .as_string()
                    .unwrap();

                if let Some(window) = self.window.upgrade() {
                    window.set_cursor(get_cursor(kind));
                }

                reply.success(&EncodableValue::Null);
            }
            _ => {
                tracing::warn!(method, "unimplemented");
                reply.not_implemented();
            }
        }
    }
}

fn get_cursor(name: &str) -> Option<HCURSOR> {
    let cursor = match name {
        "none" => return None,
        "basic" => IDC_ARROW,
        "click" => IDC_HAND,
        "text" => IDC_IBEAM,
        _ => IDC_ARROW,
    };

    unsafe { LoadCursorW(None, cursor).ok() }
}
