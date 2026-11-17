use std::cell::Cell;
use std::ffi::c_void;
use std::num::NonZeroIsize;
use std::sync::Arc;

use egl::ClientBuffer;
use eyre::bail;
use khronos_egl as egl;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tracing::error;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::core::Interface;

const EGL_PLATFORM_DEVICE_EXT: egl::Enum = 0x313F;

const EGL_D3D11_DEVICE_ANGLE: egl::Int = 0x33A1;
const EGL_D3D_TEXTURE_ANGLE: egl::Enum = 0x33A3;

const EGL_TEXTURE_OFFSET_X_ANGLE: i32 = 0x3490;
const EGL_TEXTURE_OFFSET_Y_ANGLE: i32 = 0x3491;

const EGL_DIRECT_COMPOSITION_ANGLE: egl::Int = 0x33A5;
const EGL_FIXED_SIZE_ANGLE: egl::Int = 0x3201;

#[derive(Clone)]
pub struct EglDisplay(Arc<EglDisplayInner>);

struct EglDisplayInner {
    display: egl::Display,
    angle_device: *mut c_void,
}

unsafe impl Send for EglDisplayInner {}

unsafe impl Sync for EglDisplayInner {}

impl Drop for EglDisplayInner {
    fn drop(&mut self) {
        if let Err(e) = EGL.terminate(self.display) {
            error!("Failed to terminate EGL: {e}");
        }

        unsafe { eglReleaseDeviceANGLE(self.angle_device) }
    }
}

impl EglDisplay {
    pub fn for_d3d11_device(device: &ID3D11Device) -> eyre::Result<Self> {
        let angle_device = unsafe {
            eglCreateDeviceANGLE(EGL_D3D11_DEVICE_ANGLE, device.as_raw(), &egl::ATTRIB_NONE)
        };

        if angle_device.is_null() {
            bail!("failed to create angle device");
        }

        let display = match unsafe {
            EGL.get_platform_display(EGL_PLATFORM_DEVICE_EXT, angle_device, &[egl::ATTRIB_NONE])
        } {
            Ok(display) => display,
            Err(e) => {
                unsafe { eglReleaseDeviceANGLE(angle_device) };
                return Err(e.into());
            }
        };

        let inner = EglDisplayInner {
            display,
            angle_device,
        };

        EGL.initialize(display)?;

        gl::load_with(|name| {
            EGL.get_proc_address(name)
                .map_or(std::ptr::null(), |f| f as *const c_void)
        });

        Ok(EglDisplay(Arc::new(inner)))
    }

    pub fn get_proc_address(&self, name: &str) -> Option<extern "system" fn()> {
        EGL.get_proc_address(name)
    }

    pub fn create_context(&self) -> eyre::Result<EglContext> {
        self.create_context_impl(None)
    }

    pub fn create_shared_context(&self, share_context: &EglContext) -> eyre::Result<EglContext> {
        self.create_context_impl(Some(share_context))
    }

    fn create_context_impl(&self, share_context: Option<&EglContext>) -> eyre::Result<EglContext> {
        let config = self.choose_config()?;

        let context_attribs = [egl::CONTEXT_CLIENT_VERSION, 2, egl::NONE];

        let context = EGL.create_context(
            self.0.display,
            config,
            share_context.map(|c| c.context),
            &context_attribs,
        )?;

        Ok(EglContext {
            display: self.0.clone(),
            context,
        })
    }

    fn choose_config(&self) -> eyre::Result<egl::Config> {
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
            egl::SURFACE_TYPE,
            egl::WINDOW_BIT | egl::PBUFFER_BIT,
            egl::NONE,
        ];

        EGL.choose_first_config(self.0.display, &config_attribs)?
            .ok_or_else(|| eyre::eyre!("failed to find a suitable EGL configuration"))
    }

    pub fn create_window_surface(
        &self,
        window: impl HasWindowHandle + Send + Sync + 'static,
        width: u32,
        height: u32,
    ) -> eyre::Result<EglSurface> {
        let config = self.choose_config()?;

        let window_handle = window.window_handle()?;
        let RawWindowHandle::Win32(handle) = window_handle.as_raw() else {
            eyre::bail!("unsupported window handle");
        };

        let hwnd = handle.hwnd;

        let direct_composition = self.has_extension("EGL_ANGLE_direct_composition");
        if !direct_composition {
            tracing::warn!("EGL_ANGLE_direct_composition is not supported");
        }

        let egl_surface = create_angle_window_surface(
            self.0.display,
            config,
            hwnd,
            direct_composition,
            (width, height),
        )?;

        Ok(EglSurface {
            display: self.0.clone(),
            config,
            surface: Cell::new(Some(egl_surface)),
            window: Some(WindowSurface {
                hwnd,
                direct_composition,
                size: Cell::new((width, height)),
                _window: Box::new(window),
            }),
        })
    }

    pub fn create_surface_from_d3d11_texture(
        &self,
        texture: &ID3D11Texture2D,
        offset: (i32, i32),
    ) -> eyre::Result<EglSurface> {
        let config = self.choose_config()?;
        let buffer = unsafe { ClientBuffer::from_ptr(texture.as_raw()) };

        let surface = EGL.create_pbuffer_from_client_buffer(
            self.0.display,
            EGL_D3D_TEXTURE_ANGLE,
            buffer,
            config,
            &[
                egl::TEXTURE_FORMAT,
                egl::TEXTURE_RGBA,
                egl::TEXTURE_TARGET,
                egl::TEXTURE_2D,
                EGL_TEXTURE_OFFSET_X_ANGLE,
                offset.0,
                EGL_TEXTURE_OFFSET_Y_ANGLE,
                offset.1,
                egl::NONE,
            ],
        )?;

        Ok(EglSurface {
            display: self.0.clone(),
            config,
            surface: Cell::new(Some(surface)),
            window: None,
        })
    }

    fn has_extension(&self, name: &str) -> bool {
        EGL.query_string(Some(self.0.display), egl::EXTENSIONS)
            .map(|extensions| {
                extensions
                    .to_bytes()
                    .split(|&b| b == b' ')
                    .any(|ext| ext == name.as_bytes())
            })
            .unwrap_or(false)
    }

    pub fn make_current(
        &self,
        context: Option<&EglContext>,
        surface: Option<&EglSurface>,
    ) -> eyre::Result<()> {
        let surface = surface.and_then(|s| s.surface.get());

        EGL.make_current(self.0.display, surface, surface, context.map(|c| c.context))?;

        Ok(())
    }

    pub fn swap_buffers(&self, surface: &EglSurface) -> eyre::Result<()> {
        let Some(surface) = surface.surface.get() else {
            eyre::bail!("surface is not available");
        };
        EGL.swap_buffers(self.0.display, surface)?;
        Ok(())
    }
}

