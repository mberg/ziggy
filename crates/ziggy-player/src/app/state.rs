use std::collections::HashMap;
use std::sync::Arc;

use crate::model::{Account, Collection, Repeat, SearchResults, Section, Track};

#[derive(Debug, Clone, PartialEq)]
pub enum Screen {
    Grid(Section),
    Detail(String),
    NowPlaying,
    Search,
    /// An artist's albums, by artist URI.
    Artist(String),
    Settings,
    Books,
    /// A book's page, by Audiobookshelf item id.
    Book(String),
}

/// Library data plus its loading status. Data stays visible while a refresh runs.
#[derive(Debug, Clone, PartialEq)]
pub struct Slot<T> {
    pub data: Option<Arc<T>>,
    pub loading: bool,
    /// True only when a fetch failed and there is no data to show.
    pub failed: bool,
    /// True when Spotify refused the fetch (HTTP 403) and there is no data to show.
    pub forbidden: bool,
    /// True when Spotify is rate-limiting this request and there is no data to show.
    pub limited: bool,
}

impl<T> Slot<T> {
    /// Records a failed fetch; it only shows when there's nothing cached to show instead.
    pub fn fail(&mut self, reason: crate::app::FailReason) {
        use crate::app::FailReason;
        self.loading = false;
        let empty = self.data.is_none();
        match reason {
            FailReason::Forbidden => self.forbidden = empty,
            FailReason::RateLimited => self.limited = empty,
            _ => self.failed = empty,
        }
    }
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Slot {
            data: None,
            loading: false,
            failed: false,
            forbidden: false,
            limited: false,
        }
    }
}

/// Which kinds of results the search screen shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchFilter {
    #[default]
    All,
    Songs,
    Artists,
    Albums,
    Playlists,
}

impl SearchFilter {
    pub fn index(self) -> i32 {
        match self {
            SearchFilter::All => 0,
            SearchFilter::Songs => 1,
            SearchFilter::Artists => 2,
            SearchFilter::Albums => 3,
            SearchFilter::Playlists => 4,
        }
    }

    pub fn from_index(index: i32) -> SearchFilter {
        match index {
            1 => SearchFilter::Songs,
            2 => SearchFilter::Artists,
            3 => SearchFilter::Albums,
            4 => SearchFilter::Playlists,
            _ => SearchFilter::All,
        }
    }
}

/// The search box and its catalog results.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SearchState {
    pub query: String,
    /// The query last sent to the catalog; results belong to it.
    pub sent: String,
    /// When `query` last changed.
    pub edited_ms: u64,
    pub results: Slot<SearchResults>,
    pub filter: SearchFilter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayStatus {
    Stopped,
    Loading,
    Playing,
    Paused,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Playback {
    pub track: Option<Track>,
    pub context_uri: Option<String>,
    pub status: PlayStatus,
    pub position_ms: u32,
    pub volume: u8,
    pub shuffle: bool,
    pub repeat: Repeat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    NoInternet,
    TrackUnavailable,
    /// Spotify refused a playlist change; the screen has been put back.
    CouldntSave,
    /// Spotify is rate-limiting the app; the change was put back.
    SpotifyBusy,
    /// A song was added to the named playlist.
    AddedTo(String),
    /// The song is already in the named playlist, so nothing was added.
    AlreadyIn(String),
    /// Several songs in a row couldn't play, so playback was paused instead of skipping on.
    SkippingStopped,
    /// Something was taken out of the named list.
    RemovedFrom(String),
    /// An empty playlist with this name was made.
    Created(String),
    /// What a voice request did, or why it didn't.
    Voice(String),
}

/// Where a voice request is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VoicePhase {
    #[default]
    Idle,
    Listening,
    Thinking,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct VoiceState {
    /// Voice control is set up on this device.
    pub enabled: bool,
    pub phase: VoicePhase,
    /// When the phase last changed, so a lost reply can't leave the music quiet.
    pub since_ms: u64,
    /// The volume before the music was lowered to listen.
    pub ducked_from: Option<u8>,
    /// A voice search waiting for results: (query, kind to play).
    pub pending_search: Option<(String, crate::app::SearchKind)>,
}

/// The Settings screen: this device's name and speaker.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DeviceSettings {
    /// What is saved; the Settings screen changes it and the player restarts to apply it.
    pub saved: crate::settings::Settings,
    /// The name Spotify Connect shows now.
    pub device_name: String,
    /// The CPU temperature in whole °C, on a Pi.
    pub temperature_c: Option<i32>,
    /// The Wi-Fi connection, on a Pi.
    pub wifi: Option<crate::app::WifiStatus>,
    /// Turn off was confirmed; the Pi is shutting down.
    pub powering_off: bool,
    /// The Bluetooth speaker in use now; None means the headphone jack.
    pub speaker: Option<crate::settings::Speaker>,
    /// Bluetooth exists on this device (the Pi, or `--fake` mode).
    pub bluetooth: bool,
    pub scanning: bool,
    pub found: Vec<crate::app::FoundSpeaker>,
    /// Address being paired and connected.
    pub connecting: Option<String>,
    /// Name of the speaker that last failed to connect.
    pub failed: Option<String>,
    /// Settings were saved and the player is restarting.
    pub restarting: bool,
    /// Sonos rooms found on the network by the last scan.
    pub sonos_rooms: Vec<crate::settings::SonosRoom>,
    pub sonos_scanning: bool,
}

