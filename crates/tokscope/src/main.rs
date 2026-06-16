//! tokscope binary — owns all terminal I/O; domain logic lives in tokscope-core.

mod cli;
mod config;
mod pricing;
mod render;
mod tui;

fn main() {
    // Logs (incl. lenient-parse warnings) go to stderr so stdout stays clean for
    // tables and JSON. Tune with TOKSCOPE_LOG (e.g. TOKSCOPE_LOG=debug).
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(tracing::level_filters::LevelFilter::WARN.into())
                .with_env_var("TOKSCOPE_LOG")
                .from_env_lossy(),
        )
        .with_writer(std::io::stderr)
        .init();

    if let Err(e) = cli::run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
