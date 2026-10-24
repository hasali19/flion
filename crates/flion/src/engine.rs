use std::collections::BTreeMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use std::thread::{self, ThreadId};
use std::{mem, ptr};

use bitflags::bitflags;
use eyre::{Context, bail, eyre};
use flutter_embedder::{
    FlutterAddViewInfo, FlutterAddViewResult, FlutterBackingStore, FlutterBackingStoreConfig,
    FlutterCustomTaskRunners, FlutterEngineAOTData, FlutterEngineAOTDataSource,
    FlutterEngineAOTDataSource__bindgen_ty_1,
    FlutterEngineAOTDataSourceType_kFlutterEngineAOTDataSourceTypeElfPath, FlutterEngineAddView,
    FlutterEngineCreateAOTData, FlutterEngineGetCurrentTime, FlutterEngineInitialize,
    FlutterEngineResult_kSuccess, FlutterEngineRunInitialized, FlutterEngineRunTask,
    FlutterEngineRunsAOTCompiledDartCode, FlutterEngineSendKeyEvent,
    FlutterEngineSendPlatformMessage, FlutterEngineSendPlatformMessageResponse,
    FlutterEngineSendPointerEvent, FlutterEngineSendWindowMetricsEvent, FlutterEngineShutdown,
    FlutterKeyEvent, FlutterKeyEventDeviceType_kFlutterKeyEventDeviceTypeKeyboard,
    FlutterKeyEventType_kFlutterKeyEventTypeDown, FlutterKeyEventType_kFlutterKeyEventTypeRepeat,
    FlutterKeyEventType_kFlutterKeyEventTypeUp, FlutterLayer, FlutterOpenGLRendererConfig,
    FlutterPlatformMessage, FlutterPlatformMessageCreateResponseHandle,
    FlutterPlatformMessageReleaseResponseHandle, FlutterPlatformMessageResponseHandle,
    FlutterPointerDeviceKind, FlutterPointerDeviceKind_kFlutterPointerDeviceKindMouse,
    FlutterPointerDeviceKind_kFlutterPointerDeviceKindStylus,
    FlutterPointerDeviceKind_kFlutterPointerDeviceKindTouch,
    FlutterPointerDeviceKind_kFlutterPointerDeviceKindTrackpad, FlutterPointerEvent,
    FlutterPointerMouseButtons, FlutterPointerMouseButtons_kFlutterPointerButtonMouseBack,
    FlutterPointerMouseButtons_kFlutterPointerButtonMouseForward,
    FlutterPointerMouseButtons_kFlutterPointerButtonMouseMiddle,
    FlutterPointerMouseButtons_kFlutterPointerButtonMousePrimary,
    FlutterPointerMouseButtons_kFlutterPointerButtonMouseSecondary, FlutterPointerPhase,
    FlutterPointerPhase_kAdd, FlutterPointerPhase_kCancel, FlutterPointerPhase_kDown,
    FlutterPointerPhase_kHover, FlutterPointerPhase_kMove, FlutterPointerPhase_kRemove,
    FlutterPointerPhase_kUp, FlutterPointerSignalKind_kFlutterPointerSignalKindNone,
    FlutterPointerSignalKind_kFlutterPointerSignalKindScroll, FlutterPresentViewInfo,
    FlutterProjectArgs, FlutterRendererConfig, FlutterRendererConfig__bindgen_ty_1,
    FlutterRendererType_kOpenGL, FlutterTaskRunnerDescription, FlutterWindowMetricsEvent,
};
use parking_lot::Mutex;
use smol_str::SmolStr;
use tracing::debug;

const FLUTTER_ENGINE_VERSION: usize = flutter_embedder::FLUTTER_ENGINE_VERSION as usize;

pub struct FlutterEngineConfig<'a, C> {
    pub assets_path: &'a str,
    pub aot_library_path: Option<&'a str>,
    pub compositor: C,
    pub platform_task_handler: Box<dyn Fn(FlutterTask) + Send + Sync>,
    pub platform_message_handlers: Vec<(&'a str, Box<dyn BinaryMessageHandler + 'static>)>,
}

pub trait FlutterCompositor {
    const AVOID_BACKING_STORE_CACHE: bool;
}

