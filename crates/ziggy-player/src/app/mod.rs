pub mod input;
pub mod state;
#[cfg(test)]
pub(crate) mod tests;
mod voice;

use std::collections::HashMap;
use std::sync::Arc;

pub use input::*;
pub use state::*;

use crate::model::{
    Collection, CollectionKind, EditOutcome, LIKED_URI, PlayRecord, PlaylistEdit, Section, Track,
    liked_collection,
};
use crate::settings::{Output, Settings, Speaker};

/// How long a notice stays on screen.
const NOTICE_MS: u64 = 4_000;
/// Identical play requests closer together than this are treated as one.
const DOUBLE_TAP_MS: u64 = 1_000;
/// A search goes out once typing has paused this long.
const SEARCH_PAUSE_MS: u64 = 400;
const MAX_QUERY_CHARS: usize = 100;
const MAX_NAME_CHARS: usize = 100;
const MAX_DEVICE_NAME_CHARS: usize = 40;
/// Listening time is saved to history at least this often while playing.
const LISTEN_REPORT_MS: u64 = 10_000;
/// How many albums and playlists Recent shows.
const RECENT_COLLECTIONS: usize = 30;
/// The last colour scheme index (Daylight).
const MAX_THEME: u32 = 4;
/// Sleep timer length unless Settings says otherwise.
pub const DEFAULT_SLEEP_MINUTES: u32 = 30;
/// The sleep timer fades the volume out over its last this-many milliseconds.
const SLEEP_FADE_MS: u64 = 5_000;
/// While a sleep timer runs, the screen dims this soon after the last touch...
const SLEEP_SCREEN_DIM_MS: u64 = 3_000;
/// ...and turns off this long after it.
const SLEEP_SCREEN_OFF_MS: u64 = 60_000;

/// What to put back if Spotify refuses an edit, and what to reload either way.
#[derive(Debug, Default)]
struct Undo {
    tracks: Vec<(String, Option<Slot<Vec<Track>>>)>,
    sections: Vec<(Section, Option<Slot<Vec<Collection>>>)>,
    /// URI of the placeholder shown while a new playlist is being created.
    placeholder: Option<String>,
    /// A song's liked state before a like or unlike.
    liked: Option<(String, Option<bool>)>,
}

#[derive(Debug, Clone)]
pub struct CoreConfig {
    pub dim_after_ms: u64,
    pub off_after_ms: u64,
    pub initial_volume: u8,
    /// The Spotify Connect name in use.
    pub device_name: String,
    /// The Bluetooth speaker in use; None for the headphone jack.
    pub speaker: Option<crate::settings::Speaker>,
    /// The saved settings, which the Settings screen builds on.
    pub saved: crate::settings::Settings,
    /// Bluetooth is available (the Pi, or `--fake` mode).
    pub bluetooth: bool,
    /// Signed in to Audiobookshelf as, if signed in.
    pub books_user: Option<String>,
    /// The Audiobookshelf address from the config file, used when Settings has none.
    pub books_config_url: Option<String>,
    /// Voice control is set up, so the microphone button shows.
    pub voice: bool,
}

impl CoreConfig {
    /// The Sonos room playing instead of this device, if one is chosen.
    pub fn sonos_room(&self) -> Option<&crate::settings::SonosRoom> {
        match &self.saved.output {
            Some(Output::Sonos(room)) => Some(room),
            _ => None,
        }
    }
}

/// Where a newly loaded context starts playing.
#[derive(Debug, Clone, PartialEq)]
enum Start {
    /// Let shuffle pick.
    Shuffled,
    Index(u32),
    /// A specific song, e.g. one picked from search results.
    Track(Track),
}

/// The app's state machine. Pure: no I/O, time is passed in.
pub struct Core {
    state: AppState,
    cfg: CoreConfig,
    last_activity_ms: u64,
    notice_until_ms: u64,
    last_load: Option<((String, Start, bool), u64)>,
    /// Where closing search returns to.
    before_search: Section,
    next_edit: u64,
    undo: HashMap<u64, Undo>,
    /// The volume before the sleep timer started fading, to put back afterwards.
    sleep_volume: Option<u8>,
    /// The song being logged in play history, and how long it has actually played.
    logged_uri: Option<String>,
    listened_ms: u64,
    /// `listened_ms` as last sent to the history store.
    listened_reported: u64,
    /// When listening time was last added up, while playing.
    listen_mark: Option<u64>,
    /// Spotify's recently-played list was asked for because the device has no history yet.
    asked_spotify_recent: bool,
    /// When recent songs failed to load. The player skips each one by itself, so without a
    /// limit a refused account races through a whole playlist asking Spotify for every song.
    failed_tracks_ms: Vec<u64>,
}

/// This many songs failing within `FAILED_TRACKS_WINDOW_MS` pauses playback.
const FAILED_TRACKS_LIMIT: usize = 3;
const FAILED_TRACKS_WINDOW_MS: u64 = 30_000;

impl Core {
    pub fn new(cfg: CoreConfig, now_ms: u64) -> (Core, Vec<Effect>) {
        let state = AppState {
            // Albums is the home screen at power-on.
            screen: Screen::Grid(Section::Albums),
            section: Section::Albums,
            back_stack: Vec::new(),
            sections: Default::default(),
            tracks: Default::default(),
            playback: Playback {
                track: None,
                context_uri: None,
                status: PlayStatus::Stopped,
                position_ms: 0,
                volume: cfg.initial_volume.min(100),
                shuffle: false,
                repeat: Default::default(),
            },
            notice: None,
            display: DisplayMode::Active,
            online: true,
            auth_needed: false,
            pairing_needed: false,
            speaker_connected: true,
            account: None,
            search: Default::default(),
            keyboard_open: false,
            artist_albums: Default::default(),
            picker: None,
            text_entry: None,
            editing: None,
            confirm_delete: None,
            liked: HashMap::new(),
            list_view: false,
            artists_seen: HashMap::new(),
            now_ms,
            history: None,
            recent_songs: false,
            sleep_ends_ms: None,
            voice: VoiceState {
                enabled: cfg.voice,
                ..VoiceState::default()
            },
            device: DeviceSettings {
                saved: cfg.saved.clone(),
                device_name: cfg.device_name.clone(),
                speaker: cfg.speaker.clone(),
                bluetooth: cfg.bluetooth,
                ..Default::default()
            },
            books: {
                let saved = cfg.saved.audiobooks.clone();
                BooksState {
                    // Settings decide; otherwise an address in the config file turns Books on.
                    enabled: saved
                        .as_ref()
                        .map_or(cfg.books_config_url.is_some(), |a| a.enabled),
                    url: saved
                        .map(|a| a.url)
                        .filter(|u| !u.is_empty())
                        .or(cfg.books_config_url.clone())
                        .unwrap_or_default(),
                    username: cfg.books_user.clone(),
                    ..Default::default()
                }
            },
        };
        let mut core = Core {
            state,
            cfg,
            last_activity_ms: now_ms,
            notice_until_ms: 0,
            last_load: None,
            before_search: Section::Albums,
            next_edit: 0,
            undo: HashMap::new(),
            sleep_volume: None,
            logged_uri: None,
            listened_ms: 0,
            listened_reported: 0,
            listen_mark: None,
            asked_spotify_recent: false,
            failed_tracks_ms: Vec::new(),
        };
        let mut fx = Vec::new();
        for section in [Section::Playlists, Section::Albums, Section::Recent] {
            core.request_section(section, &mut fx);
        }
        // Liked Songs answers "is this liked?" for every song without asking Spotify.
        core.request_tracks(LIKED_URI, &mut fx);
        fx.push(Effect::Library(LibraryRequest::Account));
        fx.push(Effect::Display(DisplayMode::Active));
        (core, fx)
    }

    pub fn state(&self) -> &AppState {
        &self.state
    }

