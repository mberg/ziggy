//! Voice requests: lower the music while listening, then run what was asked like a tap.

use super::*;

/// How far the music is lowered while listening, as a share of its volume.
const DUCK_PERCENT: u8 = 25;
/// A step for "louder" and "quieter".
const VOLUME_STEP: u8 = 15;
/// After this long without a reply, give up and bring the music back.
const VOICE_TIMEOUT_MS: u64 = 20_000;
/// At most this many library items go to Gemini.
const MAX_ITEMS: usize = 500;

impl Core {
    pub(super) fn on_voice(&mut self, update: VoiceUpdate, now_ms: u64, fx: &mut Vec<Effect>) {
        match update {
            VoiceUpdate::Woke => {
                self.wake(now_ms, fx);
                self.set_voice_phase(VoicePhase::Listening, now_ms);
                if self.state.voice.ducked_from.is_none()
                    && self.play_status() == PlayStatus::Playing
                {
                    let volume = self.volume();
                    self.state.voice.ducked_from = Some(volume);
                    let percent = (u16::from(volume) * u16::from(DUCK_PERCENT) / 100) as u8;
                    fx.push(match self.state.airplay.session {
                        Some(_) => Effect::AirPlay(AirPlayCommand::SetVolume { percent }),
                        None => Effect::Player(PlayerCommand::SetVolume { percent }),
                    });
                }
                fx.push(Effect::VoiceContext(self.voice_context()));
            }
            VoiceUpdate::Thinking => self.set_voice_phase(VoicePhase::Thinking, now_ms),
            VoiceUpdate::Command(command) => {
                self.set_voice_phase(VoicePhase::Idle, now_ms);
                self.run_voice_command(command, now_ms, fx);
            }
            VoiceUpdate::NotUnderstood => {
                self.end_voice(now_ms, fx);
                self.notify(Notice::Voice("Didn't catch that".into()), now_ms);
            }
            VoiceUpdate::Failed(message) => {
                self.end_voice(now_ms, fx);
                self.notify(Notice::Voice(message), now_ms);
            }
        }
    }

    fn set_voice_phase(&mut self, phase: VoicePhase, now_ms: u64) {
        self.state.voice.phase = phase;
        self.state.voice.since_ms = now_ms;
    }

    /// Back to idle with the music at its old volume.
    fn end_voice(&mut self, now_ms: u64, fx: &mut Vec<Effect>) {
        self.set_voice_phase(VoicePhase::Idle, now_ms);
        if let Some(volume) = self.state.voice.ducked_from.take() {
            self.on_ui(UiAction::SetVolume { percent: volume }, now_ms, fx);
        }
    }

    /// Called every second: a request that never got a reply mustn't leave the music quiet.
    pub(super) fn voice_tick(&mut self, now_ms: u64, fx: &mut Vec<Effect>) {
        let voice = &self.state.voice;
        if voice.phase != VoicePhase::Idle
            && now_ms.saturating_sub(voice.since_ms) >= VOICE_TIMEOUT_MS
        {
            self.end_voice(now_ms, fx);
        }
    }

