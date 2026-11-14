//! Spotify and the music on the card side by side: one of them has the speaker at a time.
//!
//! The core sends its commands here as if there were one player. Loading something from the
//! card hands the speaker to the local player and pauses Spotify; loading anything else hands
//! it back. Each player's reports reach the core only while it has the speaker. Spotify
//! starting to play by itself (someone picked this player in a Spotify app) takes it back.

use std::time::{Duration, Instant};

use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::app::{Input, PlayerCommand, PlayerUpdate};
use crate::music::index::is_local;

/// Spotify saying it plays this soon after the card took over is the pause catching up.
const SPOTIFY_SETTLE: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Spotify,
    Local,
}

/// What a player last said about itself, replayed to the core when it gets the speaker back.
#[derive(Default)]
struct Remembered {
    connected: Option<PlayerUpdate>,
    track: Option<PlayerUpdate>,
    shuffle: Option<PlayerUpdate>,
    repeat: Option<PlayerUpdate>,
    status: Option<PlayerUpdate>,
}

impl Remembered {
    fn note(&mut self, update: &PlayerUpdate) {
        let slot = match update {
            PlayerUpdate::Connected | PlayerUpdate::Disconnected => &mut self.connected,
            PlayerUpdate::TrackChanged(_) => &mut self.track,
            PlayerUpdate::Shuffle(_) => &mut self.shuffle,
            PlayerUpdate::Repeat(_) => &mut self.repeat,
            PlayerUpdate::Playing { .. } | PlayerUpdate::Paused { .. } | PlayerUpdate::Stopped => {
                &mut self.status
            }
            _ => return,
        };
        *slot = Some(update.clone());
    }

    fn replay(&self) -> Vec<PlayerUpdate> {
        [
            &self.connected,
            &self.track,
            &self.shuffle,
            &self.repeat,
            &self.status,
        ]
        .into_iter()
        .flatten()
        .cloned()
        .collect()
    }
}

/// The routing rules, apart from channels, so they can be tested.
pub struct Router {
    pub active: Source,
    /// None on a card-only player.
    has_spotify: bool,
    switched: Instant,
    spotify: Remembered,
    local: Remembered,
}

/// Where a command goes.
#[derive(Debug, PartialEq)]
pub enum Send {
    Spotify(PlayerCommand),
    Local(PlayerCommand),
}

impl Router {
    pub fn new(has_spotify: bool, now: Instant) -> Router {
        Router {
            active: if has_spotify {
                Source::Spotify
            } else {
                Source::Local
            },
            has_spotify,
            switched: now,
            spotify: Remembered::default(),
            local: Remembered::default(),
        }
    }

    fn to(&self, source: Source, command: PlayerCommand) -> Option<Send> {
        match source {
            Source::Spotify if self.has_spotify => Some(Send::Spotify(command)),
            Source::Spotify => None,
            Source::Local => Some(Send::Local(command)),
        }
    }

    pub fn command(&mut self, command: PlayerCommand, now: Instant) -> Vec<Send> {
        match command {
            PlayerCommand::Load {
                ref context_uri, ..
            } => {
                let wanted = if is_local(context_uri) || !self.has_spotify {
                    Source::Local
                } else {
                    Source::Spotify
                };
                let mut out = Vec::new();
                if wanted != self.active {
                    out.extend(self.to(self.active, PlayerCommand::Pause));
                    self.active = wanted;
                    self.switched = now;
                }
                out.extend(self.to(wanted, command));
                out
            }
            // Both keep the same volume, so switching doesn't jump.
            PlayerCommand::SetVolume { .. } => [Source::Spotify, Source::Local]
                .into_iter()
                .filter_map(|s| self.to(s, command.clone()))
                .collect(),
            other => self.to(self.active, other).into_iter().collect(),
        }
    }

    /// A report from one of the players: what the core should hear, and any commands that
    /// follow from it.
    pub fn update(
        &mut self,
        from: Source,
        update: PlayerUpdate,
        now: Instant,
    ) -> (Vec<PlayerUpdate>, Vec<Send>) {
        match from {
            Source::Spotify => self.spotify.note(&update),
            Source::Local => self.local.note(&update),
        }
        if from == self.active {
            return (vec![update], vec![]);
        }
        let took_over = from == Source::Spotify
            && matches!(update, PlayerUpdate::Playing { .. })
            && now.duration_since(self.switched) >= SPOTIFY_SETTLE;
        if !took_over {
            return (vec![], vec![]);
        }
        self.active = Source::Spotify;
        self.switched = now;
        (
            self.spotify.replay(),
            vec![Send::Local(PlayerCommand::Pause)],
        )
    }
}

