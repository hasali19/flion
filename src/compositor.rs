use std::ffi::c_void;

use egl::ClientBuffer;
use flutter_embedder::{
    FlutterBackingStore, FlutterBackingStoreConfig,
    FlutterBackingStoreType_kFlutterBackingStoreTypeOpenGL, FlutterBackingStore__bindgen_ty_1,
    FlutterLayer, FlutterOpenGLBackingStore, FlutterOpenGLBackingStore__bindgen_ty_1,
    FlutterOpenGLFramebuffer, FlutterOpenGLTargetType_kFlutterOpenGLTargetTypeFramebuffer,
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
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::System::WinRT::Composition::{
    ICompositionDrawingSurfaceInterop, ICompositorInterop,
};
use windows::UI::Composition::{CompositionDrawingSurface, SpriteVisual};

use crate::Gl;

const EGL_D3D_TEXTURE_2D_SHARE_HANDLE_ANGLE: egl::Enum = 0x3200;
const EGL_D3D_TEXTURE_ANGLE: egl::Enum = 0x33a3;

pub struct Compositor {
    gl: *mut Gl,
    render_targets: Vec<RenderTarget>,
}

impl Compositor {
    pub fn new(gl: *mut Gl) -> Compositor {
        Compositor {
            gl,
            render_targets: vec![],
        }
    }
}

struct RenderTarget {
    width: f64,
    height: f64,
    visual: SpriteVisual,
    composition_surface: CompositionDrawingSurface,
    texture: ID3D11Texture2D,
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

    gl.root.Children().unwrap().InsertAtTop(&visual).unwrap();

    let composition_device = unsafe {
        gl.compositor_controller
            .Compositor()
            .unwrap()
            .cast::<ICompositorInterop>()
            .unwrap()
            .CreateGraphicsDevice(&gl.device)
            .unwrap()
    };

    let composition_surface = composition_device
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

    let render_target = RenderTarget {
        width: size.width,
        height: size.height,
        visual,
        composition_surface,
        texture,
    };

    render_target
        .visual
        .SetBrush(
            &gl.compositor_controller
                .Compositor()
                .unwrap()
                .CreateSurfaceBrushWithSurface(&render_target.composition_surface)
                .unwrap(),
        )
        .unwrap();

    let render_target = {
        compositor.render_targets.push(render_target);
        compositor.render_targets.last().unwrap_unchecked()
    };

    let egl_surface = gl
        .egl
        .create_pbuffer_from_client_buffer(
            gl.display,
            EGL_D3D_TEXTURE_ANGLE,
            ClientBuffer::from_ptr(render_target.texture.as_raw()),
            gl.config,
            &[
                egl::WIDTH,
                size.width as i32,
                egl::HEIGHT,
                size.height as i32,
                // 0x3490,
                // update_offset.x,
                // 0x3491,
                // update_offset.y,
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

    (*out).type_ = FlutterBackingStoreType_kFlutterBackingStoreTypeOpenGL;
    (*out).__bindgen_anon_1 = FlutterBackingStore__bindgen_ty_1 {
        open_gl: FlutterOpenGLBackingStore {
            type_: FlutterOpenGLTargetType_kFlutterOpenGLTargetTypeFramebuffer,
            __bindgen_anon_1: FlutterOpenGLBackingStore__bindgen_ty_1 {
                framebuffer: FlutterOpenGLFramebuffer {
                    name: framebuffer,
                    target: 0x93a1,
                    user_data: (compositor.render_targets.len() - 1) as _,
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
    let layers = std::slice::from_raw_parts_mut(layers, layers_count);

    gl::Flush();

    for &mut layer in layers {
        let backing_store = (*layer).__bindgen_anon_1.backing_store.as_ref().unwrap();
        let render_target_index = backing_store.user_data as usize;
        let render_target = &compositor.render_targets[render_target_index];

        let composition_surface_interop = render_target
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
            &render_target.texture,
            0,
            None,
        );

        context.Flush();
        composition_surface_interop.EndDraw().unwrap();
    }

    gl.compositor_controller.Commit().unwrap();

    true
}
