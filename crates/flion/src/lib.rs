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

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::{env, mem};

use engine::{PointerButtons, PointerDeviceKind, PointerEvent};
use eyre::OptionExt;
use platform_views::{PlatformViewFactory, PlatformViewsMessageHandler};
use plugins_shim::FlutterPluginsEngine;
use raw_window_handle::{HasWindowHandle, RawWindowHandle, Win32WindowHandle};
use resize_controller::ResizeController;
use task_runner::FlutterTaskExecutor;
use windows::core::Interface;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice2, IDCompositionDevice, IDCompositionTarget,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::Input::Touch::{
    CloseTouchInputHandle, GetTouchInputInfo, RegisterTouchWindow, HTOUCHINPUT,
    REGISTER_TOUCH_WINDOW_FLAGS, TOUCHEVENTF_DOWN, TOUCHEVENTF_MOVE, TOUCHEVENTF_UP, TOUCHINPUT,
};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, LoadCursorW, SetCursor, SystemParametersInfoW, HCURSOR, HTCLIENT, IDC_ARROW,
    SPI_GETWHEELSCROLLLINES, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, WHEEL_DELTA, WM_CHAR,
    WM_DEADCHAR, WM_DPICHANGED_BEFOREPARENT, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCALCSIZE,
    WM_NCCREATE, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETCURSOR, WM_TOUCH, WM_XBUTTONDOWN,
    WM_XBUTTONUP, XBUTTON1, XBUTTON2,
};
use winit::dpi::PhysicalSize;
use winit::event::WindowEvent;
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
    keyboard: Keyboard,
    cursor: Rc<RefCell<Option<HCURSOR>>>,
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

    fn on_mouse_scroll(&mut self, hwnd: HWND, dx: f64, dy: f64) -> eyre::Result<()> {
        let mut point = POINT::default();
        let mut lines_per_scroll = 3u32;

        unsafe {
            GetCursorPos(&mut point)?;
            ScreenToClient(hwnd, &mut point).ok()?;
            SystemParametersInfoW(
                SPI_GETWHEELSCROLLLINES,
                0,
                Some(&raw mut lines_per_scroll as *mut c_void),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS::default(),
            )?;
        }

        let scroll_multiplier = f64::from(lines_per_scroll) * 100.0 / 3.0;

        let x = -dx * scroll_multiplier;
        let y = -dy * scroll_multiplier;

        unsafe {
            let _ = (*self.engine)
                .send_scroll_event(point.x as f64, point.y as f64, x, y)
                .trace_err();
        }

        Ok(())
    }
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

    pub fn build(self, window: Rc<Window>) -> eyre::Result<FlionEngine<'e>> {
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
        let cursor_state = Rc::new(RefCell::new(Some(unsafe { LoadCursorW(None, IDC_ARROW)? })));

        let mut platform_message_handlers: Vec<(&str, Box<dyn BinaryMessageHandler>)> = vec![
            (
                "flutter/mousecursor",
                Box::new(MouseCursorHandler::new(cursor_state.clone())),
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

        let task_executor = FlutterTaskExecutor::new()?;
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

        let mut plugins_engine = Box::new(FlutterPluginsEngine::new(engine.clone(), &window)?);

        for init in self.plugin_initializers {
            unsafe {
                (init)(&raw mut *plugins_engine as *mut c_void);
            }
        }

        engine.send_window_metrics_event(width as usize, height as usize, window.scale_factor())?;

        settings::send_to_engine(&engine)?;

        let scale_factor = unsafe { GetDpiForWindow(hwnd) } as f64 / 96.0;

        let window_data = Box::into_raw(Box::new(WindowData {
            engine: &*engine,
            resize_controller,
            scale_factor,
            keyboard: Keyboard::new(engine.clone(), text_input.clone()),
            cursor: cursor_state,
            cursor_position: (0.0, 0.0),
            buttons: PointerButtons::empty(),
            is_tracking_mouse_leave: false,
        }));

        unsafe {
            SetWindowSubclass(hwnd, Some(wnd_proc), 696969, window_data as usize).ok()?;
        }

        Ok(FlionEngine {
            env: PhantomData,
            engine,
            window,
            window_data,
            _plugins: plugins_engine,
            _composition_target: composition_target,
            _task_executor: task_executor,
        })
    }
}

