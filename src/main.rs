use std::ffi::{c_char, c_void, CStr};
use std::sync::{Condvar, Mutex};
use std::{mem, ptr};

use color_eyre::Result;
use egl::ClientBuffer;
use flutter_embedder::{
    FlutterEngine, FlutterEngineResult_kSuccess, FlutterEngineRun,
    FlutterEngineSendWindowMetricsEvent, FlutterOpenGLRendererConfig, FlutterProjectArgs,
    FlutterRendererConfig, FlutterRendererType_kOpenGL, FlutterWindowMetricsEvent,
    FLUTTER_ENGINE_VERSION,
};
use khronos_egl as egl;
use windows::core::ComInterface;
use windows::w;
use windows::Foundation::Numerics::Matrix3x2;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Texture2D, D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionVisual,
};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_UNKNOWN,
    DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGIDevice2, IDXGIFactory2, IDXGISwapChain1, DXGI_SWAP_CHAIN_DESC1,
    DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect, GetMessageW,
    GetWindowLongPtrW, LoadCursorW, PostQuitMessage, RegisterClassW, SetWindowLongPtrW, ShowWindow,
    TranslateMessage, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, IDC_ARROW, MSG, SW_SHOWNORMAL,
    WM_DESTROY, WM_ERASEBKGND, WM_NCCALCSIZE, WNDCLASSW, WS_EX_NOREDIRECTIONBITMAP,
    WS_OVERLAPPEDWINDOW,
};

macro_rules! cstr {
    ($v:literal) => {
        concat!($v, "\0").as_ptr() as *const std::ffi::c_char
    };
}

type EglInstance = egl::Instance<egl::Static>;

enum ResizeState {
    Started(u32, u32),
    FrameGenerated(u32),
    Done,
}

struct Gl {
    egl: EglInstance,
    display: egl::Display,
    context: egl::Context,
    resource_context: egl::Context,
    surface: egl::Surface,
    // debug: ID3D11Debug,
    swapchain: IDXGISwapChain1,
    config: egl::Config,
    resize_condvar: Condvar,
    resize_state: Mutex<ResizeState>,
    composition_device: IDCompositionDevice,
    root_visual: IDCompositionVisual,
}

const EGL_PLATFORM_ANGLE_ANGLE: egl::Enum = 0x3202;
const EGL_PLATFORM_ANGLE_TYPE_ANGLE: egl::Attrib = 0x3203;
const EGL_PLATFORM_ANGLE_TYPE_D3D11_ANGLE: egl::Attrib = 0x3208;

struct WindowData {
    engine: FlutterEngine,
    gl: *mut Gl,
}

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
    error: egl::Enum,
    command: *const c_char,
    message_type: egl::Int,
    thread_label: *const c_void,
    object_label: *const c_void,
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

    let (device, context) = unsafe {
        let mut device = None;
        let mut context = None;

        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            None,
            D3D11_CREATE_DEVICE_FLAG::default(),
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;

        (device.unwrap(), context.unwrap())
    };

    let dxgi_factory = unsafe {
        device
            .cast::<IDXGIDevice2>()?
            .GetAdapter()?
            .GetParent::<IDXGIFactory2>()?
    };

    let swapchain_desc = DXGI_SWAP_CHAIN_DESC1 {
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Width: width as u32,
        Height: height as u32,
        AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
        ..Default::default()
    };

    let swapchain =
        unsafe { dxgi_factory.CreateSwapChainForComposition(&device, &swapchain_desc, None)? };

    // let swapchain_composition_surface = unsafe {
    //     compositor
    //         .cast::<ICompositorInterop>()?
    //         .CreateCompositionSurfaceForSwapChain(&swapchain)?
    // };

    let dcomp: IDCompositionDevice = unsafe { DCompositionCreateDevice(None)? };
    let target = unsafe { dcomp.CreateTargetForHwnd(window, true)? };
    let root = unsafe { dcomp.CreateVisual()? };
    unsafe {
        root.SetOffsetY2(height as f32).unwrap();
        root.SetTransform2(&Matrix3x2 {
            M11: 1.0,
            M21: 0.0,
            M31: 0.0,
            M12: 0.0,
            M22: -1.0,
            M32: 0.0,
        })
        .unwrap();

        target.SetRoot(&root)?;
        root.SetContent(&swapchain)?;
        dcomp.Commit()?;
    }

    // let brush = compositor.CreateSurfaceBrushWithSurface(&swapchain_composition_surface)?;
    // root.SetBrush(&brush)?;

    let swapchain_back_buffer = unsafe { swapchain.GetBuffer::<ID3D11Texture2D>(0)? };

    let egl = EglInstance::new(egl::Static);

    let attribs = [egl::NONE as egl::Attrib];
    unsafe { eglDebugMessageControlKHR(debug_callback, attribs.as_ptr()) };

    let angle_device =
        unsafe { eglCreateDeviceANGLE(0x33A1, mem::transmute_copy(&device), ptr::null()) };

    let display = egl.get_platform_display(
        0x313f,
        angle_device,
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

    let client_buffer =
        unsafe { ClientBuffer::from_ptr(mem::transmute_copy(&swapchain_back_buffer)) };

    let surface = egl.create_pbuffer_from_client_buffer(
        display,
        0x33A3,
        client_buffer,
        configs[0],
        &[egl::NONE],
    )?;

    drop(swapchain_back_buffer);

    egl.make_current(display, Some(surface), Some(surface), Some(context))?;

    gl::Flush::load_with(|name| egl.get_proc_address(name).unwrap() as _);

    let gl = Box::leak(Box::new(Gl {
        egl,
        display,
        context,
        resource_context,
        surface,
        swapchain,
        config: configs[0],
        resize_condvar: Condvar::new(),
        resize_state: Mutex::new(ResizeState::Done),
        composition_device: dcomp,
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
        ResizeState::Started(_, _) => return false,
        ResizeState::FrameGenerated(height) => {
            gl.root_visual.SetOffsetY2(height as f32).unwrap();
            gl.root_visual
                .SetTransform2(&Matrix3x2 {
                    M11: 1.0,
                    M21: 0.0,
                    M31: 0.0,
                    M12: 0.0,
                    M22: -1.0,
                    M32: 0.0,
                })
                .unwrap();
            gl.composition_device.Commit().unwrap();
            gl::Flush();
            gl.egl.swap_buffers(gl.display, gl.surface).unwrap();
            gl.swapchain.Present(0, 0).unwrap();
            *resize_state = ResizeState::Done;
            drop(resize_state);
            gl.resize_condvar.notify_all();
            DwmFlush().unwrap();
        }
        ResizeState::Done => {
            gl::Flush();
            gl.egl.swap_buffers(gl.display, gl.surface).unwrap();
            gl.swapchain.Present(0, 0).unwrap();
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

        gl.swapchain
            .ResizeBuffers(2, width, height, DXGI_FORMAT_UNKNOWN, 0)
            .unwrap();

        let swapchain_back_buffer = gl.swapchain.GetBuffer::<ID3D11Texture2D>(0).unwrap();

        let client_buffer =
            unsafe { ClientBuffer::from_ptr(mem::transmute_copy(&swapchain_back_buffer)) };

        gl.surface = gl
            .egl
            .create_pbuffer_from_client_buffer(
                gl.display,
                0x33A3,
                client_buffer,
                gl.config,
                &[egl::NONE],
            )
            .unwrap();

        gl.egl
            .make_current(
                gl.display,
                Some(gl.surface),
                Some(gl.surface),
                Some(gl.context),
            )
            .unwrap();

        *resize_state = ResizeState::FrameGenerated(height);
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
