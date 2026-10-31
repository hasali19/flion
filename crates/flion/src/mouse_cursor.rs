use calloop::channel::Sender;
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::Shape;

use crate::codec::EncodableValue;
use crate::standard_method_channel::{StandardMethodHandler, StandardMethodReply};

/// Handler for the `flutter/mousecursor` channel. Requested shapes are sent to the event loop,
/// which applies them to the current pointer.
pub struct MouseCursorHandler {
    sender: Sender<Option<Shape>>,
}

impl MouseCursorHandler {
    pub fn new(sender: Sender<Option<Shape>>) -> MouseCursorHandler {
        MouseCursorHandler { sender }
    }
}

impl StandardMethodHandler for MouseCursorHandler {
    fn handle(&self, method: &str, args: EncodableValue, reply: StandardMethodReply) {
        match method {
            "activateSystemCursor" => {
                let kind = args
                    .as_map()
                    .and_then(|args| args.get(&EncodableValue::Str("kind")))
                    .and_then(|kind| kind.as_string());

                let Some(kind) = kind else {
                    tracing::error!("activateSystemCursor: missing cursor kind");
                    // TODO: Error reply
                    reply.success(&EncodableValue::Null);
                    return;
                };

                let _ = self.sender.send(get_cursor_shape(kind));

                reply.success(&EncodableValue::Null);
            }
            _ => {
                tracing::warn!(method, "unimplemented");
                reply.not_implemented();
            }
        }
    }
}

fn get_cursor_shape(kind: &str) -> Option<Shape> {
    Some(match kind {
        "none" => return None,
        "basic" => Shape::Default,
        "click" => Shape::Pointer,
        "forbidden" => Shape::NotAllowed,
        "wait" => Shape::Wait,
        "progress" => Shape::Progress,
        "contextMenu" => Shape::ContextMenu,
        "help" => Shape::Help,
        "text" => Shape::Text,
        "verticalText" => Shape::VerticalText,
        "cell" => Shape::Cell,
        "precise" => Shape::Crosshair,
        "move" => Shape::Move,
        "grab" => Shape::Grab,
        "grabbing" => Shape::Grabbing,
        "noDrop" => Shape::NoDrop,
        "alias" => Shape::Alias,
        "copy" => Shape::Copy,
        "allScroll" => Shape::AllScroll,
        "resizeLeftRight" => Shape::EwResize,
        "resizeUpDown" => Shape::NsResize,
        "resizeUpLeftDownRight" => Shape::NwseResize,
        "resizeUpRightDownLeft" => Shape::NeswResize,
        "resizeUp" => Shape::NResize,
        "resizeDown" => Shape::SResize,
        "resizeLeft" => Shape::WResize,
        "resizeRight" => Shape::EResize,
        "resizeUpLeft" => Shape::NwResize,
        "resizeUpRight" => Shape::NeResize,
        "resizeDownLeft" => Shape::SwResize,
        "resizeDownRight" => Shape::SeResize,
        "resizeColumn" => Shape::ColResize,
        "resizeRow" => Shape::RowResize,
        "zoomIn" => Shape::ZoomIn,
        "zoomOut" => Shape::ZoomOut,
        _ => Shape::Default,
    })
}
