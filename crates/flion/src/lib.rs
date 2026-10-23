mod compositor;
mod egl;
mod engine;
mod task_queue;
mod window;

use std::env;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::sync::Arc;

use raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
use wayland_client::Connection;
use wayland_client::backend::Backend;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::WindowAttributes;

use crate::compositor::OpenGLCompositor;
use crate::egl::EglDisplay;
use crate::engine::{FlutterEngine, FlutterEngineConfig, FlutterTask};
use crate::window::FlutterWindow;

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
        let event_loop = EventLoop::with_user_event().build()?;

        let mut app = AppState {
            proxy: event_loop.create_proxy(),
            display: None,
            main_window: None,
        };

        event_loop.run_app(&mut app)?;

        Ok(())
    }
}

struct AppState {
    proxy: EventLoopProxy<FlutterTask>,
    display: Option<EglDisplay>,
    main_window: Option<FlutterWindow>,
}

impl ApplicationHandler<FlutterTask> for AppState {
    fn resumed(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        let display_handle = event_loop.display_handle().unwrap().as_raw();

        let RawDisplayHandle::Wayland(display_handle) = display_handle else {
            todo!()
        };

        let connection = unsafe {
            Connection::from_backend(Backend::from_foreign_display(
                display_handle.display.as_ptr().cast(),
            ))
        };

        let egl = EglDisplay::for_wayland_connection(&connection).unwrap();

        let window = Arc::new(
            event_loop
                .create_window(WindowAttributes::default())
                .unwrap(),
        );

        let size = window.inner_size();
        let surface = egl
            .create_window_surface(window.clone(), size.width, size.height)
            .unwrap();

        let aot_library_path = env::var("FLION_AOT_LIBRARY_PATH").map(PathBuf::from).ok();

        let engine = FlutterEngine::new(FlutterEngineConfig {
            assets_path: &env::var("FLION_ASSETS_PATH").unwrap(),
            aot_library_path: aot_library_path.as_deref().and_then(|p| p.to_str()),
            compositor: OpenGLCompositor::new(egl.clone(), surface).unwrap(),
            platform_task_handler: Box::new({
                let proxy = self.proxy.clone();
                move |task| {
                    let _ = proxy.send_event(task);
                }
            }),
        })
        .unwrap();

        self.display = Some(egl);
        self.main_window = Some(FlutterWindow::new(window, engine));
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, task: FlutterTask) {
        if let Some(main_window) = &mut self.main_window {
            main_window.push_platform_task(task);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(main_window) = &mut self.main_window else {
            event_loop.set_control_flow(ControlFlow::Wait);
            return;
        };

        event_loop.set_control_flow(match main_window.run_platform_tasks() {
            Some(deadline) => ControlFlow::WaitUntil(deadline),
            None => ControlFlow::Wait,
        });
    }

    fn suspended(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        drop(self.main_window.take());
        drop(self.display.take());
    }

    fn window_event(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        window_id: winit::window::WindowId,
        event: winit::event::WindowEvent,
    ) {
        let Some(main_window) = &mut self.main_window else {
            return;
        };

        if main_window.id() != window_id {
            return;
        }

        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }
            WindowEvent::Resized(size) => main_window.on_resized(size),
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                main_window.on_scale_factor_changed(scale_factor)
            }
            WindowEvent::CursorEntered { .. } => main_window.on_cursor_entered(),
            WindowEvent::CursorLeft { .. } => main_window.on_cursor_left(),
            WindowEvent::CursorMoved { position, .. } => main_window.on_cursor_moved(position),
            WindowEvent::MouseInput { state, button, .. } => {
                main_window.on_mouse_input(state, button)
            }
            WindowEvent::MouseWheel { delta, .. } => main_window.on_mouse_wheel(delta),
            WindowEvent::Touch(touch) => main_window.on_touch(touch),
            _ => {}
        }
    }
}
