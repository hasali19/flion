use std::ffi::{c_char, c_void, CStr};
use std::{mem, ptr};

use color_eyre::Result;
use flutter_embedder::{
    FlutterEngine, FlutterEngineResult_kSuccess, FlutterEngineRun,
    FlutterEngineSendWindowMetricsEvent, FlutterOpenGLRendererConfig, FlutterProjectArgs,
    FlutterRendererConfig, FlutterRendererType_kOpenGL, FlutterWindowMetricsEvent,
    FLUTTER_ENGINE_VERSION,
};
use khronos_egl as egl;
use windows::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect, GetMessageW,
    GetWindowLongPtrW, LoadCursorW, PostQuitMessage, RegisterClassW, SetWindowLongPtrW, ShowWindow,
    TranslateMessage, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, IDC_ARROW, MSG, SW_SHOWNORMAL,
    WM_DESTROY, WM_SIZE, WNDCLASSW, WS_OVERLAPPEDWINDOW,
};

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
    resource_context: egl::Context,
    surface: egl::Surface,
}

const EGL_PLATFORM_ANGLE_ANGLE: egl::Enum = 0x3202;
const EGL_PLATFORM_ANGLE_TYPE_ANGLE: egl::Attrib = 0x3203;
const EGL_PLATFORM_ANGLE_TYPE_D3D11_ANGLE: egl::Attrib = 0x3208;

struct WindowData {
    engine: FlutterEngine,
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
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE::default(),
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

    unsafe {
        ShowWindow(window, SW_SHOWNORMAL);
    }

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
    let resource_context =
        egl.create_context(display, configs[0], Some(context), &context_attribs)?;

    let surface = unsafe { egl.create_window_surface(display, configs[0], window.0 as _, None)? };

    egl.make_current(display, Some(surface), Some(surface), Some(context))?;

    let gl = Box::leak(Box::new(Gl {
        egl,
        display,
        context,
        resource_context,
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
                width: 800,
                height: 600,
                pixel_ratio: 1.0,
                ..Default::default()
            },
        );
    }

    unsafe {
        SetWindowLongPtrW(
            window,
            GWLP_USERDATA,
            Box::leak(Box::new(WindowData { engine })) as *mut _ as _,
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
        WM_SIZE => {
            let data = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut WindowData;
            if !data.is_null() {
                let mut rect = Default::default();
                GetClientRect(window, &mut rect).unwrap();

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
            }
        }
        _ => return DefWindowProcW(window, msg, wparam, lparam),
    }

    LRESULT(0)
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
