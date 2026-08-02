use std::cell::RefCell;
use std::rc::Rc;

use eyre::Context;
use serde::{Deserialize, Serialize};

use crate::engine::{FlutterEngine, KeyEvent, KeyEventType};
use crate::error_utils::ResultExt;
use crate::text_input::TextInputState;
use crate::{keymap, window};

pub struct Keyboard {
    engine: Rc<FlutterEngine>,
    text_input: Rc<RefCell<TextInputState>>,
}

impl Keyboard {
    pub fn new(engine: Rc<FlutterEngine>, text_input: Rc<RefCell<TextInputState>>) -> Keyboard {
        Keyboard { engine, text_input }
    }

    pub fn handle_event(&self, event: window::KeyEvent) -> eyre::Result<()> {
        let text_input = self.text_input.clone();

        send_embedder_key_event(&self.engine, event, {
            let engine = self.engine.clone();
            move |event| {
                let _ = send_channel_key_event(&engine, event, {
                    let engine = engine.clone();
                    move |event| {
                        let mut text_input = text_input.borrow_mut();
                        let _ = text_input
                            .process_key_event(&event, &engine)
                            .wrap_err("text input plugin failed to process key event")
                            .trace_err();
                    }
                })
                .trace_err();
            }
        })?;

        Ok(())
    }
}

fn send_embedder_key_event(
    engine: &FlutterEngine,
    event: window::KeyEvent,
    next_handler: impl FnOnce(window::KeyEvent) + 'static,
) -> eyre::Result<()> {
    let key_event = KeyEvent {
        event_type: match event.action {
            window::KeyAction::Up => KeyEventType::Up,
            window::KeyAction::Down => KeyEventType::Down,
            window::KeyAction::Repeat => KeyEventType::Repeat,
        },
        synthesized: false,
        character: event.character.clone(),
        logical: event
            .logical
            .map(|k| keymap::map_windows_to_logical(k as u32).unwrap_or(k)),
        physical: event.physical,
    };

    engine.send_key_event(&key_event, move |handled| {
        if !handled {
            next_handler(event);
        }
    })
}

fn send_channel_key_event(
    engine: &FlutterEngine,
    event: window::KeyEvent,
    next_handler: impl FnOnce(window::KeyEvent) + 'static,
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

    let character = match event.character.as_deref() {
        Some(c) => {
            if (1..=4).contains(&c.len()) {
                let mut bytes = [0u8; 4];
                bytes[..c.len()].copy_from_slice(c.as_bytes());
                Some(u32::from_le_bytes(bytes) as u64)
            } else {
                None
            }
        }
        None => None,
    };

    let message = Message {
        keymap: "windows",
        event_type: match event.action {
            window::KeyAction::Up => "keyup",
            window::KeyAction::Down => "keydown",
            window::KeyAction::Repeat => "keydown",
        },
        character_code_point: character,
        key_code: event.logical,
        scan_code: event.physical,
        modifiers: event.modifiers.bits(),
    };

    engine.messenger().send_platform_message_with_reply(
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
