use std::{ptr, sync::Arc};

use flutter_embedder::{
    FlutterEngineSendPlatformMessageResponse, FlutterPlatformMessageResponseHandle,
};
use parking_lot::Mutex;

pub trait BinaryMessageHandler {
    fn handle(&self, message: &[u8], reply: BinaryMessageReply);
}

pub struct BinaryMessageReply {
    engine: flutter_embedder::FlutterEngine,
    engine_is_running: Arc<Mutex<bool>>,
    response_handle: *const FlutterPlatformMessageResponseHandle,
}

impl BinaryMessageReply {
    pub(crate) fn new(
        engine: flutter_embedder::FlutterEngine,
        engine_is_running: Arc<Mutex<bool>>,
        response_handle: *const FlutterPlatformMessageResponseHandle,
    ) -> BinaryMessageReply {
        BinaryMessageReply {
            engine,
            engine_is_running,
            response_handle,
        }
    }

    pub fn send(self, message: &[u8]) {
        self.send_raw(message.as_ptr(), message.len());
    }

    pub fn not_implemented(self) {
        self.send_raw(ptr::null(), 0);
    }

    fn send_raw(self, data: *const u8, length: usize) {
        if *self.engine_is_running.lock() {
            unsafe {
                FlutterEngineSendPlatformMessageResponse(
                    self.engine,
                    self.response_handle,
                    data,
                    length,
                );
            }
        } else {
            tracing::error!("send called on a stopped engine");
        }
    }

    pub(crate) fn into_raw(self) -> *const FlutterPlatformMessageResponseHandle {
        self.response_handle
    }
}
