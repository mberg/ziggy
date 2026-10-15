pub mod fake;
pub mod soloist;
pub mod sonos;

use tokio::sync::mpsc::{self, UnboundedSender};

use crate::app::{Input, PlayerCommand, PlayerUpdate};

/// Where Soloist can't run (a Mac in real mode), the device plays nothing itself; a Sonos room
/// still works, and `--fake` shows the whole UI.
pub fn spawn_unavailable(inputs: UnboundedSender<Input>) -> UnboundedSender<PlayerCommand> {
    let (tx, mut rx) = mpsc::unbounded_channel::<PlayerCommand>();
    tracing::warn!("Spotify playback runs on the Raspberry Pi (Soloist); this computer can't play");
    let _ = inputs.send(Input::Player(PlayerUpdate::Disconnected));
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    tx
}
