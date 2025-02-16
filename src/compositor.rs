use std::ffi::c_void;
use std::sync::Arc;
use std::{mem, ptr};

use color_eyre::eyre;
use flutter_embedder::{
    FlutterBackingStore, FlutterBackingStoreConfig,
    FlutterBackingStoreType_kFlutterBackingStoreTypeOpenGL, FlutterBackingStore__bindgen_ty_1,
    FlutterLayer, FlutterLayerContentType_kFlutterLayerContentTypeBackingStore,
    FlutterOpenGLBackingStore, FlutterOpenGLBackingStore__bindgen_ty_1, FlutterOpenGLSurface,
    FlutterOpenGLTargetType_kFlutterOpenGLTargetTypeSurface,
};
use khronos_egl::{self as egl};
use windows::core::ComInterface;
use windows::Foundation::Numerics::{Matrix4x4, Vector2, Vector3};
use windows::Foundation::Size;
use windows::Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::System::WinRT::Composition::{
    ICompositionDrawingSurfaceInterop, ICompositorDesktopInterop, ICompositorInterop,
};
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
use windows::UI::Composition::Core::CompositorController;
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{
    CompositionDrawingSurface, CompositionGraphicsDevice, ContainerVisual, SpriteVisual,
};

use crate::egl_manager::EglManager;
use crate::resize_controller::ResizeController;

pub struct Compositor {
    compositor_controller: CompositorController,
    composition_device: CompositionGraphicsDevice,
    _composition_target: DesktopWindowTarget,
    egl_manager: Arc<EglManager>,
    resize_controller: Arc<ResizeController>,
    root_visual: ContainerVisual,
    layers: Vec<*const FlutterLayer>,
}

struct CompositorFlutterLayer {
    egl_manager: Arc<EglManager>,
    visual: SpriteVisual,
    composition_surface: CompositionDrawingSurface,
    egl_surface: Option<egl::Surface>,
}

impl Compositor {
    pub fn new(
        window: HWND,
        device: ID3D11Device,
        egl_manager: Arc<EglManager>,
        resize_controller: Arc<ResizeController>,
    ) -> eyre::Result<Compositor> {
        let compositor_controller = CompositorController::new()?;
        let composition_target = unsafe {
            compositor_controller
                .Compositor()?
                .cast::<ICompositorDesktopInterop>()?
                .CreateDesktopWindowTarget(window, false)?
        };

        let root = compositor_controller
            .Compositor()?
            .CreateContainerVisual()?;

        let (width, height) = unsafe {
            let mut rect = RECT::default();
            GetClientRect(window, &mut rect)?;
            (rect.right - rect.left, rect.bottom - rect.top)
        };

        root.SetSize(Vector2 {
            X: width as f32,
            Y: height as f32,
        })?;

        root.SetTransformMatrix(Matrix4x4 {
            M11: 1.0,
            M22: -1.0,
            M33: 1.0,
            M44: 1.0,
            ..Default::default()
        })?;

        root.SetOffset(Vector3::new(0.0, height as f32, 0.0))?;

        composition_target.SetRoot(&root)?;

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
            compositor_controller,
            composition_device,
            _composition_target: composition_target,
            egl_manager,
            resize_controller,
            root_visual: root,
            layers: vec![],
        })
    }

    pub fn create_backing_store(
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

        let surface_brush = self
            .compositor_controller
            .Compositor()?
            .CreateSurfaceBrushWithSurface(&composition_surface)?;

        visual.SetBrush(&surface_brush)?;

        let compositor_layer = Box::leak(Box::new(CompositorFlutterLayer {
            egl_manager: self.egl_manager.clone(),
            visual,
            composition_surface,
            egl_surface: None,
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

            let composition_surface_interop = layer
                .composition_surface
                .cast::<ICompositionDrawingSurfaceInterop>()
                .unwrap();

            let mut update_offset = POINT::default();
            let texture: ID3D11Texture2D = unsafe {
                composition_surface_interop
                    .BeginDraw(None, &mut update_offset)
                    .unwrap()
            };

            assert!(layer.egl_surface.is_none());

            let egl_surface = layer.egl_surface.insert(
                layer
                    .egl_manager
                    .create_surface_from_d3d11_texture(&texture, (update_offset.x, update_offset.y))
                    .unwrap(),
            );

            layer
                .egl_manager
                .make_surface_current(*egl_surface)
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
        let mut render_target =
            unsafe { Box::from_raw(backing_store.user_data.cast::<CompositorFlutterLayer>()) };

        if let Some(egl_surface) = render_target.egl_surface.take() {
            self.egl_manager.destroy_surface(egl_surface)?;
        }

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

            if let Some(egl_surface) = compositor_layer.egl_surface.take() {
                unsafe { composition_surface_interop.EndDraw()? };
                compositor_layer.egl_manager.destroy_surface(egl_surface)?;
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
            let (width, height) = resize.size();

            self.root_visual
                .SetSize(Vector2::new(width as f32, height as f32))
                .unwrap();

            self.root_visual
                .SetOffset(Vector3::new(0.0, height as f32, 0.0))
                .unwrap();

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
