// WARNING: This file was mostly written by claude since writing keyboard code sucks :(

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::mem::ManuallyDrop;
use std::os::fd::OwnedFd;
use std::rc::Rc;
use std::time::Duration;

use calloop::timer::{TimeoutAction, Timer};
use calloop::{LoopHandle, RegistrationToken};
use eyre::{Context, OptionExt};
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use tracing::error;
use wayland_client::WEnum;
use wayland_client::protocol::wl_keyboard;
use xkbcommon::xkb;

use crate::AppState;
use crate::codec::EncodableValue;
use crate::engine::{FlutterEngine, KeyEvent, KeyEventType};
use crate::keymap;
use crate::standard_method_channel::{StandardMethodHandler, StandardMethodReply};
use crate::text_input::{TextInputKeyEvent, TextInputState};

/// Plane used by the GTK embedder for keys that have no Flutter mapping.
const GTK_PLANE: u64 = 0x01500000000;

/// Offset between evdev scancodes (sent by wayland) and xkb keycodes.
const EVDEV_OFFSET: u32 = 8;

// GDK modifier masks, which is what the framework expects in raw key messages with the "gtk"
// toolkit.
const GDK_SHIFT_MASK: u32 = 1 << 0;
const GDK_LOCK_MASK: u32 = 1 << 1;
const GDK_CONTROL_MASK: u32 = 1 << 2;
const GDK_MOD1_MASK: u32 = 1 << 3;
const GDK_MOD2_MASK: u32 = 1 << 4;
const GDK_SUPER_MASK: u32 = 1 << 26;

/// Lock modifiers, with the physical and logical key that toggles each. The framework toggles
/// its lock state on every key down of these keys.
const LOCK_MODIFIERS: [(&str, u64, u64); 2] = [
    (xkb::MOD_NAME_CAPS, 0x00070039, 0x00100000104), // capsLock
    (xkb::MOD_NAME_NUM, 0x00070053, 0x0010000010a),  // numLock
];

pub struct KeyboardState {
    context: xkb::Context,
    state: Option<xkb::State>,
    pressed: Rc<RefCell<BTreeMap<u64, u64>>>,
    /// Logical keys of the lock modes that the framework thinks are enabled.
    enabled_locks: BTreeSet<u64>,
    repeat_rate: u32,
    repeat_delay: Duration,
    /// Tracks the current pressed key and an event loop timer registration for repeats.
    active_repeat: Option<(xkb::Keycode, RegistrationToken)>,
    text_input: Rc<RefCell<TextInputState>>,
}

impl KeyboardState {
    pub fn new(text_input: Rc<RefCell<TextInputState>>) -> KeyboardState {
        KeyboardState {
            context: xkb::Context::new(xkb::CONTEXT_NO_FLAGS),
            state: None,
            pressed: Rc::default(),
            enabled_locks: BTreeSet::new(),
            repeat_rate: 25,
            repeat_delay: Duration::from_millis(600),
            active_repeat: None,
            text_input,
        }
    }

    /// Handler for the legacy `flutter/keyboard` channel.
    pub fn channel_handler(&self) -> KeyboardChannelHandler {
        KeyboardChannelHandler {
            pressed: self.pressed.clone(),
        }
    }

