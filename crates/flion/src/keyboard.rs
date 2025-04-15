use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use bitflags::bitflags;
use eyre::{bail, Context};
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VIRTUAL_KEY, VK_CONTROL, VK_LCONTROL, VK_LSHIFT, VK_RCONTROL, VK_RSHIFT, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    PeekMessageW, PM_NOREMOVE, WM_CHAR, WM_KEYDOWN, WM_KEYFIRST, WM_KEYLAST, WM_KEYUP,
};

use crate::engine::{FlutterEngine, KeyEvent, KeyEventType};
use crate::error_utils::ResultExt;
use crate::keymap;
use crate::text_input::TextInputState;

#[derive(Clone)]
pub struct SystemKeyEvent {
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
}

impl SystemKeyEvent {
    pub fn code(&self) -> u64 {
        self.wparam.0 as u64
    }

    pub fn scan_code(&self) -> u8 {
        ((self.lparam.0 >> 16) & 0xff) as u8
    }

    pub fn was_down(&self) -> bool {
        self.lparam.0 & (1 << 30) != 0
    }
}

pub struct Keyboard {
    engine: Rc<FlutterEngine>,
    text_input: Rc<RefCell<TextInputState>>,
    modifiers: ModifierState,
    session: VecDeque<SystemKeyEvent>,
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
    pub fn new(engine: Rc<FlutterEngine>, text_input: Rc<RefCell<TextInputState>>) -> Keyboard {
        Keyboard {
            engine,
            text_input,
            modifiers: ModifierState::default(),
            session: VecDeque::new(),
        }
    }

    pub fn handle_message(
        &mut self,
        hwnd: HWND,
        msg: u32,
        wparam: windows::Win32::Foundation::WPARAM,
        lparam: windows::Win32::Foundation::LPARAM,
    ) -> eyre::Result<bool> {
        let event = SystemKeyEvent {
            msg,
            wparam,
            lparam,
        };

        self.session.push_back(event.clone());

        let event = match msg {
            WM_KEYDOWN => {
                let next_msg = unsafe { peek_next_message(hwnd) };
                if let Some(WM_CHAR) = next_msg {
                    return Ok(true);
                }

                let scan_code = event.scan_code();
                let key_code = event.code();

                KeyEvent {
                    event_type: if event.was_down() {
                        KeyEventType::Repeat
                    } else {
                        KeyEventType::Down
                    },
                    synthesized: false,
                    character: None,
                    logical: Some(key_code),
                    physical: Some(scan_code as u64),
                }
            }
            WM_CHAR => {
                let next_msg = unsafe { peek_next_message(hwnd) };
                if let Some(WM_CHAR) = next_msg {
                    return Ok(true);
                }

                let Some(
                    key_down @ SystemKeyEvent {
                        msg: WM_KEYDOWN, ..
                    },
                ) = self.session.pop_front()
                else {
                    bail!("Got char event without a key down")
                };

                let scan_code = key_down.scan_code();
                let key_code = key_down.code();

                let code_points = self.session.iter().map(|e| e.code() as u16);
                let chars = char::decode_utf16(code_points.clone())
                    .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER));
                let text = SmolStr::from_iter(chars);

                KeyEvent {
                    event_type: if key_down.was_down() {
                        KeyEventType::Repeat
                    } else {
                        KeyEventType::Down
                    },
                    synthesized: false,
                    character: Some(text),
                    logical: Some(key_code),
                    physical: Some(scan_code as u64),
                }
            }
            WM_KEYUP => {
                let scan_code = event.scan_code();
                let key_code = event.code();

                KeyEvent {
                    event_type: KeyEventType::Up,
                    synthesized: false,
                    character: None,
                    logical: Some(key_code),
                    physical: Some(scan_code as u64),
                }
            }
            _ => return Ok(false),
        };

        if let Some(logical) = event.logical {
            let vk = VIRTUAL_KEY(logical as u16);

            let is_pressed = event.event_type != KeyEventType::Up;

            let modifier = match vk {
                VK_CONTROL => Some(ModifierState::CONTROL),
                VK_LCONTROL => Some(ModifierState::CONTROL_LEFT),
                VK_RCONTROL => Some(ModifierState::CONTROL_RIGHT),
                VK_SHIFT => Some(ModifierState::SHIFT),
                VK_LSHIFT => Some(ModifierState::SHIFT_LEFT),
                VK_RSHIFT => Some(ModifierState::SHIFT_RIGHT),
                _ => None,
            };

            if let Some(modifier) = modifier {
                self.modifiers.set(modifier, is_pressed);
            }
        }

        let text_input = self.text_input.clone();
        let modifiers = self.modifiers;

        send_embedder_key_event(&self.engine, event, {
            let engine = self.engine.clone();
            move |event| {
                let _ = send_channel_key_event(&engine, event, modifiers, {
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

        self.session.clear();

        Ok(true)
    }
}

unsafe fn peek_next_message(hwnd: HWND) -> Option<u32> {
    let mut msg = Default::default();

    unsafe { PeekMessageW(&mut msg, Some(hwnd), WM_KEYFIRST, WM_KEYLAST, PM_NOREMOVE) }
        .as_bool()
        .then_some(msg.message)
}

fn send_embedder_key_event(
    engine: &FlutterEngine,
    event: KeyEvent,
    next_handler: impl FnOnce(KeyEvent) + 'static,
) -> eyre::Result<()> {
    let mut key_event = event.clone();
    key_event.logical = event
        .logical
        .map(|k| keymap::map_windows_to_logical(k as u32).unwrap_or(k));

    engine.send_key_event(&key_event, move |handled| {
        if !handled {
            next_handler(event);
        }
    })
}

fn send_channel_key_event(
    engine: &FlutterEngine,
    event: crate::engine::KeyEvent,
    modifiers: ModifierState,
    next_handler: impl FnOnce(KeyEvent) + 'static,
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
        event_type: match event.event_type {
            KeyEventType::Down => "keydown",
            KeyEventType::Up => "keyup",
            KeyEventType::Repeat => "keydown",
        },
        character_code_point: character,
        key_code: event.logical,
        scan_code: event.physical,
        modifiers: modifiers.bits(),
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
