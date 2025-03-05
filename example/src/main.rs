use std::error::Error;
use std::ffi::c_void;

use flion::{FlionEngine, include_plugins};

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

    FlionEngine::new("build/flutter_assets")
        .with_plugins(PLUGINS)
        .run()?;

    Ok(())
}