    pub fn handle_event(
        &mut self,
        engine: &FlutterEngine,
        loop_handle: &LoopHandle<'static, AppState>,
        event: wl_keyboard::Event,
    ) {
        match event {
            wl_keyboard::Event::Keymap { format, fd, size } => {
                if let Err(e) = self.load_keymap(format, fd, size) {
                    error!("failed to load keymap: {e:?}");
                }
            }
            wl_keyboard::Event::Enter { keys, .. } => {
                let keycodes = keys
                    .chunks_exact(4)
                    .map(|b| u32::from_ne_bytes([b[0], b[1], b[2], b[3]]) + EVDEV_OFFSET);

                for keycode in keycodes {
                    self.synthesize_modifier_down(engine, keycode.into());
                }
            }
            wl_keyboard::Event::Leave { .. } => {
                self.stop_repeat(loop_handle);
                self.release_all(engine);
            }
            wl_keyboard::Event::Key { key, state, .. } => {
                let keycode = xkb::Keycode::new(key + EVDEV_OFFSET);
                match state {
                    WEnum::Value(wl_keyboard::KeyState::Pressed) => {
                        self.on_key(engine, keycode, true);
                        self.start_repeat(loop_handle, keycode);
                    }
                    WEnum::Value(wl_keyboard::KeyState::Released) => {
                        if matches!(self.active_repeat, Some((k, _)) if k == keycode) {
                            self.stop_repeat(loop_handle);
                        }
                        self.on_key(engine, keycode, false);
                    }
                    _ => {}
                }
            }
            wl_keyboard::Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                if let Some(state) = &mut self.state {
                    state.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                }

                self.sync_locks(engine);
            }
            wl_keyboard::Event::RepeatInfo { rate, delay } => {
                self.repeat_rate = rate.max(0) as u32;
                self.repeat_delay = Duration::from_millis(delay.max(0) as u64);
            }
            _ => {}
        }
    }

    fn load_keymap(
        &mut self,
        format: WEnum<wl_keyboard::KeymapFormat>,
        fd: OwnedFd,
        size: u32,
    ) -> eyre::Result<()> {
        if format != WEnum::Value(wl_keyboard::KeymapFormat::XkbV1) {
            eyre::bail!("unsupported keymap format: {format:?}");
        }

        let keymap = unsafe {
            xkb::Keymap::new_from_fd(
                &self.context,
                fd,
                size as usize,
                xkb::KEYMAP_FORMAT_TEXT_V1,
                xkb::KEYMAP_COMPILE_NO_FLAGS,
            )
        }
        .wrap_err("failed to read keymap")?
        .ok_or_eyre("failed to compile keymap")?;

        self.state = Some(xkb::State::new(&keymap));

        Ok(())
    }

    fn on_key(&mut self, engine: &FlutterEngine, keycode: xkb::Keycode, pressed: bool) {
        let Some(state) = &self.state else {
            return;
        };

        let physical = physical_key(keycode);
        let keysym = state.key_get_one_sym(keycode);

        let mut pressed_keys = self.pressed.borrow_mut();

        let event_type = match (pressed, pressed_keys.contains_key(&physical)) {
            (true, false) => KeyEventType::Down,
            (true, true) => KeyEventType::Repeat,
            (false, true) => KeyEventType::Up,
            // We never saw this key go down, so the framework doesn't think it's pressed.
            (false, false) => return,
        };

        let logical = match event_type {
            KeyEventType::Down => {
                let logical = logical_key(keysym);
                pressed_keys.insert(physical, logical);
                toggle_lock(&mut self.enabled_locks, logical);
                logical
            }
            KeyEventType::Repeat => pressed_keys[&physical],
            KeyEventType::Up => pressed_keys.remove(&physical).unwrap(),
        };

        drop(pressed_keys);

        let character = match event_type {
            KeyEventType::Up => None,
            _ => key_character(state, keycode),
        };

        let raw_message = RawKeyMessage {
            keymap: "linux",
            toolkit: "gtk",
            event_type: if pressed { "keydown" } else { "keyup" },
            scan_code: keycode.raw(),
            key_code: keysym.raw(),
            modifiers: gdk_modifiers(state),
            unicode_scalar_values: xkb::keysym_to_utf32(keysym),
        };

        let text_input = self.text_input.clone();
        let messenger = engine.messenger();
        let text_input_event = TextInputKeyEvent {
            event_type,
            logical,
            character: character.clone(),
        };

        send_key_event(
            engine,
            KeyEvent {
                event_type,
                synthesized: false,
                character,
                logical: Some(logical),
                physical: Some(physical),
            },
            Some(raw_message),
            move |handled| {
                // If the raw key event was unhandled, pass it on to the text input plugin.
                if !handled {
                    text_input
                        .borrow_mut()
                        .handle_event(&messenger, &text_input_event);
                }
            },
        );
    }

    fn sync_locks(&mut self, engine: &FlutterEngine) {
        let Some(state) = &self.state else {
            return;
        };

        for (name, physical, logical) in LOCK_MODIFIERS {
            let active = state.mod_name_is_active(name, xkb::STATE_MODS_LOCKED);
            let enabled = self.enabled_locks.contains(&logical);

            // A press can't be synthesized while the key is down. This shouldn't happen anyway,
            // since pressing the key toggles both states together.
            if active == enabled || self.pressed.borrow().contains_key(&physical) {
                continue;
            }

            toggle_lock(&mut self.enabled_locks, logical);

            for event_type in [KeyEventType::Down, KeyEventType::Up] {
                send_key_event(
                    engine,
                    KeyEvent {
                        event_type,
                        synthesized: true,
                        character: None,
                        logical: Some(logical),
                        physical: Some(physical),
                    },
                    None,
                    |_| {},
                );
            }
        }
    }

    fn synthesize_modifier_down(&mut self, engine: &FlutterEngine, keycode: xkb::Keycode) {
        let Some(state) = &self.state else {
            return;
        };

        let keysym = state.key_get_one_sym(keycode);
        if !is_held_modifier(keysym) {
            return;
        }

        let physical = physical_key(keycode);
        let logical = logical_key(keysym);

        if self.pressed.borrow().contains_key(&physical) {
            return;
        }

        self.pressed.borrow_mut().insert(physical, logical);

        send_key_event(
            engine,
            KeyEvent {
                event_type: KeyEventType::Down,
                synthesized: true,
                character: None,
                logical: Some(logical),
                physical: Some(physical),
            },
            None,
            |_| {},
        );
    }

    fn release_all(&mut self, engine: &FlutterEngine) {
        for (physical, logical) in self.pressed.take() {
            send_key_event(
                engine,
                KeyEvent {
                    event_type: KeyEventType::Up,
                    synthesized: true,
                    character: None,
                    logical: Some(logical),
                    physical: Some(physical),
                },
                None,
                |_| {},
            );
        }
    }

    fn start_repeat(&mut self, loop_handle: &LoopHandle<'static, AppState>, keycode: xkb::Keycode) {
        self.stop_repeat(loop_handle);

        let repeats = self
            .state
            .as_ref()
            .is_some_and(|state| state.get_keymap().key_repeats(keycode));

        if !repeats || self.repeat_rate == 0 {
            return;
        }

        let interval = Duration::from_secs(1) / self.repeat_rate;

        let token = loop_handle
            .insert_source(Timer::from_duration(self.repeat_delay), move |_, _, app| {
                app.keyboard_state.on_key(&app.engine, keycode, true);
                TimeoutAction::ToDuration(interval)
            })
            .ok();

        self.active_repeat = token.map(|token| (keycode, token));
    }

    fn stop_repeat(&mut self, loop_handle: &LoopHandle<'static, AppState>) {
        if let Some((_, token)) = self.active_repeat.take() {
            loop_handle.remove(token);
        }
    }
}

