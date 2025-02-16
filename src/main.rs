#![feature(lint_reasons)]

mod compositor;
mod egl_manager;
mod engine;
mod error_utils;
mod keyboard;
mod keymap;
mod mouse_cursor;
mod resize_controller;
mod settings;
mod standard_method_channel;
mod task_runner;
mod text_input;

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::mem;
use std::rc::Rc;
use std::sync::Arc;

use color_eyre::eyre::OptionExt;
use color_eyre::Result;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use resize_controller::ResizeController;
use task_runner::Task;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMSBT_MAINWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DWM_SYSTEMBACKDROP_TYPE,
};
use windows::Win32::System::WinRT::{
    CreateDispatcherQueueController, DispatcherQueueOptions, DQTAT_COM_ASTA, DQTYPE_THREAD_CURRENT,
};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::WM_NCCALCSIZE;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Event, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoopBuilder};
use winit::platform::windows::WindowBuilderExtWindows;
use winit::window::WindowBuilder;

use crate::compositor::Compositor;
use crate::egl_manager::EglManager;
use crate::engine::{FlutterEngine, FlutterEngineConfig, PointerPhase};
use crate::error_utils::ResultExt;
use crate::keyboard::Keyboard;
use crate::mouse_cursor::MouseCursorHandler;
use crate::task_runner::TaskRunnerExecutor;
use crate::text_input::{TextInputHandler, TextInputState};

struct WindowData {
    engine: *const engine::FlutterEngine,
    resize_controller: Arc<ResizeController>,
    scale_factor: Cell<f64>,
}

#[derive(Debug)]
enum PlatformEvent {
    PostFlutterTask(Task),
}

fn main() -> Result<()> {
    color_eyre::install()?;

    #[cfg(debug_assertions)]
    {
        use tracing_subscriber::fmt::format::FmtSpan;
        tracing_subscriber::fmt()
            .with_span_events(FmtSpan::ENTER)
            .with_thread_names(true)
            .with_max_level(tracing::Level::DEBUG)
            .init();
    }

    let event_loop = EventLoopBuilder::<PlatformEvent>::with_user_event().build()?;
    let window = WindowBuilder::new()
        .with_inner_size(LogicalSize::new(800, 600))
        .with_no_redirection_bitmap(true)
        .build(&event_loop)?;

    let hwnd = match window.window_handle()?.as_raw() {
        RawWindowHandle::Win32(handle) => HWND(handle.hwnd.get()),
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
            None,
            D3D11_CREATE_DEVICE_FLAG::default(),
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )?;

        device.ok_or_eyre("failed to create D3D11 device")?
    };

    let egl_manager = EglManager::create(&device)?;
    let resize_controller = Arc::new(ResizeController::new());

    let window = Rc::new(window);
    let text_input = Rc::new(RefCell::new(TextInputState::new()));

    let engine = Rc::new(FlutterEngine::new(FlutterEngineConfig {
        egl_manager: egl_manager.clone(),
        compositor: Compositor::new(hwnd, device, egl_manager.clone(), resize_controller.clone())?,
        platform_task_handler: Box::new({
            let event_loop = event_loop.create_proxy();
            move |task| {
                if let Err(e) = event_loop.send_event(PlatformEvent::PostFlutterTask(task)) {
                    tracing::error!("{e}");
                }
            }
        }),
        platform_message_handlers: vec![
            (
                "flutter/mousecursor",
                Box::new(MouseCursorHandler::new(window.clone())),
            ),
            (
                "flutter/textinput",
                Box::new(TextInputHandler::new(text_input.clone())),
            ),
        ],
    })?);

    engine.send_window_metrics_event(width as usize, height as usize, window.scale_factor())?;

    settings::send_to_engine(&engine)?;

    let window_data = Box::leak(Box::new(WindowData {
        engine: &*engine,
        resize_controller,
        scale_factor: Cell::new(window.scale_factor()),
    }));

    unsafe { SetWindowSubclass(hwnd, Some(wnd_proc), 696969, window_data as *mut _ as _) };

    let mut cursor_pos = PhysicalPosition::new(0.0, 0.0);
    let mut task_executor = TaskRunnerExecutor::default();
    let mut keyboard = Keyboard::new(engine.clone(), text_input);

    let mut pointer_is_down = false;

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
                } => {
                    window_data.scale_factor.set(scale_factor);
                }
                WindowEvent::CursorMoved { position, .. } => {
                    cursor_pos = position;

                    let phase = if pointer_is_down {
                        PointerPhase::Move
                    } else {
                        PointerPhase::Hover
                    };

                    engine
                        .send_pointer_event(phase, position.x, position.y)
                        .unwrap();
                }
                WindowEvent::CursorEntered { .. } => {
                    engine
                        .send_pointer_event(PointerPhase::Add, cursor_pos.x, cursor_pos.y)
                        .unwrap();
                }
                WindowEvent::CursorLeft { .. } => {
                    engine
                        .send_pointer_event(PointerPhase::Remove, cursor_pos.x, cursor_pos.y)
                        .unwrap();
                }
                WindowEvent::MouseInput { state, .. } => {
                    let phase = match state {
                        ElementState::Pressed => PointerPhase::Down,
                        ElementState::Released => PointerPhase::Up,
                    };

                    pointer_is_down = state == ElementState::Pressed;

                    engine
                        .send_pointer_event(phase, cursor_pos.x, cursor_pos.y)
                        .unwrap();
                }
                WindowEvent::ModifiersChanged(modifiers) => {
                    let _ = keyboard.handle_modifiers_changed(modifiers).trace_err();
                }
                WindowEvent::KeyboardInput {
                    device_id: _,
                    event,
                    is_synthetic,
                } => {
                    tracing::debug!(
                        key = ?event.logical_key,
                        state = ?event.state,
                        is_synthetic,
                        "keyboard event"
                    );

                    let _ = keyboard
                        .handle_keyboard_input(event, is_synthetic)
                        .trace_err();
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