    fn run_voice_command(&mut self, command: VoiceCommand, now_ms: u64, fx: &mut Vec<Effect>) {
        // Volume changes start from the volume before the music was lowered.
        let volume = self.state.voice.ducked_from.unwrap_or(self.volume());
        match command {
            VoiceCommand::Louder => {
                self.state.voice.ducked_from = None;
                let percent = volume.saturating_add(VOLUME_STEP).min(100);
                self.on_ui(UiAction::SetVolume { percent }, now_ms, fx);
                return;
            }
            VoiceCommand::Quieter => {
                self.state.voice.ducked_from = None;
                let percent = volume.saturating_sub(VOLUME_STEP);
                self.on_ui(UiAction::SetVolume { percent }, now_ms, fx);
                return;
            }
            VoiceCommand::SetVolume(percent) => {
                self.state.voice.ducked_from = None;
                self.on_ui(UiAction::SetVolume { percent }, now_ms, fx);
                return;
            }
            _ => self.end_voice(now_ms, fx),
        }
        let status = self.play_status();
        match command {
            VoiceCommand::Play(uri) => {
                let Some(item) = self
                    .voice_context()
                    .items
                    .into_iter()
                    .find(|i| i.uri == uri)
                else {
                    // Never play something Gemini made up.
                    self.notify(Notice::Voice("Didn't catch that".into()), now_ms);
                    return;
                };
                if item.kind == "artist" {
                    self.on_ui(UiAction::PlayArtist(uri), now_ms, fx);
                } else {
                    let shuffle = false;
                    self.on_ui(UiAction::PlayCollection { uri, shuffle }, now_ms, fx);
                }
                self.notify(Notice::Voice(format!("Playing {}", item.name)), now_ms);
            }
            VoiceCommand::Search { query, kind } => {
                let query = query.trim().to_string();
                if query.is_empty() {
                    self.notify(Notice::Voice("Didn't catch that".into()), now_ms);
                    return;
                }
                let search = &mut self.state.search;
                search.query = query.clone();
                search.sent = query.clone();
                search.edited_ms = now_ms;
                search.results = Slot {
                    loading: true,
                    ..Slot::default()
                };
                self.state.voice.pending_search = Some((query.clone(), kind));
                fx.push(Effect::Library(LibraryRequest::Search(query.clone())));
                self.notify(Notice::Voice(format!("Looking for {query}")), now_ms);
            }
            VoiceCommand::Pause => {
                if matches!(status, PlayStatus::Playing | PlayStatus::Loading) {
                    self.on_ui(UiAction::TogglePlay, now_ms, fx);
                }
            }
            VoiceCommand::Resume => {
                if matches!(status, PlayStatus::Paused | PlayStatus::Stopped) {
                    self.on_ui(UiAction::TogglePlay, now_ms, fx);
                }
            }
            VoiceCommand::Next => self.on_ui(UiAction::Next, now_ms, fx),
            VoiceCommand::Previous => self.on_ui(UiAction::Previous, now_ms, fx),
            VoiceCommand::SleepTimer(minutes) => {
                let minutes = (minutes > 0).then_some(minutes);
                self.on_ui(UiAction::SetSleepTimer(minutes), now_ms, fx);
            }
            VoiceCommand::LikeSong => {
                let Some(track) = self.state.playback.track.clone() else {
                    self.notify(Notice::Voice("Nothing is playing".into()), now_ms);
                    return;
                };
                // The same path as the add sheet's heart, which also says if it's already there.
                self.state.picker = Some(track.uri);
                self.on_ui(UiAction::PickLiked, now_ms, fx);
            }
            VoiceCommand::SaveAlbum => {
                let Some(album_uri) = self
                    .state
                    .playback
                    .track
                    .as_ref()
                    .and_then(|t| t.album_uri.clone())
                else {
                    self.notify(Notice::Voice("Nothing is playing".into()), now_ms);
                    return;
                };
                if self.in_library(&album_uri, Section::Albums) {
                    self.notify(
                        Notice::AlreadyIn(Section::Albums.title().to_string()),
                        now_ms,
                    );
                } else {
                    // Says "Added to Albums" itself.
                    self.on_ui(UiAction::ToggleSaveAlbum(album_uri), now_ms, fx);
                }
            }
            VoiceCommand::AddToPlaylist {
                uri: playlist_uri,
                heard,
            } => {
                let Some(track) = self.state.playback.track.clone() else {
                    self.notify(Notice::Voice("Nothing is playing".into()), now_ms);
                    return;
                };
                // The playlist whose name was said: Gemini's pick if it matches, otherwise any
                // of the listener's own playlists that does.
                let named = |uri: &str| {
                    self.editable(uri)
                        && self
                            .playlist(uri)
                            .is_some_and(|p| names_match(&heard, &p.name))
                };
                let playlist_uri = if named(&playlist_uri) {
                    Some(playlist_uri)
                } else {
                    self.state
                        .sections
                        .get(&Section::Playlists)
                        .and_then(|slot| slot.data.as_ref())
                        .and_then(|list| list.iter().find(|p| named(&p.uri)))
                        .map(|p| p.uri.clone())
                };
                let Some(playlist_uri) = playlist_uri else {
                    // No playlist was named ("add this song"): heart it.
                    self.state.picker = Some(track.uri);
                    self.on_ui(UiAction::PickLiked, now_ms, fx);
                    return;
                };
                self.state.picker = Some(track.uri);
                self.on_ui(UiAction::PickPlaylist(playlist_uri), now_ms, fx);
            }
            VoiceCommand::Home { ids, action } => self.voice_home(ids, action, now_ms, fx),
            VoiceCommand::Louder | VoiceCommand::Quieter | VoiceCommand::SetVolume(_) => {}
        }
    }

