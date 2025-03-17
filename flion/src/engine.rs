use std::collections::BTreeMap;
use std::ffi::{c_char, c_void, CStr, CString};
use std::str::FromStr;
use std::sync::Arc;
use std::{mem, ptr};

use eyre::bail;
use flutter_embedder::{
    FlutterBackingStore, FlutterBackingStoreConfig, FlutterCustomTaskRunners,
    FlutterEngineGetCurrentTime, FlutterEngineInitialize, FlutterEngineResult_kSuccess,
    FlutterEngineRunInitialized, FlutterEngineRunTask, FlutterEngineSendKeyEvent,
    FlutterEngineSendPlatformMessage, FlutterEngineSendPlatformMessageResponse,
    FlutterEngineSendPointerEvent, FlutterEngineSendWindowMetricsEvent, FlutterKeyEvent,
    FlutterKeyEventDeviceType_kFlutterKeyEventDeviceTypeKeyboard,
    FlutterKeyEventType_kFlutterKeyEventTypeDown, FlutterKeyEventType_kFlutterKeyEventTypeRepeat,
    FlutterKeyEventType_kFlutterKeyEventTypeUp, FlutterLayer, FlutterOpenGLRendererConfig,
    FlutterPlatformMessage, FlutterPlatformMessageCreateResponseHandle,
    FlutterPlatformMessageReleaseResponseHandle, FlutterPlatformMessageResponseHandle,
    FlutterPointerDeviceKind, FlutterPointerDeviceKind_kFlutterPointerDeviceKindMouse,
    FlutterPointerDeviceKind_kFlutterPointerDeviceKindStylus,
    FlutterPointerDeviceKind_kFlutterPointerDeviceKindTouch,
    FlutterPointerDeviceKind_kFlutterPointerDeviceKindTrackpad, FlutterPointerEvent,
    FlutterPointerPhase, FlutterPointerPhase_kAdd, FlutterPointerPhase_kDown,
    FlutterPointerPhase_kHover, FlutterPointerPhase_kMove, FlutterPointerPhase_kRemove,
    FlutterPointerPhase_kUp, FlutterPointerSignalKind_kFlutterPointerSignalKindScroll,
    FlutterProjectArgs, FlutterRendererConfig, FlutterRendererType_kOpenGL, FlutterTask,
    FlutterTaskRunnerDescription, FlutterTransformation, FlutterWindowMetricsEvent,
    FLUTTER_ENGINE_VERSION,
};
use parking_lot::Mutex;
use smol_str::SmolStr;

use crate::compositor::FlutterCompositor;
use crate::egl_manager::EglManager;
use crate::task_runner::{self, Task, TaskRunner};

pub struct FlutterEngineConfig<'a> {
    pub assets_path: &'a str,
    pub egl_manager: Arc<EglManager>,
    pub compositor: FlutterCompositor,
    pub platform_task_handler: Box<dyn Fn(Task)>,
    pub platform_message_handlers: Vec<(&'a str, Box<dyn BinaryMessageHandler + 'static>)>,
}

pub struct FlutterEngine {
    inner: &'static FlutterEngineInner,
}

struct FlutterEngineInner {
    handle: flutter_embedder::FlutterEngine,
    egl_manager: Arc<EglManager>,
    compositor: *mut FlutterCompositor,
    platform_message_handlers: Mutex<BTreeMap<String, Box<dyn BinaryMessageHandler + 'static>>>,
}

#[repr(i32)]
pub enum PointerDeviceKind {
    Mouse = FlutterPointerDeviceKind_kFlutterPointerDeviceKindMouse,
    Touch = FlutterPointerDeviceKind_kFlutterPointerDeviceKindTouch,
    Stylus = FlutterPointerDeviceKind_kFlutterPointerDeviceKindStylus,
    Trackpad = FlutterPointerDeviceKind_kFlutterPointerDeviceKindTrackpad,
}

#[derive(Clone, Copy)]
#[repr(i32)]
pub enum PointerPhase {
    Up = FlutterPointerPhase_kUp,
    Down = FlutterPointerPhase_kDown,
    Add = FlutterPointerPhase_kAdd,
    Remove = FlutterPointerPhase_kRemove,
    Hover = FlutterPointerPhase_kHover,
    Move = FlutterPointerPhase_kMove,
}

#[repr(i32)]
pub enum KeyEventType {
    Up = FlutterKeyEventType_kFlutterKeyEventTypeUp,
    Down = FlutterKeyEventType_kFlutterKeyEventTypeDown,
    Repeat = FlutterKeyEventType_kFlutterKeyEventTypeRepeat,
}

