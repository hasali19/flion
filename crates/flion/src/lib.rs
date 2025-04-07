#![feature(let_chains)]

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

pub mod codec;
pub mod standard_method_channel;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::env;
use std::ffi::c_void;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use engine::{PointerButtons, PointerDeviceKind, PointerEvent};
use eyre::OptionExt;
use platform_views::{PlatformViewFactory, PlatformViewsMessageHandler};
use plugins_shim::FlutterPluginsEngine;
use raw_window_handle::{HasWindowHandle, RawWindowHandle, Win32WindowHandle};
use resize_controller::ResizeController;
use task_runner::Task;
use windows::core::Interface;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice2, IDCompositionDevice, IDCompositionTarget,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    SystemParametersInfoW, SPI_GETWHEELSCROLLLINES, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    WM_NCCALCSIZE,
};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent};
use winit::event_loop::EventLoopWindowTarget;
use winit::window::Window;

use crate::compositor::FlutterCompositor;
use crate::egl::EglDevice;
use crate::engine::{FlutterEngine, FlutterEngineConfig, PointerPhase};
use crate::error_utils::ResultExt;
use crate::keyboard::Keyboard;
use crate::mouse_cursor::MouseCursorHandler;
use crate::text_input::{TextInputHandler, TextInputState};

pub use crate::engine::{BinaryMessageHandler, BinaryMessageReply, BinaryMessenger};
pub use crate::platform_views::{CompositorContext, PlatformView, PlatformViewUpdateArgs};
pub use crate::task_runner::TaskRunnerExecutor;

#[macro_export]
macro_rules! include_plugins {
    () => {
        include!(concat!(env!("OUT_DIR"), "/plugin_registrant.rs"));
    };
}

struct WindowData {
    engine: *const engine::FlutterEngine,
    resize_controller: Arc<ResizeController>,
    scale_factor: Rc<Cell<f64>>,
}

pub struct FlionEngineEnvironment;

impl FlionEngineEnvironment {
    pub fn init() -> eyre::Result<FlionEngineEnvironment> {
        Ok(FlionEngineEnvironment)
    }

    pub fn new_engine_builder<'a>(&self) -> FlionEngineBuilder<'_, 'a> {
        FlionEngineBuilder::new()
    }
}

pub struct FlionEngineBuilder<'e, 'a> {
    env: PhantomData<&'e FlionEngineEnvironment>,
    bundle_path: PathBuf,
    plugin_initializers: &'a [unsafe extern "C" fn(*mut c_void)],
    platform_message_handlers: Vec<(&'a str, Box<dyn BinaryMessageHandler>)>,
    platform_view_factories: HashMap<String, Box<dyn PlatformViewFactory>>,
}

pub type PlatformTask = Task;

impl<'e, 'a> FlionEngineBuilder<'e, 'a> {
    fn new() -> FlionEngineBuilder<'e, 'a> {
        let bundle_path = if let Ok(exe) = env::current_exe()
            && let Some(dir) = exe.parent()
        {
            dir.join("data")
        } else {
            PathBuf::from("data")
        };

        FlionEngineBuilder {
            env: PhantomData,
            bundle_path,
            plugin_initializers: &[],
            platform_message_handlers: vec![],
            platform_view_factories: HashMap::new(),
        }
    }

    pub fn with_bundle_path(mut self, path: &'a Path) -> Self {
        self.bundle_path = path.to_owned();
        self
    }

