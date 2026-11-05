mod compositor;
mod egl;
mod engine;
mod standard_method_channel;
mod task_queue;
mod text_input;
mod wayland;

pub mod codec;
mod platform;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::env;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use calloop::EventLoop;
use calloop_wayland_source::WaylandSource;
use raw_window_handle::{HasWindowHandle, RawWindowHandle, WaylandWindowHandle, WindowHandle};
use tracing::debug;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Proxy};

use crate::compositor::{OpenGLCompositor, ViewSurfaceRegistry};
use crate::egl::EglDisplay;
use crate::engine::{FlutterEngine, FlutterEngineConfig};
use crate::standard_method_channel::StandardMethodHandler;
use crate::task_queue::FlutterTaskQueue;
use crate::text_input::{TextInputHandler, TextInputState};
use crate::wayland::clipboard::{Clipboard, PlatformHandler};
use crate::wayland::keyboard::KeyboardState;
use crate::wayland::mouse::MouseState;
use crate::wayland::mouse_cursor::{MouseCursor, MouseCursorHandler};
use crate::wayland::seat::Seat;
use crate::wayland::state::{AppState, WindowCreator};
use crate::wayland::touch::TouchState;

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
