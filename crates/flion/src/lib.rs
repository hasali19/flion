mod codec;
mod compositor;
mod egl;
mod engine;
mod keyboard;
mod keymap;
mod mouse;
mod mouse_cursor;
mod standard_method_channel;
mod task_queue;

use std::cell::RefCell;
use std::env;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::rc::Rc;
use std::time::Instant;

use calloop::{EventLoop, LoopHandle};
use calloop_wayland_source::WaylandSource;
use raw_window_handle::{HasWindowHandle, RawWindowHandle, WaylandWindowHandle, WindowHandle};
use tracing::debug;
use wayland_client::protocol::wl_compositor::{self, WlCompositor};
use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_client::protocol::wl_pointer::{self, WlPointer};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::{self, Capability, WlSeat};
use wayland_client::protocol::wl_surface::{self, WlSurface};
use wayland_client::{Connection, Dispatch, Proxy};
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
use wayland_protocols::xdg::shell::client::xdg_wm_base::{self, XdgWmBase};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel};

use crate::compositor::OpenGLCompositor;
use crate::egl::EglDisplay;
use crate::engine::{BinaryMessageHandler, FlutterEngine, FlutterEngineConfig};
use crate::keyboard::KeyboardState;
use crate::mouse::MouseState;
use crate::mouse_cursor::{MouseCursor, MouseCursorHandler};
use crate::task_queue::FlutterTaskQueue;

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

        let compositor: WlCompositor = globals.bind(&qh, 1..=4, ())?;
        let xdg_wm_base: XdgWmBase = globals.bind(&qh, 7..=7, ())?;
        let viewporter: WpViewporter = globals.bind(&qh, 1..=1, ())?;
        let fractional_scale_manager: WpFractionalScaleManagerV1 = globals.bind(&qh, 1..=1, ())?;
        let seat: WlSeat = globals.bind(&qh, 5..=11, ())?;
        let cursor_shape_manager: Option<WpCursorShapeManagerV1> =
            globals.bind(&qh, 1..=1, ()).ok();

        let mut event_loop: EventLoop<'_, AppState> = EventLoop::try_new()?;

        let egl = EglDisplay::for_wayland_connection(&conn)?;

        WaylandSource::new(conn, event_queue).insert(event_loop.handle())?;

        let surface = compositor.create_surface(&qh, ());
        let xdg_surface = xdg_wm_base.get_xdg_surface(&surface, &qh, ());
        let top_level = xdg_surface.get_toplevel(&qh, ());
        let viewport = viewporter.get_viewport(&surface, &qh, ());
        let fractional_scale = fractional_scale_manager.get_fractional_scale(&surface, &qh, ());

        surface.commit();

        struct Window(WlSurface);

        impl HasWindowHandle for Window {
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

        let egl_surface = egl
            .create_window_surface(Window(surface.clone()), 1, 1)
            .unwrap();

        let mouse_cursor = Rc::new(RefCell::new(MouseCursor::new()));

        let mouse_state = MouseState::default();
        let keyboard_state = KeyboardState::new();

        let platform_message_handlers: Vec<(&str, Box<dyn BinaryMessageHandler>)> = vec![(
            "flutter/mousecursor",
            Box::new(MouseCursorHandler::new(mouse_cursor.clone())),
        )];

        let aot_library_path = env::var("FLION_AOT_LIBRARY_PATH").map(PathBuf::from).ok();

        let engine = FlutterEngine::new(FlutterEngineConfig {
            assets_path: &env::var("FLION_ASSETS_PATH").unwrap(),
            aot_library_path: aot_library_path.as_deref().and_then(|p| p.to_str()),
            compositor: OpenGLCompositor::new(egl.clone(), egl_surface).unwrap(),
            platform_task_handler: Box::new(move |task| {
                let _ = task_sender.send(task);
            }),
            platform_message_handlers,
        })?;

        engine.set_platform_message_handler("flutter/keyboard", keyboard_state.channel_handler());

        let mut app = AppState {
            task_queue: FlutterTaskQueue::new(),
            engine,
            close_requested: false,
            window_size: (0, 0),
            scale_factor: 1.0,
            viewport,
            loop_handle: event_loop.handle(),
            pointer: None,
            mouse: mouse_state,
            keyboard: None,
            keyboard_state,
            mouse_cursor,
            cursor_shape_manager,
        };

        while !app.close_requested {
            let timeout = app
                .task_queue
                .next_deadline()
                .map(|deadline| deadline - Instant::now());

            event_loop.dispatch(timeout, &mut app)?;

            app.task_queue.run_tasks(&app.engine);
        }

        fractional_scale.destroy();
        app.viewport.destroy();
        top_level.destroy();
        xdg_surface.destroy();
        surface.destroy();

        app.mouse_cursor.borrow_mut().set_pointer(None, None);

        if let Some(keyboard) = app.keyboard.take() {
            keyboard.release();
        }

        if let Some(manager) = app.cursor_shape_manager.take() {
            manager.destroy();
        }

        seat.release();
        fractional_scale.destroy();
        viewporter.destroy();
        xdg_wm_base.destroy();
        compositor.release();

        Ok(())
    }
}