    pub fn handle(&mut self, input: Input, now_ms: u64) -> Vec<Effect> {
        let mut fx = Vec::new();
        self.state.now_ms = now_ms;
        match input {
            Input::Ui(action) => {
                self.wake(now_ms, &mut fx);
                self.on_ui(action, now_ms, &mut fx);
            }
            Input::Player(update) => self.on_player(update, now_ms, &mut fx),
            Input::Library(update) => self.on_library(update, now_ms, &mut fx),
            Input::Speaker { connected } => self.state.speaker_connected = connected,
            Input::AuthInvalid => self.state.auth_needed = true,
            Input::Tick => {
                self.on_tick(now_ms, &mut fx);
                self.maybe_search(now_ms, &mut fx);
            }
            Input::SearchTick => {
                self.maybe_search(now_ms, &mut fx);
                self.on_sleep_tick(now_ms, &mut fx);
            }
            Input::Bluetooth(update) => self.on_bluetooth(update, &mut fx),
            Input::Books(update) => self.on_books(update, &mut fx),
            Input::Voice(update) => self.on_voice(update, now_ms, &mut fx),
            Input::Temperature(c) => self.state.device.temperature_c = c,
            Input::Wifi(wifi) => self.state.device.wifi = wifi,
            Input::PairingNeeded(needed) => self.state.pairing_needed = needed,
            Input::SonosRooms(rooms) => {
                self.state.device.sonos_rooms = rooms;
                self.state.device.sonos_scanning = false;
            }
            Input::HistoryRecent(plays) => {
                if plays.is_empty() && !self.asked_spotify_recent {
                    // A new device has no history yet: borrow Spotify's list meanwhile.
                    self.asked_spotify_recent = true;
                    fx.push(Effect::Library(LibraryRequest::Section(Section::Recent)));
                } else if !plays.is_empty() {
                    let slot = self.state.sections.entry(Section::Recent).or_default();
                    slot.data = Some(Arc::new(Self::recent_collections(&plays)));
                    slot.loading = false;
                    slot.failed = false;
                }
                self.state.history = Some(Arc::new(plays));
            }
        }
        if !matches!(self.state.screen, Screen::Search | Screen::Books) {
            self.state.keyboard_open = false;
        }
        // Edit mode belongs to the playlist on screen; leaving it ends editing.
        if let Some(uri) = &self.state.editing
            && self.state.screen != Screen::Detail(uri.clone())
        {
            self.state.editing = None;
            self.state.confirm_delete = None;
        }
        fx
    }

    /// The runtime sends the fast `Input::SearchTick` only while this is true: on the search
    /// screen, and during the sleep timer's fade so the volume steps down smoothly.
    pub fn wants_search_tick(&self) -> bool {
        let fading = self
            .state
            .sleep_ends_ms
            .is_some_and(|ends| ends.saturating_sub(self.state.now_ms) <= SLEEP_FADE_MS + 1_000);
        self.state.screen == Screen::Search || fading
    }

    fn maybe_search(&mut self, now_ms: u64, fx: &mut Vec<Effect>) {
        let search = &mut self.state.search;
        if search.query == search.sent || now_ms.saturating_sub(search.edited_ms) < SEARCH_PAUSE_MS
        {
            return;
        }
        search.sent = search.query.clone();
        let query = search.query.trim().to_string();
        if query.is_empty() {
            search.results = Slot::default();
            return;
        }
        search.results.loading = true;
        fx.push(Effect::Library(LibraryRequest::Search(query)));
    }

    fn edit_query(&mut self, now_ms: u64, change: impl FnOnce(&mut String)) {
        if self.state.screen == Screen::Books {
            // The Books screen filters the loaded library on the device.
            change(&mut self.state.books.query);
            return;
        }
        if self.state.screen != Screen::Search {
            return;
        }
        let search = &mut self.state.search;
        change(&mut search.query);
        if let Some((cut, _)) = search.query.char_indices().nth(MAX_QUERY_CHARS) {
            search.query.truncate(cut);
        }
        search.edited_ms = now_ms;
    }

    // ---- Playlist editing ----

    fn playlists_mut(&mut self) -> &mut Vec<Collection> {
        let slot = self.state.sections.entry(Section::Playlists).or_default();
        Arc::make_mut(slot.data.get_or_insert_with(Default::default))
    }

    fn playlist(&self, uri: &str) -> Option<Collection> {
        self.state
            .sections
            .get(&Section::Playlists)?
            .data
            .as_ref()?
            .iter()
            .find(|p| p.uri == uri)
            .cloned()
    }

    /// Only playlists the signed-in account owns can be changed.
    fn editable(&self, uri: &str) -> bool {
        let me = self.state.account.as_ref().map(|a| a.id.as_str());
        self.playlist(uri)
            .is_some_and(|p| me.is_some() && p.owner_id.as_deref() == me)
    }

    fn loaded_tracks(&self, uri: &str) -> Option<&Vec<Track>> {
        self.state.tracks.get(uri)?.data.as_deref()
    }

    fn tracks_mut(&mut self, uri: &str) -> Option<&mut Vec<Track>> {
        self.state
            .tracks
            .get_mut(uri)?
            .data
            .as_mut()
            .map(Arc::make_mut)
    }

    /// Records what an edit may change, then sends it. Returns the edit's id, or None when
    /// offline (the user is told and nothing changes).
    fn begin_edit(
        &mut self,
        edit: PlaylistEdit,
        tracks: &[&str],
        sections: &[Section],
        now_ms: u64,
        fx: &mut Vec<Effect>,
    ) -> Option<u64> {
        if !self.state.online {
            self.notify(Notice::NoInternet, now_ms);
            return None;
        }
        self.next_edit += 1;
        let id = self.next_edit;
        let undo = Undo {
            tracks: tracks
                .iter()
                .map(|uri| (uri.to_string(), self.state.tracks.get(*uri).cloned()))
                .collect(),
            sections: sections
                .iter()
                .map(|s| (*s, self.state.sections.get(s).cloned()))
                .collect(),
            placeholder: None,
            liked: None,
        };
        self.undo.insert(id, undo);
        fx.push(Effect::Library(LibraryRequest::Edit { id, edit }));
        Some(id)
    }

    /// After an edit settles, reload what it touched so the cache matches Spotify.
    fn reload(&mut self, undo: &Undo, fx: &mut Vec<Effect>) {
        for (uri, _) in &undo.tracks {
            if !uri.starts_with("ziggy:") {
                self.state.tracks.entry(uri.clone()).or_default().loading = true;
                fx.push(Effect::Library(LibraryRequest::ReloadTracks {
                    collection_uri: uri.clone(),
                }));
            }
        }
        for (section, _) in &undo.sections {
            if matches!(
                section,
                Section::Playlists | Section::Albums | Section::Artists
            ) {
                self.state.sections.entry(*section).or_default().loading = true;
                fx.push(Effect::Library(LibraryRequest::Reload(*section)));
            }
        }
    }

    fn edit_done(&mut self, id: u64, outcome: EditOutcome, fx: &mut Vec<Effect>) {
        let Some(mut undo) = self.undo.remove(&id) else {
            return;
        };
        if let (EditOutcome::Created(created), Some(placeholder)) = (outcome, &undo.placeholder) {
            for p in self
                .playlists_mut()
                .iter_mut()
                .filter(|p| p.uri == *placeholder)
            {
                *p = created.clone();
            }
            if let Some(slot) = self.state.tracks.remove(placeholder) {
                self.state.tracks.insert(created.uri.clone(), slot);
            }
            undo.tracks.push((created.uri.clone(), None));
        }
        self.reload(&undo, fx);
    }

    fn edit_failed(&mut self, id: u64, reason: FailReason, now_ms: u64, fx: &mut Vec<Effect>) {
        let Some(undo) = self.undo.remove(&id) else {
            return;
        };
        for (uri, slot) in &undo.tracks {
            match slot {
                Some(slot) => self.state.tracks.insert(uri.clone(), slot.clone()),
                None => self.state.tracks.remove(uri),
            };
        }
        for (section, slot) in &undo.sections {
            match slot {
                Some(slot) => self.state.sections.insert(*section, slot.clone()),
                None => self.state.sections.remove(section),
            };
        }
        if let Some((uri, before)) = &undo.liked {
            match before {
                Some(liked) => self.state.liked.insert(uri.clone(), *liked),
                None => self.state.liked.remove(uri),
            };
        }
        match reason {
            FailReason::Offline => {
                self.state.online = false;
                self.notify(Notice::NoInternet, now_ms);
            }
            FailReason::RateLimited => self.notify(Notice::SpotifyBusy, now_ms),
            _ => self.notify(Notice::CouldntSave, now_ms),
        }
        self.reload(&undo, fx);
    }

