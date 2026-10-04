mod egl;

use std::marker::PhantomData;

use gl::COLOR_BUFFER_BIT;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use wayland_client::backend::{Backend, ObjectId};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Proxy};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent::{self, CloseRequested};
use winit::event_loop::EventLoop;
use winit::window::{Window, WindowAttributes};

use crate::egl::{EglContext, EglSurface};

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
    egl: Option<EglContext>,
    main_window: Option<WindowState>,
}

struct WindowState {
    window: Window,
    egl_surface: EglSurface,
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

        let egl = EglContext::new(&connection).unwrap();

        let window = event_loop
            .create_window(WindowAttributes::default())
            .unwrap();

        let RawWindowHandle::Wayland(window_handle) = window.window_handle().unwrap().as_raw()
        else {
            todo!()
        };

        let wl_surface = unsafe {
            WlSurface::from_id(
                &connection,
                ObjectId::from_ptr(
                    WlSurface::interface(),
                    window_handle.surface.as_ptr().cast(),
                )
                .unwrap(),
            )
            .unwrap()
        };

        let surface = egl
            .create_surface(
                &wl_surface,
                window.inner_size().width,
                window.inner_size().height,
            )
            .unwrap();

        *self = AppState {
            egl: Some(egl),
            main_window: Some(WindowState {
                window,
                egl_surface: surface,
            }),
        };
    }

    fn suspended(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        let egl = self.egl.take();
        if let Some(window) = self.main_window.take()
            && let Some(egl) = egl
        {
            egl.destroy_surface(window.egl_surface).unwrap();
        }
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
                main_window.egl_surface.resize(size.width, size.height);

                let Some(egl) = &self.egl else { return };

                egl.make_current(&main_window.egl_surface).unwrap();

                unsafe {
                    gl::ClearColor(1.0, 0.0, 0.0, 1.0);
                    gl::Clear(COLOR_BUFFER_BIT);
                }

                egl.swap_buffers(&main_window.egl_surface).unwrap();
            }
            _ => {}
        }
    }
}
