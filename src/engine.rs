use std::collections::BTreeMap;
use std::ffi::{c_char, c_void, CStr};
use std::sync::Arc;
use std::{mem, ptr};

use color_eyre::eyre::{self, bail};
use flutter_embedder::{
    FlutterBackingStore, FlutterBackingStoreConfig, FlutterCompositor, FlutterCustomTaskRunners,
    FlutterEngineGetCurrentTime, FlutterEngineInitialize, FlutterEngineResult_kSuccess,
    FlutterEngineRunInitialized, FlutterEngineRunTask, FlutterEngineSendPlatformMessageResponse,
    FlutterEngineSendPointerEvent, FlutterEngineSendWindowMetricsEvent, FlutterLayer,
    FlutterOpenGLRendererConfig, FlutterPlatformMessage, FlutterPlatformMessageResponseHandle,
    FlutterPointerEvent, FlutterPointerPhase, FlutterPointerPhase_kAdd, FlutterPointerPhase_kDown,
    FlutterPointerPhase_kHover, FlutterPointerPhase_kRemove, FlutterPointerPhase_kUp,
    FlutterProjectArgs, FlutterRendererConfig, FlutterRendererType_kOpenGL, FlutterTask,
    FlutterTaskRunnerDescription, FlutterWindowMetricsEvent, FLUTTER_ENGINE_VERSION,
};

use crate::compositor::Compositor;
use crate::egl_manager::EglManager;
use crate::task_runner::{self, Task, TaskRunner};

pub struct FlutterEngineConfig<'a> {
    pub egl_manager: Arc<EglManager>,
    pub compositor: Compositor,
    pub platform_task_handler: Box<dyn Fn(Task)>,
    pub platform_message_handlers: Vec<(&'a str, Box<dyn BinaryMessageHandler>)>,
}

pub struct FlutterEngine {
    inner: &'static FlutterEngineInner,
}

struct FlutterEngineInner {
    handle: flutter_embedder::FlutterEngine,
    egl_manager: Arc<EglManager>,
    platform_message_handlers: BTreeMap<String, Box<dyn BinaryMessageHandler>>,
}

#[repr(i32)]
pub enum PointerPhase {
    Up = FlutterPointerPhase_kUp,
    Down = FlutterPointerPhase_kDown,
    Add = FlutterPointerPhase_kAdd,
    Remove = FlutterPointerPhase_kRemove,
    Hover = FlutterPointerPhase_kHover,
}

