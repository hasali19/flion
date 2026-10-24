use std::collections::HashMap;
use std::ffi::{CStr, c_char};
use std::sync::Arc;
use std::{mem, ptr};

use eyre::bail;
use flutter_embedder::{
    FlutterBackingStore__bindgen_ty_1, FlutterBackingStoreType_kFlutterBackingStoreTypeOpenGL,
    FlutterLayerContentType_kFlutterLayerContentTypeBackingStore, FlutterOpenGLBackingStore,
    FlutterOpenGLBackingStore__bindgen_ty_1, FlutterOpenGLFramebuffer,
    FlutterOpenGLTargetType_kFlutterOpenGLTargetTypeFramebuffer,
};
use parking_lot::Mutex;
use tracing::warn;

use crate::egl::{EglContext, EglDisplay, EglSurface};
use crate::engine::{FlutterCompositor, FlutterOpenGLCompositor};

pub struct OpenGLCompositor {
    display: EglDisplay,
    resources: EglResources,
    renderer: LayerRenderer,
    surfaces: Arc<ViewSurfaceRegistry>,
}

struct EglResources {
    context: EglContext,
    resource_context: EglContext,
    composite_context: EglContext,
}

#[derive(Default)]
pub struct ViewSurfaceRegistry {
    map: Mutex<HashMap<i64, ViewSurface>>,
}

struct ViewSurface {
    egl_surface: EglSurface,
    size: Option<(u32, u32)>,
}

impl ViewSurfaceRegistry {
    pub fn add(&self, view_id: i64, surface: EglSurface) {
        self.map.lock().insert(
            view_id,
            ViewSurface {
                egl_surface: surface,
                size: None,
            },
        );
    }

    pub fn remove(&self, view_id: i64) -> Option<EglSurface> {
        self.map
            .lock()
            .remove(&view_id)
            .map(|view| view.egl_surface)
    }
}

impl OpenGLCompositor {
    pub fn new(display: EglDisplay, surfaces: Arc<ViewSurfaceRegistry>) -> eyre::Result<Self> {
        let context = display.create_context()?;

        let resources = EglResources {
            resource_context: display.create_shared_context(&context)?,
            composite_context: display.create_shared_context(&context)?,
            context,
        };

        display.make_current(Some(&resources.composite_context), None)?;
        let renderer = unsafe { LayerRenderer::new()? };
        display.make_current(None, None)?;

        Ok(OpenGLCompositor {
            display,
            resources,
            renderer,
            surfaces,
        })
    }

    fn composite_layers(
        &self,
        (width, height): (u32, u32),
        layers: &[&flutter_embedder::FlutterLayer],
    ) {
        unsafe {
            gl::Viewport(0, 0, width as i32, height as i32);
            gl::ClearColor(0.0, 0.0, 0.0, 0.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);
        }

        for layer in layers {
            if layer.type_ != FlutterLayerContentType_kFlutterLayerContentTypeBackingStore {
                // TODO: Support platform views
                continue;
            }

            let framebuffer = unsafe {
                let backing_store = &*layer.__bindgen_anon_1.backing_store;
                &*backing_store.user_data.cast::<Framebuffer>()
            };

            unsafe {
                self.renderer
                    .draw(framebuffer.texture, layer, width, height)
            };
        }
    }
}

unsafe impl Send for EglResources {}
unsafe impl Sync for EglResources {}

impl FlutterCompositor for OpenGLCompositor {
    const AVOID_BACKING_STORE_CACHE: bool = false;
}

impl FlutterOpenGLCompositor for OpenGLCompositor {
    fn make_current(&self) -> eyre::Result<()> {
        self.resources.context.make_current()
    }

    fn make_resource_current(&self) -> eyre::Result<()> {
        self.resources.resource_context.make_current()
    }

    fn clear_current(&self) -> eyre::Result<()> {
        self.display.make_current(None, None)
    }

    fn get_proc_address(&self, name: &str) -> Option<extern "system" fn()> {
        self.display.get_proc_address(name)
    }

    fn create_backing_store(
        &self,
        config: &flutter_embedder::FlutterBackingStoreConfig,
        backing_store: &mut flutter_embedder::FlutterBackingStore,
    ) -> eyre::Result<()> {
        self.resources.context.make_current()?;

        let framebuffer =
            unsafe { Framebuffer::new(config.size.width as i32, config.size.height as i32)? };

        backing_store.type_ = FlutterBackingStoreType_kFlutterBackingStoreTypeOpenGL;
        backing_store.__bindgen_anon_1 = FlutterBackingStore__bindgen_ty_1 {
            open_gl: FlutterOpenGLBackingStore {
                type_: FlutterOpenGLTargetType_kFlutterOpenGLTargetTypeFramebuffer,
                __bindgen_anon_1: FlutterOpenGLBackingStore__bindgen_ty_1 {
                    framebuffer: FlutterOpenGLFramebuffer {
                        target: gl::RGBA8,
                        name: framebuffer.framebuffer,
                        user_data: ptr::null_mut(),
                        destruction_callback: None,
                    },
                },
            },
        };

        backing_store.user_data = Box::into_raw(Box::new(framebuffer)).cast();

        Ok(())
    }

