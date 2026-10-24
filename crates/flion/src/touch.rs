use std::collections::BTreeMap;
use std::mem;

use wayland_client::protocol::wl_touch;

use crate::engine::{FlutterEngine, PointerButtons, PointerDeviceKind, PointerEvent, PointerPhase};

const TOUCH_DEVICE_ID_BASE: i32 = 1 << 16;

#[derive(Default)]
pub struct TouchState {
    active_touches: BTreeMap<i32, (f64, f64)>,
    pending_events: Vec<PointerEvent>,
}

impl TouchState {
    pub fn handle_event(
        &mut self,
        engine: &FlutterEngine,
        scale_factor: f64,
        event: wl_touch::Event,
    ) {
        match event {
            wl_touch::Event::Down { id, x, y, .. } => {
                let position = (x * scale_factor, y * scale_factor);

                if let Some(old) = self.active_touches.insert(id, position) {
                    // I don't think we should get a down event for a touch id that hasn't ended, but
                    // just in case synthesize a cancel if so.
                    self.push_event(id, old, PointerPhase::Cancel);
                    self.push_event(id, old, PointerPhase::Remove);
                }

                self.push_event(id, position, PointerPhase::Add);
                self.push_event(id, position, PointerPhase::Down);
            }
            wl_touch::Event::Motion { id, x, y, .. } => {
                if let Some(position) = self.active_touches.get_mut(&id) {
                    let new_position = (x * scale_factor, y * scale_factor);
                    *position = new_position;
                    self.push_event(id, new_position, PointerPhase::Move);
                }
            }
            wl_touch::Event::Up { id, .. } => {
                if let Some(position) = self.active_touches.remove(&id) {
                    self.push_event(id, position, PointerPhase::Up);
                    self.push_event(id, position, PointerPhase::Remove);
                }
            }
            wl_touch::Event::Frame => self.flush_pending_events(engine),
            wl_touch::Event::Cancel => self.cancel_all(engine),
            _ => {}
        }
    }

    pub fn cancel_all(&mut self, engine: &FlutterEngine) {
        for (id, position) in mem::take(&mut self.active_touches) {
            self.push_event(id, position, PointerPhase::Cancel);
            self.push_event(id, position, PointerPhase::Remove);
        }

        self.flush_pending_events(engine);
    }

    fn push_event(&mut self, id: i32, (x, y): (f64, f64), phase: PointerPhase) {
        let buttons = match phase {
            PointerPhase::Down | PointerPhase::Move => PointerButtons::PRIMARY,
            _ => PointerButtons::empty(),
        };

        self.pending_events.push(PointerEvent {
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