pub struct KeyEvent<'a> {
    pub event_type: KeyEventType,
    pub synthesized: bool,
    pub character: Option<&'a SmolStr>,
    pub logical: Option<u64>,
    pub physical: Option<u64>,
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
                    surface_transformation: Some(gl_get_surface_transformation),
                    ..Default::default()
                },
            },
        };

        let assets_path = CString::from_str(config.assets_path)?;

        let compositor = &raw mut *Box::leak(Box::new(config.compositor));

        let project_args = FlutterProjectArgs {
            struct_size: mem::size_of::<FlutterProjectArgs>(),
            assets_path: assets_path.as_ptr(),
            icu_data_path: c"icudtl.dat".as_ptr(),
            custom_task_runners: &FlutterCustomTaskRunners {
                struct_size: mem::size_of::<FlutterCustomTaskRunners>(),
                platform_task_runner: &platform_task_runner,
                render_task_runner: ptr::null(),
                ui_task_runner: ptr::null(),
                thread_priority_setter: Some(task_runner::set_thread_priority),
            },
            compositor: &flutter_embedder::FlutterCompositor {
                struct_size: mem::size_of::<FlutterCompositor>(),
                create_backing_store_callback: Some(compositor_create_backing_store),
                collect_backing_store_callback: Some(compositor_collect_backing_store),
                present_layers_callback: Some(compositor_present_layers),
                present_view_callback: None,
                user_data: compositor.cast(),
                avoid_backing_store_cache: false,
            },
            platform_message_callback: Some(platform_message_callback),
            log_message_callback: Some(log_message),
            // vsync_callback: Some(vsync_callback),
            ..Default::default()
        };

        let engine = Box::leak(Box::new(FlutterEngineInner {
            handle: ptr::null_mut(),
            egl_manager: config.egl_manager,
            platform_message_handlers: Mutex::new(BTreeMap::from_iter(
                config
                    .platform_message_handlers
                    .into_iter()
                    .map(|(channel, handler)| (channel.to_owned(), handler)),
            )),
            compositor,
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

    pub(crate) fn as_raw(&self) -> flutter_embedder::FlutterEngine {
        self.inner.handle
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

    pub fn send_pointer_event(
        &self,
        device_kind: PointerDeviceKind,
        device_id: i32,
        phase: PointerPhase,
        x: f64,
        y: f64,
    ) -> eyre::Result<()> {
        let result = unsafe {
            FlutterEngineSendPointerEvent(
                self.inner.handle,
                &FlutterPointerEvent {
                    struct_size: mem::size_of::<FlutterPointerEvent>(),
                    device_kind: device_kind as FlutterPointerDeviceKind,
                    device: device_id,
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
            bail!("failed to send pointer event: {result}");
        }

        Ok(())
    }

    pub fn send_scroll_event(
        &self,
        x: f64,
        y: f64,
        scroll_delta_x: f64,
        scroll_delta_y: f64,
    ) -> eyre::Result<()> {
        let result = unsafe {
            FlutterEngineSendPointerEvent(
                self.inner.handle,
                &FlutterPointerEvent {
                    struct_size: mem::size_of::<FlutterPointerEvent>(),
                    signal_kind: FlutterPointerSignalKind_kFlutterPointerSignalKindScroll,
                    x,
                    y,
                    scroll_delta_x,
                    scroll_delta_y,
                    timestamp: FlutterEngineGetCurrentTime() as usize,
                    ..Default::default()
                },
                1,
            )
        };

        if result != FlutterEngineResult_kSuccess {
            bail!("failed to send pointer event: {result}");
        }

        Ok(())
    }

    pub fn send_key_event<F>(&self, event: KeyEvent, callback: F) -> eyre::Result<()>
    where
        F: FnOnce(bool) + 'static,
    {
        unsafe extern "C" fn _callback<F: FnOnce(bool)>(
            handled: bool,
            user_data: *mut ::std::os::raw::c_void,
        ) {
            Box::from_raw(user_data.cast::<F>())(handled);
        }

        let reply = Box::leak(Box::new(callback));

        let event = FlutterKeyEvent {
            struct_size: mem::size_of::<FlutterKeyEvent>(),
            timestamp: unsafe { FlutterEngineGetCurrentTime() as f64 },
            type_: event.event_type as i32,
            character: event
                .character
                .map(|c| c.as_ptr().cast())
                .unwrap_or(ptr::null()),
            synthesized: event.synthesized,
            logical: event.logical.unwrap_or(0),
            physical: event.physical.unwrap_or(0),
            device_type: FlutterKeyEventDeviceType_kFlutterKeyEventDeviceTypeKeyboard,
        };

        unsafe {
            let result = FlutterEngineSendKeyEvent(
                self.inner.handle,
                &event,
                Some(_callback::<F>),
                reply as *mut F as _,
            );

            if result != FlutterEngineResult_kSuccess {
                bail!("failed to send key event: {result}");
            }
        }

        Ok(())
    }

    pub fn send_platform_message(&self, channel: &CStr, message: &[u8]) -> eyre::Result<()> {
        unsafe {
            let result = FlutterEngineSendPlatformMessage(
                self.inner.handle,
                &FlutterPlatformMessage {
                    struct_size: mem::size_of::<FlutterPlatformMessage>(),
                    channel: channel.as_ptr(),
                    message: message.as_ptr(),
                    message_size: message.len(),
                    response_handle: ptr::null_mut(),
                },
            );

            if result != FlutterEngineResult_kSuccess {
                bail!("failed to send platform message: {result}");
            }

            Ok(())
        }
    }

    pub fn send_platform_message_with_reply<F>(
        &self,
        channel: &CStr,
        message: &[u8],
        reply_handler: F,
    ) -> eyre::Result<()>
    where
        F: FnOnce(&[u8]) + 'static,
    {
        unsafe extern "C" fn callback<F: FnOnce(&[u8])>(
            data: *const u8,
            size: usize,
            user_data: *mut ::std::os::raw::c_void,
        ) {
            let reply_handler = Box::from_raw(user_data.cast::<F>());
            if data.is_null() {
                tracing::warn!("null reply from platform message");
            } else {
                reply_handler(std::slice::from_raw_parts(data, size));
            }
        }

        unsafe {
            let mut response_handle = ptr::null_mut();

            let reply = Box::leak(Box::new(reply_handler));
            let result = FlutterPlatformMessageCreateResponseHandle(
                self.inner.handle,
                Some(callback::<F>),
                reply as *mut F as _,
                &mut response_handle,
            );

            if result != FlutterEngineResult_kSuccess {
                bail!("failed to create response handle: {result}");
            }

            let result = FlutterEngineSendPlatformMessage(
                self.inner.handle,
                &FlutterPlatformMessage {
                    struct_size: mem::size_of::<FlutterPlatformMessage>(),
                    channel: channel.as_ptr(),
                    message: message.as_ptr(),
                    message_size: message.len(),
                    response_handle,
                },
            );

            if result != FlutterEngineResult_kSuccess {
                bail!("failed to send platform message: {result}");
            }

            let result =
                FlutterPlatformMessageReleaseResponseHandle(self.inner.handle, response_handle);

            if result != FlutterEngineResult_kSuccess {
                bail!("failed to release response handle: {result}");
            }

            Ok(())
        }
    }

    pub fn set_platform_message_handler(
        &self,
        name: impl Into<String>,
        handler: impl BinaryMessageHandler + 'static,
    ) {
        self.inner
            .platform_message_handlers
            .lock()
            .insert(name.into(), Box::new(handler));
    }
}

fn create_task_runner<F: Fn(Task) + 'static>(
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
        destruction_callback: None,
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
    pub(crate) fn new(
        engine: flutter_embedder::FlutterEngine,
        response_handle: *const FlutterPlatformMessageResponseHandle,
    ) -> BinaryMessageReply {
        BinaryMessageReply {
            engine,
            response_handle,
        }
    }

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

    pub(crate) fn into_raw(self) -> *const FlutterPlatformMessageResponseHandle {
        self.response_handle
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

    let handlers = engine.platform_message_handlers.lock();
    let Some(handler) = handlers.get(channel) else {
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

unsafe extern "C" fn gl_get_surface_transformation(
    user_data: *mut c_void,
) -> FlutterTransformation {
    let engine = user_data.cast::<FlutterEngineInner>().as_ref().unwrap();
    let compositor = engine.compositor.as_mut().unwrap();
    match compositor.get_surface_transformation() {
        Ok(transformation) => transformation,
        Err(e) => {
            tracing::error!("failed to get surface transformation: {e:?}");
            FlutterTransformation {
                scaleX: 1.0,
                scaleY: 1.0,
                pers2: 1.0,
                ..Default::default()
            }
        }
    }
}

pub unsafe extern "C" fn compositor_create_backing_store(
    config: *const FlutterBackingStoreConfig,
    out: *mut FlutterBackingStore,
    user_data: *mut c_void,
) -> bool {
    let Some(compositor) = user_data.cast::<FlutterCompositor>().as_mut() else {
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
        tracing::error!("{e:?}");
        return false;
    }

    true
}

pub unsafe extern "C" fn compositor_collect_backing_store(
    backing_store: *const FlutterBackingStore,
    user_data: *mut c_void,
) -> bool {
    let Some(compositor) = user_data.cast::<FlutterCompositor>().as_mut() else {
        tracing::error!("user_data is null");
        return false;
    };

    let Some(backing_store) = backing_store.as_ref() else {
        tracing::error!("config is null");
        return false;
    };

    if let Err(e) = compositor.collect_backing_store(backing_store) {
        tracing::error!("{e:?}");
        return false;
    }

    true
}

pub unsafe extern "C" fn compositor_present_layers(
    layers: *mut *const FlutterLayer,
    layers_count: usize,
    user_data: *mut c_void,
) -> bool {
    let Some(compositor) = user_data.cast::<FlutterCompositor>().as_mut() else {
        tracing::error!("user_data is null");
        return false;
    };

    if layers.is_null() {
        tracing::error!("layers is null");
        return false;
    }

    let layers = std::slice::from_raw_parts(layers.cast::<&FlutterLayer>(), layers_count);

    if let Err(e) = compositor.present_layers(layers) {
        tracing::error!("{e:?}");
        return false;
    };

    true
}

unsafe extern "C" fn log_message(tag: *const c_char, message: *const c_char, _: *mut c_void) {
    let tag = CStr::from_ptr(tag).to_string_lossy();
    let message = CStr::from_ptr(message).to_string_lossy();
    eprintln!("{tag}: {message}");
}
