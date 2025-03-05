#![feature(sync_unsafe_cell)]

use std::ffi::{c_char, c_void};

macro_rules! declare_procs {
    ($(fn $name:ident ($($arg:ident : $arg_type:ty),* $(,)?) $(-> $ret:ty)?;)*) => {
        #[allow(non_snake_case)]
        #[derive(Clone)]
        #[repr(C)]
        pub struct ProcTable {
            $(pub $name: unsafe extern "C" fn($($arg : $arg_type),*) $(-> $ret)?),*
        }

        impl ProcTable {
            const DEFAULT: Self = Self {
                $($name : stubs::$name),*
            };
        }

        impl Default for ProcTable {
            fn default() -> Self {
                Self::DEFAULT
            }
        }

        mod stubs {
            use super::*;

            $(
                #[allow(non_snake_case, unused)]
                pub unsafe extern "C" fn $name($($arg : $arg_type),*) $(-> $ret)? {
                    unimplemented!(stringify!($name))
                }
            )*
        }

        $(
            #[cfg(cdylib)]
            #[unsafe(no_mangle)]
            extern "C" fn $name($($arg : $arg_type),*) $(-> $ret)? {
                unsafe {
                    ((*PROC_TABLE.get()).$name)($($arg),*)
                }
            }
        )*
    };
}

#[repr(C)]
pub struct FlutterDesktopMessage {
    pub struct_size: usize,
    pub channel: *const c_char,
    pub message: *const u8,
    pub message_size: usize,
    pub response_handle: *const c_void,
}

pub type FlutterDesktopMessageCallback = unsafe extern "C" fn(
    messenger: *mut c_void,
    message: *const FlutterDesktopMessage,
    user_data: *mut c_void,
);

declare_procs! {
    fn FlutterDesktopPluginRegistrarGetMessenger(registrar: *mut c_void) -> *mut c_void;

    fn FlutterDesktopPluginRegistrarGetView(registrar: *mut c_void) -> *mut c_void;

    fn FlutterDesktopRegistrarGetTextureRegistrar(registrar: *mut c_void) -> *mut c_void;

    fn FlutterDesktopMessengerSend(
        messenger: *mut c_void,
        channel: *const c_char,
        message: *const u8,
        message_size: usize,
    ) -> bool;

    fn FlutterDesktopMessengerSendWithReply(
        messenger: *mut c_void,
        channel: *const c_char,
        message: *const u8,
        message_size: usize,
        reply: unsafe extern "C" fn(*const u8, usize, *mut c_void),
        user_data: *mut c_void,
    ) -> bool;

    fn FlutterDesktopMessengerSendResponse(
        messenger: *mut c_void,
        handle: *const c_void,
        data: *const u8,
        data_length: usize,
    );

    fn FlutterDesktopMessengerSetCallback(
        messenger: *mut c_void,
        channel: *const c_char,
        callback: FlutterDesktopMessageCallback,
        user_data: *mut c_void,
    );

    fn FlutterDesktopMessengerAddRef(messenger: *mut c_void) -> *mut c_void;

    fn FlutterDesktopMessengerRelease(messenger: *mut c_void);

    fn FlutterDesktopMessengerIsAvailable(messenger: *mut c_void) -> bool;

    fn FlutterDesktopMessengerLock(messenger: *mut c_void) -> *mut c_void;

    fn FlutterDesktopMessengerUnlock(messenger: *mut c_void);

    fn FlutterDesktopTextureRegistrarRegisterExternalTexture();

    fn FlutterDesktopTextureRegistrarUnregisterExternalTexture();

    fn FlutterDesktopTextureRegistrarMarkExternalTextureFrameAvailable();

    fn FlutterDesktopPluginRegistrarSetDestructionHandler(
        registrar: *mut c_void,
        callback: unsafe extern "C" fn(registrar: *mut c_void),
    );

    fn FlutterDesktopViewGetHWND(view: *mut c_void) -> *mut c_void;
}

#[cfg(cdylib)]
static PROC_TABLE: std::cell::SyncUnsafeCell<ProcTable> =
    std::cell::SyncUnsafeCell::new(ProcTable::DEFAULT);

#[cfg(cdylib)]
#[unsafe(no_mangle)]
extern "C" fn flion_plugins_shim_set_proc_table(proc_table: &ProcTable) {
    unsafe {
        PROC_TABLE.get().write(proc_table.clone());
    }
}
