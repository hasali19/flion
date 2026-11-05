use std::ffi::c_void;
use std::sync::Arc;

use khronos_egl as egl;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tracing::error;
use wayland_client::Connection;
use wayland_egl::WlEglSurface;

#[derive(Clone)]
pub struct EglDisplay(Arc<EglDisplayInner>);

struct EglDisplayInner {
    display: egl::Display,
    // Keep the connection open for the lifetime of EglDisplay
    _conn: Connection,
}

unsafe impl Send for EglDisplayInner {}

unsafe impl Sync for EglDisplayInner {}

impl Drop for EglDisplayInner {
    fn drop(&mut self) {
        if let Err(e) = EGL.terminate(self.display) {
            error!("Failed to terminate EGL: {e}");
        }
    }
}

impl EglDisplay {
    pub fn for_wayland_connection(conn: &Connection) -> eyre::Result<Self> {
        // TODO: In theory multiple EglDisplays could be created for the same wayland connection in
        // which case dropping one of them invalidates the rest. We should enforce one per connection
        // or do some clever ref counting.
        let display = unsafe { EGL.get_display(conn.backend().display_ptr().cast()) }
            .ok_or_else(|| eyre::eyre!("failed to get EGL display"))?;

        EGL.initialize(display)?;

        gl::load_with(|name| {
            EGL.get_proc_address(name)
                .map_or(std::ptr::null(), |f| f as *const c_void)
        });

        Ok(EglDisplay(Arc::new(EglDisplayInner {
            display,
            _conn: conn.clone(),
        })))
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
            .choose_first_config(self.0.display, &config_attribs)?
            .ok_or_else(|| eyre::eyre!("failed to find a suitable EGL configuration"))?;

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
            .choose_first_config(self.0.display, &config_attribs)?
            .ok_or_else(|| eyre::eyre!("failed to find a suitable EGL configuration"))?;

        let window_handle = window.window_handle()?;
        let RawWindowHandle::Wayland(handle) = window_handle.as_raw() else {
            eyre::bail!("unsupported window handle");
        };

        let wl_egl_surface = unsafe {
            WlEglSurface::new_from_raw(handle.surface.as_ptr().cast(), width as i32, height as i32)
        }?;

        let egl_surface = unsafe {
            EGL.create_window_surface(
                self.0.display,
                config,
                wl_egl_surface.ptr() as egl::NativeWindowType,
                None,
            )
        }?;

        Ok(EglSurface {
            display: self.0.clone(),
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

        EGL.make_current(self.0.display, surface, surface, context.map(|c| c.context))?;

        Ok(())
    }

    pub fn swap_buffers(&self, surface: &EglSurface) -> eyre::Result<()> {
        EGL.swap_buffers(self.0.display, surface.surface)?;
        Ok(())
    }
}

pub struct EglContext {
    display: Arc<EglDisplayInner>,
    context: egl::Context,
}

pub struct EglSurface {
    display: Arc<EglDisplayInner>,
    surface: egl::Surface,
    wl_egl_surface: WlEglSurface,
    _window: Box<dyn HasWindowHandle + Send + Sync + 'static>,
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
