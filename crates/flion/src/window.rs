use std::sync::Arc;
use std::time::Instant;

use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, Touch, TouchPhase};
use winit::window::{Window, WindowId};

use crate::engine::{
    FlutterEngine, FlutterTask, PointerButtons, PointerDeviceKind, PointerEvent, PointerPhase,
    PointerSignal,
};
use crate::task_queue::FlutterTaskQueue;

pub struct FlutterWindow {
    window: Arc<Window>,
    engine: FlutterEngine,
    platform_tasks: FlutterTaskQueue,
    mouse: MouseState,
}

const MOUSE_DEVICE_ID: i32 = 0;

/// Matches the gtk embedder
const SCROLL_LINE_HEIGHT: f64 = 53.0;

#[derive(Default)]
struct MouseState {
    added: bool,
    entered: bool,
    x: f64,
    y: f64,
    buttons: PointerButtons,
}

impl FlutterWindow {
    pub fn new(window: Arc<Window>, engine: FlutterEngine) -> FlutterWindow {
        FlutterWindow {
            window,
            engine,
            platform_tasks: FlutterTaskQueue::new(),
            mouse: MouseState::default(),
        }
    }

    pub fn id(&self) -> WindowId {
        self.window.id()
    }

    pub fn push_platform_task(&mut self, task: FlutterTask) {
        self.platform_tasks.push(task);
    }

    pub fn run_platform_tasks(&mut self) -> Option<Instant> {
        self.platform_tasks.run_tasks(&self.engine);
        self.platform_tasks.next_deadline()
    }

    pub fn on_resized(&mut self, size: PhysicalSize<u32>) {
        self.send_window_metrics(size, self.window.scale_factor());
    }

    pub fn on_scale_factor_changed(&mut self, scale_factor: f64) {
        self.send_window_metrics(self.window.inner_size(), scale_factor);
    }

    pub fn on_cursor_entered(&mut self) {
        self.mouse.entered = true;
        self.add_mouse();
    }

    pub fn on_cursor_left(&mut self) {
        self.mouse.entered = false;
        if self.mouse.buttons.is_empty() {
            self.remove_mouse();
        }
    }

    pub fn on_cursor_moved(&mut self, position: PhysicalPosition<f64>) {
        self.mouse.x = position.x;
        self.mouse.y = position.y;

        self.add_mouse();

        let phase = if self.mouse.buttons.is_empty() {
            PointerPhase::Hover
        } else {
            PointerPhase::Move
        };

        self.send_pointer_event(&self.mouse_event(phase));
    }

    pub fn on_mouse_input(&mut self, state: ElementState, button: MouseButton) {
        let button = match button {
            MouseButton::Left => PointerButtons::PRIMARY,
            MouseButton::Right => PointerButtons::SECONDARY,
            MouseButton::Middle => PointerButtons::MIDDLE,
            MouseButton::Back => PointerButtons::BACK,
            MouseButton::Forward => PointerButtons::FORWARD,
            MouseButton::Other(_) => return,
        };

        self.add_mouse();

        let was_pressed = !self.mouse.buttons.is_empty();

        match state {
            ElementState::Pressed => self.mouse.buttons.insert(button),
            ElementState::Released => self.mouse.buttons.remove(button),
        }

        let is_pressed = !self.mouse.buttons.is_empty();

        let phase = match (was_pressed, is_pressed) {
            (false, true) => PointerPhase::Down,
            (true, false) => PointerPhase::Up,
            (true, true) => PointerPhase::Move,
            (false, false) => return,
        };

        self.send_pointer_event(&self.mouse_event(phase));

        if !is_pressed && !self.mouse.entered {
            self.remove_mouse();
        }
    }

    pub fn on_mouse_wheel(&mut self, delta: MouseScrollDelta) {
        let (dx, dy) = match delta {
            MouseScrollDelta::LineDelta(x, y) => {
                let line_height = SCROLL_LINE_HEIGHT * self.window.scale_factor();
                (x as f64 * line_height, y as f64 * line_height)
            }
            MouseScrollDelta::PixelDelta(delta) => (delta.x, delta.y),
        };

        self.add_mouse();

        let phase = if self.mouse.buttons.is_empty() {
            PointerPhase::Hover
        } else {
            PointerPhase::Move
        };

        self.send_pointer_event(&PointerEvent {
            signal: PointerSignal::Scroll {
                delta_x: -dx,
                delta_y: -dy,
            },
            ..self.mouse_event(phase)
        });
    }

    pub fn on_touch(&mut self, touch: Touch) {
        let phases: &[PointerPhase] = match touch.phase {
            TouchPhase::Started => &[PointerPhase::Add, PointerPhase::Down],
            TouchPhase::Moved => &[PointerPhase::Move],
            TouchPhase::Ended => &[PointerPhase::Up, PointerPhase::Remove],
            TouchPhase::Cancelled => &[PointerPhase::Cancel, PointerPhase::Remove],
        };

        for &phase in phases {
            self.send_pointer_event(&Self::touch_event(&touch, phase));
        }
    }

    fn send_window_metrics(&self, size: PhysicalSize<u32>, scale_factor: f64) {
        self.engine
            .send_window_metrics_event(size.width as _, size.height as _, scale_factor)
            .unwrap();
    }

    fn add_mouse(&mut self) {
        if !self.mouse.added {
            self.send_pointer_event(&self.mouse_event(PointerPhase::Add));
            self.mouse.added = true;
        }
    }

    fn remove_mouse(&mut self) {
        if self.mouse.added {
            self.send_pointer_event(&self.mouse_event(PointerPhase::Remove));
            self.mouse.added = false;
        }
    }

    fn send_pointer_event(&self, event: &PointerEvent) {
        if let Err(e) = self.engine.send_pointer_event(event) {
            tracing::error!("{e:?}");
        }
    }

    fn mouse_event(&self, phase: PointerPhase) -> PointerEvent {
        PointerEvent {
            device_kind: PointerDeviceKind::Mouse,
            device_id: MOUSE_DEVICE_ID,
            phase,
            x: self.mouse.x,
            y: self.mouse.y,
            buttons: self.mouse.buttons,
            signal: PointerSignal::None,
        }
    }

    fn touch_event(touch: &Touch, phase: PointerPhase) -> PointerEvent {
        PointerEvent {
            device_kind: PointerDeviceKind::Touch,
            device_id: (touch.id as i32).wrapping_add(MOUSE_DEVICE_ID + 1),
            phase,
            x: touch.location.x,
            y: touch.location.y,
            buttons: PointerButtons::empty(),
            signal: PointerSignal::None,
        }
    }
}