    fn release_backing_store(
        &self,
        backing_store: &flutter_embedder::FlutterBackingStore,
    ) -> eyre::Result<()> {
        let framebuffer = unsafe { Box::from_raw(backing_store.user_data.cast::<Framebuffer>()) };

        self.resources.context.make_current()?;

        unsafe { framebuffer.delete() };

        Ok(())
    }

    fn present_view(
        &self,
        view_id: i64,
        layers: &[&flutter_embedder::FlutterLayer],
    ) -> eyre::Result<()> {
        let mut surfaces = self.surfaces.map.lock();

        let Some(surface) = surfaces.get_mut(&view_id) else {
            warn!("Could not present view {view_id}: no surface");
            return Ok(());
        };

        let Some(size) = get_view_size_for_layers(layers).or(surface.size) else {
            return Ok(());
        };

        if surface.size != Some(size) {
            // TODO: Should this resize be done here?
            surface.egl_surface.resize(size.0, size.1);
            surface.size = Some(size);
        }

        self.display.make_current(
            Some(&self.resources.composite_context),
            Some(&surface.egl_surface),
        )?;

        self.composite_layers(size, layers);

        // TODO: This blocks waiting on vsync which will be a problem for multiple views
        self.display.swap_buffers(&surface.egl_surface)?;

        self.display.make_current(None, None)?;

        Ok(())
    }
}

fn get_view_size_for_layers(layers: &[&flutter_embedder::FlutterLayer]) -> Option<(u32, u32)> {
    layers
        .iter()
        .map(|layer| {
            (
                (layer.offset.x + layer.size.width).ceil() as u32,
                (layer.offset.y + layer.size.height).ceil() as u32,
            )
        })
        .reduce(|(w1, h1), (w2, h2)| (w1.max(w2), h1.max(h2)))
}

struct Framebuffer {
    framebuffer: u32,
    texture: u32,
}

impl Framebuffer {
    unsafe fn new(width: i32, height: i32) -> eyre::Result<Framebuffer> {
        unsafe {
            let mut prev_framebuffer = 0;
            let mut prev_texture = 0;
            gl::GetIntegerv(gl::FRAMEBUFFER_BINDING, &mut prev_framebuffer);
            gl::GetIntegerv(gl::TEXTURE_BINDING_2D, &mut prev_texture);

            let mut texture = 0;
            gl::GenTextures(1, &mut texture);
            gl::BindTexture(gl::TEXTURE_2D, texture);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::NEAREST as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::NEAREST as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);
            gl::TexImage2D(
                gl::TEXTURE_2D,
                0,
                gl::RGBA as i32,
                width,
                height,
                0,
                gl::RGBA,
                gl::UNSIGNED_BYTE,
                ptr::null(),
            );

            let mut framebuffer = 0;
            gl::GenFramebuffers(1, &mut framebuffer);
            gl::BindFramebuffer(gl::FRAMEBUFFER, framebuffer);
            gl::FramebufferTexture2D(
                gl::FRAMEBUFFER,
                gl::COLOR_ATTACHMENT0,
                gl::TEXTURE_2D,
                texture,
                0,
            );

            let status = gl::CheckFramebufferStatus(gl::FRAMEBUFFER);

            gl::BindTexture(gl::TEXTURE_2D, prev_texture as u32);
            gl::BindFramebuffer(gl::FRAMEBUFFER, prev_framebuffer as u32);

            let framebuffer = Framebuffer {
                framebuffer,
                texture,
            };

            if status != gl::FRAMEBUFFER_COMPLETE {
                framebuffer.delete();
                bail!("framebuffer is incomplete (status: {status:#x})");
            }

            Ok(framebuffer)
        }
    }

    unsafe fn delete(&self) {
        unsafe {
            gl::DeleteFramebuffers(1, &self.framebuffer);
            gl::DeleteTextures(1, &self.texture);
        }
    }
}

const VERTEX_SHADER: &CStr = c"
attribute vec2 a_position;
attribute vec2 a_tex_coord;

varying vec2 v_tex_coord;

void main() {
    v_tex_coord = a_tex_coord;
    gl_Position = vec4(a_position, 0.0, 1.0);
}
";

const FRAGMENT_SHADER: &CStr = c"
precision mediump float;

uniform sampler2D u_texture;

varying vec2 v_tex_coord;

void main() {
    gl_FragColor = texture2D(u_texture, v_tex_coord);
}
";

struct LayerRenderer {
    _program: u32,
    _vertex_buffer: u32,
}