pub struct FlionEngine<'e> {
    env: PhantomData<&'e FlionEngineEnvironment>,
    engine: Rc<FlutterEngine>,
    window: Rc<Window>,
    window_data: *mut WindowData,
    _plugins: Box<FlutterPluginsEngine>,
    _composition_target: IDCompositionTarget,
    _task_executor: FlutterTaskExecutor,
}

impl FlionEngine<'_> {
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

    pub fn handle_window_event<T: 'static>(
        &mut self,
        event: &WindowEvent,
        target: &EventLoopWindowTarget<T>,
    ) -> eyre::Result<()> {
        if *event == WindowEvent::CloseRequested {
            target.exit();
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
    let data = (dwrefdata as *mut WindowData).as_mut().unwrap();
    match msg {
        WM_NCCREATE => {
            RegisterTouchWindow(window, REGISTER_TOUCH_WINDOW_FLAGS::default()).unwrap();
            return LRESULT(0);
        }
        WM_DPICHANGED_BEFOREPARENT => {
            data.scale_factor = GetDpiForWindow(window) as f64 / 96.0;
            return LRESULT(0);
        }
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

            return LRESULT(0);
        }
        WM_SETCURSOR => {
            let hit_test_result = lparam.0 & 0xffff;
            if hit_test_result as u32 == HTCLIENT {
                SetCursor(*data.cursor.borrow());
                return LRESULT(1);
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

            return LRESULT(0);
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

            return LRESULT(0);
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

            return LRESULT(0);
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

            return LRESULT(0);
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam.0 >> 16) & 0xffff) as i16 / WHEEL_DELTA as i16;
            let _ = data.on_mouse_scroll(window, 0.0, delta.into()).trace_err();
            return LRESULT(0);
        }
        WM_MOUSEHWHEEL => {
            let delta = ((wparam.0 >> 16) & 0xffff) as i16 / WHEEL_DELTA as i16;
            let _ = data.on_mouse_scroll(window, delta.into(), 0.0).trace_err();
            return LRESULT(0);
        }
        WM_TOUCH => {
            // TODO: Why doesn't this work?

            let num_points = wparam.0 & 0xffff;
            let touch_input_handle = HTOUCHINPUT(lparam.0 as _);

            let mut touch_points = vec![TOUCHINPUT::default(); num_points];

            if GetTouchInputInfo(
                touch_input_handle,
                &mut touch_points,
                mem::size_of::<TOUCHINPUT>() as i32,
            )
            .is_ok()
            {
                for touch in touch_points {
                    let touch_id = touch.dwID;

                    let mut point = POINT {
                        x: touch.x / 100,
                        y: touch.y / 100,
                    };

                    let _ = ScreenToClient(window, &mut point);

                    let x = touch.x as f64;
                    let y = touch.y as f64;

                    let phases: &[PointerPhase] = if touch.dwFlags.contains(TOUCHEVENTF_DOWN) {
                        &[PointerPhase::Add, PointerPhase::Down]
                    } else if touch.dwFlags.contains(TOUCHEVENTF_MOVE) {
                        &[PointerPhase::Move]
                    } else if touch.dwFlags.contains(TOUCHEVENTF_UP) {
                        &[PointerPhase::Up, PointerPhase::Remove]
                    } else {
                        return LRESULT(0);
                    };

                    for &phase in phases {
                        let _ = (*data.engine)
                            .send_pointer_event(&PointerEvent {
                                device_kind: PointerDeviceKind::Touch,
                                device_id: touch_id as i32,
                                phase,
                                x,
                                y,
                                ..Default::default()
                            })
                            .trace_err();
                    }
                }
            }

            CloseTouchInputHandle(touch_input_handle).unwrap();

            return LRESULT(0);
        }
        WM_KEYDOWN | WM_CHAR | WM_DEADCHAR | WM_KEYUP => {
            match data.keyboard.handle_message(window, msg, wparam, lparam) {
                Ok(handled) => {
                    if handled {
                        return LRESULT(0);
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to handle keyboard event: {e:?}");
                }
            }
        }
        _ => {}
    }

    DefSubclassProc(window, msg, wparam, lparam)
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