    fn add_to_playlist(&mut self, playlist_uri: String, now_ms: u64, fx: &mut Vec<Effect>) {
        let Some(track_uri) = self.state.picker.take() else {
            return;
        };
        let Some(playlist) = self.playlist(&playlist_uri) else {
            return;
        };
        if self
            .loaded_tracks(&playlist_uri)
            .is_some_and(|tracks| tracks.iter().any(|t| t.uri == track_uri))
        {
            self.notify(Notice::AlreadyIn(playlist.name), now_ms);
            return;
        }
        let track = self.find_track(&track_uri);
        let edit = PlaylistEdit::Add {
            playlist_uri: playlist_uri.clone(),
            track_uri,
        };
        if self
            .begin_edit(edit, &[&playlist_uri], &[], now_ms, fx)
            .is_none()
        {
            return;
        }
        if let (Some(list), Some(track)) = (self.tracks_mut(&playlist_uri), track) {
            list.push(track);
        }
        self.notify(Notice::AddedTo(playlist.name), now_ms);
    }

    fn submit_text(&mut self, now_ms: u64, fx: &mut Vec<Effect>) {
        let Some(entry) = self.state.text_entry.as_ref() else {
            return;
        };
        let name = entry.text.trim().to_string();
        if name.is_empty() {
            return;
        }
        let Some(entry) = self.state.text_entry.take() else {
            return;
        };
        match entry.purpose {
            TextPurpose::NewPlaylist { track_uri } => {
                let track = track_uri.as_deref().and_then(|uri| self.find_track(uri));
                let empty = track_uri.is_none();
                let edit = PlaylistEdit::Create {
                    name: name.clone(),
                    track_uri,
                };
                let Some(id) = self.begin_edit(edit, &[], &[Section::Playlists], now_ms, fx) else {
                    return;
                };
                let placeholder = format!("ziggy:new:{id}");
                let account = self.state.account.clone();
                self.playlists_mut().insert(
                    0,
                    Collection {
                        uri: placeholder.clone(),
                        name: name.clone(),
                        subtitle: account.as_ref().map(|a| a.name.clone()).unwrap_or_default(),
                        owner_id: account.map(|a| a.id),
                        ..Default::default()
                    },
                );
                self.state.tracks.insert(
                    placeholder.clone(),
                    Slot {
                        data: Some(Arc::new(track.into_iter().collect())),
                        ..Default::default()
                    },
                );
                if let Some(undo) = self.undo.get_mut(&id) {
                    undo.placeholder = Some(placeholder.clone());
                    undo.tracks.push((placeholder, None));
                }
                if empty {
                    self.notify(Notice::Created(name), now_ms);
                } else {
                    self.notify(Notice::AddedTo(name), now_ms);
                }
            }
            TextPurpose::BooksServer => {
                self.state.books.url = crate::audiobooks::client::normalize_url(&name);
                self.save_books_settings(fx);
            }
            TextPurpose::BooksUsername => {
                // Then the password, on its own screen.
                self.state.text_entry = Some(TextEntry {
                    purpose: TextPurpose::BooksPassword { username: name },
                    text: String::new(),
                });
            }
            TextPurpose::BooksPassword { username } => {
                // Passwords keep their spaces; `name` was trimmed.
                let password = entry.text.clone();
                self.state.books.signing_in = true;
                self.state.books.message = None;
                fx.push(Effect::Books(BooksRequest::SignIn {
                    url: self.state.books.url.clone(),
                    username,
                    password,
                }));
            }
            TextPurpose::DeviceName => {
                let name: String = name.chars().take(MAX_DEVICE_NAME_CHARS).collect();
                if name != self.state.device.device_name {
                    self.apply_settings(|s| s.device_name = Some(name), fx);
                }
            }
            TextPurpose::Rename { playlist_uri } => {
                let edit = PlaylistEdit::Rename {
                    playlist_uri: playlist_uri.clone(),
                    name: name.clone(),
                };
                let sections = [Section::Playlists, Section::Recent];
                if self.begin_edit(edit, &[], &sections, now_ms, fx).is_none() {
                    return;
                }
                for section in sections {
                    if let Some(data) = self
                        .state
                        .sections
                        .get_mut(&section)
                        .and_then(|slot| slot.data.as_mut())
                    {
                        for p in Arc::make_mut(data)
                            .iter_mut()
                            .filter(|p| p.uri == playlist_uri)
                        {
                            p.name = name.clone();
                        }
                    }
                }
            }
        }
    }

    fn remove_track(&mut self, index: usize, now_ms: u64, fx: &mut Vec<Effect>) {
        let Some(uri) = self.state.editing.clone() else {
            return;
        };
        let Some(track) = self.loaded_tracks(&uri).and_then(|t| t.get(index)).cloned() else {
            return;
        };
        let edit = PlaylistEdit::Remove {
            playlist_uri: uri.clone(),
            track_uri: track.uri.clone(),
            snapshot_id: self.playlist(&uri).and_then(|p| p.snapshot_id),
        };
        if self.begin_edit(edit, &[&uri], &[], now_ms, fx).is_none() {
            return;
        }
        if let Some(list) = self.tracks_mut(&uri) {
            list.retain(|t| t.uri != track.uri);
        }
    }

    fn move_track(&mut self, from: usize, to: usize, now_ms: u64, fx: &mut Vec<Effect>) {
        let Some(uri) = self.state.editing.clone() else {
            return;
        };
        let len = self.loaded_tracks(&uri).map_or(0, Vec::len);
        if from == to || from >= len || to >= len {
            return;
        }
        let edit = PlaylistEdit::Move {
            playlist_uri: uri.clone(),
            from,
            to,
            snapshot_id: self.playlist(&uri).and_then(|p| p.snapshot_id),
        };
        if self.begin_edit(edit, &[&uri], &[], now_ms, fx).is_none() {
            return;
        }
        if let Some(list) = self.tracks_mut(&uri) {
            let track = list.remove(from);
            list.insert(to, track);
        }
    }

    // ---- Audiobooks ----

    fn load_books(&mut self, fx: &mut Vec<Effect>) {
        let books = &mut self.state.books;
        if books.enabled && books.username.is_some() {
            books.library.loading = true;
            fx.push(Effect::Books(BooksRequest::Load));
        }
    }

    fn save_books_settings(&mut self, fx: &mut Vec<Effect>) {
        self.state.device.saved.audiobooks = Some(crate::settings::AudiobooksSettings {
            enabled: self.state.books.enabled,
            url: self.state.books.url.clone(),
        });
        fx.push(Effect::SaveSettings(self.state.device.saved.clone()));
    }

    fn on_books(&mut self, update: BooksUpdate, fx: &mut Vec<Effect>) {
        let books = &mut self.state.books;
        match update {
            BooksUpdate::Library(library) => {
                books.library.data = Some(Arc::new(library));
                books.library.loading = false;
                books.library.failed = false;
            }
            BooksUpdate::LibraryFailed(message) => {
                books.library.loading = false;
                books.library.failed = books.library.data.is_none();
                books.message = Some(message);
            }
            BooksUpdate::Detail {
                id,
                detail,
                progress,
            } => {
                let slot = books.details.entry(id).or_default();
                slot.data = Some(Arc::new((detail, progress)));
                slot.loading = false;
                slot.failed = false;
            }
            BooksUpdate::DetailFailed { id, message } => {
                let slot = books.details.entry(id).or_default();
                slot.loading = false;
                slot.failed = slot.data.is_none();
                books.message = Some(message);
            }
            BooksUpdate::SignedIn { username } => {
                books.username = Some(username);
                books.signing_in = false;
                books.message = None;
                self.load_books(fx);
            }
            BooksUpdate::SignInFailed(message) => {
                books.signing_in = false;
                books.message = Some(message);
            }
            BooksUpdate::SignedOut => {
                books.username = None;
                books.library = Slot::default();
                books.details.clear();
            }
        }
    }

    // ---- Settings ----

    fn playing_on_sonos(&self) -> bool {
        matches!(self.state.device.saved.output, Some(Output::Sonos(_)))
    }

    /// Saves a change to the settings; the player then restarts to apply it.
    fn apply_settings(&mut self, change: impl FnOnce(&mut Settings), fx: &mut Vec<Effect>) {
        let mut settings = self.state.device.saved.clone();
        change(&mut settings);
        self.state.device.saved = settings.clone();
        self.state.device.restarting = true;
        fx.push(Effect::ApplySettings(settings));
    }

