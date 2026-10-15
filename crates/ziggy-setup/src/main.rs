//! `ziggy`: sets up and looks after Ziggy players from this computer.

mod devconfig;
mod install;
mod remote;
mod spotify;
mod store;
mod wizard;

use std::path::PathBuf;

use anyhow::anyhow;
use clap::{Parser, Subcommand};

use install::PlayerSource;

#[derive(Parser)]
#[command(name = "ziggy", version, about = "Set up and look after Ziggy players")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Args)]
struct Source {
    /// Install this release tag instead of the newest, e.g. v0.2.0.
    #[arg(long)]
    release: Option<String>,
    /// Install a player built on this computer (scripts/build-pi.sh) instead of a release.
    #[arg(long, conflicts_with = "release")]
    player_binary: Option<PathBuf>,
}

impl Source {
    fn player(self) -> PlayerSource {
        match self.player_binary {
            Some(path) => PlayerSource::Local(path),
            None => PlayerSource::Release(self.release),
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Set up a Raspberry Pi as a player, step by step.
    Setup {
        /// The Pi's address, e.g. ziggy-kitchen.local; asked for when left out.
        host: Option<String>,
        #[command(flatten)]
        source: Source,
    },
    /// Change a player's settings.
    Config { host: String },
    /// Install the newest player (or a chosen one) and restart it.
    Update {
        host: String,
        #[command(flatten)]
        source: Source,
    },
    /// Check a player is running.
    Status { host: String },
    /// Show a player's log as it happens.
    Logs { host: String },
    /// Sign a Spotify account in on a player again: the library, then pairing from a phone.
    Signin { host: String },
    /// Advanced: sign in through the developer app into a folder.
    #[command(hide = true)]
    AuthWeb {
        #[arg(long)]
        state_dir: PathBuf,
        #[arg(long)]
        client_id: String,
    },
    /// Advanced: check the sign-in in a folder can read the library.
    #[command(hide = true)]
    Probe {
        #[arg(long)]
        state_dir: PathBuf,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // The OAuth and HTTP crates bring rustls with different crypto backends; pick one.
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow!("rustls crypto provider already installed"))?;
    match Cli::parse().command {
        Command::Setup { host, source } => {
            wizard::setup(wizard::SetupOptions {
                host,
                player: source.player(),
            })
            .await
        }
        Command::Config { host } => wizard::config(&host),
        Command::Update { host, source } => wizard::update(&host, source.player()),
        Command::Status { host } => wizard::status(&host),
        Command::Logs { host } => wizard::logs(&host),
        Command::Signin { host } => wizard::signin(&host).await,
        Command::AuthWeb {
            state_dir,
            client_id,
        } => spotify::auth_web(&state_dir, &client_id).await,
        Command::Probe { state_dir } => spotify::probe(&state_dir).await,
    }
}