/// Whether the keysym is a modifier that is held rather than toggled. Lock keys are excluded,
/// since a synthesized press would toggle their state in the framework.
fn is_held_modifier(keysym: xkb::Keysym) -> bool {
    matches!(
        keysym,
        xkb::Keysym::Shift_L
            | xkb::Keysym::Shift_R
            | xkb::Keysym::Control_L
            | xkb::Keysym::Control_R
            | xkb::Keysym::Meta_L
            | xkb::Keysym::Meta_R
            | xkb::Keysym::Alt_L
            | xkb::Keysym::Alt_R
            | xkb::Keysym::Super_L
            | xkb::Keysym::Super_R
            | xkb::Keysym::Hyper_L
            | xkb::Keysym::Hyper_R
            | xkb::Keysym::ISO_Level3_Shift
            | xkb::Keysym::ISO_Level5_Shift
            | xkb::Keysym::Mode_switch
    )
}

fn toggle_lock(enabled_locks: &mut BTreeSet<u64>, logical: u64) {
    let is_lock = LOCK_MODIFIERS.iter().any(|&(_, _, l)| l == logical);
    if is_lock && !enabled_locks.remove(&logical) {
        enabled_locks.insert(logical);
    }
}

fn physical_key(keycode: xkb::Keycode) -> u64 {
    keymap::xkb_keycode_to_physical(keycode.raw()).unwrap_or(GTK_PLANE | u64::from(keycode.raw()))
}

fn logical_key(keysym: xkb::Keysym) -> u64 {
    let keysym = keysym.raw();

    if let Some(logical) = keymap::keysym_to_logical(keysym) {
        return logical;
    }

    // Latin-1 keysyms are the same as their unicode code points.
    if keysym < 256 {
        return to_lower(u64::from(keysym));
    }

    GTK_PLANE | u64::from(keysym)
}

fn to_lower(n: u64) -> u64 {
    match n {
        // A-Z
        0x41..=0x5a => n - 0x41 + 0x61,
        // À-Þ, excluding ×
        0xc0..=0xde if n != 0xd7 => n - 0xc0 + 0xe0,
        _ => n,
    }
}

fn key_character(state: &xkb::State, keycode: xkb::Keycode) -> Option<SmolStr> {
    let text = state.key_get_utf8(keycode);
    if text.is_empty() || text.chars().any(char::is_control) {
        None
    } else {
        Some(SmolStr::new(text))
    }
}

