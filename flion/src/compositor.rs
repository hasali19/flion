use std::ffi::c_void;
use std::mem;
use std::sync::Arc;

use flutter_embedder::{
    FlutterBackingStore, FlutterBackingStoreConfig,
    FlutterBackingStoreType_kFlutterBackingStoreTypeOpenGL, FlutterBackingStore__bindgen_ty_1,
    FlutterLayer, FlutterLayerContentType_kFlutterLayerContentTypeBackingStore,
    FlutterOpenGLBackingStore, FlutterOpenGLBackingStore__bindgen_ty_1, FlutterOpenGLSurface,
    FlutterOpenGLTargetType_kFlutterOpenGLTargetTypeSurface,
};
use khronos_egl::{self as egl};
use windows::core::Interface;
use windows::Foundation::Numerics::Vector2;
use windows::Win32::Foundation::BOOL;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGIDevice, IDXGIDevice2, IDXGIFactory2, IDXGISwapChain1, DXGI_PRESENT, DXGI_SCALING_STRETCH,
    DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject, INFINITE};
use windows::Win32::System::WinRT::Composition::ICompositorInterop;
use windows::UI::Composition::{Compositor, ContainerVisual, SpriteVisual};

use crate::egl::EglDevice;

pub trait CompositionHandler: Send {
    /// Returns the current size of the rendering area.
    fn get_surface_size(&mut self) -> eyre::Result<(u32, u32)>;

    /// Commits the current compositor frame. This will be called by the compositor after all
    /// surfaces are ready to be presented.
    fn present(&mut self) -> eyre::Result<()>;
}

pub struct FlutterCompositor {
    device: ID3D11Device,
    compositor: Compositor,
    root_visual: ContainerVisual,
    egl: Arc<EglDevice>,
    layers: Vec<*const CompositorFlutterLayer>,
    handler: Box<dyn CompositionHandler>,
}

struct CompositorFlutterLayer {
    egl: Arc<EglDevice>,
    visual: SpriteVisual,
    swapchain: IDXGISwapChain1,
    egl_surface: egl::Surface,
    is_first_present: bool,
}

impl FlutterCompositor {
    pub fn new(
        visual: ContainerVisual,
        device: ID3D11Device,
        egl: Arc<EglDevice>,
        handler: Box<dyn CompositionHandler>,
    ) -> eyre::Result<FlutterCompositor> {
        let compositor = visual.Compositor()?;

        Ok(FlutterCompositor {
            device,
            compositor,
            egl,
            root_visual: visual,
            layers: vec![],
            handler,
        })
    }

    pub fn get_surface_transformation(
        &mut self,
    ) -> eyre::Result<flutter_embedder::FlutterTransformation> {
        let (_width, height) = self.handler.get_surface_size()?;

        Ok(flutter_embedder::FlutterTransformation {
            scaleX: 1.0,
            scaleY: -1.0,
            transY: height.into(),
            pers2: 1.0,
            ..Default::default()
        })
    }

