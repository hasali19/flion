use std::ffi::c_void;

use khronos_egl as egl;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use wayland_client::Connection;
use wayland_egl::WlEglSurface;

#[derive(Clone)]
pub struct EglDisplay {
    display: egl::Display,
}

unsafe impl Send for EglDisplay {}

unsafe impl Sync for EglDisplay {}

impl EglDisplay {
    pub fn for_wayland_connection(conn: &Connection) -> eyre::Result<Self> {
        // FIXME: Probably unsound?
        let display = unsafe { EGL.get_display(conn.backend().display_ptr() as *mut c_void) }
            .ok_or_else(|| eyre::eyre!("failed to get EGL display"))?;

        // TODO: Call eglTerminate somwhere?

        EGL.initialize(display)?;

        gl::load_with(|name| {
            EGL.get_proc_address(name)
                .map_or(std::ptr::null(), |f| f as *const c_void)
        });

        Ok(EglDisplay { display })
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
        let config_attribs = [
            egl::RED_SIZE,
            8,
            egl::GREEN_SIZE,
            8,
            egl::BLUE_SIZE,
            8,
            egl::ALPHA_SIZE,
            8,
            egl::NONE,
        ];

        let config = EGL
            .choose_first_config(self.display, &config_attribs)?
            .ok_or_else(|| eyre::eyre!("failed to find a suitable EGL configuration"))?;

        let context_attribs = [egl::CONTEXT_CLIENT_VERSION, 2, egl::NONE];

        let context = EGL.create_context(
            self.display,
            config,
            share_context.map(|c| c.context),
            &context_attribs,
        )?;

        Ok(EglContext {
            display: self.clone(),
            context,
        })
    }

    pub fn create_window_surface(
        &self,
        window: impl HasWindowHandle + Send + Sync + 'static,
        width: u32,
        height: u32,
    ) -> eyre::Result<EglSurface> {
        let config_attribs = [
            egl::RED_SIZE,
            8,
            egl::GREEN_SIZE,
            8,
            egl::BLUE_SIZE,
            8,
            egl::ALPHA_SIZE,
            8,
            egl::NONE,
        ];

        let config = EGL
            .choose_first_config(self.display, &config_attribs)?
            .ok_or_else(|| eyre::eyre!("failed to find a suitable EGL configuration"))?;

        let RawWindowHandle::Wayland(handle) = window.window_handle()?.as_raw() else {
            eyre::bail!("unsupported window handle");
        };

        let wl_egl_surface = unsafe {
            WlEglSurface::new_from_raw(handle.surface.as_ptr().cast(), width as i32, height as i32)
        }?;

        let egl_surface = unsafe {
            EGL.create_window_surface(
                self.display,
                config,
                wl_egl_surface.ptr() as egl::NativeWindowType,
                None,
            )
        }?;

        Ok(EglSurface {
            display: self.clone(),
            surface: egl_surface,
            wl_egl_surface,
            _window: Box::new(window),
        })
    }

    pub fn make_current(
        &self,
        context: Option<&EglContext>,
        surface: Option<&EglSurface>,
    ) -> eyre::Result<()> {
        let surface = surface.map(|s| s.surface);

        EGL.make_current(self.display, surface, surface, context.map(|c| c.context))?;

        Ok(())
    }

    pub fn swap_buffers(&self, surface: &EglSurface) -> eyre::Result<()> {
        EGL.swap_buffers(self.display, surface.surface)?;
        Ok(())
    }
}

pub struct EglContext {
    display: EglDisplay,
    context: egl::Context,
}

pub struct EglSurface {
    display: EglDisplay,
    surface: egl::Surface,
    wl_egl_surface: WlEglSurface,
    _window: Box<dyn HasWindowHandle + Send + Sync>,
}

// SAFETY: All fields implement Send except egl::Surface, which is safe to send between threads.
unsafe impl Send for EglSurface {}

impl EglSurface {
    pub fn resize(&self, width: u32, height: u32) {
        self.wl_egl_surface
            .resize(width as i32, height as i32, 0, 0);
    }
}

impl Drop for EglSurface {
    fn drop(&mut self) {
        if let Err(e) = EGL.destroy_surface(self.display.display, self.surface) {
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
