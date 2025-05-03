#![feature(default_field_values, let_chains)]

mod compositor;
mod egl;
mod engine;
mod error_utils;
mod keyboard;
mod keymap;
mod mouse_cursor;
mod platform_views;
mod plugins_shim;
mod resize_controller;
mod settings;
mod task_runner;
mod text_input;
mod window;

pub mod codec;
pub mod standard_method_channel;

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::{env, mem};

use engine::{PointerButtons, PointerDeviceKind, PointerEvent};
use eyre::OptionExt;
use platform_views::{PlatformViewFactory, PlatformViewsMessageHandler};
use plugins_shim::FlutterPluginsEngine;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use resize_controller::ResizeController;
use task_runner::FlutterTaskExecutor;
use window::{MouseAction, Window, WindowHandler};
use windows::core::Interface;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice2, IDCompositionDevice, IDCompositionVisual,
};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMSBT_MAINWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DWM_SYSTEMBACKDROP_TYPE,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{MoveWindow, SetParent};
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event_loop::EventLoopBuilder;
use winit::platform::windows::WindowBuilderExtWindows;
use winit::window::WindowBuilder;

use crate::compositor::FlutterCompositor;
use crate::egl::EglDevice;
use crate::engine::{FlutterEngine, FlutterEngineConfig, PointerPhase};
use crate::error_utils::ResultExt;
use crate::keyboard::Keyboard;
use crate::mouse_cursor::MouseCursorHandler;
use crate::text_input::{TextInputHandler, TextInputState};

pub use crate::engine::{BinaryMessageHandler, BinaryMessageReply, BinaryMessenger};
pub use crate::platform_views::{CompositorContext, PlatformView, PlatformViewUpdateArgs};

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

pub struct FlionEngineBuilder<'a> {
    bundle_path: PathBuf,
    platform_message_handlers: Vec<(&'a str, Box<dyn BinaryMessageHandler>)>,
    platform_view_factories: HashMap<String, Box<dyn PlatformViewFactory>>,
}

impl<'a> FlionEngineBuilder<'a> {
    fn new() -> FlionEngineBuilder<'a> {
        let bundle_path = if let Ok(exe) = env::current_exe()
            && let Some(dir) = exe.parent()
        {
            dir.join("data")
        } else {
            PathBuf::from("data")
        };

