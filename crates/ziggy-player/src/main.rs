use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use slint::ComponentHandle;
use tracing_subscriber::EnvFilter;
use ziggy_player::config::Config;
use ziggy_player::{AppWindow, runtime, ui_bridge};

#[derive(Parser)]
#[command(version, about = "Ziggy touchscreen Spotify player")]
struct Args {
    /// Path to the device config.
    #[arg(long, default_value = "dev/config.toml")]
    config: PathBuf,
    /// Use the built-in fake library and player instead of Spotify.
    #[arg(long)]
    fake: bool,
}

fn main() -> anyhow::Result<()> {
    // Several crates bring rustls with different crypto backends, so pick one explicitly.
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("rustls crypto provider already installed"))?;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let args = Args::parse();
    let config = Config::load(&args.config)?;
    let window = AppWindow::new()?;
    let (image_tx, image_rx) = tokio::sync::mpsc::unbounded_channel();
    ui_bridge::install(&window, image_tx);
    let inputs = runtime::start(
        config,
        args.fake,
        image_rx,
        Box::new(ui_bridge::publish),
        Arc::new(ui_bridge::deliver_image),
    )?;
    ui_bridge::wire_callbacks(&window, inputs);
    window.run()?;
    Ok(())
}
