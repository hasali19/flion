mod compositor;
mod egl;
mod engine;
mod task_queue;

use std::env;
use std::marker::PhantomData;
use std::sync::Arc;

use raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
use wayland_client::Connection;
use wayland_client::backend::Backend;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowAttributes};

use crate::compositor::OpenGLCompositor;
use crate::egl::EglDisplay;
use crate::engine::{FlutterEngine, FlutterEngineConfig, FlutterTask};
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
    main_window: Option<WindowState>,
}

struct WindowState {
    window: Arc<Window>,
    engine: FlutterEngine,
    platform_tasks: FlutterTaskQueue,
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

        let engine = FlutterEngine::new(FlutterEngineConfig {
            assets_path: &env::var("FLION_ASSETS_PATH").unwrap(),
            aot_library_path: None,
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
        self.main_window = Some(WindowState {
            window,
            engine,
            platform_tasks: FlutterTaskQueue::new(),
        });
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, task: FlutterTask) {
        if let Some(main_window) = &mut self.main_window {
            main_window.platform_tasks.push(task);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(main_window) = &mut self.main_window else {
            event_loop.set_control_flow(ControlFlow::Wait);
            return;
        };

        main_window.platform_tasks.run_tasks(&main_window.engine);

        event_loop.set_control_flow(match main_window.platform_tasks.next_deadline() {
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
