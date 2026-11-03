mod clipboard;
mod compositor;
mod egl;
mod engine;
mod keyboard;
mod keymap;
mod mouse;
mod mouse_cursor;
mod standard_method_channel;
mod task_queue;
mod text_input;
mod touch;

pub mod codec;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::env;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use calloop::{EventLoop, LoopHandle};
use calloop_wayland_source::WaylandSource;
use raw_window_handle::{HasWindowHandle, RawWindowHandle, WaylandWindowHandle, WindowHandle};
use tracing::debug;
use wayland_client::globals::GlobalList;
use wayland_client::protocol::wl_compositor::{self, WlCompositor};
use wayland_client::protocol::wl_data_device::{self, WlDataDevice};
use wayland_client::protocol::wl_data_device_manager;
use wayland_client::protocol::wl_data_offer::{self, WlDataOffer};
use wayland_client::protocol::wl_data_source;
use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_client::protocol::wl_pointer::{self, WlPointer};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::{self, Capability, WlSeat};
use wayland_client::protocol::wl_surface::{self, WlSurface};
use wayland_client::protocol::wl_touch::{self, WlTouch};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1;
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_manager_v1::{
    self, WpCursorShapeManagerV1,
};
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::{
    self, WpFractionalScaleManagerV1,
};
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1;
use wayland_protocols::wp::viewporter::client::wp_viewport::{self, WpViewport};
use wayland_protocols::wp::viewporter::client::wp_viewporter::{self, WpViewporter};
use wayland_protocols::xdg::shell::client::xdg_surface::{self, XdgSurface};
use wayland_protocols::xdg::shell::client::xdg_toplevel::{self, XdgToplevel};
use wayland_protocols::xdg::shell::client::xdg_wm_base::{self, XdgWmBase};

use crate::clipboard::{Clipboard, OfferData, PlatformHandler};
use crate::compositor::{OpenGLCompositor, ViewSurfaceRegistry};
use crate::egl::EglDisplay;
use crate::engine::{FlutterEngine, FlutterEngineConfig};
use crate::keyboard::KeyboardState;
use crate::mouse::MouseState;
use crate::mouse_cursor::{MouseCursor, MouseCursorHandler};
use crate::standard_method_channel::StandardMethodHandler;
use crate::task_queue::FlutterTaskQueue;
use crate::text_input::{TextInputHandler, TextInputState};
use crate::touch::TouchState;

pub struct FlionAppBuilder<'a> {
    _p: PhantomData<&'a ()>,
}

impl<'a> FlionAppBuilder<'a> {
    pub fn build(self) -> eyre::Result<FlionApp> {
        Ok(FlionApp)
    }
}

pub struct FlionApp;

impl FlionApp {
    pub fn builder<'a>() -> FlionAppBuilder<'a> {
        FlionAppBuilder { _p: PhantomData }
    }

