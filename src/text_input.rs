use std::cell::RefCell;
use std::rc::Rc;

use color_eyre::eyre;
use serde::{Deserialize, Serialize};
use serde_json::json;
use winit::event::KeyEvent;
use winit::keyboard::{Key, NamedKey};

use crate::engine::{BinaryMessageHandler, BinaryMessageReply, FlutterEngine};

pub struct TextInputState {
    client: Option<u32>,
    value: TextEditingValue,
}

impl TextInputState {
    pub fn new() -> TextInputState {
        TextInputState {
            client: None,
            value: TextEditingValue::default(),
        }
    }

    pub fn process_key_event(
        &mut self,
        event: &KeyEvent,
        engine: &FlutterEngine,
    ) -> eyre::Result<()> {
        if event.state.is_pressed() {
            match &event.logical_key {
                Key::Named(NamedKey::Space) => self.insert_text(" "),
                Key::Named(NamedKey::Enter) => {
                    // TODO: Handle enter key
                    return Ok(());
                }
                Key::Character(c) => self.insert_text(c.as_str()),
                // Key::Unidentified(_) => todo!(),
                // Key::Dead(_) => todo!(),
                _ => return Ok(()),
            }
        }

        if let Some(client) = self.client {
            let message = json!({
                "method": "TextInputClient.updateEditingState",
                "args": [
                    client,
                    &self.value,
                ],
            });

            let message = serde_json::to_vec(&message).unwrap();

            engine
                .send_platform_message(c"flutter/textinput", &message)
                .unwrap();
        }

        Ok(())
    }

    fn insert_text(&mut self, text: &str) {
        self.delete_selected();
        self.value.text.insert_str(self.value.selection_base, text);
        self.value.selection_base += text.len();
        self.value.selection_extent = self.value.selection_base;
    }

    fn delete_selected(&mut self) {
        let range = if self.value.selection_base < self.value.selection_extent {
            self.value.selection_base..self.value.selection_extent
        } else {
            self.value.selection_extent..self.value.selection_base
        };

        if range.is_empty() {
            return;
        }

        self.value.text.drain(range.clone());

        self.value.selection_base = range.start;
        self.value.selection_extent = range.end;
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "method", content = "args")]
enum TextInputRequest {
    #[serde(rename = "TextInput.setClient")]
    SetClient(u32, #[allow(unused)] serde_json::Value),
    #[serde(rename = "TextInput.clearClient")]
    ClearClient,
    #[serde(rename = "TextInput.show")]
    Show,
    #[serde(rename = "TextInput.hide")]
    Hide,
    #[serde(rename = "TextInput.setEditingState")]
    SetEditingState(TextEditingValue),
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextEditingValue {
    text: String,
    selection_base: usize,
    selection_extent: usize,
    selection_affinity: String,
    selection_is_directional: bool,
    composing_base: i32,
    composing_extent: i32,
}

pub struct TextInputHandler {
    state: Rc<RefCell<TextInputState>>,
}

impl TextInputHandler {
    pub fn new(state: Rc<RefCell<TextInputState>>) -> TextInputHandler {
        TextInputHandler { state }
    }
}

impl BinaryMessageHandler for TextInputHandler {
    fn handle(&self, message: &[u8], reply: BinaryMessageReply) {
        let Ok(req) = serde_json::from_slice::<TextInputRequest>(message) else {
            let message = std::str::from_utf8(message).unwrap();
            tracing::warn!("unimplemented: {message}");
            reply.not_implemented();
            return;
        };

        tracing::debug!("{req:?}");

        const RES_SUCCESS: &[u8] = c"[null]".to_bytes();

        match req {
            TextInputRequest::SetClient(client, _) => {
                self.state.borrow_mut().client = Some(client);
                reply.send(RES_SUCCESS);
            }
            TextInputRequest::ClearClient => {
                self.state.borrow_mut().client = None;
                reply.send(RES_SUCCESS);
            }
            TextInputRequest::Show => {
                reply.not_implemented();
            }
            TextInputRequest::Hide => {
                reply.not_implemented();
            }
            TextInputRequest::SetEditingState(value) => {
                self.state.borrow_mut().value = value;
                reply.send(RES_SUCCESS);
            }
        }
    }
}
