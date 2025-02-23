use std::error::Error;

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

    fluyt::run("build/flutter_assets")?;

    Ok(())
}