    pub fn with_plugins(mut self, plugins: &'a [unsafe extern "C" fn(*mut c_void)]) -> Self {
        self.plugin_initializers = plugins;
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

    pub fn build(
        self,
        window: Rc<Window>,
        platform_task_callback: impl Fn(PlatformTask) + 'static,
    ) -> eyre::Result<FlionEngine<'e>> {
        let RawWindowHandle::Win32(Win32WindowHandle { hwnd, .. }) =
            window.window_handle()?.as_raw()
        else {
            unreachable!()
        };

        let hwnd = HWND(hwnd.get() as *mut c_void);

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

        let composition_target = unsafe { composition_device.CreateTargetForHwnd(hwnd, true)? };

        let root_visual = unsafe { composition_device.CreateVisual()? };

        let PhysicalSize { width, height } = window.inner_size();

        unsafe {
            composition_target.SetRoot(&root_visual)?;
        }

        let egl = EglDevice::create(&device)?;

        let resize_controller = Arc::new(ResizeController::new());
        let text_input = Rc::new(RefCell::new(TextInputState::new()));

        let compositor = FlutterCompositor::new(
            device.clone(),
            composition_device.clone(),
            root_visual.clone(),
            egl.clone(),
            Box::new(CompositionHandler {
                composition_device: composition_device.clone(),
                resize_controller: resize_controller.clone(),
                surface_size: (width, height),
            }),
        )?;

        let platform_views = compositor.platform_views();

        let mut platform_message_handlers: Vec<(&str, Box<dyn BinaryMessageHandler>)> = vec![
            (
                "flutter/mousecursor",
                Box::new(MouseCursorHandler::new(window.clone())),
            ),
            (
                "flutter/textinput",
                Box::new(TextInputHandler::new(text_input.clone())),
            ),
            (
                "flion/platform_views",
                Box::new(PlatformViewsMessageHandler::new(
                    platform_views,
                    device,
                    composition_device,
                    self.platform_view_factories,
                )),
            ),
        ];

        platform_message_handlers.extend(self.platform_message_handlers);

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
            platform_task_handler: Box::new(platform_task_callback),
            platform_message_handlers,
        })?);

        let mut plugins_engine = Box::new(FlutterPluginsEngine::new(engine.clone(), &window)?);

        for init in self.plugin_initializers {
            unsafe {
                (init)(&raw mut *plugins_engine as *mut c_void);
            }
        }

        engine.send_window_metrics_event(width as usize, height as usize, window.scale_factor())?;

        settings::send_to_engine(&engine)?;

        let scale_factor = Rc::new(Cell::new(window.scale_factor()));

        let window_data = Box::into_raw(Box::new(WindowData {
            engine: &*engine,
            resize_controller,
            scale_factor: scale_factor.clone(),
        }));

        unsafe {
            SetWindowSubclass(hwnd, Some(wnd_proc), 696969, window_data as usize).ok()?;
        }

        Ok(FlionEngine::new(
            engine,
            window,
            text_input,
            plugins_engine,
            composition_target,
            scale_factor,
            window_data,
        ))
    }
}

pub struct FlionEngine<'e> {
    env: PhantomData<&'e FlionEngineEnvironment>,
    engine: Rc<FlutterEngine>,
    window: Rc<Window>,
    scale_factor: Rc<Cell<f64>>,
    cursor_pos: PhysicalPosition<f64>,
    pointer_is_down: bool,
    buttons: PointerButtons,
    keyboard: Keyboard,
    window_data: *mut WindowData,
    _plugins: Box<FlutterPluginsEngine>,
    _composition_target: IDCompositionTarget,
}

impl<'e> FlionEngine<'e> {
    fn new(
        engine: Rc<FlutterEngine>,
        window: Rc<Window>,
        text_input: Rc<RefCell<TextInputState>>,
        plugins: Box<FlutterPluginsEngine>,
        composition_target: IDCompositionTarget,
        scale_factor: Rc<Cell<f64>>,
        window_data: *mut WindowData,
    ) -> FlionEngine<'e> {
        FlionEngine {
            env: PhantomData,
            engine: engine.clone(),
            window,
            scale_factor,
            cursor_pos: PhysicalPosition::new(0.0, 0.0),
            pointer_is_down: false,
            buttons: PointerButtons::empty(),
            keyboard: Keyboard::new(engine, text_input),
            window_data,
            _plugins: plugins,
            _composition_target: composition_target,
        }
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

    pub fn process_tasks(
        &mut self,
        task_executor: &mut TaskRunnerExecutor,
    ) -> Option<std::time::Instant> {
        task_executor.process_all(&self.engine)
    }