    fn on_bluetooth(&mut self, update: BtUpdate, fx: &mut Vec<Effect>) {
        let device = &mut self.state.device;
        match update {
            BtUpdate::Found(speaker) => {
                match device
                    .found
                    .iter_mut()
                    .find(|s| s.address == speaker.address)
                {
                    Some(existing) => *existing = speaker,
                    None => device.found.push(speaker),
                }
            }
            BtUpdate::ScanFinished => device.scanning = false,
            BtUpdate::Connected(address) => {
                device.connecting = None;
                let name = device
                    .found
                    .iter()
                    .find(|s| s.address == address)
                    .map(|s| s.name.clone())
                    .unwrap_or_else(|| address.clone());
                let speaker = Speaker { address, name };
                self.apply_settings(|s| s.output = Some(Output::Bluetooth(speaker)), fx);
            }
            BtUpdate::ConnectFailed(address) => {
                device.connecting = None;
                device.failed = Some(
                    device
                        .found
                        .iter()
                        .find(|s| s.address == address)
                        .map(|s| s.name.clone())
                        .unwrap_or(address),
                );
            }
            BtUpdate::Forgotten(address) => {
                let current = device
                    .speaker
                    .as_ref()
                    .is_some_and(|s| s.address == address);
                device.found.retain(|s| s.address != address);
                if current {
                    self.apply_settings(|s| s.output = Some(Output::Jack), fx);
                }
            }
            BtUpdate::Unavailable => {
                device.bluetooth = false;
                device.scanning = false;
                device.connecting = None;
            }
        }
    }

    fn toggle_like(&mut self, now_ms: u64, fx: &mut Vec<Effect>) {
        let Some(track) = self.state.playback.track.clone() else {
            return;
        };
        let before = self.state.liked.get(&track.uri).copied();
        let liked = !before.unwrap_or(false);
        let edit = if liked {
            PlaylistEdit::Like {
                track_uri: track.uri.clone(),
            }
        } else {
            PlaylistEdit::Unlike {
                track_uri: track.uri.clone(),
            }
        };
        let Some(id) = self.begin_edit(edit, &[LIKED_URI], &[], now_ms, fx) else {
            return;
        };
        if let Some(undo) = self.undo.get_mut(&id) {
            undo.liked = Some((track.uri.clone(), before));
        }
        self.state.liked.insert(track.uri.clone(), liked);
        if let Some(list) = self.tracks_mut(LIKED_URI) {
            if liked {
                list.insert(0, track);
            } else {
                list.retain(|t| t.uri != track.uri);
            }
        }
    }

    /// Whether liked, saved or followed can be answered without asking Spotify: from an
    /// earlier answer, or from the cached list it would appear in.
    fn knows_saved(&self, uri: &str) -> bool {
        if self.state.liked.contains_key(uri) {
            return true;
        }
        if uri.starts_with("spotify:track:") {
            return self.loaded_tracks(LIKED_URI).is_some();
        }
        let section = if uri.starts_with("spotify:album:") {
            Section::Albums
        } else if uri.starts_with("spotify:artist:") {
            Section::Artists
        } else {
            return false;
        };
        self.state
            .sections
            .get(&section)
            .is_some_and(|slot| slot.data.is_some())
    }

    /// Whether a song is in Liked Songs, from what Spotify said or the loaded list.
    fn is_liked(&self, uri: &str) -> bool {
        self.state.liked.get(uri).copied().unwrap_or_else(|| {
            self.loaded_tracks(LIKED_URI)
                .is_some_and(|tracks| tracks.iter().any(|t| t.uri == uri))
        })
    }

    fn like_picked(&mut self, now_ms: u64, fx: &mut Vec<Effect>) {
        let Some(uri) = self.state.picker.take() else {
            return;
        };
        let liked = Section::Liked.title().to_string();
        if self.is_liked(&uri) {
            self.notify(Notice::AlreadyIn(liked), now_ms);
            return;
        }
        let before = self.state.liked.get(&uri).copied();
        let edit = PlaylistEdit::Like {
            track_uri: uri.clone(),
        };
        let Some(id) = self.begin_edit(edit, &[LIKED_URI], &[], now_ms, fx) else {
            return;
        };
        if let Some(undo) = self.undo.get_mut(&id) {
            undo.liked = Some((uri.clone(), before));
        }
        self.state.liked.insert(uri.clone(), true);
        if let Some(track) = self.find_track(&uri)
            && let Some(list) = self.tracks_mut(LIKED_URI)
        {
            list.insert(0, track);
        }
        self.notify(Notice::AddedTo(liked), now_ms);
    }

    /// Whether an album is saved or an artist followed, from what Spotify said or the
    /// section that lists them.
    fn in_library(&self, uri: &str, section: Section) -> bool {
        self.state.liked.get(uri).copied().unwrap_or_else(|| {
            self.state
                .sections
                .get(&section)
                .and_then(|slot| slot.data.as_ref())
                .is_some_and(|items| items.iter().any(|a| a.uri == uri))
        })
    }

    /// Saves or removes an album (`Section::Albums`), or follows or unfollows an artist
    /// (`Section::Artists`), updating that section at once.
    fn toggle_in_library(
        &mut self,
        uri: String,
        section: Section,
        now_ms: u64,
        fx: &mut Vec<Effect>,
    ) {
        let saved = !self.in_library(&uri, section);
        let before = self.state.liked.get(&uri).copied();
        let edit = match (section, saved) {
            (Section::Artists, true) => PlaylistEdit::Follow {
                artist_uri: uri.clone(),
            },
            (Section::Artists, false) => PlaylistEdit::Unfollow {
                artist_uri: uri.clone(),
            },
            (_, true) => PlaylistEdit::SaveAlbum {
                album_uri: uri.clone(),
            },
            (_, false) => PlaylistEdit::UnsaveAlbum {
                album_uri: uri.clone(),
            },
        };
        let Some(id) = self.begin_edit(edit, &[], &[section], now_ms, fx) else {
            return;
        };
        if let Some(undo) = self.undo.get_mut(&id) {
            undo.liked = Some((uri.clone(), before));
        }
        self.state.liked.insert(uri.clone(), saved);
        let album = self.find_collection(&uri);
        let slot = self.state.sections.entry(section).or_default();
        let albums = Arc::make_mut(slot.data.get_or_insert_with(Default::default));
        if saved {
            if let Some(album) = album.filter(|_| albums.iter().all(|a| a.uri != uri)) {
                albums.insert(0, album);
            }
        } else {
            albums.retain(|a| a.uri != uri);
        }
        let name = self
            .find_collection(&uri)
            .map(|a| a.name)
            .unwrap_or_default();
        if saved {
            self.notify(Notice::AddedTo(Section::Albums.title().to_string()), now_ms);
        } else if !name.is_empty() {
            self.notify(
                Notice::RemovedFrom(Section::Albums.title().to_string()),
                now_ms,
            );
        }
    }

    /// An album or playlist known from any section, search results or artist page.
    fn find_collection(&self, uri: &str) -> Option<Collection> {
        let from_sections = self
            .state
            .sections
            .values()
            .chain(self.state.artist_albums.values())
            .filter_map(|slot| slot.data.as_ref())
            .flat_map(|items| items.iter())
            .chain(self.state.artists_seen.values());
        let from_search = self.state.search.results.data.iter().flat_map(|found| {
            found
                .albums
                .iter()
                .chain(&found.playlists)
                .chain(&found.artists)
        });
        from_sections
            .chain(from_search)
            .find(|c| c.uri == uri)
            .cloned()
    }

    fn delete_playlist(&mut self, now_ms: u64, fx: &mut Vec<Effect>) {
        let Some(uri) = self.state.confirm_delete.take() else {
            return;
        };
        let edit = PlaylistEdit::Delete {
            playlist_uri: uri.clone(),
        };
        let sections = [Section::Playlists, Section::Recent];
        if self.begin_edit(edit, &[], &sections, now_ms, fx).is_none() {
            return;
        }
        for section in sections {
            if let Some(data) = self
                .state
                .sections
                .get_mut(&section)
                .and_then(|slot| slot.data.as_mut())
            {
                Arc::make_mut(data).retain(|p| p.uri != uri);
            }
        }
        self.state.editing = None;
        self.state.section = Section::Playlists;
        self.state.back_stack.clear();
        self.state.screen = Screen::Grid(Section::Playlists);
    }