    pub fn create_backing_store(
        &mut self,
        config: &FlutterBackingStoreConfig,
        out: &mut FlutterBackingStore,
    ) -> eyre::Result<()> {
        let size = config.size;

        let visual = self.compositor.CreateSpriteVisual()?;

        visual.SetSize(Vector2::new(size.width as f32, size.height as f32))?;

        let dxgi_device: IDXGIDevice = self.device.cast()?;
        let dxgi_factory: IDXGIFactory2 = unsafe { dxgi_device.GetAdapter()?.GetParent()? };

        let swapchain = unsafe {
            dxgi_factory.CreateSwapChainForComposition(
                &self.device,
                &DXGI_SWAP_CHAIN_DESC1 {
                    Width: size.width as u32,
                    Height: size.height as u32,
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    Stereo: BOOL::from(false),
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                    BufferCount: 2,
                    Scaling: DXGI_SCALING_STRETCH,
                    SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                    AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                    Flags: 0,
                },
                None,
            )?
        };

        let back_buffer: ID3D11Texture2D = unsafe { swapchain.GetBuffer(0)? };

        let egl_surface = self
            .egl
            .create_surface_from_d3d11_texture(&back_buffer, (0, 0))?;

        let composition_surface = unsafe {
            self.compositor
                .cast::<ICompositorInterop>()?
                .CreateCompositionSurfaceForSwapChain(&swapchain)?
        };

        let surface_brush = self
            .compositor
            .CreateSurfaceBrushWithSurface(&composition_surface)?;

        visual.SetBrush(&surface_brush)?;

        // This is freed when collect_backing_store is called.
        let compositor_layer = Box::into_raw(Box::new(CompositorFlutterLayer {
            egl: self.egl.clone(),
            visual,
            egl_surface,
            swapchain,
            is_first_present: true,
        }));

        extern "C" fn make_surface_current(
            user_data: *mut c_void,
            gl_state_changed: *mut bool,
        ) -> bool {
            let layer = unsafe {
                user_data
                    .cast::<CompositorFlutterLayer>()
                    .as_mut()
                    .expect("layer is not null")
            };

            if let Err(e) = layer.egl.make_surface_current(layer.egl_surface) {
                tracing::error!("{e:?}");
                return false;
            };

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
                    .expect("layer is not null")
            };

            if let Err(e) = layer.egl.clear_current() {
                tracing::error!("{e:?}");
                return false;
            }

            true
        }

        const GL_BGRA8_EXT: u32 = 0x93A1;

        out.type_ = FlutterBackingStoreType_kFlutterBackingStoreTypeOpenGL;
        out.user_data = compositor_layer.cast();
        out.__bindgen_anon_1 = FlutterBackingStore__bindgen_ty_1 {
            open_gl: FlutterOpenGLBackingStore {
                type_: FlutterOpenGLTargetType_kFlutterOpenGLTargetTypeSurface,
                __bindgen_anon_1: FlutterOpenGLBackingStore__bindgen_ty_1 {
                    surface: FlutterOpenGLSurface {
                        struct_size: mem::size_of::<FlutterOpenGLSurface>(),
                        format: GL_BGRA8_EXT,
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
        let layer =
            unsafe { Box::from_raw(backing_store.user_data.cast::<CompositorFlutterLayer>()) };

        self.egl.destroy_surface(layer.egl_surface)?;

        Ok(())
    }

    pub fn present_layers(&mut self, layers: &[&FlutterLayer]) -> eyre::Result<()> {
        // Composition layers need to be updated if flutter layers are added or removed.
        let mut should_update_composition_layers = self.layers.len() != layers.len();
        let mut should_flush_rendering = false;

        for (i, &layer) in layers.iter().enumerate() {
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

            // Composition layers need to be updated if flutter layers have been reordered.
            should_update_composition_layers =
                should_update_composition_layers || self.layers[i] != compositor_layer;

            unsafe {
                compositor_layer
                    .swapchain
                    .Present(0, DXGI_PRESENT::default())
                    .ok()?;
            }

            should_flush_rendering = should_flush_rendering || compositor_layer.is_first_present;

            compositor_layer.is_first_present = false;
        }

        if should_flush_rendering {
            unsafe {
                // Flush outstanding rendering commands if this is the first present. This is taken from Chromium:
                // https://github.com/chromium/chromium/blob/2764576ca3ae948e9274da637b535b4113f421f2/ui/gl/swap_chain_presenter.cc#L1702-L1710.
                // Seems to help avoid some flickering when the swapchain gets recreating while resizing.
                // Interestingly the buffer copying Chromium uses in addition to this doesn't seem necessary here.
                let event = CreateEventW(None, false, false, None)?;
                let dxgi_device = self.device.cast::<IDXGIDevice2>()?;
                dxgi_device.EnqueueSetEvent(event)?;
                WaitForSingleObject(event, INFINITE);
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

                self.layers.push(compositor_layer);
            }
        }

        self.handler.present()
    }
}