impl LayerRenderer {
    unsafe fn new() -> eyre::Result<LayerRenderer> {
        unsafe {
            let program = link_program(VERTEX_SHADER, FRAGMENT_SHADER)?;

            let position = gl::GetAttribLocation(program, c"a_position".as_ptr());
            let tex_coord = gl::GetAttribLocation(program, c"a_tex_coord".as_ptr());
            let texture = gl::GetUniformLocation(program, c"u_texture".as_ptr());

            if position < 0 || tex_coord < 0 || texture < 0 {
                gl::DeleteProgram(program);
                bail!("failed to find layer shader inputs");
            }

            gl::UseProgram(program);
            gl::Uniform1i(texture, 0);

            let mut vertex_buffer = 0;
            gl::GenBuffers(1, &mut vertex_buffer);
            gl::BindBuffer(gl::ARRAY_BUFFER, vertex_buffer);

            let stride = 4 * mem::size_of::<f32>() as i32;
            gl::EnableVertexAttribArray(position as u32);
            gl::VertexAttribPointer(
                position as u32,
                2,
                gl::FLOAT,
                gl::FALSE,
                stride,
                ptr::null(),
            );
            gl::EnableVertexAttribArray(tex_coord as u32);
            gl::VertexAttribPointer(
                tex_coord as u32,
                2,
                gl::FLOAT,
                gl::FALSE,
                stride,
                (2 * mem::size_of::<f32>()) as *const _,
            );

            gl::ActiveTexture(gl::TEXTURE0);

            // Flutter renders with premultiplied alpha.
            gl::Enable(gl::BLEND);
            gl::BlendFunc(gl::ONE, gl::ONE_MINUS_SRC_ALPHA);

            Ok(LayerRenderer {
                _program: program,
                _vertex_buffer: vertex_buffer,
            })
        }
    }

    unsafe fn draw(
        &self,
        texture: u32,
        layer: &flutter_embedder::FlutterLayer,
        view_width: u32,
        view_height: u32,
    ) {
        let (view_width, view_height) = (view_width as f64, view_height as f64);

        let left = layer.offset.x / view_width * 2.0 - 1.0;
        let right = (layer.offset.x + layer.size.width) / view_width * 2.0 - 1.0;
        let top = 1.0 - layer.offset.y / view_height * 2.0;
        let bottom = 1.0 - (layer.offset.y + layer.size.height) / view_height * 2.0;

        let (left, right, top, bottom) = (left as f32, right as f32, top as f32, bottom as f32);

        #[rustfmt::skip]
        let vertices: [f32; 16] = [
            left,  bottom, 0.0, 0.0,
            right, bottom, 1.0, 0.0,
            left,  top,    0.0, 1.0,
            right, top,    1.0, 1.0,
        ];

        unsafe {
            gl::BufferData(
                gl::ARRAY_BUFFER,
                mem::size_of_val(&vertices) as isize,
                vertices.as_ptr().cast(),
                gl::STREAM_DRAW,
            );

            gl::BindTexture(gl::TEXTURE_2D, texture);
            gl::DrawArrays(gl::TRIANGLE_STRIP, 0, 4);
        }
    }
}

unsafe fn link_program(vertex_source: &CStr, fragment_source: &CStr) -> eyre::Result<u32> {
    unsafe {
        let vertex_shader = compile_shader(gl::VERTEX_SHADER, vertex_source)?;
        let fragment_shader = match compile_shader(gl::FRAGMENT_SHADER, fragment_source) {
            Ok(shader) => shader,
            Err(e) => {
                gl::DeleteShader(vertex_shader);
                return Err(e);
            }
        };

        let program = gl::CreateProgram();
        gl::AttachShader(program, vertex_shader);
        gl::AttachShader(program, fragment_shader);
        gl::LinkProgram(program);

        gl::DeleteShader(vertex_shader);
        gl::DeleteShader(fragment_shader);

        let mut status = 0;
        gl::GetProgramiv(program, gl::LINK_STATUS, &mut status);

        if status == 0 {
            let log = info_log(program, gl::GetProgramiv, gl::GetProgramInfoLog);
            gl::DeleteProgram(program);
            bail!("failed to link layer shader program: {log}");
        }

        Ok(program)
    }
}

unsafe fn compile_shader(kind: u32, source: &CStr) -> eyre::Result<u32> {
    unsafe {
        let shader = gl::CreateShader(kind);
        gl::ShaderSource(shader, 1, &source.as_ptr(), ptr::null());
        gl::CompileShader(shader);

        let mut status = 0;
        gl::GetShaderiv(shader, gl::COMPILE_STATUS, &mut status);

        if status == 0 {
            let log = info_log(shader, gl::GetShaderiv, gl::GetShaderInfoLog);
            gl::DeleteShader(shader);
            bail!("failed to compile layer shader: {log}");
        }

        Ok(shader)
    }
}

unsafe fn info_log(
    object: u32,
    get_iv: unsafe fn(u32, u32, *mut i32),
    get_info_log: unsafe fn(u32, i32, *mut i32, *mut c_char),
) -> String {
    unsafe {
        let mut len = 0;
        get_iv(object, gl::INFO_LOG_LENGTH, &mut len);

        let mut log = vec![0u8; len.max(1) as usize];
        let mut written = 0;
        get_info_log(object, len, &mut written, log.as_mut_ptr().cast());
        log.truncate(written as usize);

        String::from_utf8_lossy(&log).into_owned()
    }
}
