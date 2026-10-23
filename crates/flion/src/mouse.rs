use std::mem;

use wayland_client::WEnum;
use wayland_client::protocol::wl_pointer;

use crate::engine::{
    FlutterEngine, PointerButtons, PointerDeviceKind, PointerEvent, PointerPhase, PointerSignal,
};

const MOUSE_DEVICE_ID: i32 = 0;

/// Matches the gtk embedder
const SCROLL_LINE_HEIGHT: f64 = 53.0;

// Linux evdev button codes
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;
const BTN_SIDE: u32 = 0x113;
const BTN_EXTRA: u32 = 0x114;
const BTN_FORWARD: u32 = 0x115;
const BTN_BACK: u32 = 0x116;

#[derive(Default)]
pub struct MouseState {
    added: bool,
    entered: bool,
    x: f64,
    y: f64,
    buttons: PointerButtons,
    pending_scroll: PendingScroll,
}

#[derive(Default)]
struct PendingScroll {
    source: Option<wl_pointer::AxisSource>,
    /// Continuous scroll distance in logical pixels
    x: f64,
    y: f64,
    /// High-resolution wheel steps (120 = one notch)
    x_high_res: i32,
    y_high_res: i32,
}

impl MouseState {
    pub fn handle_event(
        &mut self,
        engine: &FlutterEngine,
        scale_factor: f64,
        event: wl_pointer::Event,
    ) {
        match event {
            wl_pointer::Event::Enter {
                surface_x,
                surface_y,
                ..
            } => {
                self.entered = true;
                self.set_position(surface_x, surface_y, scale_factor);
                self.add_pointer(engine);
            }
            wl_pointer::Event::Leave { .. } => {
                self.entered = false;
                if self.buttons.is_empty() {
                    self.remove_pointer(engine);
                }
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                self.set_position(surface_x, surface_y, scale_factor);
                self.add_pointer(engine);
                send_pointer_event(engine, &self.pointer_event(self.get_phase_for_motion()));
            }
            wl_pointer::Event::Button { button, state, .. } => {
                let pressed = matches!(state, WEnum::Value(wl_pointer::ButtonState::Pressed));
                self.on_button(engine, button, pressed);
            }
            wl_pointer::Event::Axis { axis, value, .. } => match axis {
                WEnum::Value(wl_pointer::Axis::VerticalScroll) => self.pending_scroll.y += value,
                WEnum::Value(wl_pointer::Axis::HorizontalScroll) => self.pending_scroll.x += value,
                _ => {}
            },
            wl_pointer::Event::AxisSource { axis_source } => {
                self.pending_scroll.source = axis_source.into_result().ok();
            }
            wl_pointer::Event::AxisValue120 { axis, value120 } => match axis {
                WEnum::Value(wl_pointer::Axis::VerticalScroll) => {
                    self.pending_scroll.y_high_res += value120
                }
                WEnum::Value(wl_pointer::Axis::HorizontalScroll) => {
                    self.pending_scroll.x_high_res += value120
                }
                _ => {}
            },
            wl_pointer::Event::Frame => self.flush_scroll(engine, scale_factor),
            _ => {}
        }
    }

    fn on_button(&mut self, engine: &FlutterEngine, button: u32, pressed: bool) {
        let button = match button {
            BTN_LEFT => PointerButtons::PRIMARY,
            BTN_RIGHT => PointerButtons::SECONDARY,
            BTN_MIDDLE => PointerButtons::MIDDLE,
            BTN_SIDE | BTN_BACK => PointerButtons::BACK,
            BTN_EXTRA | BTN_FORWARD => PointerButtons::FORWARD,
            _ => return,
        };

        self.add_pointer(engine);

        let was_pressed = !self.buttons.is_empty();

        self.buttons.set(button, pressed);

        let is_pressed = !self.buttons.is_empty();

        let phase = match (was_pressed, is_pressed) {
            (false, true) => PointerPhase::Down,
            (true, false) => PointerPhase::Up,
            (true, true) => PointerPhase::Move,
            (false, false) => return,
        };

        send_pointer_event(engine, &self.pointer_event(phase));

        if !is_pressed && !self.entered {
            self.remove_pointer(engine);
        }
    }

    fn flush_scroll(&mut self, engine: &FlutterEngine, scale_factor: f64) {
        let scroll = mem::take(&mut self.pending_scroll);

        let is_wheel = matches!(scroll.source, Some(wl_pointer::AxisSource::Wheel))
            && (scroll.x_high_res != 0 || scroll.y_high_res != 0);

        let (dx, dy) = if is_wheel {
            let line_height = SCROLL_LINE_HEIGHT * scale_factor;
            (
                f64::from(scroll.x_high_res) / 120.0 * line_height,
                f64::from(scroll.y_high_res) / 120.0 * line_height,
            )
        } else {
            (scroll.x * scale_factor, scroll.y * scale_factor)
        };

        if dx == 0.0 && dy == 0.0 {
            return;
        }

        self.add_pointer(engine);

        send_pointer_event(
            engine,
            &PointerEvent {
                signal: PointerSignal::Scroll {
                    delta_x: dx,
                    delta_y: dy,
                },
                ..self.pointer_event(self.get_phase_for_motion())
            },
        );
    }

    fn add_pointer(&mut self, engine: &FlutterEngine) {
        if !self.added {
            send_pointer_event(engine, &self.pointer_event(PointerPhase::Add));
            self.added = true;
        }
    }

    fn remove_pointer(&mut self, engine: &FlutterEngine) {
        if self.added {
            send_pointer_event(engine, &self.pointer_event(PointerPhase::Remove));
            self.added = false;
        }
    }

    fn set_position(&mut self, surface_x: f64, surface_y: f64, scale_factor: f64) {
        self.x = surface_x * scale_factor;
        self.y = surface_y * scale_factor;
    }

    fn get_phase_for_motion(&self) -> PointerPhase {
        if self.buttons.is_empty() {
            PointerPhase::Hover
        } else {
            PointerPhase::Move
        }
    }

    fn pointer_event(&self, phase: PointerPhase) -> PointerEvent {
        PointerEvent {
            device_kind: PointerDeviceKind::Mouse,
            device_id: MOUSE_DEVICE_ID,
            phase,
            x: self.x,
            y: self.y,
            buttons: self.buttons,
            signal: PointerSignal::None,
        }
    }
}

fn send_pointer_event(engine: &FlutterEngine, event: &PointerEvent) {
    if let Err(e) = engine.send_pointer_event(event) {
        tracing::error!("{e:?}");
    }
}