fn gdk_modifiers(state: &xkb::State) -> u32 {
    let mods = [
        (xkb::MOD_NAME_SHIFT, GDK_SHIFT_MASK),
        (xkb::MOD_NAME_CAPS, GDK_LOCK_MASK),
        (xkb::MOD_NAME_CTRL, GDK_CONTROL_MASK),
        (xkb::MOD_NAME_ALT, GDK_MOD1_MASK),
        (xkb::MOD_NAME_NUM, GDK_MOD2_MASK),
        (xkb::MOD_NAME_LOGO, GDK_SUPER_MASK),
    ];

    mods.into_iter()
        .filter(|(name, _)| state.mod_name_is_active(name, xkb::STATE_MODS_EFFECTIVE))
        .fold(0, |acc, (_, mask)| acc | mask)
}

/// TODO: I eagerly await being able to remove this
/// https://github.com/flutter/flutter/issues/136419
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RawKeyMessage {
    keymap: &'static str,
    toolkit: &'static str,
    #[serde(rename = "type")]
    event_type: &'static str,
    scan_code: u32,
    key_code: u32,
    modifiers: u32,
    unicode_scalar_values: u32,
}

#[derive(Deserialize)]
struct RawKeyResponse {
    handled: bool,
}

fn send_key_event(
    engine: &FlutterEngine,
    event: KeyEvent,
    raw_message: Option<RawKeyMessage>,
    on_response: impl FnOnce(bool) + 'static,
) {
    let responses = KeyResponses::new(on_response);

    let response_handle = responses.handle();
    if let Err(e) = engine.send_key_event(&event, move |handled| response_handle.respond(handled)) {
        error!("{e:?}");
    }

    let Some(raw_message) = raw_message else {
        return;
    };

    let message = match serde_json::to_vec(&raw_message) {
        Ok(message) => message,
        Err(e) => {
            error!("failed to serialize key message: {e:?}");
            return;
        }
    };

    let response = responses.handle();
    let result = engine.messenger().send_platform_message_with_reply(
        c"flutter/keyevent",
        &message,
        move |reply| match serde_json::from_slice::<RawKeyResponse>(reply) {
            Ok(reply) => response.respond(reply.handled),
            Err(e) => error!("invalid response from flutter/keyevent: {e:?}"),
        },
    );

    if let Err(e) = result {
        error!("{e:?}");
    }

    // This annoying thing is necessary because we need a callback after both the embedder API and the
    // keyevent channel have sent a response, to forward unhandled events to the text input plugin.
    struct KeyResponses<F: FnOnce(bool)> {
        handled: Cell<bool>,
        on_unhandled: ManuallyDrop<F>,
    }

    impl<F: FnOnce(bool)> KeyResponses<F> {
        fn new(on_unhandled: F) -> Rc<KeyResponses<F>> {
            Rc::new(KeyResponses {
                handled: Cell::new(false),
                on_unhandled: ManuallyDrop::new(on_unhandled),
            })
        }

        fn handle(self: &Rc<Self>) -> KeyResponseHandle<F> {
            KeyResponseHandle(self.clone())
        }
    }

    impl<F: FnOnce(bool)> Drop for KeyResponses<F> {
        fn drop(&mut self) {
            // SAFETY: on_unhandled is not used after this
            unsafe { ManuallyDrop::take(&mut self.on_unhandled)(self.handled.get()) };
        }
    }

    struct KeyResponseHandle<F: FnOnce(bool)>(Rc<KeyResponses<F>>);

    impl<F: FnOnce(bool)> KeyResponseHandle<F> {
        fn respond(self, handled: bool) {
            if handled {
                self.0.handled.set(true);
            }
        }
    }
}

pub struct KeyboardChannelHandler {
    pressed: Rc<RefCell<BTreeMap<u64, u64>>>,
}

impl StandardMethodHandler for KeyboardChannelHandler {
    fn handle(&self, method: &str, _args: EncodableValue, reply: StandardMethodReply) {
        match method {
            "getKeyboardState" => {
                let pressed = self
                    .pressed
                    .borrow()
                    .iter()
                    .map(|(&physical, &logical)| {
                        (
                            EncodableValue::I64(physical as i64),
                            EncodableValue::I64(logical as i64),
                        )
                    })
                    .collect();

                reply.success(&EncodableValue::Map(pressed));
            }
            _ => reply.not_implemented(),
        }
    }
}