    pub fn run_event_loop(self) -> eyre::Result<()> {
        let conn = Connection::connect_to_env()?;

        let (globals, event_queue) = wayland_client::globals::registry_queue_init(&conn)?;

        globals.contents().with_list(|globals| {
            for global in globals {
                let wayland_client::globals::Global {
                    interface,
                    name,
                    version,
                } = global;

                debug!("[{name}] {interface} (v{version})");
            }
        });

        let qh = event_queue.handle();

        let mut event_loop: EventLoop<'_, AppState> = EventLoop::try_new()?;

        let egl = EglDisplay::for_wayland_connection(&conn)?;

        WaylandSource::new(conn, event_queue).insert(event_loop.handle())?;

        struct SurfaceHandle(WlSurface);

        impl HasWindowHandle for SurfaceHandle {
            fn window_handle(
                &self,
            ) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError>
            {
                Ok(unsafe {
                    WindowHandle::borrow_raw(RawWindowHandle::Wayland(WaylandWindowHandle::new(
                        NonNull::new_unchecked(self.0.id().as_ptr()).cast(),
                    )))
                })
            }
        }

        let (task_sender, task_receiver) = calloop::channel::channel();

        event_loop
            .handle()
            .insert_source(task_receiver, |event, _, app| {
                if let calloop::channel::Event::Msg(task) = event {
                    app.task_queue.push(task);
                }
            })
            .unwrap();

        let mouse_cursor = Rc::new(RefCell::new(MouseCursor::new()));

        let mouse_state = MouseState::default();
        let touch_state = TouchState::default();
        let text_input = Rc::new(RefCell::new(TextInputState::default()));
        let keyboard_state = KeyboardState::new(text_input.clone());

        let seat = globals.bind(&qh, 5..=11, ())?;
        let clipboard = Rc::new(RefCell::new(Clipboard::bind(
            &globals,
            &qh,
            &seat,
            event_loop.handle(),
        )?));

        let aot_library_path = env::var("FLION_AOT_LIBRARY_PATH").map(PathBuf::from).ok();

        let view_surfaces = Arc::new(ViewSurfaceRegistry::default());
        let opengl_compositor = OpenGLCompositor::new(egl.clone(), view_surfaces.clone()).unwrap();

        let engine = FlutterEngine::new(FlutterEngineConfig {
            assets_path: &env::var("FLION_ASSETS_PATH").unwrap(),
            aot_library_path: aot_library_path.as_deref().and_then(|p| p.to_str()),
            compositor: opengl_compositor,
            platform_task_handler: Box::new(move |task| {
                let _ = task_sender.send(task);
            }),
            platform_message_handlers: vec![],
        })?;

        engine.set_platform_message_handler("flutter/keyboard", keyboard_state.channel_handler());
        engine.set_platform_message_handler("flutter/textinput", TextInputHandler::new(text_input));
        engine.set_platform_message_handler(
            "flutter/platform",
            PlatformHandler::new(clipboard.clone()),
        );
        engine.set_platform_message_handler(
            "flutter/mousecursor",
            MouseCursorHandler::new(mouse_cursor.clone()),
        );

        let (create_view_sender, create_view_receiver) = calloop::channel::channel();
        let (add_view_result_sender, add_view_result_receiver) = calloop::channel::channel();

        struct ViewsHandler {
            sender: calloop::channel::Sender<standard_method_channel::StandardMethodReply>,
        }

        event_loop
            .handle()
            .insert_source(create_view_receiver, move |event, _, app| {
                if let calloop::channel::Event::Msg(reply) = event {
                    let next_view_id = app.windows.keys().max().copied().unwrap_or_default() + 1;
                    app.engine
                        .add_view(next_view_id, 0, 0, 1.0, |success| {
                            let _ = add_view_result_sender.send((next_view_id, success, reply));
                        })
                        .unwrap();
                }
            })
            .unwrap();

        impl StandardMethodHandler for ViewsHandler {
            fn handle(
                &self,
                method: &str,
                _args: codec::EncodableValue,
                reply: standard_method_channel::StandardMethodReply,
            ) {
                if method == "addView" {
                    self.sender.send(reply).unwrap();
                }
            }
        }

        engine.set_platform_message_handler(
            "flion/views",
            ViewsHandler {
                sender: create_view_sender,
            },
        );

        let window_creator = Rc::new(WindowCreator::bind(&globals, &qh)?);

        event_loop
            .handle()
            .insert_source(add_view_result_receiver, {
                let qh = qh.clone();
                move |event, _, app| {
                    if let calloop::channel::Event::Msg((view_id, success, reply)) = event {
                        if success {
                            let window = window_creator.clone().create_window(view_id, &qh);

                            app.view_surfaces.add(
                                window.id,
                                app.egl
                                    .create_window_surface(
                                        SurfaceHandle(window.surface.clone()),
                                        1,
                                        1,
                                    )
                                    .unwrap(),
                            );

                            app.windows.insert(view_id, window);

                            reply.success(&codec::EncodableValue::I64(view_id));
                        } else {
                            reply.success(&codec::EncodableValue::Null);
                        }
                    }
                }
            })
            .unwrap();

        let mut app = AppState {
            egl,
            task_queue: FlutterTaskQueue::new(),
            engine,
            view_surfaces,
            windows: BTreeMap::from([]),
            loop_handle: event_loop.handle(),
            seat: Seat {
                seat,
                pointer: None,
                mouse: mouse_state,
                touch: None,
                touch_state,
                keyboard: None,
                keyboard_state,
            },
            cursor_shape_manager: globals.bind(&qh, 1..=1, ()).ok(),
            mouse_cursor,
            clipboard,
        };

        while app.windows.is_empty() || !app.windows.values().any(|window| window.close_requested) {
            let timeout = app
                .task_queue
                .next_deadline()
                .map(|deadline| deadline - Instant::now());

            event_loop.dispatch(timeout, &mut app)?;

            app.task_queue.run_tasks(&app.engine);
        }

        for window in std::mem::take(&mut app.windows).into_values() {
            app.view_surfaces.remove(window.id);
        }

        app.mouse_cursor.borrow_mut().set_pointer(None, None);

        Ok(())
    }
}