    /// Keeps an artist's name so their page has a title even if no list holds them.
    fn remember_artist(&mut self, uri: &str, name: String) {
        if self.find_collection(uri).is_none() {
            self.state.artists_seen.insert(
                uri.to_string(),
                Collection {
                    uri: uri.to_string(),
                    kind: crate::model::CollectionKind::Artist,
                    name,
                    subtitle: "Artist".into(),
                    ..Default::default()
                },
            );
        }
    }

    /// Finds a song by URI in search results or any loaded track list.
    fn find_track(&self, uri: &str) -> Option<Track> {
        let from_search = self
            .state
            .search
            .results
            .data
            .iter()
            .flat_map(|r| r.tracks.iter());
        let from_lists = self
            .state
            .tracks
            .values()
            .filter_map(|slot| slot.data.as_ref())
            .flat_map(|tracks| tracks.iter());
        let playing = self.state.playback.track.iter();
        from_search
            .chain(from_lists)
            .chain(playing)
            .find(|t| t.uri == uri)
            .cloned()
    }

    fn on_ui(&mut self, action: UiAction, now_ms: u64, fx: &mut Vec<Effect>) {
        match action {
            UiAction::ShowSection(Section::Books) => {
                self.state.section = Section::Books;
                self.state.back_stack.clear();
                self.state.screen = Screen::Books;
                self.load_books(fx);
            }
            UiAction::ShowSection(Section::Settings) => {
                self.state.section = Section::Settings;
                self.state.back_stack.clear();
                self.state.screen = Screen::Settings;
            }
            UiAction::ShowSection(Section::Search) => {
                if self.state.section != Section::Search {
                    self.before_search = self.state.section;
                }
                self.state.section = Section::Search;
                self.state.back_stack.clear();
                self.state.screen = Screen::Search;
                self.state.keyboard_open = true;
            }
            UiAction::ShowSection(Section::Liked) => {
                self.state.section = Section::Liked;
                self.state.back_stack.clear();
                self.state.screen = Screen::Detail(LIKED_URI.to_string());
                self.request_tracks(LIKED_URI, fx);
            }
            UiAction::ShowSection(section) => {
                self.state.section = section;
                self.state.back_stack.clear();
                self.state.screen = Screen::Grid(section);
                self.request_section(section, fx);
            }
            // Artists drill down to their albums.
            UiAction::OpenCollection(uri) if uri.starts_with("spotify:artist:") => {
                self.on_ui(UiAction::OpenArtist(uri), now_ms, fx);
            }
            UiAction::OpenCollection(uri) => {
                // Albums show whether they're saved, which Spotify has to be asked.
                if uri.starts_with("spotify:album:") && !self.knows_saved(&uri) {
                    fx.push(Effect::Library(LibraryRequest::IsLiked {
                        track_uri: uri.clone(),
                    }));
                }
                self.navigate(Screen::Detail(uri.clone()));
                self.request_tracks(&uri, fx);
            }
            UiAction::Back => {
                if let Some(previous) = self.state.back_stack.pop() {
                    self.state.screen = previous;
                }
            }
            UiAction::OpenNowPlaying => {
                let pb = &self.state.playback;
                if pb.track.is_some() || pb.context_uri.is_some() {
                    self.navigate(Screen::NowPlaying);
                }
            }
            UiAction::PlayCollection { uri, shuffle } => {
                let start = if shuffle {
                    Start::Shuffled
                } else {
                    Start::Index(0)
                };
                self.start_playback(uri, start, shuffle, now_ms, fx);
            }
            UiAction::PlayTrack {
                collection_uri,
                index,
            } => {
                let shuffle = self.state.playback.shuffle;
                self.start_playback(
                    collection_uri,
                    Start::Index(index as u32),
                    shuffle,
                    now_ms,
                    fx,
                );
            }
            UiAction::TogglePlay => {
                let pb = &mut self.state.playback;
                let has_something = pb.track.is_some() || pb.context_uri.is_some();
                match pb.status {
                    PlayStatus::Playing | PlayStatus::Loading => {
                        pb.status = PlayStatus::Paused;
                        fx.push(Effect::Player(PlayerCommand::Pause));
                    }
                    PlayStatus::Paused | PlayStatus::Stopped if has_something => {
                        pb.status = PlayStatus::Playing;
                        fx.push(Effect::Player(PlayerCommand::Play));
                    }
                    _ => {}
                }
            }
            UiAction::Next => {
                self.state.playback.position_ms = 0;
                fx.push(Effect::Player(PlayerCommand::Next));
            }
            UiAction::Previous => {
                self.state.playback.position_ms = 0;
                fx.push(Effect::Player(PlayerCommand::Previous));
            }
            UiAction::Seek { position_ms } => {
                let max = self
                    .state
                    .playback
                    .track
                    .as_ref()
                    .map_or(u32::MAX, |t| t.duration_ms);
                let position_ms = position_ms.min(max);
                self.state.playback.position_ms = position_ms;
                fx.push(Effect::Player(PlayerCommand::Seek { position_ms }));
            }
            UiAction::SetVolume { percent } => {
                let percent = percent.min(100);
                self.state.playback.volume = percent;
                fx.push(Effect::Player(PlayerCommand::SetVolume { percent }));
            }
            UiAction::ToggleShuffle => {
                let shuffle = !self.state.playback.shuffle;
                self.state.playback.shuffle = shuffle;
                fx.push(Effect::Player(PlayerCommand::SetShuffle(shuffle)));
            }
            UiAction::CycleRepeat => {
                let repeat = self.state.playback.repeat.next();
                self.state.playback.repeat = repeat;
                fx.push(Effect::Player(PlayerCommand::SetRepeat(repeat)));
            }
            UiAction::Touch => {}
            UiAction::KeyPressed(text) => match &mut self.state.text_entry {
                Some(entry) => {
                    entry.text.push_str(&text);
                    if let Some((cut, _)) = entry.text.char_indices().nth(MAX_NAME_CHARS) {
                        entry.text.truncate(cut);
                    }
                }
                None => self.edit_query(now_ms, |q| q.push_str(&text)),
            },
            UiAction::Backspace => match &mut self.state.text_entry {
                Some(entry) => {
                    entry.text.pop();
                }
                None => self.edit_query(now_ms, |q| {
                    q.pop();
                }),
            },
            UiAction::KeyboardDone => {
                if self.state.text_entry.is_some() {
                    self.submit_text(now_ms, fx);
                } else {
                    self.state.keyboard_open = false;
                }
            }
            UiAction::CancelText => self.state.text_entry = None,
            UiAction::OpenPicker(track_uri) => self.state.picker = Some(track_uri),
            UiAction::ClosePicker => self.state.picker = None,
            UiAction::PickPlaylist(playlist_uri) => self.add_to_playlist(playlist_uri, now_ms, fx),
            UiAction::OpenBook(id) => {
                self.navigate(Screen::Book(id.clone()));
                self.state
                    .books
                    .details
                    .entry(id.clone())
                    .or_default()
                    .loading = true;
                fx.push(Effect::Books(BooksRequest::Detail(id)));
            }
            UiAction::SetBooksEnabled(enabled) => {
                self.state.books.enabled = enabled;
                self.save_books_settings(fx);
                if enabled {
                    self.load_books(fx);
                }
            }
            UiAction::EditBooksServer => {
                self.state.text_entry = Some(TextEntry {
                    purpose: TextPurpose::BooksServer,
                    text: self.state.books.url.clone(),
                });
            }
            UiAction::BooksSignIn => {
                self.state.text_entry = Some(TextEntry {
                    purpose: TextPurpose::BooksUsername,
                    text: self.state.books.username.clone().unwrap_or_default(),
                });
            }
            UiAction::BooksSignOut => fx.push(Effect::Books(BooksRequest::SignOut)),
            UiAction::NewEmptyPlaylist => {
                self.state.text_entry = Some(TextEntry {
                    purpose: TextPurpose::NewPlaylist { track_uri: None },
                    text: String::new(),
                });
            }
            UiAction::NewPlaylist => {
                if let Some(track_uri) = self.state.picker.take() {
                    self.state.text_entry = Some(TextEntry {
                        purpose: TextPurpose::NewPlaylist {
                            track_uri: Some(track_uri),
                        },
                        text: String::new(),
                    });
                }
            }
            UiAction::EditPlaylist(uri) => {
                if self.editable(&uri) {
                    self.state.editing = Some(uri);
                }
            }
            UiAction::FinishEditing => {
                self.state.editing = None;
                self.state.confirm_delete = None;
            }
            UiAction::RemoveTrack(index) => self.remove_track(index, now_ms, fx),
            UiAction::MoveTrack { from, to } => self.move_track(from, to, now_ms, fx),
            UiAction::RenamePlaylist => {
                if let Some(uri) = self.state.editing.clone() {
                    let name = self.playlist(&uri).map(|p| p.name).unwrap_or_default();
                    self.state.text_entry = Some(TextEntry {
                        purpose: TextPurpose::Rename { playlist_uri: uri },
                        text: name,
                    });
                }
            }
            UiAction::AskDelete => self.state.confirm_delete = self.state.editing.clone(),
            UiAction::CancelDelete => self.state.confirm_delete = None,
            UiAction::ConfirmDelete => self.delete_playlist(now_ms, fx),
            UiAction::ToggleLike => self.toggle_like(now_ms, fx),
            UiAction::PickLiked => self.like_picked(now_ms, fx),
            UiAction::ToggleSaveAlbum(uri) => {
                self.toggle_in_library(uri, Section::Albums, now_ms, fx)
            }
            UiAction::ToggleFollow(uri) => {
                self.toggle_in_library(uri, Section::Artists, now_ms, fx)
            }
            UiAction::ToggleListView => self.state.list_view = !self.state.list_view,
            UiAction::Listen => {
                if self.state.voice.enabled && self.state.voice.phase == VoicePhase::Idle {
                    self.state.keyboard_open = false;
                    fx.push(Effect::VoiceListen);
                }
            }
            UiAction::RenameDevice => {
                self.state.text_entry = Some(TextEntry {
                    purpose: TextPurpose::DeviceName,
                    text: self.state.device.device_name.clone(),
                });
            }
            UiAction::FindSpeakers => {
                // Sonos rooms are found over the network on any device.
                if !self.state.device.sonos_scanning && !self.state.device.restarting {
                    self.state.device.sonos_scanning = true;
                    fx.push(Effect::ScanSonos);
                }
                let device = &mut self.state.device;
                if device.bluetooth && !device.scanning && !device.restarting {
                    device.scanning = true;
                    device.found.clear();
                    device.failed = None;
                    fx.push(Effect::Bluetooth(BtCommand::Scan));
                }
            }
            UiAction::ConnectSpeaker(address) => {
                let device = &mut self.state.device;
                if device.connecting.is_none() && !device.restarting {
                    device.connecting = Some(address.clone());
                    device.failed = None;
                    fx.push(Effect::Bluetooth(BtCommand::Connect(address)));
                }
            }
            UiAction::ChooseSonos(uuid) => {
                if let Some(room) = self
                    .state
                    .device
                    .sonos_rooms
                    .iter()
                    .find(|r| r.uuid == uuid)
                    .cloned()
                {
                    self.apply_settings(|s| s.output = Some(Output::Sonos(room)), fx);
                }
            }
            UiAction::UseJack => {
                if self.state.device.speaker.is_some() || self.playing_on_sonos() {
                    self.apply_settings(|s| s.output = Some(Output::Jack), fx);
                }
            }
            UiAction::ForgetSpeaker => {
                if let Some(speaker) = self.state.device.speaker.clone() {
                    fx.push(Effect::Bluetooth(BtCommand::Forget(speaker.address)));
                }
            }
            UiAction::ClearSearch => {
                self.edit_query(now_ms, String::clear);
                // Clearing is deliberate, so there is no reason to wait.
                self.maybe_search(u64::MAX, fx);
            }
            UiAction::OpenKeyboard => {
                self.state.keyboard_open =
                    matches!(self.state.screen, Screen::Search | Screen::Books);
            }
            UiAction::CloseKeyboard => self.state.keyboard_open = false,
            UiAction::SetSearchFilter(filter) => self.state.search.filter = filter,
            UiAction::CloseSearch => {
                let previous = self.before_search;
                self.on_ui(UiAction::ShowSection(previous), now_ms, fx);
            }
            UiAction::OpenArtist(uri) => {
                if !self.knows_saved(&uri) {
                    fx.push(Effect::Library(LibraryRequest::IsLiked {
                        track_uri: uri.clone(),
                    }));
                }
                self.navigate(Screen::Artist(uri.clone()));
                self.state
                    .artist_albums
                    .entry(uri.clone())
                    .or_default()
                    .loading = true;
                fx.push(Effect::Library(LibraryRequest::ArtistAlbums {
                    artist_uri: uri,
                }));
            }
            UiAction::OpenAlbumArtist(album_uri) => {
                let Some(album) = self.find_collection(&album_uri) else {
                    return;
                };
                let Some(artist_uri) = album.artist_uri.clone() else {
                    // Cached before albums recorded their artist: find them by name.
                    let name = album.subtitle.split(", ").next().unwrap_or_default();
                    if !name.is_empty() {
                        fx.push(Effect::Library(LibraryRequest::FindArtist {
                            album_uri,
                            name: name.to_string(),
                        }));
                    }
                    return;
                };
                // Already came from this artist's page: step back rather than stack it again.
                if self.state.back_stack.last() == Some(&Screen::Artist(artist_uri.clone())) {
                    self.on_ui(UiAction::Back, now_ms, fx);
                    return;
                }
                let name = album
                    .subtitle
                    .split(", ")
                    .next()
                    .unwrap_or_default()
                    .to_string();
                self.remember_artist(&artist_uri, name);
                self.on_ui(UiAction::OpenArtist(artist_uri), now_ms, fx);
            }
            UiAction::TapSleepTimer => {
                let minutes = if self.state.sleep_ends_ms.is_some() {
                    None
                } else {
                    Some(
                        self.state
                            .device
                            .saved
                            .sleep_minutes
                            .unwrap_or(DEFAULT_SLEEP_MINUTES),
                    )
                };
                self.on_ui(UiAction::SetSleepTimer(minutes), now_ms, fx);
            }
            UiAction::SetRecentSongs(songs) => self.state.recent_songs = songs,
            UiAction::PlayRecentSong(index) => {
                let Some(play) = self
                    .state
                    .history
                    .as_ref()
                    .and_then(|h| h.get(index))
                    .cloned()
                else {
                    return;
                };
                // Play it from where it was heard, starting at that song.
                let context = play
                    .context_uri
                    .clone()
                    .or(play.track.album_uri.clone())
                    .unwrap_or_else(|| play.track.uri.clone());
                let shuffle = self.state.playback.shuffle;
                self.start_playback(context, Start::Track(play.track), shuffle, now_ms, fx);
            }
            UiAction::SetTheme(index) => {
                self.state.device.saved.theme = Some(index.min(MAX_THEME));
                fx.push(Effect::SaveSettings(self.state.device.saved.clone()));
            }
            UiAction::PowerOff => {
                if !self.state.device.powering_off {
                    self.state.device.powering_off = true;
                    fx.push(Effect::Player(PlayerCommand::Pause));
                    fx.push(Effect::PowerOff);
                }
            }
            UiAction::SetSleepLength(minutes) => {
                self.state.device.saved.sleep_minutes = Some(minutes);
                fx.push(Effect::SaveSettings(self.state.device.saved.clone()));
            }
            UiAction::SetSleepTimer(minutes) => {
                self.restore_sleep_volume(fx);
                self.state.sleep_ends_ms = minutes.map(|m| now_ms + u64::from(m) * 60_000);
            }
            UiAction::OpenPlayingArtist => {
                let Some(track) = self.state.playback.track.clone() else {
                    return;
                };
                let Some(uri) = track.artist_uri.clone() else {
                    return;
                };
                let name = track
                    .artists
                    .split(", ")
                    .next()
                    .unwrap_or_default()
                    .to_string();
                self.remember_artist(&uri, name);
                self.on_ui(UiAction::OpenArtist(uri), now_ms, fx);
            }
            UiAction::PlayArtist(uri) => {
                self.start_playback(uri, Start::Shuffled, false, now_ms, fx);
            }
            UiAction::PlaySong(uri) => {
                let Some(track) = self.find_track(&uri) else {
                    return;
                };
                // Play it in its album so music continues afterwards.
                let context = track.album_uri.clone().unwrap_or_else(|| track.uri.clone());
                let shuffle = self.state.playback.shuffle;
                self.start_playback(context, Start::Track(track), shuffle, now_ms, fx);
            }
        }
    }

