use std::cell::RefCell;
use std::rc::Rc;

use wayland_client::protocol::wl_pointer::WlPointer;
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
    Shape, WpCursorShapeDeviceV1,
};

use crate::codec::EncodableValue;
use crate::standard_method_channel::{StandardMethodHandler, StandardMethodReply};

pub struct MouseCursor {
    pointer: Option<WlPointer>,
    shape_device: Option<WpCursorShapeDeviceV1>,
    enter_serial: Option<u32>,
    shape: Option<Shape>,
}

impl MouseCursor {
    pub fn new() -> MouseCursor {
        MouseCursor {
            pointer: None,
            shape_device: None,
            enter_serial: None,
            shape: Some(Shape::Default),
        }
    }

    pub fn set_pointer(
        &mut self,
        pointer: Option<WlPointer>,
        shape_device: Option<WpCursorShapeDeviceV1>,
    ) {
        if let Some(device) = self.shape_device.take() {
            device.destroy();
        }

        self.pointer = pointer;
        self.shape_device = shape_device;
        self.enter_serial = None;
    }

    pub fn on_enter(&mut self, serial: u32) {
        self.enter_serial = Some(serial);
        self.apply();
    }

    pub fn on_leave(&mut self) {
        self.enter_serial = None;
    }

    fn set_shape(&mut self, shape: Option<Shape>) {
        if self.shape != shape {
            self.shape = shape;
            self.apply();
        }
    }

    fn apply(&self) {
        let Some(serial) = self.enter_serial else {
            return;
        };

        match self.shape {
            Some(shape) => {
                if let Some(device) = &self.shape_device {
                    device.set_shape(serial, shape);
                }
            }
            None => {
                if let Some(pointer) = &self.pointer {
                    pointer.set_cursor(serial, None, 0, 0);
                }
            }
        }
    }
}

pub struct MouseCursorHandler {
    cursor: Rc<RefCell<MouseCursor>>,
}

impl MouseCursorHandler {
    pub fn new(cursor: Rc<RefCell<MouseCursor>>) -> MouseCursorHandler {
        MouseCursorHandler { cursor }
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

                self.cursor.borrow_mut().set_shape(get_cursor_shape(kind));

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
