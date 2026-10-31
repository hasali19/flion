use calloop::LoopHandle;
use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_client::protocol::wl_pointer::{self, WlPointer};
use wayland_client::protocol::wl_seat::{self, Capability, WlSeat};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::protocol::wl_touch::{self, WlTouch};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
    self, Shape, WpCursorShapeDeviceV1,
};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_manager_v1::{
    self, WpCursorShapeManagerV1,
};

use crate::engine::FlutterEngine;
use crate::keyboard::KeyboardState;
use crate::mouse::MouseState;
use crate::touch::TouchState;
use crate::{AppState, WindowId};

/// Owns the seat and the input devices on it.
///
/// Devices are created when the compositor reports the matching capability and released when
/// it goes away, so the state for a device only exists while the device does.
pub struct Seat {
    seat: WlSeat,
    cursor_shape_manager: Option<WpCursorShapeManagerV1>,
    /// The cursor shape requested by Flutter. Kept here so it carries over to new pointers.
    cursor_shape: Option<Shape>,
    pointer: Option<Pointer>,
    touch: Option<Touch>,
    keyboard: Option<WlKeyboard>,
    /// Lives as long as the seat rather than the keyboard device, since the `flutter/keyboard`
    /// channel handler reads the pressed keys from it.
    pub keyboard_state: KeyboardState,
}

struct Pointer {
    pointer: WlPointer,
    shape_device: Option<WpCursorShapeDeviceV1>,
    /// Serial of the last enter event, needed to set the cursor. `None` while outside our surfaces.
    enter_serial: Option<u32>,
    mouse: MouseState,
}

struct Touch {
    touch: WlTouch,
    state: TouchState,
}

impl Seat {
    pub fn new(
        seat: WlSeat,
        cursor_shape_manager: Option<WpCursorShapeManagerV1>,
        keyboard_state: KeyboardState,
    ) -> Seat {
        Seat {
            seat,
            cursor_shape_manager,
            cursor_shape: Some(Shape::Default),
            pointer: None,
            touch: None,
            keyboard: None,
            keyboard_state,
        }
    }

    pub fn wl_seat(&self) -> &WlSeat {
        &self.seat
    }

    pub fn set_cursor_shape(&mut self, shape: Option<Shape>) {
        if self.cursor_shape != shape {
            self.cursor_shape = shape;
            if let Some(pointer) = &self.pointer {
                pointer.apply_cursor(shape);
            }
        }
    }

    fn set_capabilities(
        &mut self,
        capabilities: Capability,
        qh: &QueueHandle<AppState>,
        engine: &FlutterEngine,
        loop_handle: &LoopHandle<'static, AppState>,
    ) {
        if capabilities.contains(Capability::Pointer) {
            if self.pointer.is_none() {
                let pointer = self.seat.get_pointer(qh, ());
                let shape_device = self
                    .cursor_shape_manager
                    .as_ref()
                    .map(|manager| manager.get_pointer(&pointer, qh, ()));

                self.pointer = Some(Pointer {
                    pointer,
                    shape_device,
                    enter_serial: None,
                    mouse: MouseState::default(),
                });
            }
        } else {
            self.release_pointer(engine);
        }

        if capabilities.contains(Capability::Touch) {
            if self.touch.is_none() {
                self.touch = Some(Touch {
                    touch: self.seat.get_touch(qh, ()),
                    state: TouchState::default(),
                });
            }
        } else {
            self.release_touch(engine);
        }

        if capabilities.contains(Capability::Keyboard) {
            if self.keyboard.is_none() {
                self.keyboard = Some(self.seat.get_keyboard(qh, ()));
            }
        } else {
            self.release_keyboard(engine, loop_handle);
        }
    }

    fn release_pointer(&mut self, engine: &FlutterEngine) {
        if let Some(pointer) = self.pointer.take() {
            pointer.release(engine);
        }
    }

    fn release_touch(&mut self, engine: &FlutterEngine) {
        if let Some(touch) = self.touch.take() {
            touch.release(engine);
        }
    }

    fn release_keyboard(
        &mut self,
        engine: &FlutterEngine,
        loop_handle: &LoopHandle<'static, AppState>,
    ) {
        if let Some(keyboard) = self.keyboard.take() {
            self.keyboard_state.reset(engine, loop_handle);
            keyboard.release();
        }
    }

