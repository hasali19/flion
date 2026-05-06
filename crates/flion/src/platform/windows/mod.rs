pub mod compositor;
pub mod cursor;
pub mod egl;
pub mod platform_views;
pub mod settings;
pub mod task_runner;
pub mod window;

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use std::{env, mem};

use eyre::OptionExt;
use parking_lot::Mutex;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event_loop::EventLoopBuilder;
use winit::platform::windows::WindowBuilderExtWindows;
use winit::window::WindowBuilder;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::DirectComposition::{DCompositionCreateDevice2, IDCompositionDevice};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMSBT_MAINWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DWM_SYSTEMBACKDROP_TYPE,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;

use crate::engine::{
    BinaryMessageHandler, FlutterEngine, FlutterEngineConfig, PointerButtons, PointerDeviceKind,
    PointerEvent,
};
use crate::error_utils::ResultExt;
use crate::input::{MouseAction, WindowHandler};
use crate::keyboard::Keyboard;
use crate::plugins_shim::FlutterPluginsEngine;
use crate::text_input::{TextInputHandler, TextInputState};
use crate::PLUGINS;

use compositor::{CompositionHandler, FlutterCompositor, ViewManager};
use cursor::MouseCursorHandler;
use egl::EglDevice;
use platform_views::{PlatformViewFactory, PlatformViewsMessageHandler};
use task_runner::{FlutterTaskExecutor, FlutterTaskQueue, Task};
use window::Window;

pub use platform_views::{CompositorContext, PlatformView, PlatformViewUpdateArgs};

pub struct FlionWindowsState {
    composition_device: IDCompositionDevice,
    view_manager: Arc<Mutex<ViewManager>>,
    task_executor: Rc<FlutterTaskExecutor>,
}