/// Audiobooks from an Audiobookshelf server.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BooksState {
    pub enabled: bool,
    pub url: String,
    /// Signed in as, if signed in.
    pub username: Option<String>,
    pub library: Slot<crate::audiobooks::types::BooksLibrary>,
    pub details: HashMap<
        String,
        Slot<(
            crate::audiobooks::types::BookDetail,
            Option<crate::audiobooks::types::BookProgress>,
        )>,
    >,
    /// The Books screen's filter.
    pub query: String,
    pub signing_in: bool,
    /// A sign-in or loading problem to show in Settings and on the Books screen.
    pub message: Option<String>,
}

/// What the on-screen text field is for, apart from search.
#[derive(Debug, Clone, PartialEq)]
pub enum TextPurpose {
    /// Name a new playlist, starting with this song or empty.
    NewPlaylist {
        /// None makes an empty playlist.
        track_uri: Option<String>,
    },
    Rename {
        playlist_uri: String,
    },
    /// This device's Spotify Connect name.
    DeviceName,
    /// The Audiobookshelf server address.
    BooksServer,
    BooksUsername,
    /// Shown as dots.
    BooksPassword {
        username: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextEntry {
    pub purpose: TextPurpose,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayMode {
    Active,
    Dim,
    Off,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppState {
    pub screen: Screen,
    /// The rail item that is highlighted.
    pub section: Section,
    pub back_stack: Vec<Screen>,
    pub sections: HashMap<Section, Slot<Vec<Collection>>>,
    /// Track lists keyed by collection URI.
    pub tracks: HashMap<String, Slot<Vec<Track>>>,
    pub playback: Playback,
    pub notice: Option<Notice>,
    pub display: DisplayMode,
    pub online: bool,
    pub auth_needed: bool,
    /// The player waits to be picked in a Spotify app before it can play.
    pub pairing_needed: bool,
    pub speaker_connected: bool,
    /// The account the library comes from, once known.
    pub account: Option<Account>,
    pub search: SearchState,
    pub keyboard_open: bool,
    /// Album lists keyed by artist URI.
    pub artist_albums: HashMap<String, Slot<Vec<Collection>>>,
    /// Track URI being added while the add-to-playlist picker is open.
    pub picker: Option<String>,
    /// The name dialog for a new or renamed playlist.
    pub text_entry: Option<TextEntry>,
    /// Playlist URI whose track list is in edit mode.
    pub editing: Option<String>,
    /// Playlist URI waiting for the user to confirm deletion.
    pub confirm_delete: Option<String>,
    /// Whether songs are in Liked Songs, by track URI, as far as known.
    pub liked: HashMap<String, bool>,
    /// Playlists, albums and artist albums show as a list instead of tiles.
    pub list_view: bool,
    pub device: DeviceSettings,
    pub books: BooksState,
    pub voice: VoiceState,
    /// Artists opened from a song, so their page has a name before it loads elsewhere.
    pub artists_seen: HashMap<String, Collection>,
    /// The core's clock at the last input, so the view can show times left.
    pub now_ms: u64,
    /// Plays on this device, newest first, once loaded.
    pub history: Option<Arc<Vec<crate::model::PlayRecord>>>,
    /// Recent shows songs rather than albums and playlists.
    pub recent_songs: bool,
    /// When the sleep timer pauses playback, if one is set.
    pub sleep_ends_ms: Option<u64>,
}
