use clap::Parser;
use crownos_screenshot::{app, cli::Cli};
use tracing_subscriber::EnvFilter;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    app::run(Cli::parse())
}
