use serde::{Deserialize, Serialize};

/// Pseudo-collection URI for the account's Liked Songs. The player maps it to
/// `spotify:user:<username>:collection`, the library to `/me/tracks`.
pub const LIKED_URI: &str = "ziggy:liked";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Section {
    Playlists,
    Albums,
    /// Artists the user follows.
    Artists,
    Liked,
    Recent,
    Search,
    Settings,
    /// Audiobooks, when an Audiobookshelf server is set up.
    Books,
}

impl Section {
    pub fn index(self) -> i32 {
        match self {
            Section::Playlists => 0,
            Section::Albums => 1,
            Section::Artists => 2,
            Section::Liked => 3,
            Section::Search => 4,
            Section::Recent => 5,
            Section::Settings => 6,
            Section::Books => 7,
        }
    }

    pub fn from_index(index: i32) -> Section {
        match index {
            1 => Section::Albums,
            2 => Section::Artists,
            3 => Section::Liked,
            4 => Section::Search,
            5 => Section::Recent,
            6 => Section::Settings,
            7 => Section::Books,
            _ => Section::Playlists,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Section::Playlists => "Playlists",
            Section::Albums => "Albums",
            Section::Artists => "Artists",
            Section::Liked => "Liked Songs",
            Section::Recent => "Recent",
            Section::Search => "Search",
            Section::Settings => "Settings",
            Section::Books => "Books",
        }
    }

    pub fn cache_key(self) -> &'static str {
        match self {
            Section::Playlists => "section-playlists",
            Section::Albums => "section-albums",
            Section::Artists => "section-artists",
            Section::Liked => "section-liked",
            Section::Recent => "section-recent",
            Section::Search => "section-search",
            Section::Settings => "section-settings",
            Section::Books => "section-books",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CollectionKind {
    #[default]
    Playlist,
    Album,
    Liked,
    Artist,
}

/// Something you can open and play as a whole: a playlist, an album, or Liked Songs.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Collection {
    pub uri: String,
    pub kind: CollectionKind,
    pub name: String,
    /// Owner for playlists, artists for albums.
    pub subtitle: String,
    pub image_url: Option<String>,
    /// Spotify ID of a playlist's owner. Only owned playlists can be edited.
    #[serde(default)]
    pub owner_id: Option<String>,
    /// A playlist's version, sent with edits so Spotify can detect conflicts.
    #[serde(default)]
    pub snapshot_id: Option<String>,
    /// An album's first artist, so the album page can lead to the artist.
    #[serde(default)]
    pub artist_uri: Option<String>,
    /// The year an album came out.
    #[serde(default)]
    pub year: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Track {
    pub uri: String,
    pub name: String,
    /// Artist names joined with ", ".
    pub artists: String,
    pub album: String,
    pub image_url: Option<String>,
    pub duration_ms: u32,
    /// The album this track is on, used to play a single song in its album.
    #[serde(default)]
    pub album_uri: Option<String>,
    /// The first credited artist, so the song can lead to the artist's page.
    #[serde(default)]
    pub artist_uri: Option<String>,
}

/// A change to one of the user's playlists.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PlaylistEdit {
    Add {
        playlist_uri: String,
        track_uri: String,
    },
    /// Create a private playlist, with one song in it or empty.
    Create {
        name: String,
        /// The song it starts with; None makes an empty playlist.
        track_uri: Option<String>,
    },
    /// Removes every copy of the song; Spotify no longer removes by position.
    Remove {
        playlist_uri: String,
        track_uri: String,
        snapshot_id: Option<String>,
    },
    /// Moves the song at `from` so it ends up at index `to`.
    Move {
        playlist_uri: String,
        from: usize,
        to: usize,
        snapshot_id: Option<String>,
    },
    Rename {
        playlist_uri: String,
        name: String,
    },
    /// Removes the playlist from the user's library.
    Delete {
        playlist_uri: String,
    },
    /// Save a song to Liked Songs.
    Like {
        track_uri: String,
    },
    Unlike {
        track_uri: String,
    },
    /// Save an album to the user's library.
    SaveAlbum {
        album_uri: String,
    },
    UnsaveAlbum {
        album_uri: String,
    },
    Follow {
        artist_uri: String,
    },
    Unfollow {
        artist_uri: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum EditOutcome {
    Done,
    /// The playlist a `Create` made.
    Created(Collection),
}

/// One song played on this device, with what it was played from. Enough to play it
/// again without asking Spotify anything.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayRecord {
    pub track: Track,
    /// The album, playlist or Liked Songs it was played from.
    pub context_uri: Option<String>,
    pub context_name: String,
    pub context_image: Option<String>,
    /// Unix seconds; 0 lets the history store use the current time.
    pub played_at: i64,
    pub listened_ms: u64,
}

/// Catalog search results. Library matches are computed from state by the view.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SearchResults {
    pub tracks: Vec<Track>,
    pub artists: Vec<Collection>,
    pub albums: Vec<Collection>,
    pub playlists: Vec<Collection>,
}

/// The Spotify account the library is read from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    /// Spotify display name; falls back to the ID when the account has none.
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Repeat {
    #[default]
    Off,
    Context,
    Track,
}

impl Repeat {
    pub fn next(self) -> Repeat {
        match self {
            Repeat::Off => Repeat::Context,
            Repeat::Context => Repeat::Track,
            Repeat::Track => Repeat::Off,
        }
    }

    pub fn index(self) -> i32 {
        match self {
            Repeat::Off => 0,
            Repeat::Context => 1,
            Repeat::Track => 2,
        }
    }
}

pub fn liked_collection() -> Collection {
    Collection {
        uri: LIKED_URI.to_string(),
        kind: CollectionKind::Liked,
        name: Section::Liked.title().to_string(),
        subtitle: String::new(),
        image_url: None,
        ..Default::default()
    }
}