    /// Search results arrived: play the first of the kind a voice search asked for.
    pub(super) fn voice_search_results(&mut self, query: &str, now_ms: u64, fx: &mut Vec<Effect>) {
        let Some((wanted, kind)) = self.state.voice.pending_search.clone() else {
            return;
        };
        if wanted != query {
            return;
        }
        self.state.voice.pending_search = None;
        let Some(results) = self.state.search.results.data.clone() else {
            return;
        };
        let action = match kind {
            SearchKind::Song => results
                .tracks
                .first()
                .map(|t| UiAction::PlaySong(t.uri.clone())),
            SearchKind::Artist => results
                .artists
                .first()
                .map(|a| UiAction::PlayArtist(a.uri.clone())),
            SearchKind::Album => results.albums.first().map(|a| UiAction::PlayCollection {
                uri: a.uri.clone(),
                shuffle: false,
            }),
            SearchKind::Playlist => results.playlists.first().map(|p| UiAction::PlayCollection {
                uri: p.uri.clone(),
                shuffle: false,
            }),
        };
        let name = match kind {
            SearchKind::Song => results
                .tracks
                .first()
                .map(|t| format!("{} by {}", t.name, t.artists)),
            SearchKind::Artist => results.artists.first().map(|a| a.name.clone()),
            SearchKind::Album => results
                .albums
                .first()
                .map(|a| format!("{} by {}", a.name, a.subtitle)),
            SearchKind::Playlist => results.playlists.first().map(|p| p.name.clone()),
        };
        match (action, name) {
            (Some(action), Some(name)) => {
                self.on_ui(action, now_ms, fx);
                self.notify(Notice::Voice(format!("Playing {name}")), now_ms);
            }
            _ => self.notify(Notice::Voice(format!("Couldn't find {query}")), now_ms),
        }
    }

    pub(super) fn voice_search_failed(&mut self, query: &str) {
        if self
            .state
            .voice
            .pending_search
            .as_ref()
            .is_some_and(|(q, _)| q == query)
        {
            self.state.voice.pending_search = None;
        }
    }

    /// The library and what's playing, for Gemini.
    pub fn voice_context(&self) -> VoiceContext {
        let mut items: Vec<VoiceItem> = Vec::new();
        let sections = [
            Section::Playlists,
            Section::Albums,
            Section::Artists,
            Section::Liked,
            Section::Recent,
        ];
        let collections = sections
            .iter()
            .filter_map(|s| self.state.sections.get(s))
            .filter_map(|slot| slot.data.as_ref())
            .flat_map(|list| list.iter())
            .chain(self.state.artists_seen.values());
        for c in collections {
            if items.len() >= MAX_ITEMS {
                break;
            }
            if items.iter().any(|i| i.uri == c.uri) {
                continue;
            }
            items.push(VoiceItem {
                uri: c.uri.clone(),
                kind: match c.kind {
                    CollectionKind::Playlist => "playlist",
                    CollectionKind::Album => "album",
                    CollectionKind::Liked => "liked",
                    CollectionKind::Artist => "artist",
                },
                name: c.name.clone(),
                by: c.subtitle.clone(),
            });
        }
        let pb = &self.state.playback;
        VoiceContext {
            now_playing: pb
                .track
                .as_ref()
                .map(|t| format!("{} by {}", t.name, t.artists)),
            playing: pb.status == PlayStatus::Playing,
            volume: self.state.voice.ducked_from.unwrap_or(pb.volume),
            items,
            home: self.home_voice_controls(),
        }
    }
}

/// Whether what the listener said is the playlist's name, allowing for small mishearings.
fn names_match(heard: &str, name: &str) -> bool {
    let clean = |s: &str| -> String {
        s.to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect()
    };
    let (heard, name) = (clean(heard), clean(name));
    if heard.is_empty() || name.is_empty() {
        return false;
    }
    // Numbers must match exactly: "playlist 2" is not "Playlist 1".
    let digits = |s: &str| -> String { s.chars().filter(|c| c.is_ascii_digit()).collect() };
    if digits(&heard) != digits(&name) {
        return false;
    }
    if heard.contains(&name) || name.contains(&heard) {
        return heard.len() * 2 >= name.len();
    }
    // Edit distance within a quarter of the name.
    let (a, b): (Vec<char>, Vec<char>) = (heard.chars().collect(), name.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        prev = cur;
    }
    prev[b.len()] * 4 <= b.len()
}

#[cfg(test)]
mod tests {
    use super::names_match;

    #[test]
    fn spoken_playlist_names_match_loosely() {
        assert!(names_match("road trip", "Road Trip"));
        assert!(names_match("the road trip", "Road Trip!"));
        assert!(names_match("rode trip", "Road Trip"));
        assert!(!names_match("", "Road Trip"));
        assert!(!names_match("party", "Road Trip"));
        assert!(!names_match("playlist 2", "Playlist 1"));
        assert!(names_match("mix 2", "Mix 2"));
        assert!(!names_match("road", "Road Trip Songs For The Long Drive"));
    }
}
