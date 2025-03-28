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

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::{env, mem};

use codec::EncodableValue;
use engine::{PointerButtons, PointerDeviceKind, PointerEvent};
use eyre::OptionExt;
use platform_views::PlatformViews;
use plugins_shim::FlutterPluginsEngine;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use resize_controller::ResizeController;
use standard_method_channel::StandardMethodHandler;
use task_runner::Task;
use windows::core::Interface;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dwm::{
    DwmFlush, DwmSetWindowAttribute, DWMSBT_MAINWINDOW, DWMWA_SYSTEMBACKDROP_TYPE,
    DWM_SYSTEMBACKDROP_TYPE,
};
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::System::WinRT::{
    CreateDispatcherQueueController, DispatcherQueueOptions, DQTAT_COM_ASTA, DQTYPE_THREAD_CURRENT,
};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    SystemParametersInfoW, SPI_GETWHEELSCROLLLINES, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE, WM_NCCALCSIZE,
    WM_RBUTTONDOWN, WM_RBUTTONUP, WM_XBUTTONDOWN, WM_XBUTTONUP, XBUTTON1, XBUTTON2,
};
use windows::UI::Composition::Core::CompositorController;
use windows::UI::Composition::{Compositor, ContainerVisual};
use windows_numerics::Vector2;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{Event, MouseScrollDelta, TouchPhase, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoopBuilder};
use winit::platform::windows::WindowBuilderExtWindows;
use winit::window::WindowBuilder;

use crate::compositor::FlutterCompositor;
use crate::egl::EglDevice;
use crate::engine::{FlutterEngine, FlutterEngineConfig, PointerPhase};
use crate::error_utils::ResultExt;
use crate::keyboard::Keyboard;
use crate::mouse_cursor::MouseCursorHandler;
use crate::task_runner::TaskRunnerExecutor;
use crate::text_input::{TextInputHandler, TextInputState};

pub use crate::engine::{BinaryMessageHandler, BinaryMessageReply};
pub use crate::platform_views::PlatformView;

#[macro_export]
macro_rules! include_plugins {
    () => {
        include!(concat!(env!("OUT_DIR"), "/plugin_registrant.rs"));
    };
}

struct WindowData {
    engine: *const engine::FlutterEngine,
    resize_controller: Arc<ResizeController>,
    scale_factor: f64,
    cursor_position: (f64, f64),
    buttons: PointerButtons,
    is_tracking_mouse_leave: bool,
}

impl WindowData {
    fn track_mouse_leave_event(&mut self, hwnd: HWND) {
        if !self.is_tracking_mouse_leave {
            let mut event = TRACKMOUSEEVENT {
                cbSize: mem::size_of::<TRACKMOUSEEVENT>() as u32,
                hwndTrack: hwnd,
                dwFlags: TME_LEAVE,
                dwHoverTime: 0,
            };

            unsafe {
                TrackMouseEvent(&mut event).unwrap();
            }

            self.is_tracking_mouse_leave = true;

            unsafe {
                tracing::info!("mouse added");
                let _ = (*self.engine)
                    .send_pointer_event(&PointerEvent {
                        device_kind: PointerDeviceKind::Mouse,
                        device_id: 1,
                        phase: PointerPhase::Add,
                        x: self.cursor_position.0,
                        y: self.cursor_position.1,
                        buttons: self.buttons,
                    })
                    .trace_err();
            }
        }
    }
}

#[derive(Debug)]
enum PlatformEvent {
    PostFlutterTask(Task),
}

pub struct FlionEngine<'a> {
    bundle_path: &'a Path,
    plugin_initializers: &'a [unsafe extern "C" fn(*mut c_void)],
    platform_message_handlers: Vec<(&'a str, Box<dyn BinaryMessageHandler>)>,
    platform_view_factories: HashMap<String, Box<dyn Fn(&Compositor) -> PlatformView>>,
}