    fn on_player(&mut self, update: PlayerUpdate, now_ms: u64, fx: &mut Vec<Effect>) {
        match update {
            PlayerUpdate::Connected => {
                self.state.online = true;
                self.refresh_after_reconnect(fx);
                self.resume_pending_load(fx);
            }
            PlayerUpdate::Disconnected => {
                self.state.online = false;
                if self.state.playback.status == PlayStatus::Playing {
                    self.state.playback.status = PlayStatus::Paused;
                }
            }
            PlayerUpdate::TrackChanged(track) => {
                self.log_track(&track, now_ms, fx);
                if !self.knows_saved(&track.uri) {
                    fx.push(Effect::Library(LibraryRequest::IsLiked {
                        track_uri: track.uri.clone(),
                    }));
                }
                self.state.playback.track = Some(track);
                self.state.playback.position_ms = 0;
            }
            PlayerUpdate::Loading => self.state.playback.status = PlayStatus::Loading,
            PlayerUpdate::Playing { position_ms } => {
                self.count_listening(now_ms);
                self.state.playback.status = PlayStatus::Playing;
                self.state.playback.position_ms = position_ms;
                self.count_listening(now_ms);
                // A new song shouldn't light the screen while falling asleep.
                if self.state.sleep_ends_ms.is_none() {
                    self.wake(now_ms, fx);
                }
            }
            PlayerUpdate::Paused { position_ms } => {
                self.count_listening(now_ms);
                self.state.playback.status = PlayStatus::Paused;
                self.state.playback.position_ms = position_ms;
                self.count_listening(now_ms);
                self.report_listening(fx);
            }
            PlayerUpdate::Stopped => {
                self.count_listening(now_ms);
                self.state.playback.status = PlayStatus::Stopped;
                self.count_listening(now_ms);
                self.report_listening(fx);
            }
            PlayerUpdate::Position { position_ms } => self.state.playback.position_ms = position_ms,
            PlayerUpdate::Volume { percent } => self.state.playback.volume = percent.min(100),
            PlayerUpdate::Shuffle(shuffle) => self.state.playback.shuffle = shuffle,
            PlayerUpdate::Repeat(repeat) => self.state.playback.repeat = repeat,
            PlayerUpdate::Unavailable => {
                self.failed_tracks_ms
                    .retain(|t| now_ms.saturating_sub(*t) < FAILED_TRACKS_WINDOW_MS);
                self.failed_tracks_ms.push(now_ms);
                if self.failed_tracks_ms.len() >= FAILED_TRACKS_LIMIT {
                    self.failed_tracks_ms.clear();
                    fx.push(Effect::Player(PlayerCommand::Pause));
                    self.state.playback.status = PlayStatus::Paused;
                    self.notify(Notice::SkippingStopped, now_ms);
                } else {
                    self.notify(Notice::TrackUnavailable, now_ms);
                }
            }
        }
    }