pub trait FlutterOpenGLCompositor: Send + Sync + 'static {
    fn make_current(&self) -> eyre::Result<()>;

    fn make_resource_current(&self) -> eyre::Result<()>;

    fn clear_current(&self) -> eyre::Result<()>;

    fn get_proc_address(&self, name: &str) -> Option<extern "system" fn()>;

    fn create_backing_store(
        &self,
        config: &FlutterBackingStoreConfig,
        backing_store: &mut FlutterBackingStore,
    ) -> eyre::Result<()>;

    fn release_backing_store(&self, backing_store: &FlutterBackingStore) -> eyre::Result<()>;

    fn present_view(&self, view_id: i64, layers: &[&FlutterLayer]) -> eyre::Result<()>;
}

#[derive(Clone, Copy, Default)]
#[repr(u32)]
pub enum PointerDeviceKind {
    #[default]
    Unknown = 0,
    Mouse = FlutterPointerDeviceKind_kFlutterPointerDeviceKindMouse,
    Touch = FlutterPointerDeviceKind_kFlutterPointerDeviceKindTouch,
    #[expect(unused)]
    Stylus = FlutterPointerDeviceKind_kFlutterPointerDeviceKindStylus,
    #[expect(unused)]
    Trackpad = FlutterPointerDeviceKind_kFlutterPointerDeviceKindTrackpad,
}

#[derive(Clone, Copy, Default, Debug)]
#[repr(u32)]
pub enum PointerPhase {
    #[default]
    Cancel = FlutterPointerPhase_kCancel,
    Up = FlutterPointerPhase_kUp,
    Down = FlutterPointerPhase_kDown,
    Add = FlutterPointerPhase_kAdd,
    Remove = FlutterPointerPhase_kRemove,
    Hover = FlutterPointerPhase_kHover,
    Move = FlutterPointerPhase_kMove,
}

bitflags! {
    #[derive(Clone, Copy, Default)]
    pub struct PointerButtons: FlutterPointerMouseButtons {
        const PRIMARY = FlutterPointerMouseButtons_kFlutterPointerButtonMousePrimary;
        const SECONDARY = FlutterPointerMouseButtons_kFlutterPointerButtonMouseSecondary;
        const MIDDLE = FlutterPointerMouseButtons_kFlutterPointerButtonMouseMiddle;
        const BACK = FlutterPointerMouseButtons_kFlutterPointerButtonMouseBack;
        const FORWARD = FlutterPointerMouseButtons_kFlutterPointerButtonMouseForward;
    }
}

#[derive(Clone, Copy, Default)]
pub enum PointerSignal {
    #[default]
    None,
    Scroll {
        delta_x: f64,
        delta_y: f64,
    },
}

