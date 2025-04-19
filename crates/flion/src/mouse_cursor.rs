use std::cell::RefCell;
use std::rc::Rc;

use windows::Win32::UI::WindowsAndMessaging::{
    LoadCursorW, SetCursor, HCURSOR, IDC_ARROW, IDC_HAND, IDC_IBEAM,
};

use crate::codec::EncodableValue;
use crate::standard_method_channel::{StandardMethodHandler, StandardMethodReply};

pub struct MouseCursorHandler {
    cursor_state: Rc<RefCell<Option<HCURSOR>>>,
}

impl MouseCursorHandler {
    pub fn new(cursor_state: Rc<RefCell<Option<HCURSOR>>>) -> MouseCursorHandler {
        MouseCursorHandler { cursor_state }
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

                let cursor = get_cursor(kind);

                unsafe { SetCursor(cursor) };

                *self.cursor_state.borrow_mut() = cursor;

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
