use std::error::Error;

use flion::{FlionEngine, PlatformView, include_plugins};
use windows::UI::Color;
use windows::UI::Composition::Compositor;
use windows::core::Interface;
use windows_numerics::{Vector2, Vector3};

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

    FlionEngine::new()
        .with_plugins(PLUGINS)
        .with_platform_view_factory("example", |compositor: &Compositor| {
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
        })
        .run()?;

    Ok(())
}
