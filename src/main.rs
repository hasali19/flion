#![feature(lint_reasons)]

mod compositor;
mod egl_manager;
mod engine;
mod error_utils;
mod logical_keys;
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

use color_eyre::eyre::{self, Context, OptionExt};
use color_eyre::Result;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use resize_controller::ResizeController;
use serde::{Deserialize, Serialize};
use task_runner::Task;
use windows::core::ComInterface;
use windows::Foundation::Numerics::{Matrix4x4, Vector2, Vector3};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMSBT_MAINWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DWM_SYSTEMBACKDROP_TYPE,
};
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::System::WinRT::{
    CreateDispatcherQueueController, DispatcherQueueOptions, DQTAT_COM_ASTA, DQTYPE_THREAD_CURRENT,
};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::WM_NCCALCSIZE;
use windows::UI::Composition::ContainerVisual;
use windows::UI::Composition::Core::CompositorController;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Event, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoopBuilder};
use winit::keyboard::Key;
use winit::platform::scancode::PhysicalKeyExtScancode;
use winit::platform::windows::WindowBuilderExtWindows;
use winit::window::WindowBuilder;

use crate::compositor::Compositor;
use crate::egl_manager::EglManager;
use crate::engine::{FlutterEngine, FlutterEngineConfig, KeyEvent, KeyEventType, PointerPhase};
use crate::error_utils::ResultExt;
use crate::mouse_cursor::MouseCursorHandler;
use crate::task_runner::TaskRunnerExecutor;
use crate::text_input::{TextInputHandler, TextInputState};

struct WindowData {
    engine: *const engine::FlutterEngine,
    resize_controller: Arc<ResizeController>,
    scale_factor: Cell<f64>,
    root_visual: ContainerVisual,
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

    let compositor_controller = CompositorController::new()?;
    let composition_target = unsafe {
        compositor_controller
            .Compositor()?
            .cast::<ICompositorDesktopInterop>()?
            .CreateDesktopWindowTarget(hwnd, false)?
    };

    let root = compositor_controller
        .Compositor()?
        .CreateContainerVisual()?;

    root.SetSize(Vector2 {
        X: width as f32,
        Y: height as f32,
    })?;

    root.SetTransformMatrix(Matrix4x4 {
        M11: 1.0,
        M22: -1.0,
        M33: 1.0,
        M44: 1.0,
        ..Default::default()
    })?;

    root.SetOffset(Vector3::new(0.0, height as f32, 0.0))?;

    composition_target.SetRoot(&root)?;

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

    let engine = FlutterEngine::new(FlutterEngineConfig {
        egl_manager: egl_manager.clone(),
        compositor: Compositor::new(
            device,
            compositor_controller,
            egl_manager.clone(),
            resize_controller.clone(),
            root.clone(),
        )?,
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
    })?;

    engine.send_window_metrics_event(width as usize, height as usize, window.scale_factor())?;

    settings::send_to_engine(&engine)?;

    let window_data = Box::leak(Box::new(WindowData {
        engine: &engine,
        resize_controller,
        scale_factor: Cell::new(window.scale_factor()),
        root_visual: root,
    }));

    unsafe { SetWindowSubclass(hwnd, Some(wnd_proc), 696969, window_data as *mut _ as _) };

    let mut cursor_pos = PhysicalPosition::new(0.0, 0.0);
    let mut task_executor = TaskRunnerExecutor::default();

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
                    engine
                        .send_pointer_event(PointerPhase::Hover, position.x, position.y)
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
                    engine
                        .send_pointer_event(phase, cursor_pos.x, cursor_pos.y)
                        .unwrap();
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

                    let process_text_input = |event: winit::event::KeyEvent| {
                        let mut text_input = text_input.borrow_mut();
                        let _ = text_input
                            .process_key_event(&event, &engine)
                            .wrap_err("text input plugin failed to process key event")
                            .trace_err();
                    };

                    let send_channel = |event: winit::event::KeyEvent| {
                        let _ =
                            send_channel_key_event(&engine, event, process_text_input).trace_err();
                    };

                    let send_embedder = |event: winit::event::KeyEvent| {
                        let _ = send_embedder_key_event(&engine, event, is_synthetic, send_channel)
                            .wrap_err("failed to send embedder key event")
                            .trace_err();
                    };

                    send_embedder(event);
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

fn send_embedder_key_event(
    engine: &FlutterEngine,
    event: winit::event::KeyEvent,
    is_synthetic: bool,
    next_handler: impl FnOnce(winit::event::KeyEvent),
) -> eyre::Result<()> {
    let character = match &event.logical_key {
        Key::Named(_) => None,
        Key::Character(c) => {
            if event.state == ElementState::Released {
                None
            } else {
                Some(c.clone())
            }
        }
        Key::Unidentified(_) => None,
        Key::Dead(_) => None,
    };

    let key_event = KeyEvent {
        event_type: match event.state {
            ElementState::Pressed if event.repeat => KeyEventType::Repeat,
            ElementState::Pressed => KeyEventType::Down,
            ElementState::Released => KeyEventType::Up,
        },
        synthesized: is_synthetic,
        character: character.as_ref(),
        logical: logical_keys::to_flutter(&event.logical_key),
        physical: event.physical_key.to_scancode().map(|code| code.into()),
    };

    engine.send_key_event(key_event, move |handled| {
        if !handled {
            next_handler(event);
        }
    })
}

fn send_channel_key_event(
    engine: &FlutterEngine,
    event: winit::event::KeyEvent,
    next_handler: impl FnOnce(winit::event::KeyEvent),
) -> eyre::Result<()> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Message<'a> {
        keymap: &'a str,
        #[serde(rename = "type")]
        event_type: &'a str,
        character_code_point: Option<u64>,
        key_code: Option<u64>,
        scan_code: Option<u64>,
        modifiers: Option<u64>,
    }

    #[derive(Debug, Deserialize)]
    struct Response {
        handled: bool,
    }

    let character = match &event.logical_key {
        Key::Character(c) => {
            if (1..=4).contains(&c.len()) {
                let mut bytes = [0u8; 4];
                bytes[..c.len()].copy_from_slice(c.as_bytes());
                Some(u32::from_le_bytes(bytes) as u64)
            } else {
                None
            }
        }
        _ => None,
    };

    let message = Message {
        keymap: "windows",
        event_type: match event.state {
            ElementState::Pressed => "keydown",
            ElementState::Released => "keyup",
        },
        character_code_point: character,
        key_code: logical_keys::to_flutter(&event.logical_key),
        scan_code: event.physical_key.to_scancode().map(|code| code.into()),
        modifiers: None,
    };

    engine.send_platform_message_with_reply(
        c"flutter/keyevent",
        &serde_json::to_vec(&message)?,
        |response| {
            let Ok(response) = serde_json::from_slice::<Response>(response)
                .wrap_err("invalid response from flutter/keyevent")
                .trace_err()
            else {
                return;
            };

            if response.handled {
                return;
            }

            next_handler(event);
        },
    )
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
                data.resize_controller.begin_and_wait(|| {
                    let width = rect.right - rect.left;
                    let height = rect.bottom - rect.top;

                    data.root_visual
                        .SetSize(Vector2::new(width as f32, height as f32))
                        .unwrap();

                    data.root_visual
                        .SetOffset(Vector3::new(0.0, height as f32, 0.0))
                        .unwrap();

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