pub struct EglContext {
    display: Arc<EglDisplayInner>,
    context: egl::Context,
}

// SAFETY: EGL context handles can be used from any thread. EGL itself prevents a context from being
// current on multiple threads at once.
unsafe impl Send for EglContext {}
unsafe impl Sync for EglContext {}

pub struct EglSurface {
    display: Arc<EglDisplayInner>,
    config: egl::Config,
    surface: Cell<Option<egl::Surface>>,
    // Only set for window surfaces.
    window: Option<WindowSurface>,
}

struct WindowSurface {
    hwnd: NonZeroIsize,
    direct_composition: bool,
    size: Cell<(u32, u32)>,
    _window: Box<dyn HasWindowHandle + Send + Sync + 'static>,
}

// SAFETY: All fields implement Send except the EGL handles, which are safe to send between threads.
unsafe impl Send for EglSurface {}

impl EglSurface {
    pub fn resize(&self, width: u32, height: u32) {
        let Some(window) = &self.window else {
            return;
        };

        if window.size.get() == (width, height) {
            return;
        }

        tracing::debug!(?width, ?height, "resizing EGL surface");

        // Only one window surface can exist for a window at a time, so the old surface must be
        // destroyed first.
        if let Some(old_surface) = self.surface.take()
            && let Err(e) = EGL.destroy_surface(self.display.display, old_surface)
        {
            tracing::error!("failed to destroy EGL surface: {e}");
        }

        match create_angle_window_surface(
            self.display.display,
            self.config,
            window.hwnd,
            window.direct_composition,
            (width, height),
        ) {
            Ok(surface) => {
                self.surface.set(Some(surface));
                window.size.set((width, height));
            }
            Err(e) => {
                tracing::error!("failed to resize EGL surface: {e:?}");
            }
        }
    }
}

fn create_angle_window_surface(
    display: egl::Display,
    config: egl::Config,
    hwnd: NonZeroIsize,
    direct_composition: bool,
    (width, height): (u32, u32),
) -> eyre::Result<egl::Surface> {
    let mut attribs = vec![
        EGL_FIXED_SIZE_ANGLE,
        egl::TRUE as egl::Int,
        egl::WIDTH,
        width.max(1) as egl::Int,
        egl::HEIGHT,
        height.max(1) as egl::Int,
    ];

    if direct_composition {
        attribs.extend([EGL_DIRECT_COMPOSITION_ANGLE, egl::TRUE as egl::Int]);
    }

    attribs.push(egl::NONE);

    let surface = unsafe {
        EGL.create_window_surface(
            display,
            config,
            hwnd.get() as egl::NativeWindowType,
            Some(&attribs),
        )
    }?;

    Ok(surface)
}

impl Drop for EglSurface {
    fn drop(&mut self) {
        if let Some(surface) = self.surface.get()
            && let Err(e) = EGL.destroy_surface(self.display.display, surface)
        {
            tracing::error!("failed to destroy EGL surface: {e}");
        }
    }
}

static EGL: egl::Instance<egl::Static> = egl::Instance::new(egl::Static);

impl EglContext {
    pub fn make_current(&self) -> eyre::Result<()> {
        EGL.make_current(
            self.display.display,
            EGL.get_current_surface(egl::DRAW),
            EGL.get_current_surface(egl::READ),
            Some(self.context),
        )?;

        Ok(())
    }
}

impl Drop for EglContext {
    fn drop(&mut self) {
        if let Err(e) = EGL.destroy_context(self.display.display, self.context) {
            tracing::error!("failed to destroy EGL context: {e}");
        }
    }
}

unsafe extern "C" {
    // fn eglDebugMessageControlKHR(
    //     callback: extern "C" fn(
    //         error: egl::Enum,
    //         command: *const std::ffi::c_char,
    //         message_type: egl::Int,
    //         thread_label: *const c_void,
    //         object_label: *const c_void,
    //         message: *const std::ffi::c_char,
    //     ),
    //     attribs: *const egl::Attrib,
    // ) -> egl::Int;

    fn eglCreateDeviceANGLE(
        device_type: egl::Int,
        native_device: *mut c_void,
        attrib_list: *const egl::Attrib,
    ) -> *mut c_void;

    fn eglReleaseDeviceANGLE(device: *mut c_void);
}

// extern "C" fn debug_callback(
//     error: egl::Enum,
//     command: *const std::ffi::c_char,
//     message_type: egl::Int,
//     thread_label: *const c_void,
//     object_label: *const c_void,
//     message: *const std::ffi::c_char,
// ) {
//     use std::ffi::CStr;
//     let message = unsafe { CStr::from_ptr(message) };
//     let message = message.to_str().unwrap_or("invalid");
//     tracing::debug!("{message}");
// }
