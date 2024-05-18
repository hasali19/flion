use std::ffi::c_void;
use std::ptr;
use std::sync::Arc;

use color_eyre::eyre::{self, ContextCompat};
use flutter_embedder::{
    FlutterBackingStore, FlutterBackingStoreConfig,
    FlutterBackingStoreType_kFlutterBackingStoreTypeOpenGL, FlutterBackingStore__bindgen_ty_1,
    FlutterLayer, FlutterLayerContentType_kFlutterLayerContentTypeBackingStore,
    FlutterOpenGLBackingStore, FlutterOpenGLBackingStore__bindgen_ty_1, FlutterOpenGLFramebuffer,
    FlutterOpenGLTargetType_kFlutterOpenGLTargetTypeFramebuffer,
};
use khronos_egl as egl;
use windows::core::ComInterface;
use windows::Foundation::Numerics::Vector2;
use windows::Foundation::Size;
use windows::Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
    D3D11_CPU_ACCESS_FLAG, D3D11_RESOURCE_MISC_SHARED, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::System::WinRT::Composition::{
    ICompositionDrawingSurfaceInterop, ICompositorInterop,
};
use windows::UI::Composition::Core::CompositorController;
use windows::UI::Composition::{
    CompositionDrawingSurface, CompositionGraphicsDevice, ContainerVisual, SpriteVisual,
};

use crate::egl_manager::EglManager;
use crate::resize_controller::ResizeController;

pub struct Compositor {
    device: ID3D11Device,
    compositor_controller: CompositorController,
    composition_device: CompositionGraphicsDevice,
    egl_manager: Arc<EglManager>,
    resize_controller: Arc<ResizeController>,
    root_visual: ContainerVisual,
    layers: Vec<*const FlutterLayer>,
}

struct CompositorFlutterLayer {
    visual: SpriteVisual,
    composition_surface: CompositionDrawingSurface,
    texture: ID3D11Texture2D,
    egl_surface: egl::Surface,
    gl_framebuffer: u32,
    gl_texture: u32,
}

impl Compositor {
    pub fn new(
        device: ID3D11Device,
        compositor_controller: CompositorController,
        egl_manager: Arc<EglManager>,
        resize_controller: Arc<ResizeController>,
        root_visual: ContainerVisual,
    ) -> eyre::Result<Compositor> {
        let composition_device = unsafe {
            compositor_controller
                .Compositor()
                .unwrap()
                .cast::<ICompositorInterop>()?
                .CreateGraphicsDevice(&device)?
        };

        macro_rules! gl_load {
            ($($name:ident)*) => {
                $(
                    gl::$name::load_with(|name| egl_manager.get_proc_address(name).unwrap_or(ptr::null_mut()));
                )*
            };
        }

        gl_load!(
            GenTextures
            GenFramebuffers
            BindTexture
            BindFramebuffer
            TexParameteri
            FramebufferTexture2D
            DeleteTextures
            DeleteFramebuffers
        );

        Ok(Compositor {
            device,
            compositor_controller,
            composition_device,
            egl_manager,
            resize_controller,
            root_visual,
            layers: vec![],
        })
    }

    fn create_backing_store(
        &mut self,
        config: &FlutterBackingStoreConfig,
        out: &mut FlutterBackingStore,
    ) -> eyre::Result<()> {
        let size = config.size;

        let visual = self
            .compositor_controller
            .Compositor()?
            .CreateSpriteVisual()?;

        visual.SetSize(Vector2::new(size.width as f32, size.height as f32))?;

        let composition_surface = self
            .composition_device
            .CreateDrawingSurface(
                Size {
                    Width: size.width as f32,
                    Height: size.height as f32,
                },
                DirectXPixelFormat::B8G8R8A8UIntNormalized,
                DirectXAlphaMode::Premultiplied,
            )
            .unwrap();

        let texture_desc = D3D11_TEXTURE2D_DESC {
            Width: size.width as u32,
            Height: size.height as u32,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            MipLevels: 1,
            ArraySize: 1,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: D3D11_CPU_ACCESS_FLAG::default().0 as u32,
            MiscFlags: D3D11_RESOURCE_MISC_SHARED.0 as u32,
        };

        let texture = unsafe {
            let mut texture = None;
            self.device
                .CreateTexture2D(&texture_desc, None, Some(&mut texture))?;
            texture.wrap_err("failed to create texture")?
        };

        let surface_brush = self
            .compositor_controller
            .Compositor()?
            .CreateSurfaceBrushWithSurface(&composition_surface)?;

        visual.SetBrush(&surface_brush)?;

        let egl_surface = self
            .egl_manager
            .create_surface_from_d3d11_texture(&texture)?;

        let mut gl_texture = 0;
        let mut gl_framebuffer = 0;
        unsafe {
            gl::GenTextures(1, &mut gl_texture);
            gl::GenFramebuffers(1, &mut gl_framebuffer);

            gl::BindFramebuffer(gl::FRAMEBUFFER, gl_framebuffer);

            gl::BindTexture(gl::TEXTURE_2D, gl_texture);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::NEAREST as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::NEAREST as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);

            self.egl_manager
                .bind_tex_image(egl_surface, egl::BACK_BUFFER)?;

            gl::BindTexture(gl::TEXTURE_2D, 0);

            gl::FramebufferTexture2D(
                gl::FRAMEBUFFER,
                gl::COLOR_ATTACHMENT0,
                gl::TEXTURE_2D,
                gl_texture,
                0,
            );
        }

        let compositor_layer = Box::leak(Box::new(CompositorFlutterLayer {
            visual,
            composition_surface,
            texture,
            egl_surface,
            gl_framebuffer,
            gl_texture,
        }));

