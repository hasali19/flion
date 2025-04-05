use std::error::Error;
use std::mem;
use std::os::raw::c_void;
use std::rc::Rc;

use flion::codec::EncodableValue;
use flion::{
    FlionEngineEnvironment, PlatformTask, PlatformView, TaskRunnerExecutor, include_plugins,
};
use windows::UI::Color;
use windows::UI::Composition::Compositor;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::{
    DWM_SYSTEMBACKDROP_TYPE, DWMSBT_MAINWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DwmSetWindowAttribute,
};
use windows::core::Interface;
use windows_numerics::{Vector2, Vector3};
use winit::dpi::LogicalSize;
use winit::event_loop::{ControlFlow, EventLoopBuilder};
use winit::platform::windows::WindowBuilderExtWindows;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::WindowBuilder;

include_plugins!();

enum AppEvent {
    EngineTask(PlatformTask),
}

fn main() -> Result<(), Box<dyn Error>> {
    #[cfg(debug_assertions)]
    {
        use tracing_subscriber::fmt::format::FmtSpan;
        tracing_subscriber::fmt()
            .with_span_events(FmtSpan::ENTER)
            .with_thread_names(true)
            .with_max_level(tracing::Level::DEBUG)
            .init();
    }

    let event_loop = EventLoopBuilder::<AppEvent>::with_user_event().build()?;

    let window = WindowBuilder::new()
        .with_inner_size(LogicalSize::new(1280, 720))
        .with_no_redirection_bitmap(true)
        .build(&event_loop)?;

    let window = Rc::new(window);

    let hwnd = match window.window_handle()?.as_raw() {
        RawWindowHandle::Win32(handle) => HWND(handle.hwnd.get() as _),
        _ => unreachable!(),
    };

    unsafe {
        let backdrop_type = DWMSBT_MAINWINDOW;
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            &raw const backdrop_type as *const c_void,
            mem::size_of::<DWM_SYSTEMBACKDROP_TYPE>() as u32,
        )?;
    }

    let env = FlionEngineEnvironment::init()?;

    let mut engine = env
        .new_engine_builder()
        .with_plugins(PLUGINS)
        .with_platform_view_factory(
            "example",
            |compositor: &Compositor, _id: i32, _args: EncodableValue| {
                let visual = compositor.CreateSpriteVisual()?;

                visual.SetBrush(&compositor.CreateColorBrushWithColor(Color {
                    R: 255,
                    G: 0,
                    B: 0,
                    A: 100,
                })?)?;

                Ok(PlatformView {
                    visual: visual.cast()?,
                    on_update: Box::new(move |args| {
                        visual.SetSize(Vector2 {
                            X: args.width as f32,
                            Y: args.height as f32,
                        })?;

                        visual.SetOffset(Vector3 {
                            X: args.x as f32,
                            Y: args.y as f32,
                            Z: 0.0,
                        })?;

                        Ok(())
                    }),
                })
            },
        )
        .build(window.clone(), {
            let event_loop = event_loop.create_proxy();
            move |task| {
                if event_loop.send_event(AppEvent::EngineTask(task)).is_err() {
                    tracing::error!("failed to post task to event loop");
                }
            }
        })?;

    let mut task_executor = TaskRunnerExecutor::default();

    event_loop.run(move |event, target| {
        match event {
            winit::event::Event::UserEvent(event) => match event {
                AppEvent::EngineTask(task) => {
                    task_executor.enqueue(task);
                }
            },

            winit::event::Event::WindowEvent { window_id, event } if window_id == window.id() => {
                if let Err(e) = engine.handle_window_event(&event, target) {
                    tracing::error!("{e:?}");
                }
            }

            _ => {}
        }

        if let Some(next_task_target_time) = engine.process_tasks(&mut task_executor) {
            target.set_control_flow(ControlFlow::WaitUntil(next_task_target_time));
        }
    })?;

    Ok(())
}
