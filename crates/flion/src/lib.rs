mod compositor;
mod egl;
mod engine;

use std::env;
use std::marker::PhantomData;
use std::sync::Arc;

use raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
use wayland_client::Connection;
use wayland_client::backend::Backend;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::EventLoop;
use winit::window::{Window, WindowAttributes};

use crate::compositor::OpenGLCompositor;
use crate::egl::EglDisplay;
use crate::engine::{FlutterEngine, FlutterEngineConfig};

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
        let event_loop = EventLoop::new()?;

        let mut app = AppState::default();

        event_loop.run_app(&mut app)?;

        Ok(())
    }
}

#[derive(Default)]
struct AppState {
    display: Option<EglDisplay>,
    main_window: Option<WindowState>,
}

struct WindowState {
    window: Arc<Window>,
    engine: FlutterEngine,
}

impl ApplicationHandler for AppState {
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

        let engine = FlutterEngine::new(FlutterEngineConfig {
            assets_path: &env::var("FLION_ASSETS_PATH").unwrap(),
            aot_library_path: None,
            compositor: OpenGLCompositor::new(egl.clone(), surface).unwrap(),
        })
        .unwrap();

        *self = AppState {
            display: Some(egl),
            main_window: Some(WindowState { window, engine }),
        };
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
        let Some(main_window) = &self.main_window else {
            return;
        };

        if main_window.window.id() != window_id {
            return;
        }

        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                let scale_factor = main_window.window.scale_factor();
                main_window
                    .engine
                    .send_window_metrics_event(size.width as _, size.height as _, scale_factor)
                    .unwrap();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                let size = main_window.window.inner_size();
                main_window
                    .engine
                    .send_window_metrics_event(size.width as _, size.height as _, scale_factor)
                    .unwrap();
            }
            _ => {}
        }
    }
}
