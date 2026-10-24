use std::cell::RefCell;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::AsFd;
use std::rc::Rc;
use std::sync::Mutex;

use calloop::generic::Generic;
use calloop::{Interest, LoopHandle, Mode, PostAction};
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::warn;
use wayland_client::protocol::wl_data_device::{self, WlDataDevice};
use wayland_client::protocol::wl_data_device_manager::WlDataDeviceManager;
use wayland_client::protocol::wl_data_offer::{self, WlDataOffer};
use wayland_client::protocol::wl_data_source::{self, WlDataSource};
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Proxy, QueueHandle};

use crate::AppState;
use crate::engine::{BinaryMessageHandler, BinaryMessageReply};

const TEXT_MIME_TYPES: [&str; 5] = [
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
    "TEXT",
    "STRING",
];

pub struct Clipboard {
    manager: Option<WlDataDeviceManager>,
    data_device: Option<WlDataDevice>,
    qh: QueueHandle<AppState>,
    loop_handle: LoopHandle<'static, AppState>,
    selection_offer: Option<WlDataOffer>,
    dnd_offer: Option<WlDataOffer>,
    app_selection: Option<(WlDataSource, String)>,
    last_input_serial: Option<u32>,
}

#[derive(Default)]
pub struct OfferData {
    mime_types: Mutex<Vec<String>>,
}

impl OfferData {
    pub fn handle_event(&self, event: wl_data_offer::Event) {
        if let wl_data_offer::Event::Offer { mime_type } = event {
            self.mime_types.lock().unwrap().push(mime_type);
        }
    }
}

impl Clipboard {
    pub fn new(
        manager: Option<WlDataDeviceManager>,
        seat: &WlSeat,
        qh: QueueHandle<AppState>,
        loop_handle: LoopHandle<'static, AppState>,
    ) -> Clipboard {
        let data_device = manager
            .as_ref()
            .map(|manager| manager.get_data_device(seat, &qh, ()));

        Clipboard {
            manager,
            data_device,
            qh,
            loop_handle,
            selection_offer: None,
            dnd_offer: None,
            app_selection: None,
            last_input_serial: None,
        }
    }

    pub fn set_input_serial(&mut self, serial: u32) {
        self.last_input_serial = Some(serial);
    }

    pub fn handle_data_device_event(&mut self, event: wl_data_device::Event) {
        match event {
            wl_data_device::Event::Selection { id } => {
                if let Some(old) = std::mem::replace(&mut self.selection_offer, id) {
                    old.destroy();
                }
            }
            wl_data_device::Event::Enter { id, .. } => {
                if let Some(old) = std::mem::replace(&mut self.dnd_offer, id) {
                    old.destroy();
                }
            }
            wl_data_device::Event::Leave | wl_data_device::Event::Drop => {
                if let Some(offer) = self.dnd_offer.take() {
                    offer.destroy();
                }
            }
            _ => {}
        }
    }

    pub fn handle_data_source_event(
        &mut self,
        source: &WlDataSource,
        event: wl_data_source::Event,
    ) {
        let is_current = matches!(&self.app_selection, Some((s, _)) if s == source);

        match event {
            wl_data_source::Event::Send { fd, .. } => {
                let text = match &self.app_selection {
                    Some((_, text)) if is_current => text.clone(),
                    _ => return,
                };

                // TODO: Avoid new thread?
                std::thread::spawn(move || {
                    if let Err(e) = std::fs::File::from(fd).write_all(text.as_bytes()) {
                        warn!("failed to write clipboard: {e:?}");
                    }
                });
            }
            wl_data_source::Event::Cancelled => {
                if is_current {
                    self.app_selection = None;
                }
                source.destroy();
            }
            _ => {}
        }
    }

    pub fn destroy(&mut self) {
        for offer in [self.selection_offer.take(), self.dnd_offer.take()]
            .into_iter()
            .flatten()
        {
            offer.destroy();
        }

        if let Some((source, _)) = self.app_selection.take() {
            source.destroy();
        }

        // `release` was only added in version 2.
        if let Some(device) = self.data_device.take()
            && device.version() >= 2
        {
            device.release();
        }

        self.manager = None;
    }

    fn set_text(&mut self, text: String) -> Result<(), &'static str> {
        let (Some(manager), Some(device)) = (&self.manager, &self.data_device) else {
            return Err("clipboard is not supported by the compositor");
        };

        let Some(serial) = self.last_input_serial else {
            return Err("no input event to set the selection with");
        };

