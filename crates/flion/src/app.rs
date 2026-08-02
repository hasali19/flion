use std::marker::PhantomData;

use winit::dpi::LogicalSize;
use winit::event_loop::EventLoop;
use winit::window::WindowBuilder;

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

        let window = WindowBuilder::new()
            .with_title("Flion Example")
            .with_inner_size(LogicalSize::new(1280, 800))
            .build(&event_loop)?;

        event_loop.run(move |event, target| match event {
            winit::event::Event::WindowEvent { window_id, event } if window_id == window.id() => {
                match event {
                    winit::event::WindowEvent::CloseRequested => target.exit(),

                    _ => {}
                }
            }

            _ => {}
        })?;

        Ok(())
    }
}