#[derive(Default)]
pub struct PointerEvent {
    pub view_id: i64,
    pub device_kind: PointerDeviceKind,
    pub device_id: i32,
    pub phase: PointerPhase,
    pub x: f64,
    pub y: f64,
    pub buttons: PointerButtons,
    pub signal: PointerSignal,
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum KeyEventType {
    Up = FlutterKeyEventType_kFlutterKeyEventTypeUp,
    Down = FlutterKeyEventType_kFlutterKeyEventTypeDown,
    Repeat = FlutterKeyEventType_kFlutterKeyEventTypeRepeat,
}

#[derive(Clone)]
pub struct KeyEvent {
    pub event_type: KeyEventType,
    pub synthesized: bool,
    pub character: Option<SmolStr>,
    pub logical: Option<u64>,
    pub physical: Option<u64>,
}

struct FlutterEngineState {
    handle: flutter_embedder::FlutterEngine,
    is_running: Arc<Mutex<bool>>,
    compositor: Box<dyn FlutterOpenGLCompositor>,
    platform_message_handlers: Mutex<BTreeMap<String, Box<dyn BinaryMessageHandler + 'static>>>,
}

pub struct FlutterEngine {
    inner: *mut FlutterEngineState,
}

impl FlutterEngine {
    pub fn new<C: FlutterCompositor + FlutterOpenGLCompositor>(
        config: FlutterEngineConfig<'_, C>,
    ) -> eyre::Result<FlutterEngine> {
        let platform_runner = Arc::new(FlutterTaskRunner::new(move |task| {
            (config.platform_task_handler)(task)
        }));

        let renderer_config = FlutterRendererConfig {
            type_: FlutterRendererType_kOpenGL,
            __bindgen_anon_1: FlutterRendererConfig__bindgen_ty_1 {
                open_gl: FlutterOpenGLRendererConfig {
                    struct_size: mem::size_of::<FlutterOpenGLRendererConfig>(),
                    make_current: Some(gl_make_current),
                    make_resource_current: Some(gl_make_resource_current),
                    clear_current: Some(gl_clear_current),
                    present: Some(gl_present),
                    fbo_callback: Some(gl_fbo_callback),
                    fbo_reset_after_present: true,
                    gl_proc_resolver: Some(gl_get_proc_address),
                    // surface_transformation: Some(gl_get_surface_transformation),
                    ..Default::default()
                },
            },
        };

        let compositor = Box::new(config.compositor);

        let platform_message_handlers = BTreeMap::from_iter(
            config
                .platform_message_handlers
                .into_iter()
                .map(|(channel, handler)| (channel.to_owned(), handler)),
        );

        let engine_state = Box::into_raw(Box::new(FlutterEngineState {
            handle: ptr::null_mut(),
            is_running: Arc::new(Mutex::new(false)),
            compositor,
            platform_message_handlers: Mutex::new(platform_message_handlers),
        }));

        let assets_path = CString::new(config.assets_path)?;
        let aot_data = load_aot_data(config.aot_library_path)?.unwrap_or(ptr::null_mut());

        let project_args = FlutterProjectArgs {
            struct_size: mem::size_of::<FlutterProjectArgs>(),
            assets_path: assets_path.as_ptr(),
            icu_data_path: c"icudtl.dat".as_ptr(),
            aot_data,
            custom_task_runners: &FlutterCustomTaskRunners {
                struct_size: mem::size_of::<FlutterCustomTaskRunners>(),
                platform_task_runner: &create_task_runner(1, platform_runner.clone()),
                render_task_runner: ptr::null(),
                ui_task_runner: &create_task_runner(1, platform_runner),
                thread_priority_setter: None,
            },
            compositor: &flutter_embedder::FlutterCompositor {
                struct_size: mem::size_of::<flutter_embedder::FlutterCompositor>(),
                avoid_backing_store_cache: C::AVOID_BACKING_STORE_CACHE,
                create_backing_store_callback: Some(compositor_create_backing_store),
                collect_backing_store_callback: Some(compositor_collect_backing_store),
                present_layers_callback: None,
                present_view_callback: Some(compositor_present_view),
                user_data: engine_state.cast(),
            },
            platform_message_callback: Some(platform_message_callback),
            log_message_callback: Some(log_message),
            ..Default::default()
        };

        let engine = unsafe {
            let mut engine_ptr = ptr::null_mut();

            let result = FlutterEngineInitialize(
                FLUTTER_ENGINE_VERSION,
                &renderer_config,
                &project_args,
                engine_state.cast(),
                &mut engine_ptr,
            );

            if result != FlutterEngineResult_kSuccess || engine_ptr.is_null() {
                panic!("could not run the flutter engine");
            }

            engine_ptr
        };

        unsafe {
            *(*engine_state).is_running.lock() = true;
            (*engine_state).handle = engine;
            FlutterEngineRunInitialized(engine);
        }

        Ok(FlutterEngine {
            inner: engine_state,
        })
    }

    pub fn add_view<F: FnOnce(bool) + Send>(
        &self,
        view_id: i64,
        width: usize,
        height: usize,
        pixel_ratio: f64,
        callback: F,
    ) -> eyre::Result<()> {
        let callback = Box::into_raw(Box::new(callback));

        extern "C" fn trampoline<F: FnOnce(bool) + Send>(result: *const FlutterAddViewResult) {
            let success = unsafe { (*result).added };
            let callback = unsafe { Box::from_raw((*result).user_data.cast::<F>()) };
            callback(success);
        }

        let result = unsafe {
            FlutterEngineAddView(
                self.engine(),
                &FlutterAddViewInfo {
                    struct_size: mem::size_of::<FlutterAddViewInfo>(),
                    view_id,
                    view_metrics: &FlutterWindowMetricsEvent {
                        struct_size: mem::size_of::<FlutterWindowMetricsEvent>(),
                        view_id,
                        width,
                        height,
                        pixel_ratio,
                        ..Default::default()
                    },
                    user_data: callback.cast(),
                    add_view_callback: Some(trampoline::<F>),
                },
            )
        };

        if result != FlutterEngineResult_kSuccess {
            unsafe { drop(Box::from_raw(callback)) };
            return Err(eyre!("Failed to add view"));
        }

        Ok(())
    }

