#![feature(c_str_literals, lint_reasons)]

mod compositor;
mod egl_manager;
mod resize_controller;
mod task_runner;

use std::cell::Cell;
use std::ffi::{c_char, c_void, CStr};
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{mem, ptr};

use color_eyre::eyre::{self, ContextCompat};
use color_eyre::Result;
use flutter_embedder::{
    FlutterCompositor, FlutterCustomTaskRunners, FlutterEngine, FlutterEngineGetCurrentTime,
    FlutterEngineResult_kSuccess, FlutterEngineRun, FlutterEngineRunTask,
    FlutterEngineSendPointerEvent, FlutterEngineSendWindowMetricsEvent,
    FlutterOpenGLRendererConfig, FlutterPointerEvent, FlutterPointerPhase_kAdd,
    FlutterPointerPhase_kDown, FlutterPointerPhase_kHover, FlutterPointerPhase_kRemove,
    FlutterPointerPhase_kUp, FlutterProjectArgs, FlutterRendererConfig,
    FlutterRendererType_kOpenGL, FlutterTask, FlutterTaskRunnerDescription,
    FlutterWindowMetricsEvent, FLUTTER_ENGINE_VERSION,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use resize_controller::ResizeController;
use windows::core::ComInterface;
use windows::Foundation::Numerics::{Matrix4x4, Vector2, Vector3};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMSBT_TABBEDWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DWM_SYSTEMBACKDROP_TYPE,
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
use winit::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use winit::platform::windows::WindowBuilderExtWindows;
use winit::window::{Theme, WindowBuilder};

use crate::compositor::Compositor;
use crate::egl_manager::EglManager;
use crate::task_runner::TaskRunner;

struct WindowData {
    engine: FlutterEngine,
    resize_controller: Arc<ResizeController>,
    scale_factor: Cell<f64>,
    root_visual: ContainerVisual,
}

#[derive(Debug)]
enum PlatformEvent {
    PostFlutterTask(u64, FlutterTask),
}

fn main() -> Result<()> {
    color_eyre::install()?;

    #[cfg(debug_assertions)]
    {
        use tracing_subscriber::fmt::format::FmtSpan;
        tracing_subscriber::fmt()
            .with_span_events(FmtSpan::ENTER)
            .with_thread_names(true)
            .init();
    }

    let event_loop = EventLoopBuilder::<PlatformEvent>::with_user_event().build()?;
    let window = WindowBuilder::new()
        .with_inner_size(LogicalSize::new(800, 600))
        .with_no_redirection_bitmap(true)
        .with_theme(Some(Theme::Light))
        .build(&event_loop)?;

    let hwnd = match window.window_handle()?.as_raw() {
        RawWindowHandle::Win32(handle) => HWND(handle.hwnd.get()),
        _ => unreachable!(),
    };

    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            &DWMSBT_TABBEDWINDOW as *const DWM_SYSTEMBACKDROP_TYPE as *const c_void,
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

        device.wrap_err("failed to create D3D11 device")?
    };

    let egl_manager = EglManager::create(&device)?;
    let resize_controller = Arc::new(ResizeController::new());

    let engine = unsafe {
        create_engine(
            Compositor::new(
                device,
                compositor_controller,
                egl_manager.clone(),
                resize_controller.clone(),
                root.clone(),
            )?,
            egl_manager.clone(),
            event_loop.create_proxy(),
        )?
    };

    unsafe {
        FlutterEngineSendWindowMetricsEvent(
            engine,
            &FlutterWindowMetricsEvent {
                struct_size: mem::size_of::<FlutterWindowMetricsEvent>(),
                width: width as usize,
                height: height as usize,
                pixel_ratio: window.scale_factor(),
                ..Default::default()
            },
        )
    };

    let window_data = Box::leak(Box::new(WindowData {
        engine,
        resize_controller,
        scale_factor: Cell::new(window.scale_factor()),
        root_visual: root,
    }));

    unsafe { SetWindowSubclass(hwnd, Some(wnd_proc), 696969, window_data as *mut _ as _) };

    let mut cursor_pos = PhysicalPosition::new(0.0, 0.0);
    let mut tasks = vec![];

    event_loop.run(move |event, target| {
        match event {
            Event::UserEvent(event) => match event {
                PlatformEvent::PostFlutterTask(target_time_nanos, task) => {
                    tasks.push((target_time_nanos, task));
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
                WindowEvent::CursorMoved { position, .. } => unsafe {
                    cursor_pos = position;
                    FlutterEngineSendPointerEvent(
                        engine,
                        &FlutterPointerEvent {
                            struct_size: mem::size_of::<FlutterPointerEvent>(),
                            phase: FlutterPointerPhase_kHover,
                            x: position.x,
                            y: position.y,
                            timestamp: FlutterEngineGetCurrentTime() as usize,
                            ..Default::default()
                        },
                        1,
                    );
                },
                WindowEvent::CursorEntered { .. } => unsafe {
                    FlutterEngineSendPointerEvent(
                        engine,
                        &FlutterPointerEvent {
                            struct_size: mem::size_of::<FlutterPointerEvent>(),
                            phase: FlutterPointerPhase_kAdd,
                            x: cursor_pos.x,
                            y: cursor_pos.y,
                            timestamp: FlutterEngineGetCurrentTime() as usize,
                            ..Default::default()
                        },
                        1,
                    );
                },
                WindowEvent::CursorLeft { .. } => unsafe {
                    FlutterEngineSendPointerEvent(
                        engine,
                        &FlutterPointerEvent {
                            struct_size: mem::size_of::<FlutterPointerEvent>(),
                            phase: FlutterPointerPhase_kRemove,
                            x: cursor_pos.x,
                            y: cursor_pos.y,
                            timestamp: FlutterEngineGetCurrentTime() as usize,
                            ..Default::default()
                        },
                        1,
                    );
                },
                WindowEvent::MouseInput { state, .. } => unsafe {
                    FlutterEngineSendPointerEvent(
                        engine,
                        &FlutterPointerEvent {
                            struct_size: mem::size_of::<FlutterPointerEvent>(),
                            phase: match state {
                                ElementState::Pressed => FlutterPointerPhase_kDown,
                                ElementState::Released => FlutterPointerPhase_kUp,
                            },
                            x: cursor_pos.x,
                            y: cursor_pos.y,
                            timestamp: FlutterEngineGetCurrentTime() as usize,
                            ..Default::default()
                        },
                        1,
                    );
                },
                _ => {}
            },
            _ => (),
        }

        let now = unsafe { FlutterEngineGetCurrentTime() };
        let mut next_task_target_time = None;

        tasks.retain(|(target_time_nanos, task)| {
            if now >= *target_time_nanos {
                unsafe { FlutterEngineRunTask(engine, task) };
                return false;
            }

            let delta = Duration::from_nanos(target_time_nanos - now);
            let target_time = Instant::now() + delta;

            next_task_target_time = Some(if let Some(next) = next_task_target_time {
                std::cmp::min(next, target_time)
            } else {
                target_time
            });

            true
        });

        if let Some(next) = next_task_target_time {
            target.set_control_flow(ControlFlow::WaitUntil(next));
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
                data.resize_controller.begin_and_wait(|| {
                    let width = rect.right - rect.left;
                    let height = rect.bottom - rect.top;

                    data.root_visual
                        .SetSize(Vector2::new(width as f32, height as f32))
                        .unwrap();

                    data.root_visual
                        .SetOffset(Vector3::new(0.0, height as f32, 0.0))
                        .unwrap();

                    FlutterEngineSendWindowMetricsEvent(
                        data.engine,
                        &FlutterWindowMetricsEvent {
                            struct_size: mem::size_of::<FlutterWindowMetricsEvent>(),
                            width: width as usize,
                            height: height as usize,
                            pixel_ratio: data.scale_factor.get(),
                            ..Default::default()
                        },
                    );
                });
            }
        }
        _ => return DefSubclassProc(window, msg, wparam, lparam),
    }

    LRESULT(0)
}

unsafe fn create_engine(
    compositor: Compositor,
    egl_manager: Arc<EglManager>,
    event_loop: EventLoopProxy<PlatformEvent>,
) -> eyre::Result<FlutterEngine> {
    fn create_task_runner<F: Fn(u64, FlutterTask)>(
        id: usize,
        runner: &'static TaskRunner<F>,
    ) -> FlutterTaskRunnerDescription {
        unsafe extern "C" fn runs_tasks_on_current_thread<F>(task_runner: *mut c_void) -> bool {
            task_runner
                .cast::<TaskRunner<F>>()
                .as_mut()
                .unwrap()
                .runs_tasks_on_current_thread()
        }

        unsafe extern "C" fn post_task_callback<F: Fn(u64, FlutterTask)>(
            task: FlutterTask,
            target_time_nanos: u64,
            user_data: *mut c_void,
        ) {
            user_data
                .cast::<TaskRunner<F>>()
                .as_mut()
                .unwrap()
                .post_task(task, target_time_nanos)
        }

        FlutterTaskRunnerDescription {
            struct_size: mem::size_of::<FlutterTaskRunnerDescription>(),
            identifier: id,
            user_data: runner as *const TaskRunner<F> as *mut c_void,
            runs_task_on_current_thread_callback: Some(runs_tasks_on_current_thread::<F>),
            post_task_callback: Some(post_task_callback::<F>),
        }
    }

    let renderer_config = FlutterRendererConfig {
        type_: FlutterRendererType_kOpenGL,
        __bindgen_anon_1: flutter_embedder::FlutterRendererConfig__bindgen_ty_1 {
            open_gl: FlutterOpenGLRendererConfig {
                struct_size: mem::size_of::<FlutterOpenGLRendererConfig>(),
                make_current: Some(gl_make_current),
                make_resource_current: Some(gl_make_resource_current),
                clear_current: Some(gl_clear_current),
                present: Some(gl_present),
                fbo_callback: Some(gl_fbo_callback),
                fbo_reset_after_present: true,
                gl_proc_resolver: Some(gl_get_proc_address),
                ..Default::default()
            },
        },
    };

    let platform_task_runner = create_task_runner(
        1,
        Box::leak(Box::new(TaskRunner::new(move |t, task| {
            event_loop
                .send_event(PlatformEvent::PostFlutterTask(t, task))
                .unwrap()
        }))),
    );

    let project_args = FlutterProjectArgs {
        struct_size: mem::size_of::<FlutterProjectArgs>(),
        assets_path: c"example/build/flutter_assets".as_ptr(),
        icu_data_path: c"icudtl.dat".as_ptr(),
        custom_task_runners: &FlutterCustomTaskRunners {
            struct_size: mem::size_of::<FlutterCustomTaskRunners>(),
            platform_task_runner: &platform_task_runner,
            render_task_runner: ptr::null(),
            thread_priority_setter: Some(task_runner::set_thread_priority),
        },
        compositor: &FlutterCompositor {
            struct_size: mem::size_of::<FlutterCompositor>(),
            create_backing_store_callback: Some(compositor::create_backing_store),
            collect_backing_store_callback: Some(compositor::collect_backing_store),
            present_layers_callback: Some(compositor::present_layers),
            present_view_callback: None,
            user_data: Box::leak(Box::new(compositor)) as *mut Compositor as *mut c_void,
            avoid_backing_store_cache: false,
        },
        ..Default::default()
    };

    let mut engine = ptr::null_mut();
    unsafe {
        let result = FlutterEngineRun(
            FLUTTER_ENGINE_VERSION as usize,
            &renderer_config,
            &project_args,
            Arc::into_raw(egl_manager) as *mut c_void,
            &mut engine,
        );

        if result != FlutterEngineResult_kSuccess || engine.is_null() {
            panic!("could not run the flutter engine");
        }
    }

    Ok(engine)
}

unsafe extern "C" fn gl_make_current(user_data: *mut c_void) -> bool {
    let egl_manager = user_data.cast::<EglManager>().as_ref().unwrap();

    if let Err(e) = egl_manager.make_context_current() {
        tracing::error!("failed to make context current: {e}");
        return false;
    }

    true
}

unsafe extern "C" fn gl_make_resource_current(user_data: *mut c_void) -> bool {
    let egl_manager = user_data.cast::<EglManager>().as_ref().unwrap();

    if let Err(e) = egl_manager.make_resource_context_current() {
        tracing::error!("failed to make resource context current: {e}");
        return false;
    }

    true
}

unsafe extern "C" fn gl_clear_current(user_data: *mut c_void) -> bool {
    let egl_manager = user_data.cast::<EglManager>().as_ref().unwrap();

    if let Err(e) = egl_manager.clear_current() {
        tracing::error!("failed to clear context: {e}");
        return false;
    }

    true
}

unsafe extern "C" fn gl_present(_user_data: *mut c_void) -> bool {
    false
}

unsafe extern "C" fn gl_fbo_callback(_user_data: *mut c_void) -> u32 {
    0
}

unsafe extern "C" fn gl_get_proc_address(
    user_data: *mut c_void,
    name: *const c_char,
) -> *mut c_void {
    let egl_manager = user_data.cast::<EglManager>().as_ref().unwrap();
    let name = CStr::from_ptr(name);
    egl_manager
        .get_proc_address(name.to_str().unwrap())
        .unwrap_or(ptr::null_mut())
}