    pub fn handle_window_event<T: 'static>(
        &mut self,
        event: &WindowEvent,
        target: &EventLoopWindowTarget<T>,
    ) -> eyre::Result<()> {
        match event {
            WindowEvent::CloseRequested => {
                target.exit();
            }
            WindowEvent::ScaleFactorChanged {
                scale_factor,
                inner_size_writer: _,
            } => {
                self.scale_factor.set(*scale_factor);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_pos = *position;

                let phase = if self.pointer_is_down {
                    PointerPhase::Move
                } else {
                    PointerPhase::Hover
                };

                let _ = self
                    .engine
                    .send_pointer_event(&PointerEvent {
                        device_kind: PointerDeviceKind::Mouse,
                        device_id: 1,
                        phase,
                        x: self.cursor_pos.x,
                        y: self.cursor_pos.y,
                        buttons: self.buttons,
                    })
                    .trace_err();
            }
            WindowEvent::CursorEntered { .. } => {
                let _ = self
                    .engine
                    .send_pointer_event(&PointerEvent {
                        device_kind: PointerDeviceKind::Mouse,
                        device_id: 1,
                        phase: PointerPhase::Add,
                        x: self.cursor_pos.x,
                        y: self.cursor_pos.y,
                        buttons: self.buttons,
                    })
                    .trace_err();
            }
            WindowEvent::CursorLeft { .. } => {
                let _ = self
                    .engine
                    .send_pointer_event(&PointerEvent {
                        device_kind: PointerDeviceKind::Mouse,
                        device_id: 1,
                        phase: PointerPhase::Remove,
                        x: self.cursor_pos.x,
                        y: self.cursor_pos.y,
                        buttons: self.buttons,
                    })
                    .trace_err();
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let phase = match state {
                    ElementState::Pressed => PointerPhase::Down,
                    ElementState::Released => PointerPhase::Up,
                };

                self.pointer_is_down = *state == ElementState::Pressed;

                let button = match button {
                    MouseButton::Left => PointerButtons::PRIMARY,
                    MouseButton::Right => PointerButtons::SECONDARY,
                    MouseButton::Middle => PointerButtons::MIDDLE,
                    MouseButton::Back => PointerButtons::BACK,
                    MouseButton::Forward => PointerButtons::FORWARD,
                    MouseButton::Other(_) => PointerButtons::empty(),
                };

                if self.pointer_is_down {
                    self.buttons.insert(button);
                } else {
                    self.buttons.remove(button);
                }

                let _ = self
                    .engine
                    .send_pointer_event(&PointerEvent {
                        device_kind: PointerDeviceKind::Mouse,
                        device_id: 1,
                        phase,
                        x: self.cursor_pos.x,
                        y: self.cursor_pos.y,
                        buttons: self.buttons,
                    })
                    .trace_err();
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                let _ = self
                    .keyboard
                    .handle_modifiers_changed(*modifiers)
                    .trace_err();
            }
            WindowEvent::KeyboardInput {
                device_id: _,
                event,
                is_synthetic,
            } => {
                let _ = self
                    .keyboard
                    .handle_keyboard_input(event.clone(), *is_synthetic)
                    .trace_err();
            }
            WindowEvent::MouseWheel { delta, .. } => match delta {
                MouseScrollDelta::LineDelta(x, y) => {
                    let mut lines_per_scroll = 3u32;
                    unsafe {
                        SystemParametersInfoW(
                            SPI_GETWHEELSCROLLLINES,
                            0,
                            Some(&raw mut lines_per_scroll as *mut c_void),
                            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS::default(),
                        )
                        .unwrap();
                    }

                    let scroll_multiplier = f64::from(lines_per_scroll) * 100.0 / 3.0;

                    let x = -f64::from(*x) * scroll_multiplier;
                    let y = -f64::from(*y) * scroll_multiplier;

                    let _ = self
                        .engine
                        .send_scroll_event(self.cursor_pos.x, self.cursor_pos.y, x, y)
                        .trace_err();
                }
                MouseScrollDelta::PixelDelta(physical_position) => {
                    tracing::debug!(?physical_position, "pixel scroll");
                }
            },
            WindowEvent::Touch(touch) => {
                let phases: &[PointerPhase] = match touch.phase {
                    TouchPhase::Started => &[PointerPhase::Add, PointerPhase::Down],
                    TouchPhase::Moved => &[PointerPhase::Move],
                    TouchPhase::Ended => &[PointerPhase::Up, PointerPhase::Remove],
                    TouchPhase::Cancelled => &[PointerPhase::Remove],
                };

                for &phase in phases {
                    let _ = self
                        .engine
                        .send_pointer_event(&PointerEvent {
                            device_kind: PointerDeviceKind::Touch,
                            device_id: touch.id as i32,
                            phase,
                            x: touch.location.x,
                            y: touch.location.y,
                            ..Default::default()
                        })
                        .trace_err();
                }
            }
            _ => {}
        }

        Ok(())
    }
}

impl Drop for FlionEngine<'_> {
    fn drop(&mut self) {
        let RawWindowHandle::Win32(Win32WindowHandle { hwnd, .. }) =
            self.window.window_handle().unwrap().as_raw()
        else {
            unreachable!()
        };

        let hwnd = HWND(hwnd.get() as *mut c_void);

        unsafe {
            RemoveWindowSubclass(hwnd, Some(wnd_proc), 696969).unwrap();
            drop(Box::from_raw(self.window_data));
        }
    }
}

unsafe extern "system" fn wnd_proc(
    window: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _uidsubclass: usize,
    dwrefdata: usize,
) -> LRESULT {
    let data = (dwrefdata as *const WindowData).as_ref().unwrap();
    match msg {
        WM_NCCALCSIZE => {
            DefSubclassProc(window, msg, wparam, lparam);

            let rect = lparam.0 as *const RECT;
            let rect = rect.as_ref().unwrap();

            if rect.right > rect.left && rect.bottom > rect.top {
                let width = rect.right - rect.left;
                let height = rect.bottom - rect.top;

                data.resize_controller
                    .begin_and_wait(width as u32, height as u32, || {
                        (*data.engine)
                            .send_window_metrics_event(
                                width as usize,
                                height as usize,
                                data.scale_factor.get(),
                            )
                            .unwrap();
                    });
            }
        }
        _ => return DefSubclassProc(window, msg, wparam, lparam),
    }

    LRESULT(0)
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
