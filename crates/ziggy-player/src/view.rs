//! Turns `AppState` into plain display data. No Slint types here, so it is unit-testable.

use std::sync::Arc;

use crate::app::{
    AppState, DisplayMode, Notice, PlayStatus, Screen, SearchFilter, Slot, TextPurpose,
};
use crate::library::matching::matches;
use crate::model::{Collection, LIKED_URI, Repeat, Section, Track, liked_collection};

/// Library matches shown per kind before the catalog results.
const LIBRARY_MATCHES: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenView {
    Grid,
    Detail,
    NowPlaying,
    Search,
    Artist,
    Settings,
    Books,
    Book,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadStatus {
    Loading,
    Empty,
    Failed,
    /// Spotify won't list it for this app, though playing it works.
    Forbidden,
    /// Spotify is rate-limiting it for now.
    Limited,
    Ready,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Header,
    Song,
    Album,
    Artist,
    Playlist,
    Book,
}

/// One line of search results: a group header or a result.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchRowView {
    pub kind: RowKind,
    pub title: String,
    pub subtitle: String,
    pub image_url: Option<String>,
    pub uri: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchView {
    pub query: String,
    pub keyboard: bool,
    pub rows: Vec<SearchRowView>,
    pub status: LoadStatus,
    /// Shown under library matches when the Spotify search could not run.
    pub note: String,
    pub filter: SearchFilter,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArtistView {
    pub header: TileView,
    pub albums: Vec<TileView>,
    pub status: LoadStatus,
    pub followed: bool,
    /// "Artists" when drilled down from the Artists list; empty otherwise.
    pub back_label: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TileView {
    pub uri: String,
    pub title: String,
    pub subtitle: String,
    pub image_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TrackRowView {
    pub uri: String,
    pub title: String,
    pub artists: String,
    pub duration: String,
    pub current: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DetailView {
    pub header: TileView,
    pub status: LoadStatus,
    pub tracks: Vec<TrackRowView>,
    /// The signed-in account owns this playlist.
    pub editable: bool,
    pub editing: bool,
    /// Rows offer "add to playlist": on albums and Liked Songs, not on playlists.
    pub can_add: bool,
    /// For example "23 songs · 1 hr 12 min"; empty until the songs load.
    pub summary: String,
    /// An album, which can be saved to or removed from the library.
    pub saveable: bool,
    pub saved: bool,
    /// The artist name under an album's title opens the artist (Back inside a drill-down).
    pub artist_link: bool,
}

/// The Books screen: Continue listening, then every book, filtered as you type.
#[derive(Debug, Clone, PartialEq)]
pub struct BooksView {
    pub query: String,
    pub keyboard: bool,
    pub rows: Vec<SearchRowView>,
    pub status: LoadStatus,
    /// Why there are no books: not signed in, or a server problem.
    pub message: String,
}

/// A book's page.
#[derive(Debug, Clone, PartialEq)]
pub struct BookView {
    pub id: String,
    pub title: String,
    pub author: String,
    pub narrator: String,
    pub cover_url: Option<String>,
    /// "5 h 12 min · 2 h 3 min left" or "5 h 12 min · Finished".
    pub info: String,
    pub description: String,
    /// (title, start as "1:02:03").
    pub chapters: Vec<(String, String)>,
    pub status: LoadStatus,
}

/// "5 h 12 min", "48 min".
pub fn fmt_duration(secs: f64) -> String {
    let minutes = (secs / 60.0).round() as u64;
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// "1:02:03" or "2:03".
fn fmt_clock(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn book_progress_text(
    duration_secs: f64,
    progress: Option<&crate::audiobooks::types::BookProgress>,
) -> String {
    match progress {
        Some(p) if p.finished => "Finished".into(),
        Some(p) if p.current_secs > 0.0 => {
            format!("{} left", fmt_duration(duration_secs - p.current_secs))
        }
        _ => fmt_duration(duration_secs),
    }
}

fn book_row(
    book: &crate::audiobooks::types::BookSummary,
    progress: Option<&crate::audiobooks::types::BookProgress>,
) -> SearchRowView {
    SearchRowView {
        kind: RowKind::Book,
        title: book.title.clone(),
        subtitle: format!(
            "{} · {}",
            book.author,
            book_progress_text(book.duration_secs, progress)
        ),
        image_url: book
            .has_cover
            .then(|| crate::images::book_cover_url(&book.id)),
        uri: book.id.clone(),
    }
}

fn books_view(state: &AppState) -> BooksView {
    let b = &state.books;
    let query = b.query.trim();
    let mut rows = Vec::new();
    if let Some(library) = b.library.data.as_ref() {
        let row = |book| book_row(book, library.progress.get(&book.id));
        if query.is_empty() {
            let continuing: Vec<SearchRowView> = library
                .continue_ids
                .iter()
                .filter_map(|id| library.books.iter().find(|b| &b.id == id))
                .map(row)
                .collect();
            if !continuing.is_empty() {
                rows.push(header_row("Continue listening"));
                rows.extend(continuing);
            }
            rows.push(header_row("All books"));
            rows.extend(library.books.iter().map(row));
        } else {
            rows.extend(
                library
                    .books
                    .iter()
                    .filter(|book| {
                        matches(
                            &format!(
                                "{} {} {} {}",
                                book.title, book.author, book.narrator, book.series
                            ),
                            query,
                        )
                    })
                    .map(row),
            );
        }
    }
    let message = if b.username.is_none() {
        "Sign in to Audiobookshelf in Settings to see your books.".to_string()
    } else {
        b.message.clone().unwrap_or_default()
    };
    BooksView {
        query: b.query.clone(),
        keyboard: state.keyboard_open && state.screen == Screen::Books,
        status: if b.username.is_none() || (rows.is_empty() && !query.is_empty()) {
            LoadStatus::Empty
        } else {
            status(Some(&Slot {
                data: b.library.data.as_ref().map(|l| Arc::new(l.books.clone())),
                loading: b.library.loading,
                failed: b.library.failed,
                forbidden: false,
                limited: false,
            }))
        },
        rows,
        message,
    }
}

fn book_view(state: &AppState) -> Option<BookView> {
    let Screen::Book(id) = &state.screen else {
        return None;
    };
    let b = &state.books;
    let slot = b.details.get(id);
    let summary = b
        .library
        .data
        .as_ref()
        .and_then(|l| l.books.iter().find(|x| &x.id == id).cloned());
    let (detail, progress) = match slot.and_then(|s| s.data.as_deref()) {
        Some((detail, progress)) => (Some(detail.clone()), *progress),
        None => (None, None),
    };
    let summary = detail.as_ref().map(|d| d.summary.clone()).or(summary)?;
    let progress = progress.or_else(|| {
        b.library
            .data
            .as_ref()
            .and_then(|l| l.progress.get(id).copied())
    });
    let length = fmt_duration(summary.duration_secs);
    let left = book_progress_text(summary.duration_secs, progress.as_ref());
    Some(BookView {
        id: id.clone(),
        title: summary.title.clone(),
        author: summary.author.clone(),
        narrator: summary.narrator.clone(),
        cover_url: summary
            .has_cover
            .then(|| crate::images::book_cover_url(&summary.id)),
        info: if left == length {
            length
        } else {
            format!("{length} · {left}")
        },
        description: detail
            .as_ref()
            .map(|d| d.description.clone())
            .unwrap_or_default(),
        chapters: detail
            .map(|d| {
                d.chapters
                    .iter()
                    .map(|c| (c.title.clone(), fmt_clock(c.start_secs)))
                    .collect()
            })
            .unwrap_or_default(),
        status: match slot {
            Some(Slot { data: Some(_), .. }) => LoadStatus::Ready,
            Some(Slot { failed: true, .. }) => LoadStatus::Failed,
            _ => LoadStatus::Loading,
        },
    })
}

/// One song in Recent's Songs view.
#[derive(Debug, Clone, PartialEq)]
pub struct RecentSongView {
    pub title: String,
    pub artists: String,
    pub image_url: Option<String>,
    /// Unix seconds; the UI turns it into "12 min ago".
    pub played_at: i64,
}

/// A Bluetooth speaker found by a scan.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeakerRowView {
    pub address: String,
    pub name: String,
    /// "Connecting…", "In use", "Paired" or empty.
    pub status: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SettingsView {
    pub account_name: String,
    pub account_id: String,
    pub device_name: String,
    /// "Headphone jack" or the speaker's name.
    pub output: String,
    pub on_speaker: bool,
    /// A Sonos room plays instead of this device.
    pub on_sonos: bool,
    /// Sonos rooms found by the last scan.
    pub sonos_rooms: Vec<SpeakerRowView>,
    pub speaker_connected: bool,
    pub bluetooth: bool,
    pub scanning: bool,
    pub speakers: Vec<SpeakerRowView>,
    /// A line under the speaker list: a failure or "Restarting…".
    pub message: String,
    /// The sleep timer's length in minutes.
    pub sleep_minutes: u32,
    /// Why there's no Bluetooth, shown instead of the speaker buttons.
    pub bluetooth_note: String,
    pub books_enabled: bool,
    pub books_url: String,
    /// Signed in to Audiobookshelf as; empty when signed out.
    pub books_user: String,
    /// "Signing in…" or a problem.
    pub books_message: String,
    pub restarting: bool,
    /// "47°C", or empty when this computer doesn't report a temperature.
    pub temperature: String,
    /// Warm enough to mention (70°C or more).
    pub temperature_hot: bool,
    /// "Wi-Fi 46Brewer · 5 GHz · 66%", "No Wi-Fi", or empty when this computer doesn't report it.
    pub wifi: String,
    /// No Wi-Fi, or a signal weak enough to drop out.
    pub wifi_weak: bool,
    /// Turn off was confirmed.
    pub powering_off: bool,
}

/// Below this, music can stall and the connection can drop.
const WEAK_WIFI: u8 = 40;

/// The add-to-playlist picker: the user's own playlists.
#[derive(Debug, Clone, PartialEq)]
pub struct PickerView {
    pub playlists: Vec<TileView>,
}

/// The name dialog for a new or renamed playlist.
#[derive(Debug, Clone, PartialEq)]
pub struct TextEntryView {
    pub title: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NowView {
    /// The playing song, for adding it to a playlist; empty when unknown.
    pub track_uri: String,
    /// The artist name can open the artist's page.
    pub has_artist: bool,
    /// Minutes left on the sleep timer, as "28 min"; empty when it's off.
    pub sleep_left: String,
    /// The playing song is in Liked Songs.
    pub liked: bool,
    pub title: String,
    pub artists: String,
    pub image_url: Option<String>,
    pub playing: bool,
    pub loading: bool,
    pub progress: f32,
    pub position: String,
    pub duration: String,
    pub volume: u8,
    pub shuffle: bool,
    pub repeat: Repeat,
}

#[derive(Debug, Clone, PartialEq)]
pub struct View {
    pub screen: ScreenView,
    pub section: Section,
    pub grid_title: String,
    pub grid: Vec<TileView>,
    pub grid_status: LoadStatus,
    pub detail: Option<DetailView>,
    pub now: NowView,
    pub mini_visible: bool,
    pub banner: Option<String>,
    /// Voice replies show at the bottom, where "Listening…" was.
    pub banner_at_bottom: bool,
    /// "Listening…" or "Working on it…" during a voice request; empty otherwise.
    pub voice_status: String,
    pub voice_listening: bool,
    /// Voice control is set up, so the microphone button shows.
    pub voice_enabled: bool,
    pub display: DisplayMode,
    /// The dim screen shows a clock, except while falling asleep to a sleep timer.
    pub show_clock: bool,
    pub auth_needed: bool,
    /// The player waits to be picked in a Spotify app; the screen says how, by its name.
    pub pairing_needed: bool,
    pub search: SearchView,
    pub artist: Option<ArtistView>,
    pub picker: Option<PickerView>,
    pub text_entry: Option<TextEntryView>,
    /// Name of the playlist the delete confirmation asks about.
    pub confirm_delete: Option<String>,
    /// The on-screen keyboard is up, for search or a name.
    pub keyboard: bool,
    /// Collection screens show a list instead of tiles.
    pub list_view: bool,
    /// Colour scheme index.
    pub theme: u32,
    /// The rail shows Books.
    pub books_enabled: bool,
    pub books: BooksView,
    pub book: Option<BookView>,
    /// Playlists shows a button to make a new, empty playlist.
    pub can_create_playlist: bool,
    /// Recent's chips (albums and playlists, or songs) show on its screen.
    pub recent_chips: bool,
    pub recent_songs: bool,
    /// Recent songs from this device's history, newest first, while Songs is picked.
    pub recent_song_rows: Vec<RecentSongView>,
    pub settings: SettingsView,
}

pub fn fmt_ms(ms: u32) -> String {
    let secs = ms / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

pub fn build(state: &AppState) -> View {
    let screen = match state.screen {
        Screen::Grid(_) => ScreenView::Grid,
        Screen::Detail(_) => ScreenView::Detail,
        Screen::NowPlaying => ScreenView::NowPlaying,
        Screen::Search => ScreenView::Search,
        Screen::Artist(_) => ScreenView::Artist,
        Screen::Settings => ScreenView::Settings,
        Screen::Books => ScreenView::Books,
        Screen::Book(_) => ScreenView::Book,
    };
    let grid_section = match state.screen {
        Screen::Grid(section) => section,
        _ if matches!(
            state.section,
            Section::Liked | Section::Search | Section::Settings
        ) =>
        {
            Section::Playlists
        }
        _ => state.section,
    };
    let grid_slot = state.sections.get(&grid_section);
    let grid = grid_slot
        .and_then(|slot| slot.data.as_ref())
        .map(|items| items.iter().map(tile).collect())
        .unwrap_or_default();
    let pb = &state.playback;
    let has_playback = pb.track.is_some() || pb.context_uri.is_some();
    let keyboard = state.keyboard_open || state.text_entry.is_some();

    View {
        screen,
        section: state.section,
        grid_title: grid_section.title().to_string(),
        grid,
        grid_status: status(grid_slot),
        detail: detail(state),
        now: now(state),
        mini_visible: has_playback && screen != ScreenView::NowPlaying && !keyboard,
        banner: banner(state),
        banner_at_bottom: matches!(state.notice, Some(crate::app::Notice::Voice(_))),
        voice_status: match state.voice.phase {
            crate::app::VoicePhase::Idle => String::new(),
            crate::app::VoicePhase::Listening => "Listening…".into(),
            crate::app::VoicePhase::Thinking => "Working on it…".into(),
        },
        voice_listening: state.voice.phase == crate::app::VoicePhase::Listening,
        voice_enabled: state.voice.enabled,
        display: state.display,
        show_clock: state.sleep_ends_ms.is_none(),
        auth_needed: state.auth_needed,
        pairing_needed: state.pairing_needed,
        search: search(state),
        artist: artist(state),
        picker: state.picker.as_ref().map(|_| PickerView {
            playlists: own_playlists(state).iter().map(tile).collect(),
        }),
        text_entry: state.text_entry.as_ref().map(|entry| TextEntryView {
            title: match &entry.purpose {
                TextPurpose::NewPlaylist { .. } => "New playlist".into(),
                TextPurpose::Rename { .. } => "Rename playlist".into(),
                TextPurpose::DeviceName => "Device name".into(),
                TextPurpose::BooksServer => "Audiobookshelf server".into(),
                TextPurpose::BooksUsername => "Audiobookshelf username".into(),
                TextPurpose::BooksPassword { username } => format!("Password for {username}"),
            },
            // Passwords show as dots.
            text: match entry.purpose {
                TextPurpose::BooksPassword { .. } => "•".repeat(entry.text.chars().count()),
                _ => entry.text.clone(),
            },
        }),
        confirm_delete: state.confirm_delete.as_ref().map(|uri| {
            find_collection(state, uri)
                .map(|c| c.name)
                .unwrap_or_default()
        }),
        keyboard,
        list_view: state.list_view,
        theme: state.device.saved.theme.unwrap_or(0),
        can_create_playlist: state.screen == Screen::Grid(Section::Playlists),
        books_enabled: state.books.enabled,
        books: books_view(state),
        book: book_view(state),
        recent_chips: state.screen == Screen::Grid(Section::Recent),
        recent_songs: state.recent_songs,
        recent_song_rows: if state.recent_songs && state.screen == Screen::Grid(Section::Recent) {
            state
                .history
                .iter()
                .flat_map(|h| h.iter())
                .map(|p| RecentSongView {
                    title: p.track.name.clone(),
                    artists: p.track.artists.clone(),
                    image_url: p.track.image_url.clone(),
                    played_at: p.played_at,
                })
                .collect()
        } else {
            Vec::new()
        },
        settings: settings(state),
    }
}

fn settings(state: &AppState) -> SettingsView {
    let d = &state.device;
    let current = d.speaker.as_ref().map(|s| s.address.as_str());
    let speakers = d
        .found
        .iter()
        .map(|s| SpeakerRowView {
            address: s.address.clone(),
            name: s.name.clone(),
            status: if d.connecting.as_deref() == Some(s.address.as_str()) {
                "Connecting…".into()
            } else if current == Some(s.address.as_str()) {
                "In use".into()
            } else if s.paired {
                "Paired".into()
            } else {
                String::new()
            },
        })
        .collect();
    let sonos = match &d.saved.output {
        Some(crate::settings::Output::Sonos(room)) => Some(room.clone()),
        _ => None,
    };
    let message = if d.restarting {
        "Saved. Restarting the player…".to_string()
    } else if let Some(name) = &d.failed {
        format!("Couldn't connect to {name}. Check it's on and ready to pair.")
    } else if d.scanning || d.sonos_scanning {
        "Looking for speakers…".to_string()
    } else {
        String::new()
    };
    SettingsView {
        account_name: state
            .account
            .as_ref()
            .map(|a| a.name.clone())
            .unwrap_or_default(),
        account_id: state
            .account
            .as_ref()
            .map(|a| a.id.clone())
            .unwrap_or_default(),
        device_name: d.device_name.clone(),
        output: match (&sonos, &d.speaker) {
            (Some(room), _) => format!("Sonos: {}", room.name),
            (None, Some(speaker)) => speaker.name.clone(),
            (None, None) => "Headphone jack".to_string(),
        },
        on_speaker: d.speaker.is_some() && sonos.is_none(),
        on_sonos: sonos.is_some(),
        sonos_rooms: d
            .sonos_rooms
            .iter()
            .map(|r| SpeakerRowView {
                address: r.uuid.clone(),
                name: r.name.clone(),
                status: if sonos.as_ref().is_some_and(|s| s.uuid == r.uuid) {
                    "In use".into()
                } else {
                    String::new()
                },
            })
            .collect(),
        speaker_connected: state.speaker_connected,
        bluetooth: d.bluetooth,
        books_enabled: state.books.enabled,
        books_url: state.books.url.clone(),
        books_user: state.books.username.clone().unwrap_or_default(),
        books_message: if state.books.signing_in {
            "Signing in…".into()
        } else {
            state.books.message.clone().unwrap_or_default()
        },
        sleep_minutes: d
            .saved
            .sleep_minutes
            .unwrap_or(crate::app::DEFAULT_SLEEP_MINUTES),
        bluetooth_note: if d.bluetooth {
            String::new()
        } else if cfg!(target_os = "linux") {
            "Bluetooth isn't working on this player. Restarting the device usually fixes it.".into()
        } else {
            "Bluetooth speakers are paired on the Raspberry Pi, from this screen. This computer \
             can't pair them; run with --fake to try the screen here."
                .into()
        },
        scanning: d.scanning || d.sonos_scanning,
        speakers,
        message,
        restarting: d.restarting,
        temperature: d
            .temperature_c
            .map_or_else(String::new, |c| format!("{c}°C")),
        temperature_hot: d.temperature_c.is_some_and(|c| c >= 70),
        wifi: match &d.wifi {
            None => String::new(),
            Some(crate::app::WifiStatus::Disconnected) => "No Wi-Fi".into(),
            Some(crate::app::WifiStatus::Connected {
                network,
                band,
                signal,
            }) => format!("Wi-Fi {network} · {band} · {signal}%"),
        },
        powering_off: d.powering_off,
        wifi_weak: match &d.wifi {
            None => false,
            Some(crate::app::WifiStatus::Disconnected) => true,
            Some(crate::app::WifiStatus::Connected { signal, .. }) => *signal < WEAK_WIFI,
        },
    }
}

/// Playlists the signed-in account owns, which are the only ones it can change.
fn own_playlists(state: &AppState) -> Vec<Collection> {
    let Some(me) = state.account.as_ref() else {
        return Vec::new();
    };
    section_items(state, Section::Playlists)
        .iter()
        .filter(|p| p.owner_id.as_deref() == Some(me.id.as_str()))
        .cloned()
        .collect()
}

fn row(kind: RowKind, c: &Collection) -> SearchRowView {
    SearchRowView {
        kind,
        title: c.name.clone(),
        subtitle: c.subtitle.clone(),
        image_url: c.image_url.clone(),
        uri: c.uri.clone(),
    }
}

fn song_row(t: &Track) -> SearchRowView {
    SearchRowView {
        kind: RowKind::Song,
        title: t.name.clone(),
        subtitle: t.artists.clone(),
        image_url: t.image_url.clone(),
        uri: t.uri.clone(),
    }
}

fn header_row(title: &str) -> SearchRowView {
    SearchRowView {
        kind: RowKind::Header,
        title: title.into(),
        subtitle: String::new(),
        image_url: None,
        uri: String::new(),
    }
}

fn section_items(state: &AppState, section: Section) -> &[Collection] {
    state
        .sections
        .get(&section)
        .and_then(|slot| slot.data.as_deref())
        .map_or(&[], Vec::as_slice)
}

/// Matches from the user's own playlists, saved albums and Liked Songs.
fn library_rows(state: &AppState, query: &str) -> Vec<SearchRowView> {
    let mut rows = Vec::new();
    let liked = state
        .tracks
        .get(LIKED_URI)
        .and_then(|slot| slot.data.as_deref())
        .map_or(&[][..], Vec::as_slice);
    rows.extend(
        liked
            .iter()
            .filter(|t| matches(&format!("{} {}", t.name, t.artists), query))
            .take(LIBRARY_MATCHES)
            .map(song_row),
    );
    rows.extend(
        section_items(state, Section::Albums)
            .iter()
            .filter(|c| matches(&format!("{} {}", c.name, c.subtitle), query))
            .take(LIBRARY_MATCHES)
            .map(|c| row(RowKind::Album, c)),
    );
    rows.extend(
        section_items(state, Section::Playlists)
            .iter()
            .filter(|c| matches(&c.name, query))
            .take(LIBRARY_MATCHES)
            .map(|c| row(RowKind::Playlist, c)),
    );
    rows
}

/// Whether a result row of `kind` passes the filter.
fn shows(filter: SearchFilter, kind: RowKind) -> bool {
    matches!(
        (filter, kind),
        (SearchFilter::All, _)
            | (SearchFilter::Songs, RowKind::Song)
            | (SearchFilter::Artists, RowKind::Artist)
            | (SearchFilter::Albums, RowKind::Album)
            | (SearchFilter::Playlists, RowKind::Playlist)
    )
}

fn search(state: &AppState) -> SearchView {
    let s = &state.search;
    let query = s.query.trim();
    let mut rows = Vec::new();
    let library: Vec<SearchRowView> = library_rows(state, query)
        .into_iter()
        .filter(|r| shows(s.filter, r.kind))
        .collect();
    let has_library = !library.is_empty();
    if has_library {
        rows.push(header_row("In your library"));
        rows.extend(library);
    }
    // Catalog results count only when they belong to what is typed now.
    let current = !query.is_empty() && s.sent.trim() == query;
    let catalog = s.results.data.as_ref().filter(|_| current);
    if let Some(found) = catalog {
        let shown: std::collections::HashSet<String> = rows.iter().map(|r| r.uri.clone()).collect();
        let fresh = |r: &SearchRowView| !shown.contains(&r.uri);
        let catalog_rows: Vec<SearchRowView> = found
            .tracks
            .iter()
            .map(song_row)
            .chain(found.artists.iter().map(|c| row(RowKind::Artist, c)))
            .chain(found.albums.iter().map(|c| row(RowKind::Album, c)))
            .chain(found.playlists.iter().map(|c| row(RowKind::Playlist, c)))
            .filter(fresh)
            .filter(|r| shows(s.filter, r.kind))
            .collect();
        if !catalog_rows.is_empty() {
            rows.push(header_row("On Spotify"));
            rows.extend(catalog_rows);
        }
    }
    let catalog_failed = current && s.results.failed;
    let status = if query.is_empty() || !rows.is_empty() {
        LoadStatus::Ready
    } else if catalog_failed {
        LoadStatus::Failed
    } else if s.results.loading || !current {
        LoadStatus::Loading
    } else {
        LoadStatus::Empty
    };
    let note = match (catalog_failed && has_library, state.online) {
        (false, _) => String::new(),
        (true, true) => "Can't search Spotify right now".into(),
        (true, false) => "No internet right now".into(),
    };
    SearchView {
        query: s.query.clone(),
        keyboard: state.keyboard_open,
        rows,
        status,
        note,
        filter: s.filter,
    }
}

fn artist(state: &AppState) -> Option<ArtistView> {
    let uri = std::iter::once(&state.screen)
        .chain(state.back_stack.iter().rev())
        .find_map(|screen| match screen {
            Screen::Artist(uri) => Some(uri.clone()),
            _ => None,
        })?;
    let header = find_collection(state, &uri)
        .map(|c| tile(&c))
        .unwrap_or_else(|| blank_tile(&uri));
    let slot = state.artist_albums.get(&uri);
    let loaded: Vec<TileView> = slot
        .and_then(|s| s.data.as_ref())
        .map(|albums| albums.iter().map(tile).collect())
        .unwrap_or_default();
    // Until the artist's own list loads (or while Spotify is limiting it), show their
    // albums from the latest search.
    let from_search: Vec<TileView> = state
        .search
        .results
        .data
        .iter()
        .flat_map(|found| found.albums.iter())
        .filter(|a| a.artist_uri.as_deref() == Some(uri.as_str()))
        .map(tile)
        .collect();
    let has_list = slot.is_some_and(|s| s.data.is_some());
    let (albums, status) = if !has_list && !from_search.is_empty() {
        (from_search, LoadStatus::Ready)
    } else {
        (loaded, status(slot))
    };
    Some(ArtistView {
        header,
        albums,
        status,
        followed: state.liked.get(&uri).copied().unwrap_or_else(|| {
            section_items(state, Section::Artists)
                .iter()
                .any(|a| a.uri == uri)
        }),
        back_label: match state.back_stack.last() {
            Some(Screen::Grid(Section::Artists)) if state.screen == Screen::Artist(uri.clone()) => {
                Section::Artists.title().into()
            }
            _ => String::new(),
        },
    })
}

fn blank_tile(uri: &str) -> TileView {
    TileView {
        uri: uri.to_string(),
        title: String::new(),
        subtitle: String::new(),
        image_url: None,
    }
}

fn tile(c: &Collection) -> TileView {
    TileView {
        uri: c.uri.clone(),
        title: c.name.clone(),
        subtitle: c.subtitle.clone(),
        image_url: c.image_url.clone(),
    }
}

fn status<T>(slot: Option<&Slot<Vec<T>>>) -> LoadStatus {
    match slot {
        Some(Slot {
            data: Some(data), ..
        }) if data.is_empty() => LoadStatus::Empty,
        Some(Slot { data: Some(_), .. }) => LoadStatus::Ready,
        Some(Slot {
            forbidden: true, ..
        }) => LoadStatus::Forbidden,
        Some(Slot { limited: true, .. }) => LoadStatus::Limited,
        Some(Slot { failed: true, .. }) => LoadStatus::Failed,
        _ => LoadStatus::Loading,
    }
}

fn find_collection(state: &AppState, uri: &str) -> Option<Collection> {
    if uri == LIKED_URI {
        return Some(liked_collection());
    }
    let from_sections = state
        .sections
        .values()
        .chain(state.artist_albums.values())
        .filter_map(|slot| slot.data.as_ref())
        .flat_map(|items: &Arc<Vec<Collection>>| items.iter());
    let from_search = state.search.results.data.iter().flat_map(|found| {
        found
            .albums
            .iter()
            .chain(&found.playlists)
            .chain(&found.artists)
    });
    from_sections
        .chain(from_search)
        .chain(state.artists_seen.values())
        .find(|c| c.uri == uri)
        .cloned()
}

fn detail(state: &AppState) -> Option<DetailView> {
    let uri = std::iter::once(&state.screen)
        .chain(state.back_stack.iter().rev())
        .find_map(|screen| match screen {
            Screen::Detail(uri) => Some(uri.clone()),
            _ => None,
        })?;
    let header = find_collection(state, &uri)
        .map(|c| tile(&c))
        .unwrap_or_else(|| blank_tile(&uri));
    let slot = state.tracks.get(&uri);
    let current_uri = state.playback.track.as_ref().map(|t| t.uri.as_str());
    let tracks = slot
        .and_then(|s| s.data.as_ref())
        .map(|tracks| {
            tracks
                .iter()
                .map(|t| TrackRowView {
                    uri: t.uri.clone(),
                    title: t.name.clone(),
                    artists: t.artists.clone(),
                    duration: fmt_ms(t.duration_ms),
                    current: Some(t.uri.as_str()) == current_uri,
                })
                .collect()
        })
        .unwrap_or_default();
    let editable = own_playlists(state).iter().any(|p| p.uri == uri);
    let summary = slot
        .and_then(|s| s.data.as_ref())
        .map(|tracks| track_summary(tracks))
        .unwrap_or_default();
    // Albums lead with the year they came out: "1986 · 11 songs · 43 min".
    let year = find_collection(state, &uri).and_then(|c| c.year);
    let summary = match (year, summary.is_empty()) {
        (Some(year), true) => year.to_string(),
        (Some(year), false) => format!("{year} · {summary}"),
        (None, _) => summary,
    };
    Some(DetailView {
        editing: editable && state.editing.as_deref() == Some(uri.as_str()),
        editable,
        summary,
        artist_link: uri.starts_with("spotify:album:")
            && find_collection(state, &uri).is_some_and(|c| !c.subtitle.is_empty()),
        saveable: uri.starts_with("spotify:album:"),
        saved: state.liked.get(&uri).copied().unwrap_or_else(|| {
            section_items(state, Section::Albums)
                .iter()
                .any(|a| a.uri == uri)
        }),
        can_add: !uri.starts_with("spotify:playlist:") && !uri.starts_with("ziggy:new:"),
        header,
        status: status(slot),
        tracks,
    })
}

/// "1 song · 3 min", "23 songs · 1 hr 12 min".
fn track_summary(tracks: &[Track]) -> String {
    let count = match tracks.len() {
        1 => "1 song".to_string(),
        n => format!("{n} songs"),
    };
    let minutes = tracks.iter().map(|t| t.duration_ms as u64).sum::<u64>() / 60_000;
    let length = match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} hr"),
        (h, m) => format!("{h} hr {m} min"),
    };
    format!("{count} · {length}")
}

fn now(state: &AppState) -> NowView {
    let pb = &state.playback;
    let (title, artists, image_url) = match &pb.track {
        Some(t) => (t.name.clone(), t.artists.clone(), t.image_url.clone()),
        None => match pb
            .context_uri
            .as_deref()
            .and_then(|uri| find_collection(state, uri))
        {
            Some(c) => (c.name, String::new(), c.image_url),
            None => (String::new(), String::new(), None),
        },
    };
    let duration_ms = pb.track.as_ref().map_or(0, |t| t.duration_ms);
    let progress = if duration_ms > 0 {
        (pb.position_ms as f32 / duration_ms as f32).min(1.0)
    } else {
        0.0
    };
    NowView {
        track_uri: pb.track.as_ref().map(|t| t.uri.clone()).unwrap_or_default(),
        has_artist: pb.track.as_ref().is_some_and(|t| t.artist_uri.is_some()),
        sleep_left: match state.sleep_ends_ms {
            // The core's clock isn't in state, so the view counts from the last tick it saw.
            Some(ends) => format!("{} min", ends.saturating_sub(state.now_ms).div_ceil(60_000)),
            None => String::new(),
        },
        liked: pb
            .track
            .as_ref()
            .and_then(|t| state.liked.get(&t.uri))
            .copied()
            .unwrap_or(false),
        title,
        artists,
        image_url,
        playing: pb.status == PlayStatus::Playing,
        loading: pb.status == PlayStatus::Loading,
        progress,
        position: fmt_ms(pb.position_ms),
        duration: fmt_ms(duration_ms),
        volume: pb.volume,
        shuffle: pb.shuffle,
        repeat: pb.repeat,
    }
}

fn banner(state: &AppState) -> Option<String> {
    match &state.notice {
        Some(Notice::NoInternet) => Some("No internet right now".into()),
        Some(Notice::TrackUnavailable) => Some("That song can't play, skipping".into()),
        Some(Notice::SkippingStopped) => Some(
            "Spotify won't play these songs right now, so Ziggy stopped. Try again later.".into(),
        ),
        Some(Notice::CouldntSave) => Some("Couldn't save that".into()),
        Some(Notice::SpotifyBusy) => {
            Some("Spotify is limiting changes right now. Try again later.".into())
        }
        Some(Notice::AddedTo(name)) => Some(format!("Added to {name}")),
        Some(Notice::AlreadyIn(name)) => Some(format!("Already in {name}")),
        Some(Notice::RemovedFrom(name)) => Some(format!("Removed from {name}")),
        Some(Notice::Created(name)) => Some(format!("Created {name}")),
        Some(Notice::Voice(text)) => Some(text.clone()),
        None if !state.speaker_connected => Some("Speaker not connected".into()),
        None => None,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{core, playlist, track, ui, with_tracks};
    use crate::app::{FailReason, Input, LibraryUpdate, PlayerUpdate, UiAction};
    use crate::model::{LIKED_URI, Section};

    #[test]
    fn fmt_ms_formats_minutes_and_seconds() {
        assert_eq!(fmt_ms(0), "0:00");
        assert_eq!(fmt_ms(65_400), "1:05");
        assert_eq!(fmt_ms(3_600_000), "60:00");
    }

    // Review focus 2: an empty library.
    #[test]
    fn grid_status_covers_loading_empty_failed_ready() {
        let mut c = core();
        c.handle(ui(UiAction::ShowSection(Section::Playlists)), 0);
        assert_eq!(build(c.state()).grid_status, LoadStatus::Loading);
        c.handle(
            Input::Library(LibraryUpdate::Section {
                section: Section::Playlists,
                items: vec![],
            }),
            0,
        );
        assert_eq!(build(c.state()).grid_status, LoadStatus::Empty);
        c.handle(
            Input::Library(LibraryUpdate::Section {
                section: Section::Playlists,
                items: vec![playlist(1)],
            }),
            0,
        );
        let v = build(c.state());
        assert_eq!(v.grid_status, LoadStatus::Ready);
        assert_eq!(v.grid[0].title, "Playlist 1");
        assert_eq!(v.grid[0].subtitle, "Mum");
        assert_eq!(v.grid_title, "Playlists");
        c.handle(
            Input::Library(LibraryUpdate::SectionFailed {
                section: Section::Albums,
                reason: FailReason::Other,
            }),
            0,
        );
        c.handle(ui(UiAction::ShowSection(Section::Albums)), 0);
        c.handle(
            Input::Library(LibraryUpdate::SectionFailed {
                section: Section::Albums,
                reason: FailReason::Other,
            }),
            0,
        );
        assert_eq!(build(c.state()).grid_status, LoadStatus::Failed);
    }

    #[test]
    fn detail_shows_collection_and_marks_current_track() {
        let mut c = core();
        c.handle(
            Input::Library(LibraryUpdate::Section {
                section: Section::Playlists,
                items: vec![playlist(1)],
            }),
            0,
        );
        with_tracks(&mut c, "spotify:playlist:p1", 3);
        c.handle(
            ui(UiAction::OpenCollection("spotify:playlist:p1".into())),
            0,
        );
        c.handle(Input::Player(PlayerUpdate::TrackChanged(track(1))), 0);
        let v = build(c.state());
        assert_eq!(v.screen, ScreenView::Detail);
        let d = v.detail.unwrap();
        assert_eq!(d.header.title, "Playlist 1");
        assert_eq!(d.status, LoadStatus::Ready);
        assert_eq!(d.tracks.len(), 3);
        assert_eq!(d.tracks[1].duration, "3:00");
        assert!(d.tracks[1].current && !d.tracks[0].current);
    }

    #[test]
    fn liked_detail_has_title_and_rail_highlights_liked() {
        let mut c = core();
        c.handle(ui(UiAction::ShowSection(Section::Liked)), 0);
        let v = build(c.state());
        assert_eq!(v.section, Section::Liked);
        let d = v.detail.unwrap();
        assert_eq!(d.header.uri, LIKED_URI);
        assert_eq!(d.header.title, "Liked Songs");
        assert_eq!(d.status, LoadStatus::Loading);
    }

    #[test]
    fn now_playing_progress_and_loading_fallback() {
        let mut c = core();
        c.handle(
            Input::Library(LibraryUpdate::Section {
                section: Section::Playlists,
                items: vec![playlist(1)],
            }),
            0,
        );
        c.handle(
            ui(UiAction::PlayCollection {
                uri: "spotify:playlist:p1".into(),
                shuffle: true,
            }),
            0,
        );
        let v = build(c.state());
        assert_eq!(v.screen, ScreenView::NowPlaying);
        assert_eq!(v.now.title, "Playlist 1");
        assert!(v.now.loading);
        assert!(!v.mini_visible, "mini player hides on Now Playing");
        c.handle(Input::Player(PlayerUpdate::TrackChanged(track(2))), 0);
        c.handle(
            Input::Player(PlayerUpdate::Playing {
                position_ms: 45_000,
            }),
            0,
        );
        let v = build(c.state());
        assert_eq!(v.now.title, "Song 2");
        assert!((v.now.progress - 0.25).abs() < 0.001);
        assert_eq!(v.now.position, "0:45");
        assert_eq!(v.now.duration, "3:00");
        assert!(v.now.playing);
        c.handle(ui(UiAction::Back), 0);
        assert!(build(c.state()).mini_visible);
    }

    #[test]
    fn detail_survives_while_now_playing_is_on_top() {
        let mut c = core();
        with_tracks(&mut c, "spotify:playlist:p1", 2);
        c.handle(
            ui(UiAction::OpenCollection("spotify:playlist:p1".into())),
            0,
        );
        c.handle(
            ui(UiAction::PlayCollection {
                uri: "spotify:playlist:p1".into(),
                shuffle: false,
            }),
            0,
        );
        let v = build(c.state());
        assert_eq!(
            v.detail.map(|d| d.header.uri),
            Some("spotify:playlist:p1".to_string())
        );
    }

    #[test]
    fn banner_prefers_notice_over_speaker_warning() {
        let mut c = core();
        c.handle(Input::Speaker { connected: false }, 0);
        assert_eq!(
            build(c.state()).banner.as_deref(),
            Some("Speaker not connected")
        );
        c.handle(Input::Player(PlayerUpdate::Unavailable), 0);
        assert_eq!(
            build(c.state()).banner.as_deref(),
            Some("That song can't play, skipping")
        );
        c.handle(Input::Player(PlayerUpdate::Disconnected), 0);
        c.handle(
            ui(UiAction::PlayCollection {
                uri: "spotify:playlist:p9".into(),
                shuffle: false,
            }),
            0,
        );
        assert_eq!(
            build(c.state()).banner.as_deref(),
            Some("No internet right now")
        );
    }

    // ---- Search ----

    fn search_for(c: &mut crate::app::Core, query: &str) {
        c.handle(ui(UiAction::ShowSection(Section::Search)), 0);
        c.handle(ui(UiAction::KeyPressed(query.into())), 0);
        c.handle(Input::SearchTick, 1_000);
    }

    fn catalog(c: &mut crate::app::Core, query: &str, results: crate::model::SearchResults) {
        c.handle(
            Input::Library(LibraryUpdate::SearchResults {
                query: query.into(),
                results,
            }),
            1_000,
        );
    }

    fn kinds(v: &View) -> Vec<(RowKind, String)> {
        v.search
            .rows
            .iter()
            .map(|r| (r.kind, r.title.clone()))
            .collect()
    }

    #[test]
    fn library_matches_come_first_and_are_not_repeated() {
        let mut c = core();
        c.handle(
            Input::Library(LibraryUpdate::Section {
                section: Section::Playlists,
                items: vec![playlist(1), playlist(2)],
            }),
            0,
        );
        search_for(&mut c, "playlist 1");
        catalog(
            &mut c,
            "playlist 1",
            crate::model::SearchResults {
                playlists: vec![playlist(1), playlist(3)],
                ..Default::default()
            },
        );
        let v = build(c.state());
        assert_eq!(
            kinds(&v),
            [
                (RowKind::Header, "In your library".to_string()),
                (RowKind::Playlist, "Playlist 1".to_string()),
                (RowKind::Header, "On Spotify".to_string()),
                (RowKind::Playlist, "Playlist 3".to_string()),
            ]
        );
        assert_eq!(v.search.status, LoadStatus::Ready);
    }

    #[test]
    fn filter_keeps_only_one_kind_and_its_headers() {
        let mut c = core();
        c.handle(
            Input::Library(LibraryUpdate::Section {
                section: Section::Playlists,
                items: vec![playlist(1)],
            }),
            0,
        );
        search_for(&mut c, "1");
        catalog(
            &mut c,
            "1",
            crate::model::SearchResults {
                tracks: vec![track(1)],
                playlists: vec![playlist(9)],
                ..Default::default()
            },
        );
        c.handle(ui(UiAction::SetSearchFilter(SearchFilter::Songs)), 1_000);
        assert_eq!(
            kinds(&build(c.state())),
            [
                (RowKind::Header, "On Spotify".to_string()),
                (RowKind::Song, "Song 1".to_string()),
            ]
        );
        c.handle(ui(UiAction::SetSearchFilter(SearchFilter::Artists)), 1_000);
        assert_eq!(build(c.state()).search.status, LoadStatus::Empty);
    }

    #[test]
    fn liked_songs_match_on_title_or_artist() {
        let mut c = core();
        with_tracks(&mut c, LIKED_URI, 3);
        search_for(&mut c, "band song 2");
        let v = build(c.state());
        assert_eq!(v.search.rows[1].kind, RowKind::Song);
        assert_eq!(v.search.rows[1].title, "Song 2");
    }

    #[test]
    fn search_status_is_loading_then_empty() {
        let mut c = core();
        search_for(&mut c, "zzz");
        assert_eq!(build(c.state()).search.status, LoadStatus::Loading);
        catalog(&mut c, "zzz", Default::default());
        assert_eq!(build(c.state()).search.status, LoadStatus::Empty);
    }

    #[test]
    fn empty_query_is_ready_with_no_rows() {
        let mut c = core();
        c.handle(ui(UiAction::ShowSection(Section::Search)), 0);
        let v = build(c.state());
        assert_eq!(v.screen, ScreenView::Search);
        assert!(v.search.rows.is_empty());
        assert_eq!(v.search.status, LoadStatus::Ready);
        assert!(v.search.keyboard);
    }

    #[test]
    fn failed_search_keeps_library_rows_with_a_note() {
        let mut c = core();
        c.handle(
            Input::Library(LibraryUpdate::Section {
                section: Section::Playlists,
                items: vec![playlist(1)],
            }),
            0,
        );
        search_for(&mut c, "playlist");
        c.handle(
            Input::Library(LibraryUpdate::SearchFailed {
                query: "playlist".into(),
                reason: FailReason::Offline,
            }),
            1_000,
        );
        let v = build(c.state());
        assert_eq!(v.search.status, LoadStatus::Ready);
        assert_eq!(v.search.note, "No internet right now");
    }

    #[test]
    fn detail_header_comes_from_search_results() {
        let mut c = core();
        search_for(&mut c, "p");
        catalog(
            &mut c,
            "p",
            crate::model::SearchResults {
                playlists: vec![playlist(5)],
                ..Default::default()
            },
        );
        c.handle(ui(UiAction::OpenCollection(playlist(5).uri)), 1_000);
        assert_eq!(build(c.state()).detail.unwrap().header.title, "Playlist 5");
    }

    #[test]
    fn add_buttons_show_on_albums_and_liked_but_not_playlists() {
        let mut c = core();
        let can_add = |c: &crate::app::Core| build(c.state()).detail.unwrap().can_add;
        c.handle(ui(UiAction::OpenCollection("spotify:album:a1".into())), 0);
        assert!(can_add(&c));
        c.handle(
            ui(UiAction::OpenCollection("spotify:playlist:p1".into())),
            0,
        );
        assert!(!can_add(&c));
        c.handle(ui(UiAction::ShowSection(Section::Liked)), 0);
        assert!(can_add(&c));
    }

    #[test]
    fn summary_counts_songs_and_length() {
        let mut tracks: Vec<Track> = (0..23).map(track).collect();
        tracks[0].duration_ms = 180_000 + 12 * 60_000 + 60 * 60_000 - 23 * 180_000;
        assert_eq!(track_summary(&tracks), "23 songs · 1 hr 12 min");
        assert_eq!(track_summary(&tracks[1..2]), "1 song · 3 min");
    }

    #[test]
    fn no_clock_while_falling_asleep() {
        let mut c = core();
        assert!(build(c.state()).show_clock);
        c.handle(ui(UiAction::SetSleepTimer(Some(30))), 0);
        assert!(!build(c.state()).show_clock);
    }

    #[test]
    fn album_summary_leads_with_the_year() {
        let mut c = core();
        let album = Collection {
            uri: "spotify:album:a1".into(),
            kind: crate::model::CollectionKind::Album,
            name: "Graceland".into(),
            year: Some(1986),
            ..Default::default()
        };
        c.handle(
            Input::Library(LibraryUpdate::Section {
                section: Section::Albums,
                items: vec![album.clone()],
            }),
            0,
        );
        c.handle(ui(UiAction::OpenCollection(album.uri.clone())), 0);
        assert_eq!(build(c.state()).detail.unwrap().summary, "1986");
        with_tracks(&mut c, &album.uri, 2);
        assert_eq!(
            build(c.state()).detail.unwrap().summary,
            "1986 · 2 songs · 6 min"
        );
    }

    #[test]
    fn a_limited_artist_page_falls_back_to_search_albums() {
        let mut c = core();
        let album = Collection {
            uri: "spotify:album:sw".into(),
            kind: crate::model::CollectionKind::Album,
            name: "Slippery When Wet".into(),
            artist_uri: Some("spotify:artist:bj".into()),
            ..Default::default()
        };
        search_for(&mut c, "bon jovi");
        catalog(
            &mut c,
            "bon jovi",
            crate::model::SearchResults {
                albums: vec![album],
                ..Default::default()
            },
        );
        c.handle(ui(UiAction::OpenArtist("spotify:artist:bj".into())), 1_000);
        c.handle(
            Input::Library(LibraryUpdate::ArtistAlbumsFailed {
                artist_uri: "spotify:artist:bj".into(),
                reason: FailReason::RateLimited,
            }),
            1_000,
        );
        let artist = build(c.state()).artist.unwrap();
        assert_eq!(artist.status, LoadStatus::Ready);
        assert_eq!(artist.albums[0].title, "Slippery When Wet");

        // With nothing from search either, it says Spotify is limiting.
        c.handle(
            ui(UiAction::OpenArtist("spotify:artist:other".into())),
            1_000,
        );
        c.handle(
            Input::Library(LibraryUpdate::ArtistAlbumsFailed {
                artist_uri: "spotify:artist:other".into(),
                reason: FailReason::RateLimited,
            }),
            1_000,
        );
        assert_eq!(build(c.state()).artist.unwrap().status, LoadStatus::Limited);
    }

    #[test]
    fn forbidden_detail_has_its_own_status() {
        let mut c = core();
        c.handle(
            ui(UiAction::OpenCollection("spotify:playlist:p9".into())),
            0,
        );
        c.handle(
            Input::Library(LibraryUpdate::TracksFailed {
                collection_uri: "spotify:playlist:p9".into(),
                reason: FailReason::Forbidden,
            }),
            0,
        );
        assert_eq!(
            build(c.state()).detail.unwrap().status,
            LoadStatus::Forbidden
        );
    }

    #[test]
    fn keyboard_hides_the_mini_player() {
        let mut c = core();
        c.handle(
            ui(UiAction::PlayCollection {
                uri: "spotify:album:a1".into(),
                shuffle: false,
            }),
            0,
        );
        c.handle(ui(UiAction::ShowSection(Section::Search)), 5_000);
        assert!(!build(c.state()).mini_visible);
        c.handle(ui(UiAction::CloseKeyboard), 5_000);
        assert!(build(c.state()).mini_visible);
    }
}