    fn on_library(&mut self, update: LibraryUpdate, now_ms: u64, fx: &mut Vec<Effect>) {
        match update {
            LibraryUpdate::Section { section, items } => {
                let slot = self.state.sections.entry(section).or_default();
                slot.data = Some(Arc::new(items));
                slot.loading = false;
                slot.failed = false;
                self.state.online = true;
            }
            LibraryUpdate::Tracks {
                collection_uri,
                tracks,
            } => {
                let slot = self.state.tracks.entry(collection_uri).or_default();
                slot.data = Some(Arc::new(tracks));
                slot.loading = false;
                slot.failed = false;
                self.state.online = true;
            }
            LibraryUpdate::SectionFailed { section, reason } => {
                self.state.sections.entry(section).or_default().fail(reason);
                self.on_failure(reason);
            }
            LibraryUpdate::TracksFailed {
                collection_uri,
                reason,
            } => {
                self.state
                    .tracks
                    .entry(collection_uri)
                    .or_default()
                    .fail(reason);
                self.on_failure(reason);
            }
            LibraryUpdate::SearchResults { query, results } => {
                if query == self.state.search.sent.trim() {
                    let slot = &mut self.state.search.results;
                    slot.data = Some(Arc::new(results));
                    slot.loading = false;
                    slot.failed = false;
                    self.state.online = true;
                    self.voice_search_results(&query, now_ms, fx);
                }
            }
            LibraryUpdate::SearchFailed { query, reason } => {
                self.voice_search_failed(&query);
                if query == self.state.search.sent.trim() {
                    let slot = &mut self.state.search.results;
                    slot.data = None;
                    slot.loading = false;
                    slot.failed = true;
                    self.on_failure(reason);
                }
            }
            LibraryUpdate::ArtistAlbums { artist_uri, albums } => {
                let slot = self.state.artist_albums.entry(artist_uri).or_default();
                slot.data = Some(Arc::new(albums));
                slot.loading = false;
                slot.failed = false;
                self.state.online = true;
            }
            LibraryUpdate::EditDone { id, outcome } => self.edit_done(id, outcome, fx),
            LibraryUpdate::Liked { track_uri, liked } => {
                self.state.liked.insert(track_uri, liked);
            }
            LibraryUpdate::ArtistFound { album_uri, artist } => {
                // Remember it on the album so the next tap goes straight there.
                for slot in self.state.sections.values_mut() {
                    if let Some(items) = slot.data.as_mut() {
                        for c in Arc::make_mut(items)
                            .iter_mut()
                            .filter(|c| c.uri == album_uri)
                        {
                            c.artist_uri = Some(artist.uri.clone());
                        }
                    }
                }
                let uri = artist.uri.clone();
                self.remember_artist(&uri, artist.name.clone());
                // Only follow through if the album is still on screen.
                if self.state.screen == Screen::Detail(album_uri) {
                    self.on_ui(UiAction::OpenArtist(uri), now_ms, fx);
                }
            }
            LibraryUpdate::EditFailed { id, reason } => self.edit_failed(id, reason, now_ms, fx),
            LibraryUpdate::ArtistAlbumsFailed { artist_uri, reason } => {
                self.state
                    .artist_albums
                    .entry(artist_uri)
                    .or_default()
                    .fail(reason);
                self.on_failure(reason);
            }
            LibraryUpdate::Account(account) => self.state.account = Some(account),
        }
    }

    fn on_failure(&mut self, reason: FailReason) {
        // A Web API 401 (after the fresh-token retry) is not proof the sign-in is gone; the
        // player reports that with `Input::AuthInvalid`. The slot shows "Can't load this right now".
        match reason {
            FailReason::Offline => self.state.online = false,
            FailReason::Auth
            | FailReason::Forbidden
            | FailReason::RateLimited
            | FailReason::Other => {}
        }
    }

    /// Puts back the volume the sleep timer's fade turned down.
    fn restore_sleep_volume(&mut self, fx: &mut Vec<Effect>) {
        if let Some(volume) = self.sleep_volume.take() {
            self.state.playback.volume = volume;
            fx.push(Effect::Player(PlayerCommand::SetVolume { percent: volume }));
        }
    }

    /// Fades the volume over the timer's last 5 seconds, then pauses and restores it.
    fn on_sleep_tick(&mut self, now_ms: u64, fx: &mut Vec<Effect>) {
        let Some(ends) = self.state.sleep_ends_ms else {
            return;
        };
        let left = ends.saturating_sub(now_ms);
        let pb = &mut self.state.playback;
        if left == 0 {
            if matches!(pb.status, PlayStatus::Playing | PlayStatus::Loading) {
                pb.status = PlayStatus::Paused;
                fx.push(Effect::Player(PlayerCommand::Pause));
            }
            self.state.sleep_ends_ms = None;
            self.restore_sleep_volume(fx);
        } else if left <= SLEEP_FADE_MS && pb.status == PlayStatus::Playing {
            let base = *self.sleep_volume.get_or_insert(pb.volume);
            let volume = (u64::from(base) * left / SLEEP_FADE_MS) as u8;
            if volume != pb.volume {
                pb.volume = volume;
                fx.push(Effect::Player(PlayerCommand::SetVolume { percent: volume }));
            }
        }
    }

