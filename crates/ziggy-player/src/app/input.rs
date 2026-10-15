use crate::app::state::DisplayMode;
use crate::model::{
    Account, Collection, EditOutcome, PlaylistEdit, Repeat, SearchResults, Section, Track,
};

/// Everything the core reacts to.
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Ui(UiAction),
    Player(PlayerUpdate),
    Library(LibraryUpdate),
    Speaker {
        connected: bool,
    },
    AuthInvalid,
    /// Sent once a second by the runtime.
    Tick,
    /// Sent every 150ms while the search screen is open, to send a search soon after typing stops.
    SearchTick,
    Bluetooth(BtUpdate),
    /// Plays on this device for Recent, newest first.
    HistoryRecent(Vec<crate::model::PlayRecord>),
    /// Sonos rooms found on the network.
    SonosRooms(Vec<crate::settings::SonosRoom>),
    Books(BooksUpdate),
    Voice(VoiceUpdate),
    /// The CPU temperature in whole °C; None when this computer doesn't report one.
    Temperature(Option<i32>),
    /// The Wi-Fi connection; None when this computer doesn't report one.
    Wifi(Option<WifiStatus>),
    /// The player isn't signed in to Spotify: it waits to be picked in a Spotify app.
    PairingNeeded(bool),
}

/// The Wi-Fi connection, as Settings shows it.
#[derive(Debug, Clone, PartialEq)]
pub enum WifiStatus {
    Connected {
        network: String,
        /// "2.4 GHz" or "5 GHz".
        band: String,
        /// 0-100, from the live signal level.
        signal: u8,
    },
    Disconnected,
}