        out.type_ = FlutterBackingStoreType_kFlutterBackingStoreTypeOpenGL;
        out.user_data = (compositor_layer as *mut CompositorFlutterLayer).cast();
        out.__bindgen_anon_1 = FlutterBackingStore__bindgen_ty_1 {
            open_gl: FlutterOpenGLBackingStore {
                type_: FlutterOpenGLTargetType_kFlutterOpenGLTargetTypeFramebuffer,
                __bindgen_anon_1: FlutterOpenGLBackingStore__bindgen_ty_1 {
                    framebuffer: FlutterOpenGLFramebuffer {
                        name: gl_framebuffer,
                        target: /* GL_BGRA8_EXT */ 0x93a1,
                        user_data: ptr::null_mut(),
                        destruction_callback: None,
                    },
                },
            },
        };

        Ok(())
    }

    fn collect_backing_store(&mut self, backing_store: &FlutterBackingStore) -> eyre::Result<()> {
        let render_target =
            unsafe { Box::from_raw(backing_store.user_data.cast::<CompositorFlutterLayer>()) };

        unsafe {
            gl::DeleteFramebuffers(1, &render_target.gl_framebuffer);
            gl::DeleteTextures(1, &render_target.gl_texture);
        }

        self.egl_manager
            .destroy_surface(render_target.egl_surface)?;

        drop(render_target);

        Ok(())
    }

    fn present_layers(&mut self, layers: &[&FlutterLayer]) -> eyre::Result<()> {
        // Composition layers need to be updated if flutter layers are added or removed.
        let mut should_update_composition_layers = self.layers.len() != layers.len();

        for (i, &layer) in layers.iter().enumerate() {
            // Composition layers need to be updated if flutter layers have been reordered.
            should_update_composition_layers =
                should_update_composition_layers || self.layers[i] != layer;

            // TODO: Support platform views
            assert_eq!(
                layer.type_,
                FlutterLayerContentType_kFlutterLayerContentTypeBackingStore
            );

            let compositor_layer = unsafe {
                (*layer.__bindgen_anon_1.backing_store)
                    .user_data
                    .cast::<CompositorFlutterLayer>()
                    .as_mut()
                    .unwrap()
            };

            let composition_surface_interop = compositor_layer
                .composition_surface
                .cast::<ICompositionDrawingSurfaceInterop>()?;

            unsafe {
                let mut update_offset = Default::default();
                let texture: ID3D11Texture2D =
                    composition_surface_interop.BeginDraw(None, &mut update_offset)?;

                let context = self.device.GetImmediateContext()?;

                context.CopySubresourceRegion(
                    &texture,
                    0,
                    update_offset.x as u32,
                    update_offset.y as u32,
                    0,
                    &compositor_layer.texture,
                    0,
                    None,
                );

                composition_surface_interop.EndDraw()?;
            }
        }

        // Flutter layers have changed. We need to re-insert all layer visuals into the root visual in
        // the correct order.
        if should_update_composition_layers {
            self.root_visual.Children()?.RemoveAll()?;
            self.layers.clear();

            for &layer in layers {
                let compositor_layer = unsafe {
                    (*layer.__bindgen_anon_1.backing_store)
                        .user_data
                        .cast::<CompositorFlutterLayer>()
                        .as_mut()
                        .unwrap()
                };

                self.root_visual
                    .Children()?
                    .InsertAtTop(&compositor_layer.visual)?;

                self.layers.push(layer);
            }
        }

        let commit_compositor = || self.compositor_controller.Commit().unwrap();

        if let Some(resize) = self.resize_controller.current_resize() {
            // Calling DwmFlush() seems to reduce glitches when resizing.
            unsafe { DwmFlush()? };
            commit_compositor();
            resize.complete();
        } else {
            commit_compositor();
        }

        Ok(())
    }
}

pub unsafe extern "C" fn create_backing_store(
    config: *const FlutterBackingStoreConfig,
    out: *mut FlutterBackingStore,
    user_data: *mut c_void,
) -> bool {
    let Some(compositor) = user_data.cast::<Compositor>().as_mut() else {
        tracing::error!("user_data is null");
        return false;
    };

    let Some(config) = config.as_ref() else {
        tracing::error!("config is null");
        return false;
    };

    let Some(backing_store) = out.as_mut() else {
        tracing::error!("out is null");
        return false;
    };

    if let Err(e) = compositor.create_backing_store(config, backing_store) {
        tracing::error!("{e}");
        return false;
    }

    true
}

pub unsafe extern "C" fn collect_backing_store(
    backing_store: *const FlutterBackingStore,
    user_data: *mut c_void,
) -> bool {
    let Some(compositor) = user_data.cast::<Compositor>().as_mut() else {
        tracing::error!("user_data is null");
        return false;
    };

    let Some(backing_store) = backing_store.as_ref() else {
        tracing::error!("config is null");
        return false;
    };

    if let Err(e) = compositor.collect_backing_store(backing_store) {
        tracing::error!("{e}");
        return false;
    }

    true
}

pub unsafe extern "C" fn present_layers(
    layers: *mut *const FlutterLayer,
    layers_count: usize,
    user_data: *mut c_void,
) -> bool {
    let Some(compositor) = user_data.cast::<Compositor>().as_mut() else {
        tracing::error!("user_data is null");
        return false;
    };

    if layers.is_null() {
        tracing::error!("layers is null");
        return false;
    }

    let layers = std::slice::from_raw_parts(layers.cast::<&FlutterLayer>(), layers_count);

    if let Err(e) = compositor.present_layers(layers) {
        tracing::error!("{e}");
        return false;
    };

    true
}
