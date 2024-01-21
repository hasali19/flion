use std::ffi::{c_char, c_void, CStr};
use std::sync::{Condvar, Mutex};
use std::{mem, ptr};

use color_eyre::Result;
use flutter_embedder::{
    FlutterEngine, FlutterEngineResult_kSuccess, FlutterEngineRun,
    FlutterEngineSendWindowMetricsEvent, FlutterOpenGLRendererConfig, FlutterProjectArgs,
    FlutterRendererConfig, FlutterRendererType_kOpenGL, FlutterWindowMetricsEvent,
    FLUTTER_ENGINE_VERSION,
};
use khronos_egl as egl;
use windows::core::{ComInterface, Interface};
use windows::w;
use windows::Foundation::Numerics::Vector2;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::System::WinRT::{
    CreateDispatcherQueueController, DispatcherQueueOptions, DQTAT_COM_ASTA, DQTYPE_THREAD_CURRENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect, GetMessageW,
    GetWindowLongPtrW, LoadCursorW, PostQuitMessage, RegisterClassW, SetWindowLongPtrW, ShowWindow,
    TranslateMessage, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, IDC_ARROW, MSG, SW_SHOWNORMAL,
    WM_DESTROY, WM_ERASEBKGND, WM_NCCALCSIZE, WNDCLASSW, WS_EX_NOREDIRECTIONBITMAP,
    WS_OVERLAPPEDWINDOW,
};
use windows::UI::Composition::{Compositor, SpriteVisual};

macro_rules! cstr {
    ($v:literal) => {
        concat!($v, "\0").as_ptr() as *const std::ffi::c_char
    };
}

type EglInstance = egl::Instance<egl::Static>;

enum ResizeState {
    Started(u32, u32),
    FrameGenerated,
    Done,
}

struct Gl {
    egl: EglInstance,
    display: egl::Display,
    context: egl::Context,
    resource_context: egl::Context,
    surface: egl::Surface,
    config: egl::Config,
    resize_condvar: Condvar,
    resize_state: Mutex<ResizeState>,
    root_visual: SpriteVisual,
}

const EGL_PLATFORM_ANGLE_ANGLE: egl::Enum = 0x3202;
const EGL_PLATFORM_ANGLE_TYPE_ANGLE: egl::Attrib = 0x3203;
const EGL_PLATFORM_ANGLE_TYPE_D3D11_ANGLE: egl::Attrib = 0x3208;

struct WindowData {
    engine: FlutterEngine,
    gl: *mut Gl,
}

#[allow(unused)]
extern "C" {
    fn eglDebugMessageControlKHR(
        callback: extern "C" fn(
            egl::Enum,
            *const c_char,
            egl::Int,
            *const c_void,
            *const c_void,
            *const c_char,
        ),
        attribs: *const egl::Attrib,
    ) -> egl::Int;

    fn eglCreateDeviceANGLE(
        device_type: egl::Int,
        native_device: *mut c_void,
        attrib_list: *const egl::Attrib,
    ) -> *mut c_void;

    fn eglReleaseDeviceANGLE(device: *mut c_void);
}

extern "C" fn debug_callback(
    _error: egl::Enum,
    _command: *const c_char,
    _message_type: egl::Int,
    _thread_label: *const c_void,
    _object_label: *const c_void,
    message: *const c_char,
) {
    let message = unsafe { CStr::from_ptr(message) };
    let message = message.to_str().unwrap();
    eprintln!("{message}");
}

fn main() -> Result<()> {
    color_eyre::install()?;

    let window = unsafe {
        let window_class = WNDCLASSW {
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap(),
            lpszClassName: w!("window_class"),
            style: CS_HREDRAW | CS_VREDRAW,
            hInstance: GetModuleHandleW(None).unwrap(),
            lpfnWndProc: Some(wnd_proc),
            ..Default::default()
        };

        RegisterClassW(&window_class);

        CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP,
            w!("window_class"),
            w!("Flutter Window"),
            WS_OVERLAPPEDWINDOW,
            300,
            300,
            800,
            600,
            None,
            None,
            GetModuleHandleW(None).unwrap(),
            None,
        )
    };

    let (width, height) = unsafe {
        ShowWindow(window, SW_SHOWNORMAL);
        let mut rect = RECT::default();
        GetClientRect(window, &mut rect).unwrap();
        (rect.right - rect.left, rect.bottom - rect.top)
    };

    println!("{width} {height}");

    let _dispatcher_queue_controller = unsafe {
        CreateDispatcherQueueController(DispatcherQueueOptions {
            dwSize: mem::size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_ASTA,
        })?
    };

    let compositor = Compositor::new()?;
    let composition_target = unsafe {
        compositor
            .cast::<ICompositorDesktopInterop>()?
            .CreateDesktopWindowTarget(window, false)?
    };

    let root = compositor.CreateSpriteVisual()?;
    root.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })?;
    composition_target.SetRoot(&root)?;

    let egl = EglInstance::new(egl::Static);

    let attribs = [egl::NONE as egl::Attrib];
    unsafe { eglDebugMessageControlKHR(debug_callback, attribs.as_ptr()) };

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
    let resource_context =
        egl.create_context(display, configs[0], Some(context), &context_attribs)?;

    let surface = unsafe {
        egl.create_window_surface(
            display,
            configs[0],
            root.as_raw(),
            Some(&[
                0x3201,
                egl::TRUE as _,
                egl::WIDTH,
                width,
                egl::HEIGHT,
                height,
                egl::NONE,
            ]),
        )?
    };

    egl.make_current(display, Some(surface), Some(surface), Some(context))?;

    gl::Flush::load_with(|name| egl.get_proc_address(name).unwrap() as _);

    let gl = Box::leak(Box::new(Gl {
        // device,
        egl,
        display,
        context,
        resource_context,
        surface,
        config: configs[0],
        resize_condvar: Condvar::new(),
        resize_state: Mutex::new(ResizeState::Done),
        root_visual: root,
    }));

    let engine = unsafe { create_engine(gl, width, height) };

    unsafe {
        SetWindowLongPtrW(
            window,
            GWLP_USERDATA,
            Box::leak(Box::new(WindowData { engine, gl })) as *mut _ as _,
        );
    }

    unsafe {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    Ok(())
}