/// Work for the audiobooks service.
#[derive(Debug, Clone, PartialEq)]
pub enum BooksRequest {
    /// The library list (cached first, then fresh).
    Load,
    /// One book's page.
    Detail(String),
    SignIn {
        url: String,
        username: String,
        password: String,
    },
    SignOut,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BooksUpdate {
    Library(crate::audiobooks::types::BooksLibrary),
    LibraryFailed(String),
    Detail {
        id: String,
        detail: crate::audiobooks::types::BookDetail,
        progress: Option<crate::audiobooks::types::BookProgress>,
    },
    DetailFailed {
        id: String,
        message: String,
    },
    SignedIn {
        username: String,
    },
    SignInFailed(String),
    SignedOut,
}

/// What the voice service tells the core.
#[derive(Debug, Clone, PartialEq)]
pub enum VoiceUpdate {
    /// The wake word was heard and recording started.
    Woke,
    /// Recording finished; Gemini is working out what was asked.
    Thinking,
    Command(VoiceCommand),
    /// Nothing the player can do was asked, or nothing was heard.
    NotUnderstood,
    /// Asking failed: no internet, no key, or a server problem.
    Failed(String),
}

/// One thing a voice request asks for. Each runs through the same code as a tap.
#[derive(Debug, Clone, PartialEq)]
pub enum VoiceCommand {
    /// A playlist, album or artist from the library list Gemini was given, by URI.
    Play(String),
    /// Search Spotify and play the first result of this kind.
    Search {
        query: String,
        kind: SearchKind,
    },
    Pause,
    Resume,
    Next,
    Previous,
    Louder,
    Quieter,
    SetVolume(u8),
    /// Minutes; 0 turns the sleep timer off.
    SleepTimer(u32),
    /// Heart the playing song: add it to Liked Songs.
    LikeSong,
    /// Save the playing song's album to the library.
    SaveAlbum,
    /// Add the playing song to one of the listener's own playlists, by URI, with the name the
    /// listener said, which must match it.
    AddToPlaylist {
        uri: String,
        heard: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchKind {
    Song,
    Album,
    Artist,
    Playlist,
}

/// What Gemini is told about the player with each request.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct VoiceContext {
    /// "Graceland by Paul Simon", if something is loaded.
    pub now_playing: Option<String>,
    pub playing: bool,
    pub volume: u8,
    /// Playlists, albums and artists in the library.
    pub items: Vec<VoiceItem>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VoiceItem {
    pub uri: String,
    /// "playlist", "album", "artist" or "liked".
    pub kind: &'static str,
    pub name: String,
    /// Owner of a playlist, artists of an album.
    pub by: String,
}

/// Work for the play-history store.
#[derive(Debug, Clone, PartialEq)]
pub enum HistoryCommand {
    /// A song started.
    Start(Box<crate::model::PlayRecord>),
    /// How long the current song has actually played.
    Listened(u64),
    LoadRecent,
}

/// A Bluetooth audio device seen in a scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundSpeaker {
    pub address: String,
    pub name: String,
    pub paired: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BtUpdate {
    Found(FoundSpeaker),
    ScanFinished,
    /// Paired, trusted and connected; the address.
    Connected(String),
    ConnectFailed(String),
    Forgotten(String),
    /// No Bluetooth adapter or service on this device.
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BtCommand {
    Scan,
    Connect(String),
    Forget(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum UiAction {
    /// Shut the Pi down, after the person confirmed it.
    PowerOff,
    ShowSection(Section),
    OpenCollection(String),
    Back,
    OpenNowPlaying,
    PlayCollection {
        uri: String,
        shuffle: bool,
    },
    PlayTrack {
        collection_uri: String,
        index: usize,
    },
    TogglePlay,
    Next,
    Previous,
    Seek {
        position_ms: u32,
    },
    SetVolume {
        percent: u8,
    },
    ToggleShuffle,
    CycleRepeat,
    /// A tap on the dim/off overlay; only wakes the screen.
    Touch,
    /// Text typed on the on-screen or physical keyboard.
    KeyPressed(String),
    Backspace,
    ClearSearch,
    OpenKeyboard,
    CloseKeyboard,
    /// Leave search and go back to the section that was open before.
    CloseSearch,
    SetSearchFilter(crate::app::state::SearchFilter),
    OpenArtist(String),
    /// From Now Playing: open the playing song's artist.
    OpenPlayingArtist,
    /// From an album page: open the album's artist (or go back to them in a drill-down).
    OpenAlbumArtist(String),
    PlayArtist(String),
    /// Play one song picked from search results, by track URI.
    PlaySong(String),
    /// Open the add-to-playlist picker for this track URI.
    OpenPicker(String),
    ClosePicker,
    PickPlaylist(String),
    /// From the picker: name a new playlist for the song.
    NewPlaylist,
    /// From Playlists: name a new, empty playlist.
    NewEmptyPlaylist,
    /// Books: open a book's page.
    OpenBook(String),
    /// Settings: turn Books on or off.
    SetBooksEnabled(bool),
    /// Settings: type the Audiobookshelf server address.
    EditBooksServer,
    /// Settings: sign in to Audiobookshelf (asks for username, then password).
    BooksSignIn,
    BooksSignOut,
    /// The keyboard's Done key: saves a name, or hides the search keyboard.
    KeyboardDone,
    CancelText,
    EditPlaylist(String),
    FinishEditing,
    RemoveTrack(usize),
    MoveTrack {
        from: usize,
        to: usize,
    },
    RenamePlaylist,
    AskDelete,
    ConfirmDelete,
    CancelDelete,
    /// Like or unlike the playing song.
    ToggleLike,
    /// Switch collection screens between tiles and a list.
    ToggleListView,
    /// From the add sheet: put the song in Liked Songs.
    PickLiked,
    /// Save the album to the library, or remove it.
    ToggleSaveAlbum(String),
    /// Follow the artist, or stop following.
    ToggleFollow(String),
    /// The moon: turns a running sleep timer off, otherwise starts it.
    TapSleepTimer,
    /// Settings: how long the sleep timer runs, in minutes.
    SetSleepLength(u32),
    /// Settings: colour scheme by index.
    SetTheme(u32),
    /// Recent: show songs (true) or albums and playlists (false).
    SetRecentSongs(bool),
    /// Recent: play the song at this index of the history.
    PlayRecentSong(usize),
    /// Set the sleep timer to these minutes; None turns it off.
    SetSleepTimer(Option<u32>),
    /// Settings: name this device with the keyboard.
    RenameDevice,
    /// The microphone button: listen for a request without the wake word.
    Listen,
    FindSpeakers,
    ConnectSpeaker(String),
    /// Play through this Sonos room (by uuid) instead of this device.
    ChooseSonos(String),
    UseJack,
    ForgetSpeaker,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlayerUpdate {
    Connected,
    Disconnected,
    TrackChanged(Track),
    Loading,
    Playing {
        position_ms: u32,
    },
    Paused {
        position_ms: u32,
    },
    Stopped,
    Position {
        position_ms: u32,
    },
    Volume {
        percent: u8,
    },
    Shuffle(bool),
    Repeat(Repeat),
    /// A song couldn't play and was skipped; this only drives a notice.
    Unavailable,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LibraryUpdate {
    Section {
        section: Section,
        items: Vec<Collection>,
    },
    Tracks {
        collection_uri: String,
        tracks: Vec<Track>,
    },
    SectionFailed {
        section: Section,
        reason: FailReason,
    },
    TracksFailed {
        collection_uri: String,
        reason: FailReason,
    },
    Account(Account),
    SearchResults {
        query: String,
        results: SearchResults,
    },
    SearchFailed {
        query: String,
        reason: FailReason,
    },
    ArtistAlbums {
        artist_uri: String,
        albums: Vec<Collection>,
    },
    ArtistAlbumsFailed {
        artist_uri: String,
        reason: FailReason,
    },
    EditDone {
        id: u64,
        outcome: EditOutcome,
    },
    EditFailed {
        id: u64,
        reason: FailReason,
    },
    Liked {
        track_uri: String,
        liked: bool,
    },
    ArtistFound {
        album_uri: String,
        artist: Collection,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailReason {
    Offline,
    Auth,
    /// Spotify refuses this to the app; playing it may still work.
    Forbidden,
    /// Spotify asked the app to stop for a while.
    RateLimited,
    Other,
}

/// Work the runtime must carry out after a state change.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Player(PlayerCommand),
    Library(LibraryRequest),
    Display(DisplayMode),
    Bluetooth(BtCommand),
    /// Save the settings and restart the player so they take effect.
    ApplySettings(crate::settings::Settings),
    /// Save the settings; nothing needs a restart.
    SaveSettings(crate::settings::Settings),
    History(HistoryCommand),
    /// Look for Sonos rooms on the network.
    ScanSonos,
    Books(BooksRequest),
    /// What the voice service passes to Gemini with the next request.
    VoiceContext(VoiceContext),
    /// Start listening without the wake word.
    VoiceListen,
    /// Blank the screen and shut the Pi down.
    PowerOff,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlayerCommand {
    Load {
        context_uri: String,
        start_index: Option<u32>,
        /// When set, playback starts at this track instead of `start_index`.
        start_uri: Option<String>,
        shuffle: bool,
    },
    Play,
    Pause,
    Next,
    Previous,
    Seek {
        position_ms: u32,
    },
    SetVolume {
        percent: u8,
    },
    SetShuffle(bool),
    SetRepeat(Repeat),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LibraryRequest {
    Section(Section),
    Tracks {
        collection_uri: String,
    },
    Account,
    Search(String),
    /// A section fetched even if the cache is fresh, after an edit changed it.
    Reload(Section),
    /// A track list fetched even if the cache is fresh, after an edit changed it.
    ReloadTracks {
        collection_uri: String,
    },
    ArtistAlbums {
        artist_uri: String,
    },
    /// A playlist change; `id` matches the reply to the core's undo record.
    Edit {
        id: u64,
        edit: PlaylistEdit,
    },
    IsLiked {
        track_uri: String,
    },
    /// Look up the artist of an album that was cached without one.
    FindArtist {
        album_uri: String,
        name: String,
    },
}