struct AppState {
    task_queue: FlutterTaskQueue,
    engine: FlutterEngine,
    close_requested: bool,
    window_size: (u32, u32),
    scale_factor: f64,
    viewport: WpViewport,
    loop_handle: LoopHandle<'static, AppState>,
    pointer: Option<WlPointer>,
    mouse: MouseState,
    keyboard: Option<WlKeyboard>,
    keyboard_state: KeyboardState,
    mouse_cursor: Rc<RefCell<MouseCursor>>,
    cursor_shape_manager: Option<WpCursorShapeManagerV1>,
}

impl Dispatch<wl_registry::WlRegistry, wayland_client::globals::GlobalListContents> for AppState {
    fn event(
        state: &mut Self,
        proxy: &WlRegistry,
        event: <WlRegistry as wayland_client::Proxy>::Event,
        data: &wayland_client::globals::GlobalListContents,
        conn: &Connection,
        qhandle: &wayland_client::QueueHandle<Self>,
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
        state: &mut Self,
        proxy: &wl_compositor::WlCompositor,
        event: <wl_compositor::WlCompositor as Proxy>::Event,
        data: &(),
        conn: &Connection,
        qhandle: &wayland_client::QueueHandle<Self>,
    ) {
        todo!()
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for AppState {
    fn event(
        state: &mut Self,
        proxy: &wl_surface::WlSurface,
        event: <wl_surface::WlSurface as Proxy>::Event,
        data: &(),
        conn: &Connection,
        qhandle: &wayland_client::QueueHandle<Self>,
    ) {
        todo!()
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

impl Dispatch<xdg_surface::XdgSurface, ()> for AppState {
    fn event(
        state: &mut Self,
        proxy: &xdg_surface::XdgSurface,
        event: <xdg_surface::XdgSurface as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            let (width, height) = state.window_size;

            state.viewport.set_destination(width as i32, height as i32);

            let width = f64::from(width) * state.scale_factor;
            let height = f64::from(height) * state.scale_factor;

            state
                .engine
                .send_window_metrics_event(width as usize, height as usize, state.scale_factor)
                .unwrap();

            proxy.ack_configure(serial);
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for AppState {
    fn event(
        state: &mut Self,
        proxy: &xdg_toplevel::XdgToplevel,
        event: <xdg_toplevel::XdgToplevel as Proxy>::Event,
        data: &(),
        conn: &Connection,
        qhandle: &wayland_client::QueueHandle<Self>,
    ) {
        match event {
            xdg_toplevel::Event::Configure {
                width,
                height,
                states,
            } => {
                let width = if width == 0 { 1280 } else { width };
                let height = if height == 0 { 800 } else { height };
                state.window_size = (width as u32, height as u32);
            }
            xdg_toplevel::Event::Close => {
                state.close_requested = true;
            }
            xdg_toplevel::Event::ConfigureBounds { width, height } => {}
            xdg_toplevel::Event::WmCapabilities { capabilities } => {}
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

impl Dispatch<wp_viewport::WpViewport, ()> for AppState {
    fn event(
        _state: &mut Self,
        _proxy: &wp_viewport::WpViewport,
        _event: <wp_viewport::WpViewport as Proxy>::Event,
        _data: &(),
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

impl Dispatch<wp_fractional_scale_v1::WpFractionalScaleV1, ()> for AppState {
    fn event(
        state: &mut Self,
        _proxy: &wp_fractional_scale_v1::WpFractionalScaleV1,
        event: <wp_fractional_scale_v1::WpFractionalScaleV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            state.scale_factor = f64::from(scale) / 120.0;
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
                        if state.pointer.is_none() {
                            let pointer = seat.get_pointer(qh, ());
                            let shape_device = state
                                .cursor_shape_manager
                                .as_ref()
                                .map(|manager| manager.get_pointer(&pointer, qh, ()));

                            state
                                .mouse_cursor
                                .borrow_mut()
                                .set_pointer(Some(pointer.clone()), shape_device);

                            state.pointer = Some(pointer);
                        }
                    } else if let Some(pointer) = state.pointer.take() {
                        state.mouse_cursor.borrow_mut().set_pointer(None, None);
                        pointer.release();
                    }

                    if capabilities.contains(Capability::Keyboard) {
                        if state.keyboard.is_none() {
                            state.keyboard = Some(seat.get_keyboard(qh, ()));
                        }
                    } else if let Some(keyboard) = state.keyboard.take() {
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
        if let wl_pointer::Event::Enter { serial, .. } = &event {
            state.mouse_cursor.borrow_mut().on_enter(*serial);
        } else if let wl_pointer::Event::Leave { .. } = &event {
            state.mouse_cursor.borrow_mut().on_leave();
        }

        state
            .mouse
            .handle_event(&state.engine, state.scale_factor, event);
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

impl Dispatch<wl_keyboard::WlKeyboard, ()> for AppState {
    fn event(
        state: &mut Self,
        _proxy: &wl_keyboard::WlKeyboard,
        event: <wl_keyboard::WlKeyboard as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qhandle: &wayland_client::QueueHandle<Self>,
    ) {
        state
            .keyboard_state
            .handle_event(&state.engine, &state.loop_handle, event);
    }
}