impl FlutterEngine {
    pub fn new(config: FlutterEngineConfig) -> eyre::Result<FlutterEngine> {
        let platform_task_runner = create_task_runner(
            1,
            // TODO: Cleanup task runner on engine shutdown
            Box::leak(Box::new(TaskRunner::new(move |task| {
                (config.platform_task_handler)(task);
            }))),
        );

        let renderer_config = FlutterRendererConfig {
            type_: FlutterRendererType_kOpenGL,
            __bindgen_anon_1: flutter_embedder::FlutterRendererConfig__bindgen_ty_1 {
                open_gl: FlutterOpenGLRendererConfig {
                    struct_size: mem::size_of::<FlutterOpenGLRendererConfig>(),
                    make_current: Some(gl_make_current),
                    make_resource_current: Some(gl_make_resource_current),
                    clear_current: Some(gl_clear_current),
                    present: Some(gl_present),
                    fbo_callback: Some(gl_fbo_callback),
                    fbo_reset_after_present: true,
                    gl_proc_resolver: Some(gl_get_proc_address),
                    ..Default::default()
                },
            },
        };

        let project_args = FlutterProjectArgs {
            struct_size: mem::size_of::<FlutterProjectArgs>(),
            assets_path: c"example/build/flutter_assets".as_ptr(),
            icu_data_path: c"icudtl.dat".as_ptr(),
            custom_task_runners: &FlutterCustomTaskRunners {
                struct_size: mem::size_of::<FlutterCustomTaskRunners>(),
                platform_task_runner: &platform_task_runner,
                render_task_runner: ptr::null(),
                thread_priority_setter: Some(task_runner::set_thread_priority),
            },
            compositor: &FlutterCompositor {
                struct_size: mem::size_of::<FlutterCompositor>(),
                create_backing_store_callback: Some(compositor_create_backing_store),
                collect_backing_store_callback: Some(compositor_collect_backing_store),
                present_layers_callback: Some(compositor_present_layers),
                present_view_callback: None,
                user_data: Box::leak(Box::new(config.compositor)) as *mut Compositor as *mut c_void,
                avoid_backing_store_cache: false,
            },
            platform_message_callback: Some(platform_message_callback),
            ..Default::default()
        };

        let engine = Box::leak(Box::new(FlutterEngineInner {
            handle: ptr::null_mut(),
            egl_manager: config.egl_manager,
            platform_message_handlers: BTreeMap::from_iter(
                config
                    .platform_message_handlers
                    .into_iter()
                    .map(|(channel, handler)| (channel.to_owned(), handler)),
            ),
        }));

        let engine_handle = unsafe {
            let mut engine_ptr = ptr::null_mut();

            let result = FlutterEngineInitialize(
                FLUTTER_ENGINE_VERSION as usize,
                &renderer_config,
                &project_args,
                engine as *mut FlutterEngineInner as _,
                &mut engine_ptr,
            );

            if result != FlutterEngineResult_kSuccess || engine_ptr.is_null() {
                panic!("could not run the flutter engine");
            }

            engine_ptr
        };

        engine.handle = engine_handle;

        unsafe {
            FlutterEngineRunInitialized(engine_handle);
        }

        Ok(FlutterEngine { inner: engine })
    }

    pub fn send_window_metrics_event(
        &self,
        width: usize,
        height: usize,
        pixel_ratio: f64,
    ) -> eyre::Result<()> {
        let result = unsafe {
            FlutterEngineSendWindowMetricsEvent(
                self.inner.handle,
                &FlutterWindowMetricsEvent {
                    struct_size: mem::size_of::<FlutterWindowMetricsEvent>(),
                    width,
                    height,
                    pixel_ratio,
                    ..Default::default()
                },
            )
        };

        if result != FlutterEngineResult_kSuccess {
            bail!("failed to send window metrics event: {result}");
        }

        Ok(())
    }

    pub fn run_task(&self, task: &FlutterTask) -> eyre::Result<()> {
        let result = unsafe { FlutterEngineRunTask(self.inner.handle, task) };

        if result != FlutterEngineResult_kSuccess {
            bail!("failed to run task: {result}");
        }

        Ok(())
    }

    pub fn send_pointer_event(&self, phase: PointerPhase, x: f64, y: f64) -> eyre::Result<()> {
        let result = unsafe {
            FlutterEngineSendPointerEvent(
                self.inner.handle,
                &FlutterPointerEvent {
                    struct_size: mem::size_of::<FlutterPointerEvent>(),
                    phase: phase as FlutterPointerPhase,
                    x,
                    y,
                    timestamp: FlutterEngineGetCurrentTime() as usize,
                    ..Default::default()
                },
                1,
            )
        };

        if result != FlutterEngineResult_kSuccess {
            bail!("failed to run task: {result}");
        }

        Ok(())
    }
}

fn create_task_runner<F: Fn(Task)>(
    id: usize,
    runner: &'static TaskRunner<F>,
) -> FlutterTaskRunnerDescription {
    unsafe extern "C" fn runs_tasks_on_current_thread<F>(task_runner: *mut c_void) -> bool {
        task_runner
            .cast::<TaskRunner<F>>()
            .as_mut()
            .unwrap()
            .runs_tasks_on_current_thread()
    }

    unsafe extern "C" fn post_task_callback<F: Fn(Task)>(
        task: FlutterTask,
        target_time_nanos: u64,
        user_data: *mut c_void,
    ) {
        let runner = user_data.cast::<TaskRunner<F>>();
        (*runner).post_task(task, target_time_nanos);
    }

    FlutterTaskRunnerDescription {
        struct_size: mem::size_of::<FlutterTaskRunnerDescription>(),
        identifier: id,
        user_data: runner as *const TaskRunner<F> as *mut c_void,
        runs_task_on_current_thread_callback: Some(runs_tasks_on_current_thread::<F>),
        post_task_callback: Some(post_task_callback::<F>),
    }
}