pub fn build<'a>(
    bundle_path: PathBuf,
    extra_handlers: Vec<(&'a str, Box<dyn BinaryMessageHandler>)>,
    view_factories: HashMap<String, Box<dyn PlatformViewFactory>>,
) -> eyre::Result<(Rc<FlutterEngine>, FlionWindowsState)> {
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

    let egl = EglDevice::create(&device)?;

    let task_executor = Rc::new(FlutterTaskExecutor::new()?);
    let task_queue = task_executor.queue().clone();

    let view_manager = Arc::new(Mutex::new(ViewManager::new()));
    let compositor = FlutterCompositor::new(
        device.clone(),
        composition_device.clone(),
        view_manager.clone(),
        egl.clone(),
        Box::new(WindowsCompositionHandler {
            composition_device: composition_device.clone(),
            view_manager: view_manager.clone(),
            platform_task_queue: task_queue.clone(),
        }),
    )?;

    let platform_views = compositor.platform_views();

    let mut platform_message_handlers: Vec<(&str, Box<dyn BinaryMessageHandler>)> = vec![(
        "flion/platform_views",
        Box::new(PlatformViewsMessageHandler::new(
            platform_views,
            device,
            composition_device.clone(),
            view_factories,
        )),
    )];

    platform_message_handlers.extend(extra_handlers);

    // TODO: Disable environment variable lookup in release builds.
    // These variables are provided by the flion cli during development, and are not intended
    // to be used in release builds.
    let assets_path = env::var("FLION_ASSETS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| bundle_path.join("flutter_assets"));

    let aot_library_path = env::var("FLION_AOT_LIBRARY_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| bundle_path.join("app.so"));

    let assets_path_str = assets_path
        .to_str()
        .ok_or_eyre("invalid assets path")?
        .to_owned();

    let aot_library_path_str = aot_library_path
        .to_str()
        .ok_or_eyre("invalid aot library path")?
        .to_owned();

    let engine = Rc::new(FlutterEngine::new(FlutterEngineConfig {
        assets_path: &assets_path_str,
        aot_library_path: Some(&aot_library_path_str),
        egl: egl.clone(),
        compositor,
        platform_task_handler: Box::new(move |task| task_queue.enqueue(task)),
        platform_message_handlers,
    })?);

    task_executor.init(engine.clone());

    settings::send_to_engine(&engine)?;

    Ok((
        engine,
        FlionWindowsState {
            composition_device,
            view_manager,
            task_executor,
        },
    ))
}

pub fn run_event_loop(engine: Rc<FlutterEngine>, state: FlionWindowsState) -> eyre::Result<()> {
    let event_loop = EventLoopBuilder::new().build()?;

    let parent_window = WindowBuilder::new()
        .with_inner_size(LogicalSize::new(1280, 720))
        .with_no_redirection_bitmap(true)
        .build(&event_loop)?;

    let parent_window = Rc::new(parent_window);

    let parent_hwnd = match parent_window.window_handle()?.as_raw() {
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

    let root_visual = unsafe { state.composition_device.CreateVisual()? };
    state.view_manager.lock().insert(0, root_visual.clone());

    let text_input = Rc::new(RefCell::new(TextInputState::new()));

    let size = parent_window.inner_size();
    let win32_window = Rc::new(Window::new(
        size.width,
        size.height,
        Box::new(FlutterWindowHandler {
            engine: engine.clone(),
            task_executor: state.task_executor.clone(),
            view_manager: state.view_manager.clone(),
            keyboard: Keyboard::new(engine.clone(), text_input.clone()),
        }),
    )?);

    win32_window.set_parent(parent_hwnd);
    win32_window.request_focus();
    win32_window.show();

    // The initial frame is scheduled by the send_window_metrics call when the window is resized
    // during its creation. However, this frame doesn't seem to show up on the window for some
    // reason. This workaround forces another frame which then is visible.
    // TODO: Figure out why this is needed.
    engine.schedule_frame();

    engine.set_platform_message_handler(
        "flutter/textinput",
        TextInputHandler::new(text_input.clone()),
    );

    engine.set_platform_message_handler(
        "flutter/mousecursor",
        MouseCursorHandler::new(Rc::downgrade(&win32_window)),
    );

    let mut plugins_engine = Box::new(FlutterPluginsEngine::new(
        engine.clone(),
        win32_window.window_handle(),
    )?);

    for init in PLUGINS {
        unsafe {
            (init)(&raw mut *plugins_engine as *mut c_void);
        }
    }

    // TODO: Composition target should be attached to parent window instead. Use the child window
    // just for input.
    let composition_target = unsafe {
        state
            .composition_device
            .CreateTargetForHwnd(win32_window.window_handle(), true)?
    };

    unsafe { composition_target.SetRoot(&root_visual)? };

    event_loop.run(move |event, target| match event {
        winit::event::Event::WindowEvent { window_id, event }
            if window_id == parent_window.id() =>
        {
            match event {
                winit::event::WindowEvent::CloseRequested => target.exit(),

                winit::event::WindowEvent::Focused(true) => win32_window.request_focus(),

                winit::event::WindowEvent::Resized(PhysicalSize { width, height }) => {
                    win32_window.set_position_and_size(0, 0, width, height)
                }

                _ => {}
            }
        }

        _ => {}
    })?;

    Ok(())
}

struct FlutterWindowHandler {
    engine: Rc<FlutterEngine>,
    task_executor: Rc<FlutterTaskExecutor>,
    view_manager: Arc<Mutex<ViewManager>>,
    keyboard: Keyboard,
}

impl WindowHandler for FlutterWindowHandler {
    fn on_resize(&self, width: u32, height: u32, scale_factor: f64) {
        // TODO: Consider moving this to WM_NCCALCSIZE on the parent window for smoother resizing.

        {
            let mut views = self.view_manager.lock();
            let Some(view) = views.get_mut(0) else {
                tracing::error!("Failed to resize non-existent view");
                return;
            };
            view.begin_resize(width, height);
        }

        let _ = self
            .engine
            .send_window_metrics_event(width as usize, height as usize, scale_factor)
            .trace_err();

        // The Flutter famework may need to run tasks on the platform executor during the resize,
        // so poll the executor instead of blocking to avoid a deadlock.
        while is_view_resizing(&self.view_manager.lock(), 0) {
            self.task_executor
                .poll_with_timeout(Duration::from_millis(100));
        }

        fn is_view_resizing(views: &ViewManager, view_id: i64) -> bool {
            match views.get(view_id) {
                None => false,
                Some(view) => view.is_resizing(),
            }
        }
    }

    fn on_mouse_event(&self, event: crate::input::MouseEvent) {
        use crate::engine::PointerPhase;

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
                    MouseAction::Enter => PointerPhase::Add,
                    MouseAction::Exit => PointerPhase::Remove,
                    MouseAction::Move => {
                        if event.buttons.is_empty() {
                            PointerPhase::Hover
                        } else {
                            PointerPhase::Move
                        }
                    }
                    MouseAction::Down => PointerPhase::Down,
                    MouseAction::Up => PointerPhase::Up,
                    MouseAction::Scroll => unreachable!(),
                },
                x: event.x,
                y: event.y,
                buttons: PointerButtons::from_bits_truncate(event.buttons.bits().into()),
            };

            let _ = self.engine.send_pointer_event(&pointer_event).trace_err();
        }
    }

    fn on_touch_event(&self, event: crate::input::TouchEvent) {
        use crate::engine::PointerPhase;
        use crate::input::TouchAction;

        let phases: &[PointerPhase] = match event.action {
            TouchAction::Down => &[PointerPhase::Add, PointerPhase::Down],
            TouchAction::Up => &[PointerPhase::Up, PointerPhase::Remove],
            TouchAction::Move => &[PointerPhase::Move],
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

    fn on_key_event(&self, event: crate::input::KeyEvent) {
        let _ = self.keyboard.handle_event(event).trace_err();
    }
}

struct WindowsCompositionHandler {
    composition_device: IDCompositionDevice,
    view_manager: Arc<Mutex<ViewManager>>,
    platform_task_queue: Arc<FlutterTaskQueue>,
}

unsafe impl Send for WindowsCompositionHandler {}

impl CompositionHandler for WindowsCompositionHandler {
    fn get_surface_size(&mut self) -> eyre::Result<(u32, u32)> {
        let views = self.view_manager.lock();
        let surface = views.get(0).ok_or_eyre("View not found")?;
        Ok(surface.size())
    }

    fn present(&mut self) -> eyre::Result<()> {
        let mut views = self.view_manager.lock();
        let surface = views.get_mut(0).ok_or_eyre("View not found")?;

        if surface.is_resizing() {
            unsafe {
                // Make sure the previous commit has completed. This reduces glitches while resizing.
                self.composition_device.WaitForCommitCompletion()?;
                self.composition_device.Commit()?;
            }

            surface.end_resize();

            // Platform thread is waiting for the resize so send it a signal to wake up.
            self.platform_task_queue.wake();
        } else {
            unsafe { self.composition_device.Commit()? };
        }

        Ok(())
    }
}
