use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use smol_str::SmolStr;

use crate::engine::{BinaryMessageHandler, BinaryMessageReply, BinaryMessenger, KeyEventType};

const LOGICAL_ENTER: u64 = 0x0010000000d;
const LOGICAL_NUMPAD_ENTER: u64 = 0x0020000020d;

pub struct TextInputKeyEvent {
    pub event_type: KeyEventType,
    pub logical: u64,
    pub character: Option<SmolStr>,
}

#[derive(Default)]
pub struct TextInputState {
    client: Option<Client>,
}

struct Client {
    id: i64,
    config: ClientConfig,
    value: TextEditingValue,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClientConfig {
    #[serde(default)]
    input_type: InputType,
    #[serde(default)]
    input_action: String,
}

#[derive(Default, Deserialize)]
struct InputType {
    #[serde(default)]
    name: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TextEditingValue {
    text: String,
    selection_base: i64,
    selection_extent: i64,
    #[serde(default = "default_affinity")]
    selection_affinity: String,
    #[serde(default)]
    selection_is_directional: bool,
    #[serde(default = "no_range")]
    composing_base: i64,
    #[serde(default = "no_range")]
    composing_extent: i64,
}

fn default_affinity() -> String {
    "TextAffinity.downstream".to_owned()
}

fn no_range() -> i64 {
    -1
}

impl Default for TextEditingValue {
    fn default() -> Self {
        TextEditingValue {
            text: String::new(),
            selection_base: -1,
            selection_extent: -1,
            selection_affinity: default_affinity(),
            selection_is_directional: false,
            composing_base: -1,
            composing_extent: -1,
        }
    }
}

impl TextInputState {
    pub fn handle_event(&mut self, messenger: &BinaryMessenger, event: &TextInputKeyEvent) {
        let Some(client) = &mut self.client else {
            return;
        };

        if event.event_type == KeyEventType::Up {
            return;
        }

        if event.logical == LOGICAL_ENTER || event.logical == LOGICAL_NUMPAD_ENTER {
            if client.config.input_type.name == "TextInputType.multiline"
                && client.config.input_action == "TextInputAction.newline"
            {
                client.value.insert_text("\n");
                send_editing_state(messenger, client);
            }

            send_client_message(
                messenger,
                "TextInputClient.performAction",
                json!([client.id, client.config.input_action]),
            );

            return;
        }

        if let Some(text) = &event.character {
            client.value.insert_text(text);
            send_editing_state(messenger, client);
        }
    }