unsafe extern "system" fn wnd_proc(
    window: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_DESTROY => PostQuitMessage(0),
        WM_ERASEBKGND => {
            return LRESULT(1);
        }
        WM_NCCALCSIZE => {
            DefWindowProcW(window, msg, wparam, lparam);

            let rect = lparam.0 as *const RECT;
            let rect = rect.as_ref().unwrap();

            let data = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut WindowData;
            if !data.is_null() && rect.right > rect.left && rect.bottom > rect.top {
                let mut resize_state = (*(*data).gl).resize_state.lock().unwrap();

                *resize_state = ResizeState::Started(
                    (rect.right - rect.left) as u32,
                    (rect.bottom - rect.top) as u32,
                );

                FlutterEngineSendWindowMetricsEvent(
                    (*data).engine,
                    &FlutterWindowMetricsEvent {
                        struct_size: mem::size_of::<FlutterWindowMetricsEvent>(),
                        width: (rect.right - rect.left) as usize,
                        height: (rect.bottom - rect.top) as usize,
                        pixel_ratio: 1.0,
                        ..Default::default()
                    },
                );

                let _unused = (*(*data).gl)
                    .resize_condvar
                    .wait_while(resize_state, |resize_state| {
                        !matches!(resize_state, ResizeState::Done)
                    })
                    .unwrap();
            }
        }
        _ => return DefWindowProcW(window, msg, wparam, lparam),
    }

    LRESULT(0)
}

unsafe fn create_engine(gl: &mut Gl, width: i32, height: i32) -> FlutterEngine {
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
                        make_resource_current: Some(gl_make_resource_current),
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
                width: width as usize,
                height: height as usize,
                pixel_ratio: 1.0,
                ..Default::default()
            },
        );
    }
    engine
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

unsafe extern "C" fn gl_make_resource_current(user_data: *mut c_void) -> bool {
    let gl = user_data.cast::<Gl>().as_mut().unwrap();
    gl.egl
        .make_current(gl.display, None, None, Some(gl.resource_context))
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
    let mut resize_state = gl.resize_state.lock().unwrap();

    match *resize_state {
        ResizeState::Started(_, _) => panic!("present called before fbo_callback during resize"),
        ResizeState::FrameGenerated => {
            gl::Flush();
            gl.egl.swap_buffers(gl.display, gl.surface).unwrap();
            DwmFlush().unwrap();
            *resize_state = ResizeState::Done;
            drop(resize_state);
            gl.resize_condvar.notify_all();
        }
        ResizeState::Done => {
            gl::Flush();
            gl.egl.swap_buffers(gl.display, gl.surface).unwrap();
        }
    }

    true
}

unsafe extern "C" fn gl_fbo_callback(user_data: *mut c_void) -> u32 {
    let gl = user_data.cast::<Gl>().as_mut().unwrap();
    let mut resize_state = gl.resize_state.lock().unwrap();

    if let ResizeState::Started(width, height) = *resize_state {
        gl.egl.destroy_surface(gl.display, gl.surface).unwrap();
        gl.egl
            .make_current(gl.display, None, None, Some(gl.context))
            .unwrap();

        gl.surface = unsafe {
            gl.egl
                .create_window_surface(
                    gl.display,
                    gl.config,
                    gl.root_visual.as_raw(),
                    Some(&[
                        0x3201,
                        egl::TRUE as _,
                        egl::WIDTH,
                        width as i32,
                        egl::HEIGHT,
                        height as i32,
                        egl::NONE,
                    ]),
                )
                .unwrap()
        };

        gl.egl
            .make_current(
                gl.display,
                Some(gl.surface),
                Some(gl.surface),
                Some(gl.context),
            )
            .unwrap();

        *resize_state = ResizeState::FrameGenerated;
    }

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
