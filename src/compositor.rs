mod renderer;

use std::ffi::c_void;
use std::sync::Arc;
use std::{mem, ptr};

use flutter_embedder::{
    FlutterBackingStore, FlutterBackingStoreConfig,
    FlutterBackingStoreType_kFlutterBackingStoreTypeOpenGL, FlutterBackingStore__bindgen_ty_1,
    FlutterLayer, FlutterLayerContentType_kFlutterLayerContentTypeBackingStore,
    FlutterOpenGLBackingStore, FlutterOpenGLBackingStore__bindgen_ty_1, FlutterOpenGLSurface,
    FlutterOpenGLTargetType_kFlutterOpenGLTargetTypeSurface,
};
use khronos_egl::{self as egl};
use renderer::Renderer;
use windows::core::Interface;
use windows::Foundation::Numerics::Vector2;
use windows::Foundation::Size;
use windows::Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11ShaderResourceView, ID3D11Texture2D,
};
use windows::Win32::System::WinRT::Composition::{
    ICompositionDrawingSurfaceInterop, ICompositorInterop,
};
use windows::UI::Composition::{
    CompositionDrawingSurface, CompositionGraphicsDevice, Compositor, ContainerVisual, SpriteVisual,
};

use crate::egl_manager::EglManager;

pub struct FlutterCompositor {
    compositor: Compositor,
    composition_device: CompositionGraphicsDevice,
    root_visual: ContainerVisual,
    egl_manager: Arc<EglManager>,
    layers: Vec<*const FlutterLayer>,
    renderer: Renderer,
    present_callback: Box<dyn FnMut() -> eyre::Result<()>>,
}

struct CompositorFlutterLayer {
    egl_manager: Arc<EglManager>,
    visual: SpriteVisual,
    composition_surface: CompositionDrawingSurface,
    texture_resource_view: ID3D11ShaderResourceView,
    egl_surface: egl::Surface,
}

impl FlutterCompositor {
    pub fn new(
        visual: ContainerVisual,
        device: ID3D11Device,
        egl_manager: Arc<EglManager>,
        present_callback: Box<dyn FnMut() -> eyre::Result<()>>,
    ) -> eyre::Result<FlutterCompositor> {
        let compositor = visual.Compositor()?;

        let composition_device = unsafe {
            compositor
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

        Ok(FlutterCompositor {
            compositor,
            composition_device,
            egl_manager,
            root_visual: visual,
            layers: vec![],
            renderer: Renderer::new(device)?,
            present_callback,
        })
    }

    pub fn root_visual(&self) -> &ContainerVisual {
        &self.root_visual
    }

    pub fn create_backing_store(
        &mut self,
        config: &FlutterBackingStoreConfig,
        out: &mut FlutterBackingStore,
    ) -> eyre::Result<()> {
        let size = config.size;

        let visual = self.compositor.CreateSpriteVisual()?;

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

        let surface_brush = self
            .compositor
            .CreateSurfaceBrushWithSurface(&composition_surface)?;

        visual.SetBrush(&surface_brush)?;

        let (render_texture, render_resource_view) = self
            .renderer
            .create_render_texture(size.width as u32, size.height as u32)?;

        let egl_surface = self
            .egl_manager
            .create_surface_from_d3d11_texture(&render_texture, (0, 0))
            .unwrap();

        let compositor_layer = Box::leak(Box::new(CompositorFlutterLayer {
            egl_manager: self.egl_manager.clone(),
            visual,
            composition_surface,
            texture_resource_view: render_resource_view,
            egl_surface,
        }));

        extern "C" fn make_surface_current(
            user_data: *mut c_void,
            gl_state_changed: *mut bool,
        ) -> bool {
            let layer = unsafe {
                user_data
                    .cast::<CompositorFlutterLayer>()
                    .as_mut()
                    .expect("layer must not be null")
            };

            layer
                .egl_manager
                .make_surface_current(layer.egl_surface)
                .unwrap();

            unsafe {
                *gl_state_changed = false;
            }

            true
        }

        extern "C" fn clear_current_surface(user_data: *mut c_void, _: *mut bool) -> bool {
            let layer = unsafe {
                user_data
                    .cast::<CompositorFlutterLayer>()
                    .as_mut()
                    .expect("layer must not be null")
            };

            layer.egl_manager.clear_current().unwrap();

            true
        }

        out.type_ = FlutterBackingStoreType_kFlutterBackingStoreTypeOpenGL;
        out.user_data = (compositor_layer as *mut CompositorFlutterLayer).cast();
        out.__bindgen_anon_1 = FlutterBackingStore__bindgen_ty_1 {
            open_gl: FlutterOpenGLBackingStore {
                type_: FlutterOpenGLTargetType_kFlutterOpenGLTargetTypeSurface,
                __bindgen_anon_1: FlutterOpenGLBackingStore__bindgen_ty_1 {
                    surface: FlutterOpenGLSurface {
                        struct_size: mem::size_of::<FlutterOpenGLSurface>(),
                        format: /* GL_BGRA8_EXT */ 0x93A1,
                        make_current_callback: Some(make_surface_current),
                        clear_current_callback: Some(clear_current_surface),
                        destruction_callback: None,
                        user_data: compositor_layer as *mut _ as _,
                    },
                },
            },
        };

        Ok(())
    }

    pub fn collect_backing_store(
        &mut self,
        backing_store: &FlutterBackingStore,
    ) -> eyre::Result<()> {
        let render_target =
            unsafe { Box::from_raw(backing_store.user_data.cast::<CompositorFlutterLayer>()) };

        self.egl_manager
            .destroy_surface(render_target.egl_surface)?;

        drop(render_target);

        Ok(())
    }

    pub fn present_layers(&mut self, layers: &[&FlutterLayer]) -> eyre::Result<()> {
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

            let mut update_offset = Default::default();

            let texture: ID3D11Texture2D =
                unsafe { composition_surface_interop.BeginDraw(None, &mut update_offset) }?;

            self.renderer.draw_flipped_texture(
                &compositor_layer.texture_resource_view,
                &texture,
                (layer.size.width as u32, layer.size.height as u32),
                (update_offset.x, update_offset.y),
            )?;

            unsafe { composition_surface_interop.EndDraw()? };
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

        (self.present_callback)()
    }
}