type WindowId = i64;

struct Seat {
    seat: WlSeat,
    pointer: Option<WlPointer>,
    mouse: MouseState,
    touch: Option<WlTouch>,
    touch_state: TouchState,
    keyboard: Option<WlKeyboard>,
    keyboard_state: KeyboardState,
}

impl Drop for Seat {
    fn drop(&mut self) {
        if let Some(keyboard) = self.keyboard.take() {
            keyboard.release();
        }

        if let Some(touch) = self.touch.take() {
            touch.release();
        }

        if let Some(pointer) = self.pointer.take() {
            pointer.release();
        }

        self.seat.release();
    }
}

struct AppState {
    egl: EglDisplay,
    task_queue: FlutterTaskQueue,
    engine: FlutterEngine,
    view_surfaces: Arc<ViewSurfaceRegistry>,
    windows: BTreeMap<WindowId, Window>,
    loop_handle: LoopHandle<'static, AppState>,
    cursor_shape_manager: Option<WpCursorShapeManagerV1>,
    mouse_cursor: Rc<RefCell<MouseCursor>>,
    clipboard: Rc<RefCell<Clipboard>>,
    seat: Seat,
}

impl Drop for AppState {
    fn drop(&mut self) {
        if let Some(cursor_shape_manager) = &self.cursor_shape_manager {
            cursor_shape_manager.destroy();
        }
    }
}

struct WindowCreator {
    compositor: WlCompositor,
    xdg_wm_base: XdgWmBase,
    viewporter: WpViewporter,
    fractional_scale_manager: WpFractionalScaleManagerV1,
}

impl WindowCreator {
    pub fn bind(globals: &GlobalList, qh: &QueueHandle<AppState>) -> eyre::Result<Self> {
        Ok(WindowCreator {
            compositor: globals.bind(qh, 1..=7, ())?,
            xdg_wm_base: globals.bind(qh, 7..=7, ())?,
            viewporter: globals.bind(qh, 1..=1, ())?,
            fractional_scale_manager: globals.bind(qh, 1..=1, ())?,
        })
    }

    pub fn create_window(self: Rc<Self>, id: WindowId, qh: &QueueHandle<AppState>) -> Window {
        let surface = self.compositor.create_surface(qh, id);
        let xdg_surface = self.xdg_wm_base.get_xdg_surface(&surface, qh, id);
        let toplevel = xdg_surface.get_toplevel(qh, id);
        let viewport = self.viewporter.get_viewport(&surface, qh, id);
        let fractional_scale = self
            .fractional_scale_manager
            .get_fractional_scale(&surface, qh, id);

        surface.commit();

        Window {
            id,
            _creator: self,
            surface,
            xdg_surface,
            toplevel,
            viewport,
            fractional_scale,
            size: (0, 0),
            scale_factor: 1.0,
            close_requested: false,
        }
    }
}

impl Drop for WindowCreator {
    fn drop(&mut self) {
        self.fractional_scale_manager.destroy();
        self.viewporter.destroy();
        self.xdg_wm_base.destroy();

        if self.compositor.version() >= 6 {
            self.compositor.release();
        }
    }
}

struct Window {
    id: WindowId,
    // WindowCreator must outlive its windows.
    _creator: Rc<WindowCreator>,
    surface: WlSurface,
    xdg_surface: XdgSurface,
    toplevel: XdgToplevel,
    viewport: WpViewport,
    fractional_scale: wp_fractional_scale_v1::WpFractionalScaleV1,
    size: (u32, u32),
    scale_factor: f64,
    close_requested: bool,
}

impl Drop for Window {
    fn drop(&mut self) {
        self.fractional_scale.destroy();
        self.viewport.destroy();
        self.toplevel.destroy();
        self.xdg_surface.destroy();
        self.surface.destroy();
    }
}

