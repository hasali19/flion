use std::ffi::c_void;
use std::ptr;
use std::sync::Arc;

use egl::ClientBuffer;
use eyre::bail;
use khronos_egl as egl;
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};

const EGL_PLATFORM_DEVICE_EXT: egl::Enum = 0x313F;

const EGL_D3D11_DEVICE_ANGLE: egl::Int = 0x33A1;
const EGL_D3D_TEXTURE_ANGLE: egl::Enum = 0x33A3;

const EGL_TEXTURE_OFFSET_X_ANGLE: i32 = 0x3490;
const EGL_TEXTURE_OFFSET_Y_ANGLE: i32 = 0x3491;

pub struct EglManager {
    egl: egl::Instance<egl::Static>,
    angle_device: *mut c_void,
    display: egl::Display,
    config: egl::Config,
    context: egl::Context,
    resource_context: egl::Context,
}

unsafe impl Send for EglManager {}
unsafe impl Sync for EglManager {}

impl EglManager {
    pub fn create(device: &ID3D11Device) -> eyre::Result<Arc<EglManager>> {
        let egl = egl::Instance::new(egl::Static);

        let angle_device = unsafe {
            eglCreateDeviceANGLE(EGL_D3D11_DEVICE_ANGLE, device.as_raw(), &egl::ATTRIB_NONE)
        };

        if angle_device.is_null() {
            bail!("failed to create angle device");
        }

        // let attribs = [egl::NONE as egl::Attrib];
        // unsafe { eglDebugMessageControlKHR(debug_callback, attribs.as_ptr()) };

        let display = unsafe {
            egl.get_platform_display(EGL_PLATFORM_DEVICE_EXT, angle_device, &[egl::ATTRIB_NONE])?
        };

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

        let config = configs[0];

        let context_attribs = [egl::CONTEXT_CLIENT_VERSION, 2, egl::NONE];
        let context = egl.create_context(display, config, None, &context_attribs)?;
        let resource_context =
            egl.create_context(display, config, Some(context), &context_attribs)?;

        Ok(Arc::new(EglManager {
            egl,
            angle_device: ptr::null_mut(),
            display,
            config,
            context,
            resource_context,
        }))
    }

    pub fn make_surface_current(&self, surface: egl::Surface) -> eyre::Result<()> {
        self.egl.make_current(
            self.display,
            Some(surface),
            Some(surface),
            Some(self.context),
        )?;
        Ok(())
    }

    pub fn make_context_current(&self) -> eyre::Result<()> {
        self.egl
            .make_current(self.display, None, None, Some(self.context))?;
        Ok(())
    }

    pub fn make_resource_context_current(&self) -> eyre::Result<()> {
        self.egl
            .make_current(self.display, None, None, Some(self.resource_context))?;
        Ok(())
    }

    pub fn clear_current(&self) -> eyre::Result<()> {
        self.egl.make_current(self.display, None, None, None)?;
        Ok(())
    }

    pub fn get_proc_address(&self, name: &str) -> Option<*mut c_void> {
        self.egl.get_proc_address(name).map(|f| f as *mut c_void)
    }

    pub fn create_surface_from_d3d11_texture(
        &self,
        texture: &ID3D11Texture2D,
        offset: (i32, i32),
    ) -> eyre::Result<egl::Surface> {
        let buffer = unsafe { ClientBuffer::from_ptr(texture.as_raw()) };

        let surface = self.egl.create_pbuffer_from_client_buffer(
            self.display,
            EGL_D3D_TEXTURE_ANGLE,
            buffer,
            self.config,
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
        Ok(surface)
    }

    pub fn destroy_surface(&self, surface: egl::Surface) -> eyre::Result<()> {
        self.egl.destroy_surface(self.display, surface)?;
        Ok(())
    }
}

impl Drop for EglManager {
    fn drop(&mut self) {
        unsafe { eglReleaseDeviceANGLE(self.angle_device) }

        self.egl
            .destroy_context(self.display, self.resource_context)
            .unwrap();

        self.egl
            .destroy_context(self.display, self.context)
            .unwrap();
    }
}

extern "C" {
    // fn eglDebugMessageControlKHR(
    //     callback: extern "C" fn(
    //         egl::Enum,
    //         *const c_char,
    //         egl::Int,
    //         *const c_void,
    //         *const c_void,
    //         *const c_char,
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