pub trait BinaryMessageHandler {
    fn handle(&self, message: &[u8], reply: BinaryMessageReply);
}

pub struct BinaryMessageReply {
    engine: flutter_embedder::FlutterEngine,
    response_handle: *const FlutterPlatformMessageResponseHandle,
}

impl BinaryMessageReply {
    pub fn send(self, message: &[u8]) {
        unsafe {
            FlutterEngineSendPlatformMessageResponse(
                self.engine,
                self.response_handle,
                message.as_ptr(),
                message.len(),
            );
        }
    }

    pub fn not_implemented(self) {
        unsafe {
            FlutterEngineSendPlatformMessageResponse(
                self.engine,
                self.response_handle,
                std::ptr::null(),
                0,
            );
        }
    }
}

unsafe extern "C" fn platform_message_callback(
    message: *const FlutterPlatformMessage,
    user_data: *mut c_void,
) {
    let engine = user_data.cast::<FlutterEngineInner>().as_ref().unwrap();
    let message = message.as_ref().unwrap();

    let reply = BinaryMessageReply {
        engine: engine.handle,
        response_handle: message.response_handle,
    };

    let channel = CStr::from_ptr(message.channel);
    let Ok(channel) = channel.to_str() else {
        tracing::error!("invalid channel name: {channel:?}");
        reply.not_implemented();
        return;
    };

    let Some(handler) = engine.platform_message_handlers.get(channel) else {
        tracing::warn!(channel, "unimplemented");
        reply.not_implemented();
        return;
    };

    if message.message.is_null() {
        tracing::error!(channel, "message is null");
        reply.not_implemented();
        return;
    }

    let bytes = std::slice::from_raw_parts(message.message, message.message_size);

    handler.handle(bytes, reply);
}

unsafe extern "C" fn gl_make_current(user_data: *mut c_void) -> bool {
    let engine = user_data.cast::<FlutterEngineInner>().as_ref().unwrap();

    if let Err(e) = engine.egl_manager.make_context_current() {
        tracing::error!("failed to make context current: {e}");
        return false;
    }

    true
}

unsafe extern "C" fn gl_make_resource_current(user_data: *mut c_void) -> bool {
    let engine = user_data.cast::<FlutterEngineInner>().as_ref().unwrap();

    if let Err(e) = engine.egl_manager.make_resource_context_current() {
        tracing::error!("failed to make resource context current: {e}");
        return false;
    }

    true
}

unsafe extern "C" fn gl_clear_current(user_data: *mut c_void) -> bool {
    let engine = user_data.cast::<FlutterEngineInner>().as_ref().unwrap();

    if let Err(e) = engine.egl_manager.clear_current() {
        tracing::error!("failed to clear context: {e}");
        return false;
    }

    true
}

unsafe extern "C" fn gl_present(_user_data: *mut c_void) -> bool {
    false
}

unsafe extern "C" fn gl_fbo_callback(_user_data: *mut c_void) -> u32 {
    0
}

unsafe extern "C" fn gl_get_proc_address(
    user_data: *mut c_void,
    name: *const c_char,
) -> *mut c_void {
    let engine = user_data.cast::<FlutterEngineInner>().as_ref().unwrap();
    let name = CStr::from_ptr(name);
    engine
        .egl_manager
        .get_proc_address(name.to_str().unwrap())
        .unwrap_or(ptr::null_mut())
}

pub unsafe extern "C" fn compositor_create_backing_store(
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

pub unsafe extern "C" fn compositor_collect_backing_store(
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

pub unsafe extern "C" fn compositor_present_layers(
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
