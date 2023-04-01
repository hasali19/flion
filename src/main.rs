use std::ffi::{c_char, c_void, CStr};
use std::{mem, ptr};

use color_eyre::Result;
use flutter_embedder::{
    FlutterEngineGetCurrentTime, FlutterEngineResult_kSuccess, FlutterEngineRun,
    FlutterEngineSendPointerEvent, FlutterEngineSendWindowMetricsEvent,
    FlutterOpenGLRendererConfig, FlutterPointerEvent, FlutterPointerPhase_kAdd,
    FlutterPointerPhase_kDown, FlutterPointerPhase_kMove, FlutterPointerPhase_kRemove,
    FlutterPointerPhase_kUp, FlutterProjectArgs, FlutterRendererConfig,
    FlutterRendererType_kOpenGL, FlutterWindowMetricsEvent, FLUTTER_ENGINE_VERSION,
};
use khronos_egl as egl;
use raw_window_handle::{HasRawWindowHandle, RawWindowHandle};
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, Event, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoop};
use winit::window::Window;

macro_rules! cstr {
    ($v:literal) => {
        concat!($v, "\0").as_ptr() as *const std::ffi::c_char
    };
}

type EglInstance = egl::Instance<egl::Static>;

struct Gl {
    egl: EglInstance,
    display: egl::Display,
    context: egl::Context,
    surface: egl::Surface,
}

const EGL_PLATFORM_ANGLE_ANGLE: egl::Enum = 0x3202;
const EGL_PLATFORM_ANGLE_TYPE_ANGLE: egl::Attrib = 0x3203;
const EGL_PLATFORM_ANGLE_TYPE_D3D11_ANGLE: egl::Attrib = 0x3208;

fn main() -> Result<()> {
    color_eyre::install()?;

    let event_loop = EventLoop::new();
    let window = Window::new(&event_loop)?;

    window.set_inner_size(LogicalSize::new(800, 600));

    let hwnd = match window.raw_window_handle() {
        RawWindowHandle::Win32(handle) => handle.hwnd,
        _ => unreachable!(),
    };

    let egl = EglInstance::new(egl::Static);

    let display = egl.get_platform_display(
        EGL_PLATFORM_ANGLE_ANGLE,
        egl::DEFAULT_DISPLAY,
        &[
            EGL_PLATFORM_ANGLE_TYPE_ANGLE,
            EGL_PLATFORM_ANGLE_TYPE_D3D11_ANGLE,
            egl::NONE as egl::Attrib,
        ],
    )?;

    egl.initialize(display)?;

    let mut configs = Vec::with_capacity(1);
    let config_attribs = [
        egl::RED_SIZE,
        8,
        egl::GREEN_SIZE,
        8,
        egl::BLUE_SIZE,
        8,
        egl::ALPHA_SIZE,
        8,
        egl::DEPTH_SIZE,
        8,
        egl::STENCIL_SIZE,
        8,
        egl::NONE,
    ];

    egl.choose_config(display, &config_attribs, &mut configs)?;

    let context_attribs = [egl::CONTEXT_CLIENT_VERSION, 2, egl::NONE];
    let context = egl.create_context(display, configs[0], None, &context_attribs)?;

    let surface = unsafe { egl.create_window_surface(display, configs[0], hwnd, None)? };

    egl.make_current(display, Some(surface), Some(surface), Some(context))?;

    let gl = Box::leak(Box::new(Gl {
        egl,
        display,
        context,
        surface,
    }));

    let mut engine = ptr::null_mut();
    unsafe {
        let result = FlutterEngineRun(
            FLUTTER_ENGINE_VERSION as usize,
            &FlutterRendererConfig {
                type_: FlutterRendererType_kOpenGL,
                __bindgen_anon_1: flutter_embedder::FlutterRendererConfig__bindgen_ty_1 {
                    open_gl: FlutterOpenGLRendererConfig {
                        struct_size: mem::size_of::<FlutterOpenGLRendererConfig>(),
                        make_current: Some(gl_make_current),
                        clear_current: Some(gl_clear_current),
                        present: Some(gl_present),
                        fbo_callback: Some(gl_fbo_callback),
                        gl_proc_resolver: Some(gl_get_proc_address),
                        ..Default::default()
                    },
                },
            },
            &FlutterProjectArgs {
                struct_size: mem::size_of::<FlutterProjectArgs>(),
                assets_path: cstr!("example/build/flutter_assets"),
                icu_data_path: cstr!("icudtl.dat"),
                ..Default::default()
            },
            gl as *mut Gl as *mut c_void,
            &mut engine,
        );

        if result != FlutterEngineResult_kSuccess || engine.is_null() {
            panic!("could not run the flutter engine");
        }

        FlutterEngineSendWindowMetricsEvent(
            engine,
            &FlutterWindowMetricsEvent {
                struct_size: mem::size_of::<FlutterWindowMetricsEvent>(),
                width: (800.0 * window.scale_factor()) as usize,
                height: (600.0 * window.scale_factor()) as usize,
                pixel_ratio: window.scale_factor(),
                ..Default::default()
            },
        );
    }

    let mut cursor_pos = PhysicalPosition::new(0.0, 0.0);

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        if let Event::WindowEvent { event, .. } = event {
            match event {
                WindowEvent::CloseRequested => {
                    *control_flow = ControlFlow::Exit;
                }
                WindowEvent::Resized(size) => unsafe {
                    FlutterEngineSendWindowMetricsEvent(
                        engine,
                        &FlutterWindowMetricsEvent {
                            struct_size: mem::size_of::<FlutterWindowMetricsEvent>(),
                            width: size.width as usize,
                            height: size.height as usize,
                            pixel_ratio: window.scale_factor(),
                            ..Default::default()
                        },
                    );
                },
                WindowEvent::CursorMoved { position, .. } => unsafe {
                    cursor_pos = position;
                    FlutterEngineSendPointerEvent(
                        engine,
                        &FlutterPointerEvent {
                            struct_size: mem::size_of::<FlutterPointerEvent>(),
                            phase: FlutterPointerPhase_kMove,
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
            }
        }
    });
}

unsafe extern "C" fn gl_make_current(user_data: *mut c_void) -> bool {
    let gl = user_data.cast::<Gl>().as_mut().unwrap();
    gl.egl
        .make_current(
            gl.display,
            Some(gl.surface),
            Some(gl.surface),
            Some(gl.context),
        )
        .unwrap();
    true
}

unsafe extern "C" fn gl_clear_current(user_data: *mut c_void) -> bool {
    let gl = user_data.cast::<Gl>().as_mut().unwrap();
    gl.egl.make_current(gl.display, None, None, None).unwrap();
    true
}

unsafe extern "C" fn gl_present(user_data: *mut c_void) -> bool {
    let gl = user_data.cast::<Gl>().as_mut().unwrap();
    gl.egl.swap_buffers(gl.display, gl.surface).unwrap();
    true
}

unsafe extern "C" fn gl_fbo_callback(_user_data: *mut c_void) -> u32 {
    0
}

unsafe extern "C" fn gl_get_proc_address(
    user_data: *mut c_void,
    name: *const c_char,
) -> *mut c_void {
    let gl = user_data.cast::<Gl>().as_mut().unwrap();
    let name = CStr::from_ptr(name);
    gl.egl.get_proc_address(name.to_str().unwrap()).unwrap() as _
}