impl Dispatch<wl_registry::WlRegistry, wayland_client::globals::GlobalListContents> for AppState {
    fn event(
        _state: &mut Self,
        _proxy: &WlRegistry,
        event: <WlRegistry as wayland_client::Proxy>::Event,
        _data: &wayland_client::globals::GlobalListContents,
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            debug!(name, interface, version);
        }
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for AppState {
    fn event(
        _state: &mut Self,
        _proxy: &wl_compositor::WlCompositor,
        _event: <wl_compositor::WlCompositor as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
        todo!()
    }
}

impl Dispatch<wl_surface::WlSurface, WindowId> for AppState {
    fn event(
        _state: &mut Self,
        _proxy: &wl_surface::WlSurface,
        _event: <wl_surface::WlSurface as Proxy>::Event,
        _data: &WindowId,
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for AppState {
    fn event(
        _state: &mut Self,
        proxy: &xdg_wm_base::XdgWmBase,
        event: <xdg_wm_base::XdgWmBase as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            proxy.pong(serial)
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, WindowId> for AppState {
    fn event(
        state: &mut Self,
        proxy: &xdg_surface::XdgSurface,
        event: <xdg_surface::XdgSurface as Proxy>::Event,
        window_id: &WindowId,
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            let Some(window) = state.windows.get(window_id) else {
                return;
            };

            let (width, height) = window.size;
            let scale_factor = window.scale_factor;

            window.viewport.set_destination(width as i32, height as i32);

            let width = f64::from(width) * scale_factor;
            let height = f64::from(height) * scale_factor;

            state
                .engine
                .send_window_metrics_event(
                    *window_id,
                    width as usize,
                    height as usize,
                    scale_factor,
                )
                .unwrap();

            proxy.ack_configure(serial);
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, WindowId> for AppState {
    fn event(
        state: &mut Self,
        _proxy: &xdg_toplevel::XdgToplevel,
        event: <xdg_toplevel::XdgToplevel as Proxy>::Event,
        window_id: &WindowId,
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
        let Some(window) = state.windows.get_mut(window_id) else {
            return;
        };

        match event {
            xdg_toplevel::Event::Configure { width, height, .. } => {
                let width = if width == 0 { 1280 } else { width };
                let height = if height == 0 { 800 } else { height };
                window.size = (width as u32, height as u32);
            }
            xdg_toplevel::Event::Close => {
                window.close_requested = true;
            }
            xdg_toplevel::Event::ConfigureBounds { .. } => {}
            xdg_toplevel::Event::WmCapabilities { .. } => {}
            _ => {}
        }
    }
}

impl Dispatch<wp_viewporter::WpViewporter, ()> for AppState {
    fn event(
        _state: &mut Self,
        _proxy: &wp_viewporter::WpViewporter,
        _event: <wp_viewporter::WpViewporter as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wp_viewport::WpViewport, WindowId> for AppState {
    fn event(
        _state: &mut Self,
        _proxy: &wp_viewport::WpViewport,
        _event: <wp_viewport::WpViewport as Proxy>::Event,
        _data: &WindowId,
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1, ()> for AppState {
    fn event(
        _state: &mut Self,
        _proxy: &wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
        _event: <wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wp_fractional_scale_v1::WpFractionalScaleV1, WindowId> for AppState {
    fn event(
        state: &mut Self,
        _proxy: &wp_fractional_scale_v1::WpFractionalScaleV1,
        event: <wp_fractional_scale_v1::WpFractionalScaleV1 as Proxy>::Event,
        window_id: &WindowId,
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event
            && let Some(window) = state.windows.get_mut(window_id)
        {
            window.scale_factor = f64::from(scale) / 120.0;
            // TODO: Probably need to send a window metrics event to flutter
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for AppState {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: <wl_seat::WlSeat as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        qh: &wayland_client::QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities } = event {
            match capabilities {
                wayland_client::WEnum::Value(capabilities) => {
                    if capabilities.contains(Capability::Pointer) {
                        if state.seat.pointer.is_none() {
                            let pointer = seat.get_pointer(qh, ());
                            let shape_device = state
                                .cursor_shape_manager
                                .as_ref()
                                .map(|manager| manager.get_pointer(&pointer, qh, ()));

                            state
                                .mouse_cursor
                                .borrow_mut()
                                .set_pointer(Some(pointer.clone()), shape_device);

                            state.seat.pointer = Some(pointer);
                        }
                    } else if let Some(pointer) = state.seat.pointer.take() {
                        state.mouse_cursor.borrow_mut().set_pointer(None, None);
                        pointer.release();
                    }

                    if capabilities.contains(Capability::Touch) {
                        if state.seat.touch.is_none() {
                            state.seat.touch = Some(seat.get_touch(qh, ()));
                        }
                    } else if let Some(touch) = state.seat.touch.take() {
                        state.seat.touch_state.cancel_all(&state.engine);
                        touch.release();
                    }

                    if capabilities.contains(Capability::Keyboard) {
                        if state.seat.keyboard.is_none() {
                            state.seat.keyboard = Some(seat.get_keyboard(qh, ()));
                        }
                    } else if let Some(keyboard) = state.seat.keyboard.take() {
                        keyboard.release();
                    }
                }
                wayland_client::WEnum::Unknown(_) => {}
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for AppState {
    fn event(
        state: &mut Self,
        _proxy: &wl_pointer::WlPointer,
        event: <wl_pointer::WlPointer as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
        if let wl_pointer::Event::Button { serial, .. } = &event {
            state.clipboard.borrow_mut().set_input_serial(*serial);
        }

        if let wl_pointer::Event::Enter { serial, .. } = &event {
            state.mouse_cursor.borrow_mut().on_enter(*serial);
        } else if let wl_pointer::Event::Leave { .. } = &event {
            state.mouse_cursor.borrow_mut().on_leave();
        }

        let windows = &state.windows;

        state.seat.mouse.handle_event(
            &state.engine,
            |surface| surface.data().copied(),
            |id| windows.get(&id).map(|window| window.scale_factor),
            event,
        );
    }
}

impl Dispatch<wp_cursor_shape_manager_v1::WpCursorShapeManagerV1, ()> for AppState {
    fn event(
        _state: &mut Self,
        _proxy: &wp_cursor_shape_manager_v1::WpCursorShapeManagerV1,
        _event: <wp_cursor_shape_manager_v1::WpCursorShapeManagerV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wp_cursor_shape_device_v1::WpCursorShapeDeviceV1, ()> for AppState {
    fn event(
        _state: &mut Self,
        _proxy: &wp_cursor_shape_device_v1::WpCursorShapeDeviceV1,
        _event: <wp_cursor_shape_device_v1::WpCursorShapeDeviceV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_touch::WlTouch, ()> for AppState {
    fn event(
        state: &mut Self,
        _proxy: &wl_touch::WlTouch,
        event: <wl_touch::WlTouch as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
        if let wl_touch::Event::Down { serial, .. } = &event {
            state.clipboard.borrow_mut().set_input_serial(*serial);
        }

        state.seat.touch_state.handle_event(
            &state.engine,
            |surface| surface.data().copied(),
            |id| state.windows.get(&id).map(|window| window.scale_factor),
            event,
        );
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for AppState {
    fn event(
        state: &mut Self,
        _proxy: &wl_keyboard::WlKeyboard,
        event: <wl_keyboard::WlKeyboard as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Enter { serial, .. } | wl_keyboard::Event::Key { serial, .. } =
            &event
        {
            state.clipboard.borrow_mut().set_input_serial(*serial);
        }

        state
            .seat
            .keyboard_state
            .handle_event(&state.engine, &state.loop_handle, event);
    }
}

impl Dispatch<wl_data_device_manager::WlDataDeviceManager, ()> for AppState {
    fn event(
        _state: &mut Self,
        _proxy: &wl_data_device_manager::WlDataDeviceManager,
        _event: <wl_data_device_manager::WlDataDeviceManager as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_data_device::WlDataDevice, ()> for AppState {
    fn event(
        state: &mut Self,
        _proxy: &wl_data_device::WlDataDevice,
        event: <wl_data_device::WlDataDevice as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
        state.clipboard.borrow_mut().handle_data_device_event(event);
    }

    wayland_client::event_created_child!(AppState, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, OfferData::default()),
    ]);
}

impl Dispatch<wl_data_offer::WlDataOffer, OfferData> for AppState {
    fn event(
        _state: &mut Self,
        _proxy: &wl_data_offer::WlDataOffer,
        event: <wl_data_offer::WlDataOffer as Proxy>::Event,
        data: &OfferData,
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
        data.handle_event(event);
    }
}

impl Dispatch<wl_data_source::WlDataSource, ()> for AppState {
    fn event(
        state: &mut Self,
        proxy: &wl_data_source::WlDataSource,
        event: <wl_data_source::WlDataSource as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
        state
            .clipboard
            .borrow_mut()
            .handle_data_source_event(proxy, event);
    }
}
