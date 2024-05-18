use std::ffi::c_void;
use std::ptr;

use egl::ClientBuffer;
use flutter_embedder::{
    FlutterBackingStore, FlutterBackingStoreConfig,
    FlutterBackingStoreType_kFlutterBackingStoreTypeOpenGL, FlutterBackingStore__bindgen_ty_1,
    FlutterLayer, FlutterLayerContentType_kFlutterLayerContentTypeBackingStore,
    FlutterOpenGLBackingStore, FlutterOpenGLBackingStore__bindgen_ty_1, FlutterOpenGLFramebuffer,
    FlutterOpenGLTargetType_kFlutterOpenGLTargetTypeFramebuffer,
};
use khronos_egl as egl;
use windows::core::{ComInterface, Interface};
use windows::Foundation::Numerics::Vector2;
use windows::Foundation::Size;
use windows::Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Texture2D, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_FLAG,
    D3D11_RESOURCE_MISC_SHARED, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::System::WinRT::Composition::{
    ICompositionDrawingSurfaceInterop, ICompositorInterop,
};
use windows::UI::Composition::{
    CompositionDrawingSurface, CompositionGraphicsDevice, SpriteVisual,
};

use crate::{Gl, ResizeState};

const EGL_D3D_TEXTURE_ANGLE: egl::Enum = 0x33a3;

pub struct Compositor {
    gl: *mut Gl,
    layers: Vec<*const FlutterLayer>,
    composition_device: CompositionGraphicsDevice,
}

impl Compositor {
    pub fn new(gl: *mut Gl) -> Compositor {
        let composition_device = unsafe {
            (*gl)
                .compositor_controller
                .Compositor()
                .unwrap()
                .cast::<ICompositorInterop>()
                .unwrap()
                .CreateGraphicsDevice(&(*gl).device)
                .unwrap()
        };

        Compositor {
            gl,
            layers: vec![],
            composition_device,
        }
    }
}

struct RenderTarget {
    visual: SpriteVisual,
    composition_surface: CompositionDrawingSurface,
    texture: ID3D11Texture2D,
    egl_surface: egl::Surface,
    gl_framebuffer: u32,
    gl_texture: u32,
}

#[tracing::instrument]
pub unsafe extern "C" fn create_backing_store(
    config: *const FlutterBackingStoreConfig,
    out: *mut FlutterBackingStore,
    user_data: *mut c_void,
) -> bool {
    let compositor = user_data.cast::<Compositor>().as_mut().unwrap();
    let gl = compositor.gl.as_mut().unwrap();
    let size = (*config).size;

    let visual = gl
        .compositor_controller
        .Compositor()
        .unwrap()
        .CreateSpriteVisual()
        .unwrap();

    visual
        .SetSize(Vector2 {
            X: size.width as f32,
            Y: size.height as f32,
        })
        .unwrap();

    let composition_surface = compositor
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
        gl.device
            .CreateTexture2D(&texture_desc, None, Some(&mut texture))
            .unwrap();
        texture.unwrap()
    };

    visual
        .SetBrush(
            &gl.compositor_controller
                .Compositor()
                .unwrap()
                .CreateSurfaceBrushWithSurface(&composition_surface)
                .unwrap(),
        )
        .unwrap();

    let egl_surface = gl
        .egl
        .create_pbuffer_from_client_buffer(
            gl.display,
            EGL_D3D_TEXTURE_ANGLE,
            ClientBuffer::from_ptr(texture.as_raw()),
            gl.config,
            &[
                egl::WIDTH,
                size.width as i32,
                egl::HEIGHT,
                size.height as i32,
                egl::TEXTURE_FORMAT,
                egl::TEXTURE_RGBA,
                egl::TEXTURE_TARGET,
                egl::TEXTURE_2D,
                egl::NONE,
            ],
        )
        .unwrap();

    let mut gl_texture = 0;
    let mut framebuffer = 0;

    gl::GenTextures(1, &mut gl_texture);
    gl::GenFramebuffers(1, &mut framebuffer);

    gl::BindFramebuffer(gl::FRAMEBUFFER, framebuffer);

    gl::BindTexture(gl::TEXTURE_2D, gl_texture);
    gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::NEAREST as i32);
    gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::NEAREST as i32);
    gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
    gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);

    gl.egl
        .bind_tex_image(gl.display, egl_surface, egl::BACK_BUFFER)
        .unwrap();

    gl::BindTexture(gl::TEXTURE_2D, 0);

    gl::FramebufferTexture2D(
        gl::FRAMEBUFFER,
        gl::COLOR_ATTACHMENT0,
        gl::TEXTURE_2D,
        gl_texture,
        0,
    );

    let render_target = Box::new(RenderTarget {
        visual,
        composition_surface,
        texture,
        egl_surface,
        gl_framebuffer: framebuffer,
        gl_texture,
    });

    (*out).type_ = FlutterBackingStoreType_kFlutterBackingStoreTypeOpenGL;
    (*out).user_data = Box::leak(render_target) as *mut _ as _;
    (*out).__bindgen_anon_1 = FlutterBackingStore__bindgen_ty_1 {
        open_gl: FlutterOpenGLBackingStore {
            type_: FlutterOpenGLTargetType_kFlutterOpenGLTargetTypeFramebuffer,
            __bindgen_anon_1: FlutterOpenGLBackingStore__bindgen_ty_1 {
                framebuffer: FlutterOpenGLFramebuffer {
                    name: framebuffer,
                    target: /* GL_BGRA8_EXT */ 0x93a1,
                    user_data: ptr::null_mut(),
                    destruction_callback: Some(destroy_texture),
                },
            },
        },
    };

    true
}

