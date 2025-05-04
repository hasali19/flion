use std::error::Error;

use flion::codec::EncodableValue;
use flion::{CompositorContext, FlionApp, PlatformView, include_plugins};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::DirectComposition::{
    DCOMPOSITION_OPACITY_MODE_MULTIPLY, IDCompositionVisual, IDCompositionVisual2,
    IDCompositionVisual3,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM,
};
use windows::core::Interface;
use windows_numerics::Matrix3x2;

include_plugins!();

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

    let app = FlionApp::builder()
        .with_platform_view_factory("example", SolidColorView::create)
        .build()?;

    app.run_event_loop()?;

    Ok(())
}

struct SolidColorView {
    visual: IDCompositionVisual,
}

impl SolidColorView {
    fn create(
        context: CompositorContext,
        _id: i32,
        _args: EncodableValue,
    ) -> color_eyre::Result<Box<dyn PlatformView>> {
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

        unsafe {
            visual.SetContent(&surface)?;
            visual
                .cast::<IDCompositionVisual2>()?
                .SetOpacityMode(DCOMPOSITION_OPACITY_MODE_MULTIPLY)?;
            visual.cast::<IDCompositionVisual3>()?.SetOpacity2(0.3)?;
        }

        Ok(Box::new(SolidColorView {
            visual: visual.cast()?,
        }))
    }
}

unsafe impl Send for SolidColorView {}
unsafe impl Sync for SolidColorView {}

impl PlatformView for SolidColorView {
    fn visual(&mut self) -> &IDCompositionVisual {
        &self.visual
    }

    fn update(&mut self, args: &flion::PlatformViewUpdateArgs) -> color_eyre::eyre::Result<()> {
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
