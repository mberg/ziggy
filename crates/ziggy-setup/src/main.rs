//! `ziggy`: sets up and looks after Ziggy players from this computer.

mod devconfig;
mod install;
mod music;
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
    /// Copy music files onto a player's SD card, and look after them there.
    #[command(subcommand)]
    Music(Music),
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

#[derive(Subcommand)]
enum Music {
    /// Copy music folders or files onto the player.
    ///
    /// A folder keeps its name and layout; files already there are skipped. Copies MP3, M4A,
    /// AAC and FLAC files, and album covers.
    Add {
        /// Folders or files to copy, e.g. "~/Music/Abba".
        #[arg(required = true, num_args = 1..)]
        paths: Vec<PathBuf>,
        /// The player's address, e.g. ziggy-kitchen.local.
        host: String,
    },
    /// List the music on the player, by folder.
    List { host: String },
    /// Delete a folder (or a file) from the player's music, e.g. "Abba/Gold".
    Remove {
        folder: String,
        host: String,
        /// Delete without asking first.
        #[arg(long)]
        yes: bool,
    },
    /// Show how much music is on the player and how much room is left.
    Space { host: String },
    /// Set or remove a password for the shared music folder.
    SharePassword { host: String },
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
        Command::Music(command) => match command {
            Music::Add { paths, host } => music::add(&paths, &host),
            Music::List { host } => music::list(&host),
            Music::Remove { folder, host, yes } => music::remove(&folder, &host, yes),
            Music::Space { host } => music::space(&host),
            Music::SharePassword { host } => music::share_password(&host),
        },
        Command::AuthWeb {
            state_dir,
            client_id,
        } => spotify::auth_web(&state_dir, &client_id).await,
        Command::Probe { state_dir } => spotify::probe(&state_dir).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn music_add_takes_several_paths_then_the_host() {
        let cli = Cli::try_parse_from(["ziggy", "music", "add", "a", "b c", "den.local"]).unwrap();
        let Command::Music(Music::Add { paths, host }) = cli.command else {
            panic!("not music add");
        };
        assert_eq!(paths, [PathBuf::from("a"), PathBuf::from("b c")]);
        assert_eq!(host, "den.local");
        assert!(Cli::try_parse_from(["ziggy", "music", "add", "den.local"]).is_err());
    }
}