    fn handle_method_call(&mut self, method: &str, args: Value) -> Result<(), MethodError> {
        match method {
            "TextInput.setClient" => {
                let (id, config): (i64, ClientConfig) = serde_json::from_value(args)?;
                self.client = Some(Client {
                    id,
                    config,
                    value: TextEditingValue::default(),
                });
            }
            "TextInput.clearClient" => {
                self.client = None;
            }
            "TextInput.updateConfig" => {
                if let Some(client) = &mut self.client {
                    client.config = serde_json::from_value(args)?;
                }
            }
            "TextInput.setEditingState" => {
                if let Some(client) = &mut self.client {
                    client.value = serde_json::from_value(args)?;
                }
            }
            // Only relevant for on-screen keyboards, IMEs or platform UI.
            // TODO: Do we want any of this?
            "TextInput.show"
            | "TextInput.hide"
            | "TextInput.setEditableSizeAndTransform"
            | "TextInput.setMarkedTextRect"
            | "TextInput.setCaretRect"
            | "TextInput.setSelectionRects"
            | "TextInput.setStyle"
            | "TextInput.requestAutofill"
            | "TextInput.finishAutofillContext" => {}
            _ => return Err(MethodError::NotImplemented),
        }

        Ok(())
    }
}

impl TextEditingValue {
    /// Replaces the selection with `text`, and places the cursor after it.
    fn insert_text(&mut self, text: &str) {
        let len = utf16_len(&self.text) as i64;

        // With no valid selection, insert at the end.
        let (base, extent) = if self.selection_base < 0 || self.selection_extent < 0 {
            (len, len)
        } else {
            (self.selection_base.min(len), self.selection_extent.min(len))
        };

        let start = base.min(extent) as usize;
        let end = base.max(extent) as usize;

        let range = byte_range(&self.text, start..end);
        self.text.replace_range(range, text);

        let cursor = (start + utf16_len(text)) as i64;
        self.selection_base = cursor;
        self.selection_extent = cursor;
        self.selection_affinity = default_affinity();
        self.selection_is_directional = false;
        self.composing_base = -1;
        self.composing_extent = -1;
    }
}

fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// Converts a range of UTF-16 offsets into a range of byte offsets. Offsets that fall inside a
/// surrogate pair are rounded up to the next character.
fn byte_range(s: &str, range: Range<usize>) -> Range<usize> {
    let to_byte = |offset: usize| {
        let mut utf16 = 0;
        for (byte, c) in s.char_indices() {
            if utf16 >= offset {
                return byte;
            }
            utf16 += c.len_utf16();
        }
        s.len()
    };

    to_byte(range.start)..to_byte(range.end)
}

fn send_editing_state(messenger: &BinaryMessenger, client: &Client) {
    send_client_message(
        messenger,
        "TextInputClient.updateEditingState",
        json!([client.id, client.value]),
    );
}

fn send_client_message(messenger: &BinaryMessenger, method: &str, args: Value) {
    let message = json!({ "method": method, "args": args });
    let message = serde_json::to_vec(&message).unwrap();

    if let Err(e) = messenger.send_platform_message(c"flutter/textinput", &message) {
        tracing::error!("{e:?}");
    }
}

enum MethodError {
    NotImplemented,
    InvalidArgs(serde_json::Error),
}

impl From<serde_json::Error> for MethodError {
    fn from(e: serde_json::Error) -> Self {
        MethodError::InvalidArgs(e)
    }
}

/// Handler for the `flutter/textinput` channel, which uses the JSON method codec.
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
        #[derive(Deserialize)]
        struct MethodCall {
            method: String,
            #[serde(default)]
            args: Value,
        }

        let call = match serde_json::from_slice::<MethodCall>(message) {
            Ok(call) => call,
            Err(e) => {
                tracing::error!("invalid flutter/textinput message: {e:?}");
                reply.not_implemented();
                return;
            }
        };

        let result = self
            .state
            .borrow_mut()
            .handle_method_call(&call.method, call.args);

        match result {
            // A successful JSON method codec reply is the result wrapped in an array.
            Ok(()) => reply.send(b"[null]"),
            Err(MethodError::NotImplemented) => {
                tracing::warn!(method = call.method, "unimplemented text input method");
                reply.not_implemented();
            }
            Err(MethodError::InvalidArgs(e)) => {
                tracing::error!("invalid arguments for {}: {e:?}", call.method);
                let error = json!(["InvalidArguments", e.to_string(), null]);
                reply.send(&serde_json::to_vec(&error).unwrap());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(text: &str, base: i64, extent: i64) -> TextEditingValue {
        TextEditingValue {
            text: text.to_owned(),
            selection_base: base,
            selection_extent: extent,
            ..Default::default()
        }
    }

    #[test]
    fn insert_at_cursor() {
        let mut v = value("hello", 2, 2);
        v.insert_text("XY");
        assert_eq!(v.text, "heXYllo");
        assert_eq!((v.selection_base, v.selection_extent), (4, 4));
    }

    #[test]
    fn insert_replaces_reversed_selection() {
        let mut v = value("hello", 4, 1);
        v.insert_text("i");
        assert_eq!(v.text, "hio");
        assert_eq!((v.selection_base, v.selection_extent), (2, 2));
    }

    #[test]
    fn insert_without_selection_appends() {
        let mut v = value("ab", -1, -1);
        v.insert_text("c");
        assert_eq!(v.text, "abc");
        assert_eq!((v.selection_base, v.selection_extent), (3, 3));
    }

    #[test]
    fn offsets_are_utf16() {
        // "😀" is 2 UTF-16 code units and 4 bytes.
        let mut v = value("a😀b", 3, 3);
        v.insert_text("é");
        assert_eq!(v.text, "a😀éb");
        assert_eq!((v.selection_base, v.selection_extent), (4, 4));

        let mut v = value("a😀b", 1, 3);
        v.insert_text("😀");
        assert_eq!(v.text, "a😀b");
        assert_eq!((v.selection_base, v.selection_extent), (3, 3));
    }
}
