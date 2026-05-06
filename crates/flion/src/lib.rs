#![feature(default_field_values, let_chains)]

mod engine;
mod error_utils;
mod input;
mod keyboard;
mod keymap;
mod platform;
#[cfg(windows)]
mod plugins_shim;
mod text_input;

pub mod codec;
pub mod standard_method_channel;

use std::collections::HashMap;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::env;

use engine::FlutterEngine;
#[cfg(windows)]
use platform::windows::platform_views::PlatformViewFactory;

pub use engine::{BinaryMessageHandler, BinaryMessageReply, BinaryMessenger};
#[cfg(windows)]
pub use platform::windows::{CompositorContext, PlatformView, PlatformViewUpdateArgs};

#[doc(hidden)]
pub use ::linkme;

#[doc(hidden)]
#[linkme::distributed_slice]
pub static PLUGINS: [unsafe extern "C" fn(*mut c_void)];

#[macro_export]
macro_rules! include_plugins {
    () => {
        include!(concat!(env!("OUT_DIR"), "/plugin_registrant.rs"));
    };
}

pub struct FlionAppBuilder<'a> {
    bundle_path: PathBuf,
    platform_message_handlers: Vec<(&'a str, Box<dyn BinaryMessageHandler>)>,
    #[cfg(windows)]
    platform_view_factories: HashMap<String, Box<dyn PlatformViewFactory>>,
}

impl<'a> FlionAppBuilder<'a> {
    fn new() -> FlionAppBuilder<'a> {
        let bundle_path = if let Ok(exe) = env::current_exe()
            && let Some(dir) = exe.parent()
        {
            dir.join("data")
        } else {
            PathBuf::from("data")
        };

        FlionAppBuilder {
            bundle_path,
            platform_message_handlers: vec![],
            #[cfg(windows)]
            platform_view_factories: HashMap::new(),
        }
    }

    pub fn with_bundle_path(mut self, path: &'a Path) -> Self {
        self.bundle_path = path.to_owned();
        self
    }

    pub fn with_platform_message_handler(
        mut self,
        name: &'a str,
        handler: Box<dyn BinaryMessageHandler>,
    ) -> Self {
        self.platform_message_handlers.push((name, handler));
        self
    }

    #[cfg(windows)]
    pub fn with_platform_view_factory(
        mut self,
        name: &'a str,
        factory: impl PlatformViewFactory + 'static,
    ) -> Self {
        self.platform_view_factories
            .insert(name.to_owned(), Box::new(factory));
        self
    }

    pub fn build(self) -> eyre::Result<FlionApp> {
        #[cfg(windows)]
        {
            let (engine, platform) = platform::windows::build(
                self.bundle_path,
                self.platform_message_handlers,
                self.platform_view_factories,
            )?;
            return Ok(FlionApp { engine, platform });
        }

        #[allow(unreachable_code)]
        Err(eyre::eyre!("unsupported platform"))
    }
}

pub struct FlionApp {
    engine: Rc<FlutterEngine>,
    #[cfg(windows)]
    platform: platform::windows::FlionWindowsState,
}

impl FlionApp {
    pub fn builder<'a>() -> FlionAppBuilder<'a> {
        FlionAppBuilder::new()
    }

    pub fn messenger(&self) -> BinaryMessenger {
        self.engine.messenger()
    }

    pub fn set_platform_message_handler(
        &self,
        name: impl Into<String>,
        handler: impl BinaryMessageHandler + 'static,
    ) {
        self.engine.set_platform_message_handler(name, handler)
    }

    pub fn run_event_loop(self) -> eyre::Result<()> {
        #[cfg(windows)]
        return platform::windows::run_event_loop(self.engine, self.platform);

        #[allow(unreachable_code)]
        Err(eyre::eyre!("unsupported platform"))
    }
}