/// Starts the router. `spotify` is None on a card-only player.
pub fn spawn(
    spotify: Option<(UnboundedSender<PlayerCommand>, UnboundedReceiver<Input>)>,
    local: (UnboundedSender<PlayerCommand>, UnboundedReceiver<Input>),
    inputs: UnboundedSender<Input>,
) -> UnboundedSender<PlayerCommand> {
    let (tx, mut commands) = mpsc::unbounded_channel::<PlayerCommand>();
    let mut router = Router::new(spotify.is_some(), Instant::now());
    let (spotify_tx, mut spotify_rx) = match spotify {
        Some((tx, rx)) => (Some(tx), Some(rx)),
        None => (None, None),
    };
    let (local_tx, mut local_rx) = local;
    tokio::spawn(async move {
        let deliver = |sends: Vec<Send>| {
            for send in sends {
                match send {
                    Send::Spotify(c) => {
                        if let Some(tx) = &spotify_tx {
                            let _ = tx.send(c);
                        }
                    }
                    Send::Local(c) => {
                        let _ = local_tx.send(c);
                    }
                }
            }
        };
        loop {
            let (from, input) = tokio::select! {
                command = commands.recv() => match command {
                    Some(command) => {
                        deliver(router.command(command, Instant::now()));
                        continue;
                    }
                    None => return,
                },
                Some(input) = async {
                    match &mut spotify_rx {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => (Source::Spotify, input),
                Some(input) = local_rx.recv() => (Source::Local, input),
            };
            match input {
                Input::Player(update) => {
                    let (forward, sends) = router.update(from, update, Instant::now());
                    for update in forward {
                        let _ = inputs.send(Input::Player(update));
                    }
                    deliver(sends);
                }
                // Anything else a player says (sign-in, pairing) goes straight on.
                other => {
                    let _ = inputs.send(other);
                }
            }
        }
    });
    tx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(uri: &str) -> PlayerCommand {
        PlayerCommand::Load {
            context_uri: uri.into(),
            start_index: None,
            start_uri: None,
            shuffle: false,
        }
    }

    #[test]
    fn loading_from_the_card_pauses_spotify_and_back_again() {
        let t0 = Instant::now();
        let mut r = Router::new(true, t0);
        assert_eq!(
            r.command(load("spotify:album:a"), t0),
            vec![Send::Spotify(load("spotify:album:a"))]
        );
        assert_eq!(
            r.command(load("local:album:x"), t0),
            vec![
                Send::Spotify(PlayerCommand::Pause),
                Send::Local(load("local:album:x"))
            ]
        );
        assert_eq!(
            r.command(PlayerCommand::Next, t0),
            vec![Send::Local(PlayerCommand::Next)]
        );
        assert_eq!(
            r.command(PlayerCommand::SetVolume { percent: 40 }, t0),
            vec![
                Send::Spotify(PlayerCommand::SetVolume { percent: 40 }),
                Send::Local(PlayerCommand::SetVolume { percent: 40 })
            ]
        );
        assert_eq!(
            r.command(load("spotify:playlist:p"), t0),
            vec![
                Send::Local(PlayerCommand::Pause),
                Send::Spotify(load("spotify:playlist:p"))
            ]
        );
    }

    #[test]
    fn only_the_player_with_the_speaker_is_heard() {
        let t0 = Instant::now();
        let mut r = Router::new(true, t0);
        r.command(load("local:album:x"), t0);
        let (heard, _) = r.update(Source::Spotify, PlayerUpdate::Paused { position_ms: 5 }, t0);
        assert!(heard.is_empty());
        let (heard, _) = r.update(Source::Local, PlayerUpdate::Playing { position_ms: 0 }, t0);
        assert_eq!(heard, vec![PlayerUpdate::Playing { position_ms: 0 }]);
    }

    #[test]
    fn spotify_playing_by_itself_takes_the_speaker_back() {
        let t0 = Instant::now();
        let mut r = Router::new(true, t0);
        let song = PlayerUpdate::TrackChanged(crate::model::Track {
            uri: "spotify:track:s".into(),
            ..Default::default()
        });
        r.update(Source::Spotify, song.clone(), t0);
        r.command(load("local:album:x"), t0);
        // The pause catching up doesn't count.
        let (heard, sends) = r.update(
            Source::Spotify,
            PlayerUpdate::Playing { position_ms: 0 },
            t0 + Duration::from_secs(1),
        );
        assert!(heard.is_empty() && sends.is_empty());
        assert_eq!(r.active, Source::Local);
        let (heard, sends) = r.update(
            Source::Spotify,
            PlayerUpdate::Playing { position_ms: 0 },
            t0 + Duration::from_secs(10),
        );
        assert_eq!(r.active, Source::Spotify);
        assert_eq!(sends, vec![Send::Local(PlayerCommand::Pause)]);
        assert!(
            heard.contains(&song),
            "Now Playing shows Spotify's song again"
        );
        assert_eq!(
            heard.last(),
            Some(&PlayerUpdate::Playing { position_ms: 0 })
        );
    }

    #[test]
    fn a_card_only_player_sends_everything_to_the_card() {
        let t0 = Instant::now();
        let mut r = Router::new(false, t0);
        assert_eq!(
            r.command(load("local:album:x"), t0),
            vec![Send::Local(load("local:album:x"))]
        );
        assert_eq!(
            r.command(PlayerCommand::SetVolume { percent: 1 }, t0),
            vec![Send::Local(PlayerCommand::SetVolume { percent: 1 })]
        );
    }
}
