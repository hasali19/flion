use std::rc::Rc;

use winit::window::{CursorIcon, Window};

use crate::codec::EncodableValue;
use crate::standard_method_channel::{StandardMethodHandler, StandardMethodReply};

pub struct MouseCursorHandler {
    window: Rc<Window>,
}

impl MouseCursorHandler {
    pub fn new(window: Rc<Window>) -> MouseCursorHandler {
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

                if kind == "none" {
                    self.window.set_cursor_visible(false);
                } else {
                    let cursor = match kind {
                        "basic" => CursorIcon::Default,
                        "click" => CursorIcon::Pointer,
                        "text" => CursorIcon::Text,
                        name => {
                            tracing::warn!("unknown cursor name: {name}");
                            CursorIcon::Default
                        }
                    };

                    self.window.set_cursor_icon(cursor);
                    self.window.set_cursor_visible(true);
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
