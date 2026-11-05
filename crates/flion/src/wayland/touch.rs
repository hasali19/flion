use std::collections::BTreeMap;
use std::mem;

use wayland_client::protocol::{wl_surface::WlSurface, wl_touch};

use crate::engine::{FlutterEngine, PointerButtons, PointerDeviceKind, PointerEvent, PointerPhase};

const TOUCH_DEVICE_ID_BASE: i32 = 1 << 16;

#[derive(Default)]
pub struct TouchState {
    active_touches: BTreeMap<i32, ActiveTouch>,
    pending_events: Vec<PointerEvent>,
}

#[derive(Clone, Copy)]
struct ActiveTouch {
    view_id: i64,
    position: (f64, f64),
}

impl TouchState {
    pub fn handle_event(
        &mut self,
        engine: &FlutterEngine,
        view_id_for_surface: impl Fn(&WlSurface) -> Option<i64>,
        scale_factor_for_view: impl Fn(i64) -> Option<f64>,
        event: wl_touch::Event,
    ) {
        match event {
            wl_touch::Event::Down {
                id, surface, x, y, ..
            } => {
                let Some(view_id) = view_id_for_surface(&surface) else {
                    return;
                };

                let Some(scale_factor) = scale_factor_for_view(view_id) else {
                    return;
                };

                let touch = ActiveTouch {
                    view_id,
                    position: (x * scale_factor, y * scale_factor),
                };

                if let Some(old) = self.active_touches.insert(id, touch) {
                    // I don't think we should get a down event for a touch id that hasn't ended, but
                    // just in case synthesize a cancel if so.
                    self.push_event(id, old, PointerPhase::Cancel);
                    self.push_event(id, old, PointerPhase::Remove);
                }

                self.push_event(id, touch, PointerPhase::Add);
                self.push_event(id, touch, PointerPhase::Down);
            }
            wl_touch::Event::Motion { id, x, y, .. } => {
                let Some(touch) = self.active_touches.get_mut(&id) else {
                    return;
                };

                let Some(scale_factor) = scale_factor_for_view(touch.view_id) else {
                    return;
                };

                touch.position = (x * scale_factor, y * scale_factor);

                let touch = *touch;
                self.push_event(id, touch, PointerPhase::Move);
            }
            wl_touch::Event::Up { id, .. } => {
                if let Some(touch) = self.active_touches.remove(&id) {
                    self.push_event(id, touch, PointerPhase::Up);
                    self.push_event(id, touch, PointerPhase::Remove);
                }
            }
            wl_touch::Event::Frame => self.flush_pending_events(engine),
            wl_touch::Event::Cancel => self.cancel_all(engine),
            _ => {}
        }
    }

    pub fn cancel_all(&mut self, engine: &FlutterEngine) {
        for (id, touch) in mem::take(&mut self.active_touches) {
            self.push_event(id, touch, PointerPhase::Cancel);
            self.push_event(id, touch, PointerPhase::Remove);
        }

        self.flush_pending_events(engine);
    }

    fn push_event(&mut self, id: i32, touch: ActiveTouch, phase: PointerPhase) {
        let buttons = match phase {
            PointerPhase::Down | PointerPhase::Move => PointerButtons::PRIMARY,
            _ => PointerButtons::empty(),
        };

        let (x, y) = touch.position;

        self.pending_events.push(PointerEvent {
            view_id: touch.view_id,
            device_kind: PointerDeviceKind::Touch,
            device_id: TOUCH_DEVICE_ID_BASE + id,
            phase,
            x,
            y,
            buttons,
            ..Default::default()
        });
    }

    fn flush_pending_events(&mut self, engine: &FlutterEngine) {
        if self.pending_events.is_empty() {
            return;
        }

        if let Err(e) = engine.send_pointer_events(&mem::take(&mut self.pending_events)) {
            tracing::error!("{e:?}");
        }
    }
}
