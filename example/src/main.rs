use std::error::Error;
use std::mem;
use std::os::raw::c_void;
use std::rc::Rc;

use flion::codec::EncodableValue;
use flion::{
    CompositorContext, FlionEngineEnvironment, PlatformTask, PlatformView, TaskRunnerExecutor,
    include_plugins,
};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::DirectComposition::{
    DCOMPOSITION_OPACITY_MODE_MULTIPLY, IDCompositionVisual, IDCompositionVisual2,
    IDCompositionVisual3,
};
use windows::Win32::Graphics::Dwm::{
    DWM_SYSTEMBACKDROP_TYPE, DWMSBT_MAINWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM,
};
use windows::core::Interface;
use windows_numerics::Matrix3x2;
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
            |context: CompositorContext,
             _id: i32,
             _args: EncodableValue|
             -> color_eyre::Result<Box<dyn PlatformView>> {
                let visual = unsafe { context.composition_device.CreateVisual()? };

                // Create a 1x1 bitmap surface. This will be scaled to fill the size of the platform
                // view.
                let surface = unsafe {
                    context.composition_device.CreateSurface(
                        1,
                        1,
                        DXGI_FORMAT_B8G8R8A8_UNORM,
                        DXGI_ALPHA_MODE_PREMULTIPLIED,
                    )?
                };

                let mut offset = Default::default();
                let texture: ID3D11Texture2D = unsafe { surface.BeginDraw(None, &mut offset)? };

                unsafe {
                    // Clear the surface texture to a solid color
                    let mut rtv = None;
                    context
                        .d3d11_device
                        .CreateRenderTargetView(&texture, None, Some(&mut rtv))?;
                    let rtv = rtv.unwrap();
                    context
                        .d3d11_device
                        .GetImmediateContext()?
                        .ClearRenderTargetView(&rtv, &[1.0, 0.0, 0.0, 1.0]);
                }

                unsafe {
                    surface.EndDraw()?;
                }

                struct SolidColorView {
                    visual: IDCompositionVisual,
                }

                unsafe {
                    visual.SetContent(&surface)?;
                    visual
                        .cast::<IDCompositionVisual2>()?
                        .SetOpacityMode(DCOMPOSITION_OPACITY_MODE_MULTIPLY)?;
                    visual.cast::<IDCompositionVisual3>()?.SetOpacity2(0.3)?;
                }

                unsafe impl Send for SolidColorView {}
                unsafe impl Sync for SolidColorView {}

                impl PlatformView for SolidColorView {
                    fn visual(&mut self) -> &IDCompositionVisual {
                        &self.visual
                    }

                    fn update(
                        &mut self,
                        args: &flion::PlatformViewUpdateArgs,
                    ) -> color_eyre::eyre::Result<()> {
                        unsafe {
                            self.visual.SetTransform2(&Matrix3x2 {
                                M11: args.width as f32,
                                M22: args.height as f32,
                                ..Default::default()
                            })?;

                            self.visual.SetOffsetX2(args.x as f32)?;
                            self.visual.SetOffsetY2(args.y as f32)?;
                        }

                        Ok(())
                    }
                }

                impl Drop for SolidColorView {
                    fn drop(&mut self) {
                        tracing::info!("destroying platform view");
                    }
                }

                Ok(Box::new(SolidColorView {
                    visual: visual.cast()?,
                }))
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