        FlionEngineBuilder {
            bundle_path,
            platform_message_handlers: vec![],
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

    pub fn with_platform_view_factory(
        mut self,
        name: &'a str,
        factory: impl PlatformViewFactory + 'static,
    ) -> Self {
        self.platform_view_factories
            .insert(name.to_owned(), Box::new(factory));
        self
    }

    pub fn build(self) -> eyre::Result<FlionEngine> {
        let device = unsafe {
            let mut device = Default::default();

            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                Default::default(),
                D3D11_CREATE_DEVICE_FLAG::default(),
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )?;

            device.ok_or_eyre("failed to create D3D11 device")?
        };

        let composition_device: IDCompositionDevice =
            unsafe { DCompositionCreateDevice2(&device.cast::<IDXGIDevice>()?)? };

        let root_visual = unsafe { composition_device.CreateVisual()? };

        let egl = EglDevice::create(&device)?;

        let resize_controller = Arc::new(ResizeController::new());

        let compositor = FlutterCompositor::new(
            device.clone(),
            composition_device.clone(),
            root_visual.clone(),
            egl.clone(),
            Box::new(CompositionHandler {
                composition_device: composition_device.clone(),
                resize_controller: resize_controller.clone(),
                surface_size: (0, 0),
            }),
        )?;

        let platform_views = compositor.platform_views();

        let mut platform_message_handlers: Vec<(&str, Box<dyn BinaryMessageHandler>)> = vec![(
            "flion/platform_views",
            Box::new(PlatformViewsMessageHandler::new(
                platform_views,
                device,
                composition_device.clone(),
                self.platform_view_factories,
            )),
        )];

        platform_message_handlers.extend(self.platform_message_handlers);

        let task_executor = Rc::new(FlutterTaskExecutor::new()?);
        let task_queue = task_executor.queue().clone();

        // TODO: Disable environment variable lookup in release builds.
        // These variables are provided by the flion cli during development, and are not intended
        // to be used in release builds.
        let assets_path = env::var("FLION_ASSETS_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| self.bundle_path.join("flutter_assets"));

        let aot_library_path = env::var("FLION_AOT_LIBRARY_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| self.bundle_path.join("app.so"));

        let engine = Rc::new(FlutterEngine::new(FlutterEngineConfig {
            assets_path: assets_path.to_str().ok_or_eyre("invalid assets path")?,
            aot_library_path: Some(
                aot_library_path
                    .to_str()
                    .ok_or_eyre("invalid aot library path")?,
            ),
            egl: egl.clone(),
            compositor,
            platform_task_handler: Box::new(move |task| task_queue.enqueue(task)),
            platform_message_handlers,
        })?);

        task_executor.init(engine.clone());

        settings::send_to_engine(&engine)?;

        Ok(FlionEngine {
            engine,
            composition_device,
            root_visual,
            resize_controller,
            task_executor,
        })
    }
}

pub struct FlionEngine {
    engine: Rc<FlutterEngine>,
    composition_device: IDCompositionDevice,
    root_visual: IDCompositionVisual,
    resize_controller: Arc<ResizeController>,
    task_executor: Rc<FlutterTaskExecutor>,
}

impl FlionEngine {
    pub fn builder<'a>() -> FlionEngineBuilder<'a> {
        FlionEngineBuilder::new()
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
        let event_loop = EventLoopBuilder::new().build()?;

        let parent_window = WindowBuilder::new()
            .with_inner_size(LogicalSize::new(1280, 720))
            .with_no_redirection_bitmap(true)
            .build(&event_loop)?;

        let parent_windoww = Rc::new(parent_window);

        let parent_hwnd = match parent_windoww.window_handle()?.as_raw() {
            RawWindowHandle::Win32(handle) => HWND(handle.hwnd.get() as _),
            _ => unreachable!(),
        };

        unsafe {
            let backdrop_type = DWMSBT_MAINWINDOW;
            DwmSetWindowAttribute(
                parent_hwnd,
                DWMWA_SYSTEMBACKDROP_TYPE,
                &raw const backdrop_type as *const c_void,
                mem::size_of::<DWM_SYSTEMBACKDROP_TYPE>() as u32,
            )?;
        }

        let text_input = Rc::new(RefCell::new(TextInputState::new()));

        let window = Rc::new(Window::new(
            800,
            600,
            Box::new(FlutterWindowHandler {
                engine: self.engine.clone(),
                resize_controller: self.resize_controller.clone(),
                keyboard: Keyboard::new(self.engine.clone(), text_input.clone()),
                task_executor: self.task_executor.clone(),
            }),
        )?);

        unsafe {
            SetParent(window.window_handle(), Some(parent_hwnd))?;
            SetFocus(Some(window.window_handle()))?;
        }

        self.engine.set_platform_message_handler(
            "flutter/textinput",
            TextInputHandler::new(text_input.clone()),
        );

        self.engine.set_platform_message_handler(
            "flutter/mousecursor",
            MouseCursorHandler::new(Rc::downgrade(&window)),
        );

        let mut plugins_engine = Box::new(FlutterPluginsEngine::new(
            self.engine.clone(),
            window.window_handle(),
        )?);

        for init in PLUGINS {
            unsafe {
                (init)(&raw mut *plugins_engine as *mut c_void);
            }
        }

        // TODO: Composition target should be attached to parent window instead. Use the child window
        // just for input.
        let composition_target = unsafe {
            self.composition_device
                .CreateTargetForHwnd(window.window_handle(), true)?
        };

        unsafe {
            composition_target.SetRoot(&self.root_visual)?;
        }

        event_loop.run(move |event, target| match event {
            winit::event::Event::WindowEvent { window_id, event }
                if window_id == parent_windoww.id() =>
            {
                match event {
                    winit::event::WindowEvent::CloseRequested => {
                        target.exit();
                    }

                    winit::event::WindowEvent::Focused(true) => unsafe {
                        SetFocus(Some(window.window_handle())).unwrap();
                    },

                    winit::event::WindowEvent::Resized(PhysicalSize { width, height }) => unsafe {
                        MoveWindow(
                            window.window_handle(),
                            0,
                            0,
                            width as i32,
                            height as i32,
                            false,
                        )
                        .unwrap();
                    },

                    _ => {}
                }
            }

            _ => {}
        })?;

        Ok(())
    }
}

struct FlutterWindowHandler {
    engine: Rc<engine::FlutterEngine>,
    task_executor: Rc<FlutterTaskExecutor>,
    resize_controller: Arc<ResizeController>,
    keyboard: Keyboard,
}

impl WindowHandler for FlutterWindowHandler {
    fn on_resized(&self, width: u32, height: u32, scale_factor: f64) {
        // TODO: Consider moving this to WM_NCCALCSIZE on the parent window for smoother resizing.
        self.resize_controller
            .begin_and_wait(width, height, &self.task_executor, || {
                let _ = self
                    .engine
                    .send_window_metrics_event(width as usize, height as usize, scale_factor)
                    .trace_err();
            });
    }

    fn on_mouse_event(&self, event: window::MouseEvent) {
        if event.action == MouseAction::Scroll {
            let _ = self
                .engine
                .send_scroll_event(event.x, event.y, event.scroll_delta_x, event.scroll_delta_y)
                .trace_err();
        } else {
            let pointer_event = PointerEvent {
                device_kind: PointerDeviceKind::Mouse,
                device_id: 1,
                phase: match event.action {
                    window::MouseAction::Enter => PointerPhase::Add,
                    window::MouseAction::Exit => PointerPhase::Remove,
                    window::MouseAction::Move => {
                        if event.buttons.is_empty() {
                            PointerPhase::Hover
                        } else {
                            PointerPhase::Move
                        }
                    }
                    window::MouseAction::Down => PointerPhase::Down,
                    window::MouseAction::Up => PointerPhase::Up,
                    window::MouseAction::Scroll => unreachable!(),
                },
                x: event.x,
                y: event.y,
                buttons: PointerButtons::from_bits_truncate(event.buttons.bits().into()),
            };

            let _ = self.engine.send_pointer_event(&pointer_event).trace_err();
        }
    }

    fn on_touch_event(&self, event: window::TouchEvent) {
        let phases: &[PointerPhase] = match event.action {
            window::TouchAction::Down => &[PointerPhase::Add, PointerPhase::Down],
            window::TouchAction::Up => &[PointerPhase::Up, PointerPhase::Remove],
            window::TouchAction::Move => &[PointerPhase::Move],
        };

        for &phase in phases {
            let _ = self
                .engine
                .send_pointer_event(&PointerEvent {
                    device_kind: PointerDeviceKind::Touch,
                    device_id: event.touch_id as i32,
                    phase,
                    x: event.x,
                    y: event.y,
                    ..Default::default()
                })
                .trace_err();
        }
    }

    fn on_key_event(&self, event: window::KeyEvent) {
        let _ = self.keyboard.handle_event(event).trace_err();
    }
}

struct CompositionHandler {
    composition_device: IDCompositionDevice,
    resize_controller: Arc<ResizeController>,
    surface_size: (u32, u32),
}

unsafe impl Send for CompositionHandler {}

impl compositor::CompositionHandler for CompositionHandler {
    fn get_surface_size(&mut self) -> eyre::Result<(u32, u32)> {
        if let Some(resize) = self.resize_controller.current_resize() {
            self.surface_size = resize.size();
        }
        Ok(self.surface_size)
    }

    fn present(&mut self) -> eyre::Result<()> {
        let commit_compositor = || unsafe { self.composition_device.Commit() };

        if let Some(resize) = self.resize_controller.current_resize() {
            // Make sure the previous commit has completed. This reduces glitches while resizing.
            unsafe {
                self.composition_device.WaitForCommitCompletion()?;
            }

            commit_compositor()?;

            resize.complete();
        } else {
            commit_compositor()?;
        }

        Ok(())
    }
}