        let source = manager.create_data_source(&self.qh, ());
        for mime_type in TEXT_MIME_TYPES {
            source.offer(mime_type.to_owned());
        }

        device.set_selection(Some(&source), serial);

        if let Some((old, _)) = self.app_selection.replace((source, text)) {
            old.destroy();
        }

        Ok(())
    }

    fn selection_text_mime_type(&self) -> Option<&'static str> {
        let offer = self.selection_offer.as_ref()?;
        let data = offer.data::<OfferData>()?;
        let mime_types = data.mime_types.lock().unwrap();

        TEXT_MIME_TYPES
            .into_iter()
            .find(|t| mime_types.iter().any(|m| m == t))
    }

    fn has_text(&self) -> bool {
        self.app_selection.is_some() || self.selection_text_mime_type().is_some()
    }

    fn read_text_from_clipboard(&self, callback: impl FnOnce(Option<String>) + 'static) {
        // If we own the selection, reading it would mean waiting on ourselves.
        if let Some((_, text)) = self.app_selection.as_ref() {
            callback(Some(text.to_owned()));
            return;
        }

        let (Some(offer), Some(mime_type)) =
            (&self.selection_offer, self.selection_text_mime_type())
        else {
            callback(None);
            return;
        };

        let (reader, writer) = match std::io::pipe() {
            Ok(pipe) => pipe,
            Err(e) => {
                tracing::error!("failed to create pipe: {e:?}");
                callback(None);
                return;
            }
        };

        offer.receive(mime_type.to_owned(), writer.as_fd());

        // The request holds its own copy of the fd. Close ours, so the read sees EOF once the
        // other client has finished writing and closed theirs.
        drop(writer);

        let mut callback = Some(callback);
        let mut data = vec![];

        let result = self.loop_handle.insert_source(
            Generic::new(reader, Interest::READ, Mode::Level),
            move |_, reader, _| {
                let mut buf = [0u8; 4096];

                let result = reader.as_ref().read(&mut buf);

                let text = match result {
                    Ok(0) => Some(String::from_utf8_lossy(&data).into_owned()),
                    Ok(n) => {
                        data.extend_from_slice(&buf[..n]);
                        return Ok(PostAction::Continue);
                    }
                    Err(e) if e.kind() == ErrorKind::Interrupted => {
                        return Ok(PostAction::Continue);
                    }
                    Err(e) => {
                        tracing::error!("failed to read clipboard: {e:?}");
                        None
                    }
                };

                if let Some(callback) = callback.take() {
                    callback(text);
                }

                Ok(PostAction::Remove)
            },
        );

        if let Err(e) = result {
            tracing::error!("failed to register clipboard read: {e:?}");
        }
    }
}

pub struct PlatformHandler {
    clipboard: Rc<RefCell<Clipboard>>,
}

impl PlatformHandler {
    pub fn new(clipboard: Rc<RefCell<Clipboard>>) -> PlatformHandler {
        PlatformHandler { clipboard }
    }
}

impl BinaryMessageHandler for PlatformHandler {
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
                tracing::error!("invalid flutter/platform message: {e:?}");
                reply.not_implemented();
                return;
            }
        };

        match call.method.as_str() {
            "Clipboard.setData" => {
                let text = call.args["text"].as_str().unwrap_or_default().to_owned();
                match self.clipboard.borrow_mut().set_text(text) {
                    Ok(()) => send_success(reply, Value::Null),
                    Err(message) => send_error(reply, "ClipboardError", message),
                }
            }
            "Clipboard.getData" => {
                let format = call.args.as_str();

                if format != Some("text/plain") {
                    send_success(reply, Value::Null);
                    return;
                }

                self.clipboard
                    .borrow()
                    .read_text_from_clipboard(move |text| {
                        let result = match text {
                            Some(text) => json!({ "text": text }),
                            None => Value::Null,
                        };
                        send_success(reply, result);
                    });
            }
            "Clipboard.hasStrings" => {
                let has_text = self.clipboard.borrow().has_text();
                send_success(reply, json!({ "value": has_text }));
            }
            _ => {
                warn!(method = call.method, "unimplemented platform method");
                reply.not_implemented();
            }
        }
    }
}

fn send_success(reply: BinaryMessageReply, result: Value) {
    reply.send(&serde_json::to_vec(&json!([result])).unwrap());
}

fn send_error(reply: BinaryMessageReply, code: &str, message: &str) {
    reply.send(&serde_json::to_vec(&json!([code, message, null])).unwrap());
}