#[tracing::instrument]
unsafe extern "C" fn destroy_texture(user_data: *mut c_void) {}

#[tracing::instrument]
pub unsafe extern "C" fn collect_backing_store(
    backing_store: *const FlutterBackingStore,
    user_data: *mut c_void,
) -> bool {
    let compositor = user_data.cast::<Compositor>().as_mut().unwrap();
    let backing_store = backing_store.as_ref().unwrap();
    let render_target = Box::from_raw(backing_store.user_data.cast::<RenderTarget>());

    gl::DeleteFramebuffers(1, &render_target.gl_framebuffer);
    gl::DeleteTextures(1, &render_target.gl_texture);

    (*compositor.gl)
        .egl
        .destroy_surface((*compositor.gl).display, render_target.egl_surface)
        .unwrap();

    drop(render_target);

    true
}

#[tracing::instrument]
pub unsafe extern "C" fn present_layers(
    layers: *mut *const FlutterLayer,
    layers_count: usize,
    user_data: *mut c_void,
) -> bool {
    let compositor = user_data.cast::<Compositor>().as_mut().unwrap();
    let gl = compositor.gl.as_mut().unwrap();

    gl::Flush();

    // Composition layers need to be updated if flutter layers are added or removed.
    let mut should_update_composition_layers = compositor.layers.len() != layers_count;

    for i in 0..layers_count {
        let layer = *layers.add(i);

        // Composition layers need to be updated if flutter layers have been reordered.
        should_update_composition_layers =
            should_update_composition_layers || compositor.layers[i] != layer;

        // TODO: Support platform views
        assert_eq!(
            (*layer).type_,
            FlutterLayerContentType_kFlutterLayerContentTypeBackingStore
        );

        let backing_store = (*layer).__bindgen_anon_1.backing_store;
        let render_target = (*backing_store).user_data.cast::<RenderTarget>();

        let composition_surface_interop = (*render_target)
            .composition_surface
            .cast::<ICompositionDrawingSurfaceInterop>()
            .unwrap();

        let mut update_offset = Default::default();
        let texture: ID3D11Texture2D = composition_surface_interop
            .BeginDraw(None, &mut update_offset)
            .unwrap();

        let context = gl.device.GetImmediateContext().unwrap();

        context.CopySubresourceRegion(
            &texture,
            0,
            update_offset.x as u32,
            update_offset.y as u32,
            0,
            &(*render_target).texture,
            0,
            None,
        );

        composition_surface_interop.EndDraw().unwrap();
    }

    // Flutter layers have changed. We need to re-insert all layer visuals into the root visual in
    // the correct order.
    if should_update_composition_layers {
        let root = &(*compositor.gl).root;

        root.Children().unwrap().RemoveAll().unwrap();
        compositor.layers.clear();

        for i in 0..layers_count {
            let layer = *layers.add(i);
            let backing_store = (*layer).__bindgen_anon_1.backing_store;
            let render_target = (*backing_store).user_data.cast::<RenderTarget>();

            root.Children()
                .unwrap()
                .InsertAtTop(&(*render_target).visual)
                .unwrap();

            compositor.layers.push(layer);
        }
    }

    let commit_compositor = || gl.compositor_controller.Commit().unwrap();

    let mut resize_state = gl.resize_state.lock().unwrap();
    if let ResizeState::Started = *resize_state {
        // Calling DwmFlush() seems to reduce glitches when resizing.
        DwmFlush().unwrap();
        commit_compositor();
        *resize_state = ResizeState::Done;
        compositor.gl.as_mut().unwrap().resize_condvar.notify_all();
    } else {
        commit_compositor();
    }

    true
}
