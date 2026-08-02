mod app;
mod messaging;

pub mod codec;
pub mod standard_method_channel;

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "windows")]
pub use windows::*;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

#[macro_export]
macro_rules! include_plugins {
    () => {
        #[cfg(target_os = "windows")]
        include!(concat!(env!("OUT_DIR"), "/plugin_registrant.rs"));
    };
}

pub use app::{FlionApp, FlionAppBuilder};
