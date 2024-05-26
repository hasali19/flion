use std::cell::RefCell;
use std::rc::Rc;

use bitflags::bitflags;
use color_eyre::eyre::{self, Context};
use serde::{Deserialize, Serialize};
use winit::event::{ElementState, Modifiers};
use winit::keyboard::{Key, NamedKey};
use winit::platform::scancode::PhysicalKeyExtScancode;

use crate::engine::{FlutterEngine, KeyEvent, KeyEventType};
use crate::error_utils::ResultExt;
use crate::keymap;
use crate::text_input::TextInputState;

pub struct Keyboard {
    text_input: Rc<RefCell<TextInputState>>,
    modifiers: ModifierState,
}

bitflags! {
    #[derive(Clone, Copy, Default, Debug)]
    struct ModifierState: u32 {
        const SHIFT = 1 << 0;
        const SHIFT_LEFT = 1 << 1;
        const SHIFT_RIGHT = 1 << 2;
        const CONTROL = 1 << 3;
        const CONTROL_LEFT = 1 << 4;
        const CONTROL_RIGHT = 1 << 5;
        const ALT = 1 << 6;
        const ALT_LEFT = 1 << 7;
        const ALT_RIGHT = 1 << 8;
        const WIN_LEFT = 1 << 9;
        const WIN_RIGHT = 1 << 10;
        const CAPS_LOCK = 1 << 11;
        const NUM_LOCK = 1 << 12;
        const SCROLL_LOCK = 1 << 13;
    }
}

impl Keyboard {
    pub fn new(text_input: Rc<RefCell<TextInputState>>) -> Keyboard {
        Keyboard {
            text_input,
            modifiers: ModifierState::default(),
        }
    }

    pub fn handle_keyboard_input(
        &mut self,
        event: winit::event::KeyEvent,
        is_synthetic: bool,
        engine: &FlutterEngine,
    ) -> eyre::Result<()> {
        if let Key::Named(key) = event.logical_key {
            match key {
                NamedKey::CapsLock => {
                    self.modifiers
                        .set(ModifierState::CAPS_LOCK, event.state.is_pressed());
                }
                NamedKey::NumLock => {
                    self.modifiers
                        .set(ModifierState::NUM_LOCK, event.state.is_pressed());
                }
                NamedKey::ScrollLock => {
                    self.modifiers
                        .set(ModifierState::SCROLL_LOCK, event.state.is_pressed());
                }
                _ => {}
            }
        }

        let text_input = &*self.text_input;
        let modifiers = self.modifiers;

        let process_text_input = |event: winit::event::KeyEvent| {
            let mut text_input = text_input.borrow_mut();
            let _ = text_input
                .process_key_event(&event, engine)
                .wrap_err("text input plugin failed to process key event")
                .trace_err();
        };

        let send_channel = move |event: winit::event::KeyEvent| {
            let _ =
                send_channel_key_event(engine, event, modifiers, process_text_input).trace_err();
        };

        let send_embedder = |event: winit::event::KeyEvent| {
            let _ = send_embedder_key_event(engine, event, is_synthetic, send_channel)
                .wrap_err("failed to send embedder key event")
                .trace_err();
        };

        send_embedder(event);

        Ok(())
    }

    pub fn handle_modifiers_changed(&mut self, modifiers: Modifiers) -> eyre::Result<()> {
        self.modifiers
            .set(ModifierState::SHIFT, modifiers.state().shift_key());
        self.modifiers
            .set(ModifierState::CONTROL, modifiers.state().control_key());
        self.modifiers
            .set(ModifierState::ALT, modifiers.state().alt_key());
        Ok(())
    }
}

fn send_embedder_key_event<'e>(
    engine: &'e FlutterEngine,
    event: winit::event::KeyEvent,
    is_synthetic: bool,
    next_handler: impl FnOnce(winit::event::KeyEvent) + 'e,
) -> eyre::Result<()> {
    let character = match &event.logical_key {
        Key::Named(_) => None,
        Key::Character(c) => {
            if event.state == ElementState::Released {
                None
            } else {
                Some(c.clone())
            }
        }
        Key::Unidentified(_) => None,
        Key::Dead(_) => None,
    };

    let key_event = KeyEvent {
        event_type: match event.state {
            ElementState::Pressed if event.repeat => KeyEventType::Repeat,
            ElementState::Pressed => KeyEventType::Down,
            ElementState::Released => KeyEventType::Up,
        },
        synthesized: is_synthetic,
        character: character.as_ref(),
        logical: keymap::to_flutter(&event.logical_key),
        physical: event.physical_key.to_scancode().map(|code| code.into()),
    };

    engine.send_key_event(key_event, move |handled| {
        if !handled {
            next_handler(event);
        }
    })
}

fn send_channel_key_event<'e>(
    engine: &'e FlutterEngine,
    event: winit::event::KeyEvent,
    modifiers: ModifierState,
    next_handler: impl FnOnce(winit::event::KeyEvent) + 'e,
) -> eyre::Result<()> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Message<'a> {
        keymap: &'a str,
        #[serde(rename = "type")]
        event_type: &'a str,
        character_code_point: Option<u64>,
        key_code: Option<u64>,
        scan_code: Option<u64>,
        modifiers: u32,
    }

    #[derive(Debug, Deserialize)]
    struct Response {
        handled: bool,
    }

    let character = match &event.logical_key {
        Key::Character(c) => {
            if (1..=4).contains(&c.len()) {
                let mut bytes = [0u8; 4];
                bytes[..c.len()].copy_from_slice(c.as_bytes());
                Some(u32::from_le_bytes(bytes) as u64)
            } else {
                None
            }
        }
        _ => None,
    };

    let message = Message {
        keymap: "windows",
        event_type: match event.state {
            ElementState::Pressed => "keydown",
            ElementState::Released => "keyup",
        },
        character_code_point: character,
        key_code: keymap::to_flutter(&event.logical_key),
        scan_code: event.physical_key.to_scancode().map(|code| code.into()),
        modifiers: modifiers.bits(),
    };

    engine.send_platform_message_with_reply(
        c"flutter/keyevent",
        &serde_json::to_vec(&message)?,
        |response| {
            let Ok(response) = serde_json::from_slice::<Response>(response)
                .wrap_err("invalid response from flutter/keyevent")
                .trace_err()
            else {
                return;
            };

            if response.handled {
                return;
            }

            next_handler(event);
        },
    )
}
