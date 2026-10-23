use std::sync::Weak;

use winit::window::{Cursor, CursorIcon, Window};

use crate::codec::EncodableValue;
use crate::standard_method_channel::{StandardMethodHandler, StandardMethodReply};

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
                    if let Some(cursor) = get_cursor(kind) {
                        window.set_cursor_visible(true);
                        window.set_cursor(cursor);
                    } else {
                        window.set_cursor_visible(false);
                    }
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

fn get_cursor(name: &str) -> Option<Cursor> {
    Some(match name {
        "none" => return None,
        "basic" => Cursor::Icon(CursorIcon::Default),
        "click" => Cursor::Icon(CursorIcon::Pointer),
        "text" => Cursor::Icon(CursorIcon::Text),
        _ => Cursor::default(),
    })
}