    fn handle_pointer_event(
        &mut self,
        engine: &FlutterEngine,
        scale_factor_for_view: impl Fn(WindowId) -> Option<f64>,
        event: wl_pointer::Event,
    ) {
        let Some(pointer) = &mut self.pointer else {
            return;
        };

        match &event {
            wl_pointer::Event::Enter { serial, .. } => {
                pointer.enter_serial = Some(*serial);
                pointer.apply_cursor(self.cursor_shape);
            }
            wl_pointer::Event::Leave { .. } => pointer.enter_serial = None,
            _ => {}
        }

        pointer
            .mouse
            .handle_event(engine, view_id_for_surface, scale_factor_for_view, event);
    }

    fn handle_touch_event(
        &mut self,
        engine: &FlutterEngine,
        scale_factor_for_view: impl Fn(WindowId) -> Option<f64>,
        event: wl_touch::Event,
    ) {
        if let Some(touch) = &mut self.touch {
            touch
                .state
                .handle_event(engine, view_id_for_surface, scale_factor_for_view, event);
        }
    }

    /// Releases all devices and the seat. Must be called before the engine shuts down, so that
    /// Flutter is told about any keys, buttons or touches still held.
    pub fn destroy(mut self, engine: &FlutterEngine, loop_handle: &LoopHandle<'static, AppState>) {
        self.release_pointer(engine);
        self.release_touch(engine);
        self.release_keyboard(engine, loop_handle);

        if let Some(manager) = self.cursor_shape_manager.take() {
            manager.destroy();
        }

        self.seat.release();
    }
}

impl Pointer {
    fn apply_cursor(&self, shape: Option<Shape>) {
        let Some(serial) = self.enter_serial else {
            return;
        };

        match shape {
            Some(shape) => {
                if let Some(device) = &self.shape_device {
                    device.set_shape(serial, shape);
                }
            }
            None => self.pointer.set_cursor(serial, None, 0, 0),
        }
    }

    fn release(mut self, engine: &FlutterEngine) {
        self.mouse.release(engine);

        if let Some(device) = self.shape_device {
            device.destroy();
        }

        self.pointer.release();
    }
}

impl Touch {
    fn release(mut self, engine: &FlutterEngine) {
        self.state.cancel_all(engine);
        self.touch.release();
    }
}

fn view_id_for_surface(surface: &WlSurface) -> Option<WindowId> {
    surface.data().copied()
}

impl Dispatch<WlSeat, ()> for AppState {
    fn event(
        state: &mut Self,
        _seat: &WlSeat,
        event: wl_seat::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        {
            state
                .seat
                .set_capabilities(capabilities, qh, &state.engine, &state.loop_handle);
        }
    }
}

impl Dispatch<WlPointer, ()> for AppState {
    fn event(
        state: &mut Self,
        _pointer: &WlPointer,
        event: wl_pointer::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wl_pointer::Event::Button { serial, .. } = &event {
            state.clipboard.borrow_mut().set_input_serial(*serial);
        }

        let windows = &state.windows;

        state.seat.handle_pointer_event(
            &state.engine,
            |id| windows.get(&id).map(|window| window.scale_factor),
            event,
        );
    }
}

impl Dispatch<WlTouch, ()> for AppState {
    fn event(
        state: &mut Self,
        _touch: &WlTouch,
        event: wl_touch::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wl_touch::Event::Down { serial, .. } = &event {
            state.clipboard.borrow_mut().set_input_serial(*serial);
        }

        let windows = &state.windows;

        state.seat.handle_touch_event(
            &state.engine,
            |id| windows.get(&id).map(|window| window.scale_factor),
            event,
        );
    }
}

impl Dispatch<WlKeyboard, ()> for AppState {
    fn event(
        state: &mut Self,
        _keyboard: &WlKeyboard,
        event: wl_keyboard::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Enter { serial, .. } | wl_keyboard::Event::Key { serial, .. } =
            &event
        {
            state.clipboard.borrow_mut().set_input_serial(*serial);
        }

        state
            .seat
            .keyboard_state
            .handle_event(&state.engine, &state.loop_handle, event);
    }
}

impl Dispatch<WpCursorShapeManagerV1, ()> for AppState {
    fn event(
        _state: &mut Self,
        _proxy: &WpCursorShapeManagerV1,
        _event: wp_cursor_shape_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpCursorShapeDeviceV1, ()> for AppState {
    fn event(
        _state: &mut Self,
        _proxy: &WpCursorShapeDeviceV1,
        _event: wp_cursor_shape_device_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}
