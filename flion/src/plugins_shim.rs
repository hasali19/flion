use std::ffi::{c_char, c_void, CStr};
use std::mem;

use flutter_embedder::FlutterPlatformMessageResponseHandle;

use crate::engine::FlutterEngine;
use crate::{BinaryMessageHandler, BinaryMessageReply};

#[link(name = "flion_plugins_shim.dll")]
unsafe extern "C" {
    fn flion_plugins_shim_set_proc_table(proc_table: &plugins_compat::ProcTable);
}

#[ctor::ctor]
fn init_plugins_shim() {
    unsafe {
        flion_plugins_shim_set_proc_table(&plugins_compat::ProcTable {
            FlutterDesktopPluginRegistrarGetMessenger:
                flutter_desktop_plugin_registrar_get_messenger,
            FlutterDesktopRegistrarGetTextureRegistrar:
                flutter_desktop_plugin_registrar_get_texture_registrar,
            FlutterDesktopPluginRegistrarGetView: flutter_desktop_plugin_registrar_get_view,
            FlutterDesktopPluginRegistrarSetDestructionHandler:
                flutter_desktop_plugin_registrar_set_destruction_handler,
            FlutterDesktopMessengerSetCallback: flutter_desktop_messenger_set_callback,
            FlutterDesktopMessengerAddRef: flutter_desktop_messenger_add_ref,
            FlutterDesktopMessengerRelease: flutter_desktop_messenger_release,
            FlutterDesktopMessengerLock: flutter_desktop_messenger_lock,
            FlutterDesktopMessengerUnlock: flutter_desktop_messenger_unlock,
            FlutterDesktopMessengerIsAvailable: flutter_desktop_messenger_is_available,
            FlutterDesktopMessengerSendResponse: flutter_dsktop_messenger_send_response,
            ..Default::default()
        });
    }
}

unsafe extern "C" fn flutter_desktop_plugin_registrar_get_messenger(
    registrar: *mut c_void,
) -> *mut c_void {
    // `registrar` is a pointer to `FlutterEngine`
    registrar
}

unsafe extern "C" fn flutter_desktop_plugin_registrar_get_texture_registrar(
    _registrar: *mut c_void,
) -> *mut c_void {
    std::ptr::null_mut()
}

unsafe extern "C" fn flutter_desktop_plugin_registrar_get_view(
    _registrar: *mut c_void,
) -> *mut c_void {
    std::ptr::null_mut()
}

unsafe extern "C" fn flutter_desktop_plugin_registrar_set_destruction_handler(
    _registrar: *mut c_void,
    _callback: unsafe extern "C" fn(registrar: *mut c_void),
) {
    // TODO: Register engine shut down callback
}

unsafe extern "C" fn flutter_desktop_messenger_set_callback(
    messenger: *mut c_void,
    channel: *const c_char,
    callback: plugins_compat::FlutterDesktopMessageCallback,
    user_data: *mut c_void,
) {
    let engine = &*messenger.cast::<FlutterEngine>();

    let channel = CStr::from_ptr(channel);
    let channel = channel.to_str().unwrap();

    tracing::debug!("setting callback for platform channel: {channel:?}");

    struct Handler {
        engine: *const FlutterEngine,
        callback: plugins_compat::FlutterDesktopMessageCallback,
        user_data: *mut c_void,
    }

    impl BinaryMessageHandler for Handler {
        fn handle(&self, message: &[u8], reply: BinaryMessageReply) {
            unsafe {
                (self.callback)(
                    self.engine.cast_mut().cast(),
                    &plugins_compat::FlutterDesktopMessage {
                        channel: std::ptr::null(),
                        message: message.as_ptr(),
                        message_size: message.len(),
                        response_handle: reply.into_raw().cast(),
                        struct_size: mem::size_of::<plugins_compat::FlutterDesktopMessage>(),
                    },
                    self.user_data,
                );
            }
        }
    }

    let handler = Handler {
        engine,
        callback,
        user_data,
    };

    engine.set_platform_message_handler(channel, handler);
}

unsafe extern "C" fn flutter_desktop_messenger_add_ref(messenger: *mut c_void) -> *mut c_void {
    messenger
}

unsafe extern "C" fn flutter_desktop_messenger_release(_messenger: *mut c_void) {}

/// When replying to a platform message, a C++ plugin will lock the messenger and then call
/// `FlutterDesktopMessengerIsAvailable` (below) to check if the engine is still running.
/// In this implementation, we don't actually need to lock anything here because our
/// `BinaryMessageReply` is already thread-safe, and should lock internally if necessary.
/// See https://github.com/flutter/flutter/blob/ad3d8f5934f0539651122770f1f68d5bd4cc5f19/engine/src/flutter/shell/platform/common/client_wrapper/core_implementations.cc#L53.
unsafe extern "C" fn flutter_desktop_messenger_lock(messenger: *mut c_void) -> *mut c_void {
    messenger
}

/// See [flutter_desktop_messenger_lock].
unsafe extern "C" fn flutter_desktop_messenger_unlock(_messenger: *mut c_void) {}

unsafe extern "C" fn flutter_desktop_messenger_is_available(_messenger: *mut c_void) -> bool {
    // TODO: This should return false if the engine is shut down
    true
}

unsafe extern "C" fn flutter_dsktop_messenger_send_response(
    messenger: *mut c_void,
    handle: *const c_void,
    data: *const u8,
    data_length: usize,
) {
    let engine = &*messenger.cast::<FlutterEngine>();
    let response_handle = handle.cast::<FlutterPlatformMessageResponseHandle>();

    let reply = BinaryMessageReply::new(engine.as_raw(), response_handle);
    if data.is_null() {
        reply.not_implemented();
    } else {
        reply.send(std::slice::from_raw_parts(data, data_length));
    }
}