    pub fn send_window_metrics_event(
        &self,
        view_id: i64,
        width: usize,
        height: usize,
        pixel_ratio: f64,
    ) -> eyre::Result<()> {
        let result = unsafe {
            FlutterEngineSendWindowMetricsEvent(
                self.engine(),
                &FlutterWindowMetricsEvent {
                    struct_size: mem::size_of::<FlutterWindowMetricsEvent>(),
                    view_id,
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
        let result = unsafe { FlutterEngineRunTask(self.engine(), &raw const task.task) };

        if result != FlutterEngineResult_kSuccess {
            bail!("failed to run task: {result}");
        }

        Ok(())
    }

    pub fn send_pointer_event(&self, event: &PointerEvent) -> eyre::Result<()> {
        self.send_pointer_events(std::slice::from_ref(event))
    }

    pub fn send_pointer_events(&self, events: &[PointerEvent]) -> eyre::Result<()> {
        let timestamp = unsafe { FlutterEngineGetCurrentTime() as usize };

        let events: Vec<FlutterPointerEvent> = events
            .iter()
            .map(|event| {
                let (signal_kind, scroll_delta_x, scroll_delta_y) = match event.signal {
                    PointerSignal::None => (
                        FlutterPointerSignalKind_kFlutterPointerSignalKindNone,
                        0.0,
                        0.0,
                    ),
                    PointerSignal::Scroll { delta_x, delta_y } => (
                        FlutterPointerSignalKind_kFlutterPointerSignalKindScroll,
                        delta_x,
                        delta_y,
                    ),
                };

                FlutterPointerEvent {
                    struct_size: mem::size_of::<FlutterPointerEvent>(),
                    view_id: event.view_id,
                    device_kind: event.device_kind as FlutterPointerDeviceKind,
                    device: event.device_id,
                    phase: event.phase as FlutterPointerPhase,
                    x: event.x,
                    y: event.y,
                    buttons: event.buttons.bits() as i64,
                    signal_kind,
                    scroll_delta_x,
                    scroll_delta_y,
                    timestamp,
                    ..Default::default()
                }
            })
            .collect();

        let result =
            unsafe { FlutterEngineSendPointerEvent(self.engine(), events.as_ptr(), events.len()) };

        if result != FlutterEngineResult_kSuccess {
            bail!("failed to send pointer event: {result}");
        }

        Ok(())
    }

    pub fn send_key_event<F>(&self, event: &KeyEvent, callback: F) -> eyre::Result<()>
    where
        F: FnOnce(bool) + 'static,
    {
        unsafe extern "C" fn _callback<F: FnOnce(bool)>(
            handled: bool,
            user_data: *mut ::std::os::raw::c_void,
        ) {
            unsafe { Box::from_raw(user_data.cast::<F>())(handled) };
        }

        // This is freed above when the calback invoked.
        let reply = Box::into_raw(Box::new(callback));

        let character = event
            .character
            .as_deref()
            .and_then(|c| CString::new(c).ok());

        let event = FlutterKeyEvent {
            struct_size: mem::size_of::<FlutterKeyEvent>(),
            timestamp: unsafe { FlutterEngineGetCurrentTime() as f64 },
            type_: event.event_type as u32,
            character: character
                .as_deref()
                .map(|c| c.as_ptr())
                .unwrap_or(ptr::null()),
            synthesized: event.synthesized,
            logical: event.logical.unwrap_or(0),
            physical: event.physical.unwrap_or(0),
            device_type: FlutterKeyEventDeviceType_kFlutterKeyEventDeviceTypeKeyboard,
        };

        unsafe {
            let result = FlutterEngineSendKeyEvent(
                self.engine(),
                &event,
                Some(_callback::<F>),
                reply.cast(),
            );

            if result != FlutterEngineResult_kSuccess {
                drop(Box::from_raw(reply));
                bail!("failed to send key event: {result}");
            }
        }

        Ok(())
    }

    pub fn set_platform_message_handler(
        &self,
        name: impl Into<String>,
        handler: impl BinaryMessageHandler + 'static,
    ) {
        unsafe {
            (*self.inner)
                .platform_message_handlers
                .lock()
                .insert(name.into(), Box::new(handler));
        }
    }

    pub fn messenger(&self) -> BinaryMessenger {
        BinaryMessenger {
            engine: unsafe { (*self.inner).handle },
            engine_is_running: unsafe { (*self.inner).is_running.clone() },
        }
    }

    unsafe fn engine(&self) -> flutter_embedder::FlutterEngine {
        unsafe { (*self.inner).handle }
    }
}

impl Drop for FlutterEngine {
    fn drop(&mut self) {
        unsafe {
            debug!("shutting down engine");

            *(*self.inner).is_running.lock() = false;

            let engine = self.engine();

            if !engine.is_null() {
                let result = FlutterEngineShutdown(engine);
                if result != FlutterEngineResult_kSuccess {
                    tracing::error!("failed to shut down flutter engine: {result}");
                }
            }

            drop(Box::from_raw(self.inner));
        }
    }
}

fn load_aot_data(path: Option<&str>) -> eyre::Result<Option<FlutterEngineAOTData>> {
    if !unsafe { FlutterEngineRunsAOTCompiledDartCode() } {
        tracing::debug!("Engine does not support AOT dart code");
        return Ok(None);
    }

    let Some(path) = path else {
        bail!("No AOT library path was provided");
    };

    if !Path::new(path).exists() {
        bail!("AOT library not found at {path}");
    }

    let c_path = CString::from_str(path)?;
    let source = FlutterEngineAOTDataSource {
        type_: FlutterEngineAOTDataSourceType_kFlutterEngineAOTDataSourceTypeElfPath,
        __bindgen_anon_1: FlutterEngineAOTDataSource__bindgen_ty_1 {
            elf_path: c_path.as_ptr(),
        },
    };

    let mut aot_data = ptr::null_mut();
    if unsafe { FlutterEngineCreateAOTData(&source, &mut aot_data) } != FlutterEngineResult_kSuccess
    {
        bail!("Failed to load AOT data from {path}");
    }

    tracing::info!("Loaded AOT data from {path}");

    Ok(Some(aot_data))
}

fn create_task_runner<F: Fn(FlutterTask) + Send + Sync + 'static>(
    id: usize,
    runner: Arc<FlutterTaskRunner<F>>,
) -> FlutterTaskRunnerDescription {
    unsafe extern "C" fn runs_tasks_on_current_thread<F>(task_runner: *mut c_void) -> bool {
        unsafe { task_runner.cast::<FlutterTaskRunner<F>>().as_ref().unwrap() }
            .runs_tasks_on_current_thread()
    }

    unsafe extern "C" fn post_task_callback<F: Fn(FlutterTask)>(
        task: flutter_embedder::FlutterTask,
        target_time_nanos: u64,
        user_data: *mut c_void,
    ) {
        let runner = unsafe { user_data.cast::<FlutterTaskRunner<F>>().as_ref().unwrap() };
        runner.post_task(task, target_time_nanos);
    }

    unsafe extern "C" fn destruction_callback<F: Fn(FlutterTask)>(user_data: *mut c_void) {
        unsafe { drop(Arc::from_raw(user_data.cast::<FlutterTaskRunner<F>>())) };
    }

    let runner = Arc::into_raw(runner).cast_mut();

    FlutterTaskRunnerDescription {
        struct_size: mem::size_of::<FlutterTaskRunnerDescription>(),
        identifier: id,
        user_data: runner.cast(),
        runs_task_on_current_thread_callback: Some(runs_tasks_on_current_thread::<F>),
        post_task_callback: Some(post_task_callback::<F>),
        destruction_callback: Some(destruction_callback::<F>),
    }
}

#[derive(Debug)]
pub struct FlutterTask {
    target_time: u64,
    task: flutter_embedder::FlutterTask,
}

// SAFETY: The `flutter_embedder::FlutterTask` is basically an opaque task handle from our
// perspective. The engine expects it to be sent between threads.
unsafe impl Send for FlutterTask {}

impl FlutterTask {
    pub fn target_time_nanos(&self) -> u64 {
        self.target_time
    }
}

pub struct FlutterTaskRunner<F> {
    main_thread_id: ThreadId,
    handler: F,
}

impl<F> FlutterTaskRunner<F> {
    pub fn new(handler: F) -> FlutterTaskRunner<F> {
        FlutterTaskRunner {
            main_thread_id: thread::current().id(),
            handler,
        }
    }

    pub fn runs_tasks_on_current_thread(&self) -> bool {
        self.main_thread_id == thread::current().id()
    }
}

impl<F> FlutterTaskRunner<F>
where
    F: Fn(FlutterTask),
{
    pub fn post_task(&self, task: flutter_embedder::FlutterTask, target_time_nanos: u64) {
        (self.handler)(FlutterTask {
            target_time: target_time_nanos,
            task,
        });
    }
}

#[derive(Clone)]
pub struct BinaryMessenger {
    engine: flutter_embedder::FlutterEngine,
    engine_is_running: Arc<Mutex<bool>>,
}

impl BinaryMessenger {
    pub fn send_platform_message(&self, channel: &CStr, message: &[u8]) -> eyre::Result<()> {
        if *self.engine_is_running.lock() {
            unsafe {
                let result = FlutterEngineSendPlatformMessage(
                    self.engine,
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
        } else {
            bail!("engine is not running");
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
            let reply_handler = unsafe { Box::from_raw(user_data.cast::<F>()) };
            if data.is_null() {
                tracing::warn!("null reply from platform message");
            } else {
                reply_handler(unsafe { std::slice::from_raw_parts(data, size) });
            }
        }

        if *self.engine_is_running.lock() {
            let mut response_handle = ptr::null_mut();

            // This is freed by the callback above, when invoked.
            let reply = Box::into_raw(Box::new(reply_handler));
            let result = unsafe {
                FlutterPlatformMessageCreateResponseHandle(
                    self.engine,
                    Some(callback::<F>),
                    reply.cast(),
                    &mut response_handle,
                )
            };

            if result != FlutterEngineResult_kSuccess {
                drop(unsafe { Box::from_raw(reply) });
                bail!("failed to create response handle: {result}");
            }

            let result = unsafe {
                FlutterEngineSendPlatformMessage(
                    self.engine,
                    &FlutterPlatformMessage {
                        struct_size: mem::size_of::<FlutterPlatformMessage>(),
                        channel: channel.as_ptr(),
                        message: message.as_ptr(),
                        message_size: message.len(),
                        response_handle,
                    },
                )
            };

            if result != FlutterEngineResult_kSuccess {
                unsafe {
                    FlutterPlatformMessageReleaseResponseHandle(self.engine, response_handle);
                    drop(Box::from_raw(reply));
                }
                bail!("failed to send platform message: {result}");
            }

            let result = unsafe {
                FlutterPlatformMessageReleaseResponseHandle(self.engine, response_handle)
            };

            if result != FlutterEngineResult_kSuccess {
                bail!("failed to release response handle: {result}");
            }

            Ok(())
        } else {
            bail!("engine is not running");
        }
    }
}

pub trait BinaryMessageHandler {
    fn handle(&self, message: &[u8], reply: BinaryMessageReply);
}

pub struct BinaryMessageReply {
    engine: flutter_embedder::FlutterEngine,
    engine_is_running: Arc<Mutex<bool>>,
    response_handle: *const FlutterPlatformMessageResponseHandle,
}

impl BinaryMessageReply {
    pub(crate) fn new(
        engine: flutter_embedder::FlutterEngine,
        engine_is_running: Arc<Mutex<bool>>,
        response_handle: *const FlutterPlatformMessageResponseHandle,
    ) -> BinaryMessageReply {
        BinaryMessageReply {
            engine,
            engine_is_running,
            response_handle,
        }
    }

    pub fn send(self, message: &[u8]) {
        self.send_raw(message.as_ptr(), message.len());
    }

    pub fn not_implemented(self) {
        self.send_raw(ptr::null(), 0);
    }

    fn send_raw(self, data: *const u8, length: usize) {
        if *self.engine_is_running.lock() {
            unsafe {
                FlutterEngineSendPlatformMessageResponse(
                    self.engine,
                    self.response_handle,
                    data,
                    length,
                );
            }
        } else {
            tracing::error!("send called on a stopped engine");
        }
    }
}

unsafe extern "C" fn platform_message_callback(
    message: *const FlutterPlatformMessage,
    user_data: *mut c_void,
) {
    let engine = unsafe { user_data.cast::<FlutterEngineState>().as_ref().unwrap() };
    let message = unsafe { message.as_ref().unwrap() };

    let reply = BinaryMessageReply::new(
        engine.handle,
        engine.is_running.clone(),
        message.response_handle,
    );

    let channel = unsafe { CStr::from_ptr(message.channel) };
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

    let bytes = unsafe { std::slice::from_raw_parts(message.message, message.message_size) };

    handler.handle(bytes, reply);
}

extern "C" fn log_message(tag: *const c_char, message: *const c_char, _: *mut c_void) {
    unsafe {
        let tag = CStr::from_ptr(tag).to_string_lossy();
        let message = CStr::from_ptr(message).to_string_lossy();
        eprintln!("{tag}: {message}");
    }
}

extern "C" fn gl_make_current(user_data: *mut c_void) -> bool {
    let engine = user_data.cast::<FlutterEngineState>();
    let compositor = unsafe { (*engine).compositor.as_ref() };

    if let Err(e) = compositor.make_current() {
        tracing::error!("failed to make context current: {e}");
        return false;
    }

    true
}

extern "C" fn gl_make_resource_current(user_data: *mut c_void) -> bool {
    let engine = user_data.cast::<FlutterEngineState>();
    let compositor = unsafe { (*engine).compositor.as_ref() };

    if let Err(e) = compositor.make_resource_current() {
        tracing::error!("failed to make resource context current: {e}");
        return false;
    }

    true
}

extern "C" fn gl_clear_current(user_data: *mut c_void) -> bool {
    let engine = user_data.cast::<FlutterEngineState>();
    let compositor = unsafe { (*engine).compositor.as_ref() };

    if let Err(e) = compositor.clear_current() {
        tracing::error!("failed to clear context: {e}");
        return false;
    }

    true
}

extern "C" fn gl_present(_user_data: *mut c_void) -> bool {
    false
}

extern "C" fn gl_fbo_callback(_user_data: *mut c_void) -> u32 {
    0
}

extern "C" fn gl_get_proc_address(user_data: *mut c_void, name: *const c_char) -> *mut c_void {
    let engine = user_data.cast::<FlutterEngineState>();
    let compositor = unsafe { (*engine).compositor.as_ref() };

    let name = unsafe { CStr::from_ptr(name) };
    let Ok(name) = name.to_str() else {
        tracing::error!("invalid proc name: {name:?}");
        return ptr::null_mut();
    };

    compositor
        .get_proc_address(name)
        .map(|f| f as *mut c_void)
        .unwrap_or_else(ptr::null_mut)
}

extern "C" fn compositor_create_backing_store(
    config: *const FlutterBackingStoreConfig,
    out: *mut FlutterBackingStore,
    user_data: *mut c_void,
) -> bool {
    let engine = user_data.cast::<FlutterEngineState>();
    let compositor = unsafe { (*engine).compositor.as_ref() };

    let config = unsafe { &*config };
    let backing_store = unsafe { &mut *out };

    assert_eq!(
        config.struct_size,
        mem::size_of::<FlutterBackingStoreConfig>()
    );

    assert_eq!(
        backing_store.struct_size,
        mem::size_of::<FlutterBackingStore>()
    );

    if let Err(e) = compositor.create_backing_store(config, backing_store) {
        tracing::error!("{e:?}");
        return false;
    }

    true
}

extern "C" fn compositor_collect_backing_store(
    backing_store: *const FlutterBackingStore,
    user_data: *mut c_void,
) -> bool {
    let engine = user_data.cast::<FlutterEngineState>();
    let compositor = unsafe { (*engine).compositor.as_ref() };
    let backing_store = unsafe { &*backing_store };

    assert_eq!(
        backing_store.struct_size,
        mem::size_of::<FlutterBackingStore>()
    );

    if let Err(e) = compositor.release_backing_store(backing_store) {
        tracing::error!("{e:?}");
        return false;
    }

    true
}

extern "C" fn compositor_present_view(info: *const FlutterPresentViewInfo) -> bool {
    let info = unsafe { &*info };

    assert_eq!(info.struct_size, mem::size_of::<FlutterPresentViewInfo>());

    let engine = info.user_data.cast::<FlutterEngineState>();
    let compositor = unsafe { (*engine).compositor.as_ref() };

    if info.layers.is_null() {
        tracing::error!("layers is null");
        return false;
    }

    let layers = unsafe {
        std::slice::from_raw_parts(info.layers.cast::<&FlutterLayer>(), info.layers_count)
    };

    if let Err(e) = compositor
        .present_view(info.view_id, layers)
        .wrap_err("Failed to present view")
    {
        tracing::error!("{e:?}");
        return false;
    };

    true
}
