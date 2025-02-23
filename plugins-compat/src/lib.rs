#![feature(sync_unsafe_cell)]

use std::ffi::{c_char, c_void};

#[unsafe(no_mangle)]
extern "C" fn FlutterDesktopPluginRegistrarGetView(registrar: *mut c_void) -> *mut c_void {
    todo!()
}

#[unsafe(no_mangle)]
extern "C" fn FlutterDesktopPluginRegistrarGetMessenger(registrar: *mut c_void) -> *mut c_void {
    todo!()
}

#[unsafe(no_mangle)]
extern "C" fn FlutterDesktopRegistrarGetTextureRegistrar(registrar: *mut c_void) -> *mut c_void {
    todo!()
}

#[unsafe(no_mangle)]
extern "C" fn FlutterDesktopMessengerSend(
    messenger: *mut c_void,
    channel: *const c_char,
    message: *const u8,
    message_size: usize,
) -> bool {
    todo!()
}

#[unsafe(no_mangle)]
extern "C" fn FlutterDesktopMessengerSendWithReply(
    messenger: *mut c_void,
    channel: *const c_char,
    message: *const u8,
    message_size: usize,
    reply: unsafe extern "C" fn(*const u8, usize, *mut c_void),
    user_data: *mut c_void,
) -> bool {
    todo!()
}

#[unsafe(no_mangle)]
extern "C" fn FlutterDesktopMessengerSendResponse(
    messenger: *mut c_void,
    handle: *const c_void,
    data: *const u8,
    data_length: usize,
) {
    todo!()
}

#[unsafe(no_mangle)]
extern "C" fn FlutterDesktopMessengerSetCallback(
    messenger: *mut c_void,
    channel: *const c_char,
    callback: *mut c_void,
    user_data: *mut c_void,
) {
}

#[unsafe(no_mangle)]
extern "C" fn FlutterDesktopMessengerAddRef(messenger: *mut c_void) {
    todo!()
}

#[unsafe(no_mangle)]
extern "C" fn FlutterDesktopMessengerRelease(messenger: *mut c_void) {
    todo!()
}

#[unsafe(no_mangle)]
extern "C" fn FlutterDesktopMessengerIsAvailable(messenger: *mut c_void) {
    todo!()
}

#[unsafe(no_mangle)]
extern "C" fn FlutterDesktopMessengerLock(messenger: *mut c_void) {
    todo!()
}

#[unsafe(no_mangle)]
extern "C" fn FlutterDesktopMessengerUnlock(messenger: *mut c_void) {
    todo!()
}

#[unsafe(no_mangle)]
extern "C" fn FlutterDesktopTextureRegistrarRegisterExternalTexture() {
    todo!()
}

#[unsafe(no_mangle)]
extern "C" fn FlutterDesktopTextureRegistrarUnregisterExternalTexture() {
    todo!()
}

#[unsafe(no_mangle)]
extern "C" fn FlutterDesktopTextureRegistrarMarkExternalTextureFrameAvailable() {
    todo!()
}

#[unsafe(no_mangle)]
extern "C" fn FlutterDesktopPluginRegistrarSetDestructionHandler(
    registrar: *mut c_void,
    callback: *mut c_void,
) {
    todo!()
}

#[unsafe(no_mangle)]
extern "C" fn FlutterDesktopViewGetHWND(view: *mut c_void) -> *mut c_void {
    todo!()
}