impl<'a> FlionEngine<'a> {
    #[expect(clippy::new_without_default)]
    pub fn new() -> FlionEngine<'a> {
        FlionEngine {
            bundle_path: Path::new("data"),
            plugin_initializers: &[],
            platform_message_handlers: vec![],
            platform_view_factories: HashMap::new(),
        }
    }

    pub fn with_bundle_path(mut self, path: &'a Path) -> Self {
        self.bundle_path = path;
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
        handler: Box<dyn Fn(&Compositor) -> PlatformView>,
    ) -> Self {
        self.platform_view_factories
            .insert(name.to_owned(), handler);
        self
    }

    pub fn run(self) -> eyre::Result<()> {
        let event_loop = EventLoopBuilder::<PlatformEvent>::with_user_event().build()?;
        let window = WindowBuilder::new()
            .with_inner_size(LogicalSize::new(1280, 720))
            .with_no_redirection_bitmap(true)
            .build(&event_loop)?;

        let hwnd = match window.window_handle()?.as_raw() {
            RawWindowHandle::Win32(handle) => HWND(handle.hwnd.get() as _),
            _ => unreachable!(),
        };

        unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_SYSTEMBACKDROP_TYPE,
                &DWMSBT_MAINWINDOW as *const DWM_SYSTEMBACKDROP_TYPE as *const c_void,
                mem::size_of::<DWM_SYSTEMBACKDROP_TYPE>() as u32,
            )
        }?;

        let PhysicalSize { width, height } = window.inner_size();

        tracing::info!(width, height);

        let _dispatcher_queue_controller = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: mem::size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_ASTA,
            })?
        };

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

        let compositor_controller = CompositorController::new()?;
        let composition_target = unsafe {
            compositor_controller
                .Compositor()?
                .cast::<ICompositorDesktopInterop>()?
                .CreateDesktopWindowTarget(hwnd, false)?
        };

        let egl = EglDevice::create(&device)?;
        let resize_controller = Arc::new(ResizeController::new());

        let window = Rc::new(window);
        let text_input = Rc::new(RefCell::new(TextInputState::new()));

        let root_visual = compositor_controller
            .Compositor()?
            .CreateContainerVisual()?;

        root_visual.SetSize(Vector2 {
            X: width as f32,
            Y: height as f32,
        })?;

        composition_target.SetRoot(&root_visual)?;

        let compositor = FlutterCompositor::new(
            root_visual.clone(),
            device,
            egl.clone(),
            Box::new(CompositionHandler {
                compositor_controller: compositor_controller.clone(),
                resize_controller: resize_controller.clone(),
                root_visual,
            }),
        )?;

        let platform_views = compositor.platform_views();

        struct PlatformViewsHandler {
            platform_views: Arc<PlatformViews>,
            compositor: Compositor,
            factories: HashMap<String, Box<dyn Fn(&Compositor) -> PlatformView>>,
        }

        impl StandardMethodHandler for PlatformViewsHandler {
            fn handle(
                &self,
                method: &str,
                args: codec::EncodableValue,
                reply: standard_method_channel::StandardMethodReply,
            ) {
                if method == "create" {
                    let args = args.as_map().unwrap();

                    let id = args
                        .get(&EncodableValue::Str("id"))
                        .unwrap()
                        .as_i32()
                        .unwrap();

                    let type_ = args
                        .get(&EncodableValue::Str("type"))
                        .unwrap()
                        .as_string()
                        .unwrap();

                    self.platform_views
                        .register(*id as u64, (self.factories[type_])(&self.compositor));

                    reply.success(&EncodableValue::Null);
                } else if method == "remove" {
                    // TODO
                    reply.success(&EncodableValue::Null);
                } else {
                    reply.not_implemented();
                }
            }
        }

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
                Box::new(PlatformViewsHandler {
                    platform_views,
                    compositor: compositor_controller.Compositor()?,
                    factories: self.platform_view_factories,
                }),
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
            platform_task_handler: Box::new({
                let event_loop = event_loop.create_proxy();
                move |task| {
                    if let Err(e) = event_loop.send_event(PlatformEvent::PostFlutterTask(task)) {
                        tracing::error!("{e}");
                    }
                }
            }),
            platform_message_handlers,
        })?);

        let mut plugins_engine =
            Box::new(FlutterPluginsEngine::new(&engine, &window, &event_loop)?);

        for init in self.plugin_initializers {
            unsafe {
                (init)(&raw mut *plugins_engine as *mut c_void);
            }
        }

        engine.send_window_metrics_event(width as usize, height as usize, window.scale_factor())?;

        settings::send_to_engine(&engine)?;

        let window_data = Box::into_raw(Box::new(WindowData {
            engine: &*engine,
            resize_controller,
            scale_factor: window.scale_factor(),
            cursor_position: (0.0, 0.0),
            buttons: PointerButtons::empty(),
            is_tracking_mouse_leave: false,
        }));

        unsafe {
            SetWindowSubclass(hwnd, Some(wnd_proc), 696969, window_data as *mut _ as _).ok()?
        };

        let mut cursor_pos = PhysicalPosition::new(0.0, 0.0);
        let mut task_executor = TaskRunnerExecutor::default();
        let mut keyboard = Keyboard::new(engine.clone(), text_input);

        event_loop.run(move |event, target| {
            match event {
                Event::UserEvent(event) => match event {
                    PlatformEvent::PostFlutterTask(task) => {
                        task_executor.enqueue(task);
                    }
                },
                Event::WindowEvent { event, .. } => match event {
                    WindowEvent::CloseRequested => {
                        target.exit();
                    }
                    WindowEvent::ScaleFactorChanged {
                        scale_factor,
                        inner_size_writer: _,
                    } => unsafe {
                        (*window_data).scale_factor = scale_factor;
                    },
                    WindowEvent::ModifiersChanged(modifiers) => {
                        let _ = keyboard.handle_modifiers_changed(modifiers).trace_err();
                    }
                    WindowEvent::KeyboardInput {
                        device_id: _,
                        event,
                        is_synthetic,
                    } => {
                        let _ = keyboard
                            .handle_keyboard_input(event, is_synthetic)
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

                            let x = -f64::from(x) * scroll_multiplier;
                            let y = -f64::from(y) * scroll_multiplier;

                            let _ = engine
                                .send_scroll_event(cursor_pos.x, cursor_pos.y, x, y)
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
                            let _ = engine
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
                },

                _ => (),
            }

            if let Some(next_task_target_time) = task_executor.process_all(&engine) {
                target.set_control_flow(ControlFlow::WaitUntil(next_task_target_time));
            }
        })?;

        Ok(())
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
    let data = &mut *(dwrefdata as *mut WindowData);
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
                                data.scale_factor,
                            )
                            .unwrap();
                    });
            }
        }
        WM_MOUSEMOVE => {
            data.track_mouse_leave_event(window);

            let x = lparam.0 & 0xffff;
            let y = (lparam.0 >> 16) & 0xffff;

            data.cursor_position = (x as f64, y as f64);

            let phase = if data.buttons.is_empty() {
                PointerPhase::Hover
            } else {
                PointerPhase::Move
            };

            let _ = (*data.engine)
                .send_pointer_event(&PointerEvent {
                    device_kind: PointerDeviceKind::Mouse,
                    device_id: 1,
                    phase,
                    x: x as f64,
                    y: y as f64,
                    buttons: data.buttons,
                })
                .trace_err();
        }
        WM_MOUSELEAVE => {
            tracing::info!("mouse removed");

            let _ = (*data.engine)
                .send_pointer_event(&PointerEvent {
                    device_kind: PointerDeviceKind::Mouse,
                    device_id: 1,
                    phase: PointerPhase::Remove,
                    x: data.cursor_position.0,
                    y: data.cursor_position.1,
                    buttons: data.buttons,
                })
                .trace_err();

            data.is_tracking_mouse_leave = false;
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN => {
            if msg == WM_LBUTTONDOWN {
                SetCapture(window);
            }

            let x = lparam.0 & 0xffff;
            let y = (lparam.0 >> 16) & 0xffff;

            let button = match msg {
                WM_LBUTTONDOWN => PointerButtons::PRIMARY,
                WM_RBUTTONDOWN => PointerButtons::SECONDARY,
                WM_MBUTTONDOWN => PointerButtons::MIDDLE,
                WM_XBUTTONDOWN => match ((wparam.0 >> 16) & 0xffff) as u16 {
                    XBUTTON1 => PointerButtons::BACK,
                    XBUTTON2 => PointerButtons::FORWARD,
                    _ => unreachable!(),
                },
                _ => unreachable!(),
            };

            data.buttons.insert(button);

            let _ = (*data.engine)
                .send_pointer_event(&PointerEvent {
                    device_kind: PointerDeviceKind::Mouse,
                    device_id: 1,
                    phase: PointerPhase::Down,
                    x: x as f64,
                    y: y as f64,
                    buttons: data.buttons,
                })
                .trace_err();
        }
        WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP | WM_XBUTTONUP => {
            if msg == WM_LBUTTONUP {
                ReleaseCapture().unwrap();
            }

            let x = lparam.0 & 0xffff;
            let y = (lparam.0 >> 16) & 0xffff;

            let button = match msg {
                WM_LBUTTONUP => PointerButtons::PRIMARY,
                WM_RBUTTONUP => PointerButtons::SECONDARY,
                WM_MBUTTONUP => PointerButtons::MIDDLE,
                WM_XBUTTONUP => match ((wparam.0 >> 16) & 0xffff) as u16 {
                    XBUTTON1 => PointerButtons::BACK,
                    XBUTTON2 => PointerButtons::FORWARD,
                    _ => unreachable!(),
                },
                _ => unreachable!(),
            };

            data.buttons.remove(button);

            let _ = (*data.engine)
                .send_pointer_event(&PointerEvent {
                    device_kind: PointerDeviceKind::Mouse,
                    device_id: 1,
                    phase: PointerPhase::Up,
                    x: x as f64,
                    y: y as f64,
                    buttons: data.buttons,
                })
                .trace_err();
        }
        _ => return DefSubclassProc(window, msg, wparam, lparam),
    }

    LRESULT(0)
}

struct CompositionHandler {
    compositor_controller: CompositorController,
    resize_controller: Arc<ResizeController>,
    root_visual: ContainerVisual,
}

impl compositor::CompositionHandler for CompositionHandler {
    fn get_surface_size(&mut self) -> eyre::Result<(u32, u32)> {
        if let Some(resize) = self.resize_controller.current_resize() {
            Ok(resize.size())
        } else {
            let size = self.root_visual.Size()?;
            Ok((size.X as u32, size.Y as u32))
        }
    }

    fn present(&mut self) -> eyre::Result<()> {
        let commit_compositor = || self.compositor_controller.Commit();

        if let Some(resize) = self.resize_controller.current_resize() {
            let (width, height) = resize.size();

            self.root_visual
                .SetSize(Vector2::new(width as f32, height as f32))
                .unwrap();

            // Calling DwmFlush() seems to reduce glitches when resizing.
            unsafe { DwmFlush()? };

            commit_compositor()?;

            resize.complete();
        } else {
            commit_compositor()?;
        }

        Ok(())
    }
}