    fn on_tick(&mut self, now_ms: u64, fx: &mut Vec<Effect>) {
        if self.state.notice.is_some() && now_ms >= self.notice_until_ms {
            self.state.notice = None;
        }
        self.on_sleep_tick(now_ms, fx);
        self.voice_tick(now_ms, fx);
        self.count_listening(now_ms);
        if self.listened_ms >= self.listened_reported + LISTEN_REPORT_MS {
            self.report_listening(fx);
        }
        // Music keeps the screen awake, except while falling asleep to it.
        if self.state.playback.status == PlayStatus::Playing && self.state.sleep_ends_ms.is_none() {
            self.last_activity_ms = now_ms;
        }
        let idle = now_ms.saturating_sub(self.last_activity_ms);
        let target = if self.state.sleep_ends_ms.is_some() {
            // Falling asleep: dim a moment after the last touch, dark after a minute.
            if idle >= SLEEP_SCREEN_OFF_MS {
                DisplayMode::Off
            } else if idle >= SLEEP_SCREEN_DIM_MS {
                DisplayMode::Dim
            } else {
                DisplayMode::Active
            }
        } else if idle >= self.cfg.off_after_ms {
            DisplayMode::Off
        } else if idle >= self.cfg.dim_after_ms {
            DisplayMode::Dim
        } else {
            DisplayMode::Active
        };
        if target != self.state.display {
            self.state.display = target;
            fx.push(Effect::Display(target));
        }
    }

    fn wake(&mut self, now_ms: u64, fx: &mut Vec<Effect>) {
        self.last_activity_ms = now_ms;
        if self.state.display != DisplayMode::Active {
            self.state.display = DisplayMode::Active;
            fx.push(Effect::Display(DisplayMode::Active));
        }
    }

    fn navigate(&mut self, screen: Screen) {
        if self.state.screen != screen {
            let previous = std::mem::replace(&mut self.state.screen, screen);
            self.state.back_stack.push(previous);
        }
    }

    fn request_section(&mut self, section: Section, fx: &mut Vec<Effect>) {
        self.state.sections.entry(section).or_default().loading = true;
        if section == Section::Recent {
            // Recent comes from this device's own play history, not Spotify.
            fx.push(Effect::History(HistoryCommand::LoadRecent));
        } else {
            fx.push(Effect::Library(LibraryRequest::Section(section)));
        }
    }

    // ---- Play history ----

    /// Adds up how long the current song has actually played.
    fn count_listening(&mut self, now_ms: u64) {
        if let Some(mark) = self.listen_mark {
            self.listened_ms += now_ms.saturating_sub(mark);
        }
        self.listen_mark = (self.state.playback.status == PlayStatus::Playing).then_some(now_ms);
    }

    fn report_listening(&mut self, fx: &mut Vec<Effect>) {
        if self.logged_uri.is_some() && self.listened_ms != self.listened_reported {
            self.listened_reported = self.listened_ms;
            fx.push(Effect::History(HistoryCommand::Listened(self.listened_ms)));
        }
    }

    /// A new song started: finish the last one's listening time and log the new one.
    fn log_track(&mut self, track: &Track, now_ms: u64, fx: &mut Vec<Effect>) {
        self.count_listening(now_ms);
        // The player can announce the same song twice as it loads; that's one play.
        if self.logged_uri.as_deref() == Some(track.uri.as_str()) && self.listened_ms < 5_000 {
            return;
        }
        self.report_listening(fx);
        let context_uri = self.state.playback.context_uri.clone();
        let context = context_uri.as_deref().and_then(|uri| {
            if uri == LIKED_URI {
                Some(liked_collection())
            } else {
                self.find_collection(uri)
            }
        });
        fx.push(Effect::History(HistoryCommand::Start(Box::new(
            PlayRecord {
                track: track.clone(),
                context_uri,
                context_name: context.as_ref().map(|c| c.name.clone()).unwrap_or_default(),
                context_image: context.and_then(|c| c.image_url),
                played_at: 0,
                listened_ms: 0,
            },
        ))));
        self.logged_uri = Some(track.uri.clone());
        self.listened_ms = 0;
        self.listened_reported = 0;
    }

    /// Albums and playlists recently played on this device, newest first.
    fn recent_collections(plays: &[PlayRecord]) -> Vec<Collection> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for play in plays {
            let collection = match play.context_uri.as_deref() {
                Some(LIKED_URI) => liked_collection(),
                Some(uri)
                    if !uri.starts_with("spotify:track:") && !play.context_name.is_empty() =>
                {
                    let album = uri.starts_with("spotify:album:");
                    Collection {
                        uri: uri.to_string(),
                        kind: if album {
                            CollectionKind::Album
                        } else if uri.starts_with("spotify:artist:") {
                            CollectionKind::Artist
                        } else {
                            CollectionKind::Playlist
                        },
                        name: play.context_name.clone(),
                        subtitle: if album {
                            play.track.artists.clone()
                        } else {
                            String::new()
                        },
                        image_url: play.context_image.clone().or(play.track.image_url.clone()),
                        artist_uri: if album {
                            play.track.artist_uri.clone()
                        } else {
                            None
                        },
                        ..Default::default()
                    }
                }
                // Played on its own, or from somewhere we don't know: its album.
                _ => match &play.track.album_uri {
                    Some(album_uri) => Collection {
                        uri: album_uri.clone(),
                        kind: CollectionKind::Album,
                        name: play.track.album.clone(),
                        subtitle: play.track.artists.clone(),
                        image_url: play.track.image_url.clone(),
                        artist_uri: play.track.artist_uri.clone(),
                        ..Default::default()
                    },
                    None => continue,
                },
            };
            if seen.insert(collection.uri.clone()) {
                out.push(collection);
                if out.len() >= RECENT_COLLECTIONS {
                    break;
                }
            }
        }
        out
    }

    fn request_tracks(&mut self, uri: &str, fx: &mut Vec<Effect>) {
        self.state
            .tracks
            .entry(uri.to_string())
            .or_default()
            .loading = true;
        fx.push(Effect::Library(LibraryRequest::Tracks {
            collection_uri: uri.to_string(),
        }));
    }

    fn start_playback(
        &mut self,
        uri: String,
        start: Start,
        shuffle: bool,
        now_ms: u64,
        fx: &mut Vec<Effect>,
    ) {
        let key = (uri.clone(), start.clone(), shuffle);
        if let Some((last_key, at)) = &self.last_load {
            if *last_key == key && now_ms.saturating_sub(*at) < DOUBLE_TAP_MS {
                return;
            }
        }
        self.last_load = Some((key, now_ms));
        if !self.state.online {
            self.notify(Notice::NoInternet, now_ms);
        }
        let (start_index, start_uri, first) = match start {
            Start::Shuffled => (None, None, None),
            Start::Index(index) => (
                Some(index),
                None,
                self.state
                    .tracks
                    .get(&uri)
                    .and_then(|slot| slot.data.as_ref())
                    .and_then(|tracks| tracks.get(index as usize))
                    .cloned(),
            ),
            Start::Track(track) => (None, Some(track.uri.clone()), Some(track)),
        };
        let pb = &mut self.state.playback;
        pb.context_uri = Some(uri.clone());
        pb.track = first;
        pb.status = PlayStatus::Loading;
        pb.position_ms = 0;
        pb.shuffle = shuffle;
        fx.push(Effect::Player(PlayerCommand::Load {
            context_uri: uri,
            start_index,
            start_uri,
            shuffle,
        }));
        self.navigate(Screen::NowPlaying);
    }

    fn notify(&mut self, notice: Notice, now_ms: u64) {
        self.state.notice = Some(notice);
        self.notice_until_ms = now_ms + NOTICE_MS;
    }

    /// The player drops commands queued while it was disconnected, so a play tapped offline is
    /// sent again once it connects.
    fn resume_pending_load(&self, fx: &mut Vec<Effect>) {
        let pb = &self.state.playback;
        if pb.status != PlayStatus::Loading {
            return;
        }
        if let Some(uri) = &pb.context_uri {
            fx.push(Effect::Player(PlayerCommand::Load {
                context_uri: uri.clone(),
                start_index: None,
                start_uri: pb.track.as_ref().map(|t| t.uri.clone()),
                shuffle: pb.shuffle,
            }));
        }
    }

    fn refresh_after_reconnect(&mut self, fx: &mut Vec<Effect>) {
        for section in [Section::Playlists, Section::Albums, Section::Recent] {
            self.request_section(section, fx);
        }
        let open_detail = std::iter::once(&self.state.screen)
            .chain(self.state.back_stack.iter().rev())
            .find_map(|screen| match screen {
                Screen::Detail(uri) => Some(uri.clone()),
                _ => None,
            });
        if let Some(uri) = open_detail {
            self.request_tracks(&uri, fx);
        }
        if self.state.account.is_none() {
            fx.push(Effect::Library(LibraryRequest::Account));
        }
    }
}
