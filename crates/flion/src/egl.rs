use std::cell::Cell;
use std::ffi::c_void;

use khronos_egl as egl;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Proxy};
use wayland_egl::WlEglSurface;

pub struct EglContext {
    display: egl::Display,
    config: egl::Config,
    context: egl::Context,
    gl_loaded: Cell<bool>,
}

pub struct EglSurface {
    surface: egl::Surface,
    wl_egl_surface: WlEglSurface,
}

impl EglSurface {
    pub fn resize(&self, width: u32, height: u32) {
        self.wl_egl_surface
            .resize(width as i32, height as i32, 0, 0);
    }
}

static EGL: egl::Instance<egl::Static> = egl::Instance::new(egl::Static);

impl EglContext {
    pub fn new(conn: &Connection) -> eyre::Result<Self> {
        let display = unsafe { EGL.get_display(conn.backend().display_ptr() as *mut c_void) }
            .ok_or_else(|| eyre::eyre!("failed to get EGL display"))?;

        EGL.initialize(display)?;

        let config_attribs = [
            egl::SURFACE_TYPE,
            egl::WINDOW_BIT,
            egl::RENDERABLE_TYPE,
            egl::OPENGL_BIT,
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
            .choose_first_config(display, &config_attribs)?
            .ok_or_else(|| eyre::eyre!("failed to find a suitable EGL configuration"))?;

        EGL.bind_api(egl::OPENGL_API)?;

        let context_attribs = [
            egl::CONTEXT_MAJOR_VERSION,
            4,
            egl::CONTEXT_MINOR_VERSION,
            5,
            egl::CONTEXT_OPENGL_PROFILE_MASK,
            egl::CONTEXT_OPENGL_CORE_PROFILE_BIT,
            egl::NONE,
        ];

        let context = EGL.create_context(display, config, None, &context_attribs)?;

        Ok(Self {
            display,
            config,
            context,
            gl_loaded: Cell::new(false),
        })
    }

    pub fn create_surface(
        &self,
        surface: &WlSurface,
        width: u32,
        height: u32,
    ) -> eyre::Result<EglSurface> {
        let wl_egl_surface = WlEglSurface::new(surface.id(), width as i32, height as i32)?;

        let egl_surface = unsafe {
            EGL.create_window_surface(
                self.display,
                self.config,
                wl_egl_surface.ptr() as egl::NativeWindowType,
                None,
            )
        }?;

        Ok(EglSurface {
            surface: egl_surface,
            wl_egl_surface,
        })
    }

    pub fn destroy_surface(&self, surface: EglSurface) -> eyre::Result<()> {
        EGL.destroy_surface(self.display, surface.surface)?;
        Ok(())
    }

    pub fn make_current(&self, surface: &EglSurface) -> eyre::Result<()> {
        EGL.make_current(
            self.display,
            Some(surface.surface),
            Some(surface.surface),
            Some(self.context),
        )?;

        if !self.gl_loaded.replace(true) {
            gl::load_with(|name| {
                EGL.get_proc_address(name)
                    .map_or(std::ptr::null(), |f| f as *const c_void)
            });
        }

        Ok(())
    }

    pub fn swap_buffers(&self, surface: &EglSurface) -> eyre::Result<()> {
        EGL.swap_buffers(self.display, surface.surface)?;
        Ok(())
    }
}
