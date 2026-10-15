//! Glue between the runtime (any thread) and the Slint window (UI thread).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;

use image::RgbaImage;
use slint::{ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};
use tokio::sync::mpsc::UnboundedSender;

use crate::app::{AppState, DisplayMode, Input, UiAction};
use crate::model::Section;
use crate::view::{self, LoadStatus, RowKind, ScreenView, SearchRowView, TileView, TrackRowView};
use crate::{
    AppWindow, ChapterData, LoadState, ScreenKind, SearchRowData, SpeakerRowData, Theme, TileData,
    TrackData,
};

/// Covers kept in memory, besides those on screen. At 160 px each is about 100 KB.
const IMAGE_CACHE_LIMIT: usize = 48;

struct Images {
    ready: HashMap<String, Image>,
    order: VecDeque<String>,
    requested: HashSet<String>,
    /// URLs the current render asked for, so the screen shows them. These are never
    /// evicted: evicting a cover on screen makes the next render request it again, which
    /// loops forever once a screen shows more covers than the cache holds.
    live: HashSet<String>,
    requests: UnboundedSender<String>,
}

/// Removes and returns the oldest URLs until `order` fits in `limit`, skipping live ones.
/// When everything left is live the cache stays over the limit: evicting a cover on screen
/// would only make the next render request it again.
fn evict(order: &mut VecDeque<String>, live: &HashSet<String>, limit: usize) -> Vec<String> {
    let mut evicted = Vec::new();
    let mut i = 0;
    while order.len() > limit && i < order.len() {
        if live.contains(&order[i]) {
            i += 1;
        } else {
            evicted.extend(order.remove(i));
        }
    }
    evicted
}

impl Images {
    /// Starts a render: covers are live again only if this render asks for them.
    fn begin_render(&mut self) {
        self.live.clear();
    }

    fn get(&mut self, url: &Option<String>) -> Option<Image> {
        let url = url.as_ref()?;
        self.live.insert(url.clone());
        if let Some(image) = self.ready.get(url) {
            return Some(image.clone());
        }
        if self.requested.insert(url.clone()) {
            let _ = self.requests.send(url.clone());
        }
        None
    }

    fn insert(&mut self, url: String, image: Image) {
        self.ready.insert(url.clone(), image);
        self.order.push_back(url);
        for old in evict(&mut self.order, &self.live, IMAGE_CACHE_LIMIT) {
            self.ready.remove(&old);
            self.requested.remove(&old);
        }
    }
}

struct Bridge {
    window: slint::Weak<AppWindow>,
    images: Images,
    tiles: Rc<VecModel<TileData>>,
    tracks: Rc<VecModel<TrackData>>,
    search_rows: Rc<VecModel<SearchRowData>>,
    artist_albums: Rc<VecModel<TileData>>,
    picker_playlists: Rc<VecModel<TileData>>,
    recent_songs: Rc<VecModel<TileData>>,
    books_rows: Rc<VecModel<SearchRowData>>,
    last: Option<AppState>,
}

thread_local! {
    static BRIDGE: RefCell<Option<Bridge>> = const { RefCell::new(None) };
}

fn with_bridge<R>(f: impl FnOnce(&mut Bridge) -> R) -> Option<R> {
    BRIDGE.with(|cell| cell.borrow_mut().as_mut().map(f))
}

/// Call once on the UI thread before `window.run()`.
pub fn install(window: &AppWindow, image_requests: UnboundedSender<String>) {
    let tiles = Rc::new(VecModel::<TileData>::default());
    let tracks = Rc::new(VecModel::<TrackData>::default());
    window.set_tiles(ModelRc::from(tiles.clone()));
    window.set_detail_tracks(ModelRc::from(tracks.clone()));
    let search_rows = Rc::new(VecModel::<SearchRowData>::default());
    let artist_albums = Rc::new(VecModel::<TileData>::default());
    window.set_search_rows(ModelRc::from(search_rows.clone()));
    window.set_artist_albums(ModelRc::from(artist_albums.clone()));
    let books_rows = Rc::new(VecModel::<SearchRowData>::default());
    window.set_books_rows(ModelRc::from(books_rows.clone()));
    let recent_songs = Rc::new(VecModel::<TileData>::default());
    window.set_recent_song_rows(ModelRc::from(recent_songs.clone()));
    let picker_playlists = Rc::new(VecModel::<TileData>::default());
    window.set_picker_playlists(ModelRc::from(picker_playlists.clone()));
    let bridge = Bridge {
        window: window.as_weak(),
        images: Images {
            ready: HashMap::new(),
            order: VecDeque::new(),
            requested: HashSet::new(),
            live: HashSet::new(),
            requests: image_requests,
        },
        tiles,
        tracks,
        search_rows,
        artist_albums,
        picker_playlists,
        recent_songs,
        books_rows,
        last: None,
    };
    BRIDGE.with(|cell| *cell.borrow_mut() = Some(bridge));
}

/// Called from the runtime thread with each new state.
pub fn publish(state: AppState) {
    let _ = slint::invoke_from_event_loop(move || {
        with_bridge(|b| {
            b.last = Some(state);
            b.render();
        });
    });
}

/// The big copy of a cover, for Now Playing and a book's page.
fn large(url: &Option<String>) -> Option<String> {
    url.as_ref().map(|u| format!("{}{u}", crate::images::LARGE))
}

/// Called from the image loader with a decoded cover.
pub fn deliver_image(url: String, image: Option<RgbaImage>) {
    let Some(image) = image else { return };
    let (width, height) = image.dimensions();
    let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(image.as_raw(), width, height);
    let _ = slint::invoke_from_event_loop(move || {
        with_bridge(|b| {
            b.images.insert(url, Image::from_rgba8(buffer));
            b.render();
        });
    });
}

pub fn wire_callbacks(window: &AppWindow, inputs: UnboundedSender<Input>) {
    let send = move |action: UiAction| {
        let _ = inputs.send(Input::Ui(action));
    };
    let s = send.clone();
    window.on_show_section(move |i| s(UiAction::ShowSection(Section::from_index(i))));
    let s = send.clone();
    window.on_open_collection(move |uri| s(UiAction::OpenCollection(uri.to_string())));
    let s = send.clone();
    window.on_back(move || s(UiAction::Back));
    let s = send.clone();
    window.on_open_now_playing(move || s(UiAction::OpenNowPlaying));
    let s = send.clone();
    window.on_play_collection(move |uri, shuffle| {
        s(UiAction::PlayCollection {
            uri: uri.to_string(),
            shuffle,
        })
    });
    let s = send.clone();
    window.on_play_track(move |uri, index| {
        s(UiAction::PlayTrack {
            collection_uri: uri.to_string(),
            index: index.max(0) as usize,
        })
    });
    let s = send.clone();
    window.on_toggle_play(move || s(UiAction::TogglePlay));
    let s = send.clone();
    window.on_next(move || s(UiAction::Next));
    let s = send.clone();
    window.on_previous(move || s(UiAction::Previous));
    let s = send.clone();
    window.on_seek(move |fraction| {
        let duration = with_bridge(|b| {
            b.last
                .as_ref()
                .and_then(|st| st.playback.track.as_ref())
                .map(|t| t.duration_ms)
        })
        .flatten()
        .unwrap_or(0);
        s(UiAction::Seek {
            position_ms: (fraction.clamp(0.0, 1.0) * duration as f32) as u32,
        });
    });
    let s = send.clone();
    window.on_set_volume(move |percent| {
        s(UiAction::SetVolume {
            percent: percent.clamp(0, 100) as u8,
        })
    });
    let s = send.clone();
    window.on_toggle_shuffle(move || s(UiAction::ToggleShuffle));
    let s = send.clone();
    window.on_cycle_repeat(move || s(UiAction::CycleRepeat));
    let s = send.clone();
    window.on_touched(move || s(UiAction::Touch));
    let s = send.clone();
    window.on_key_pressed(move |text| s(UiAction::KeyPressed(text.to_string())));
    let s = send.clone();
    window.on_backspace(move || s(UiAction::Backspace));
    let s = send.clone();
    window.on_clear_search(move || s(UiAction::ClearSearch));
    let s = send.clone();
    window.on_open_keyboard(move || s(UiAction::OpenKeyboard));
    let s = send.clone();
    window.on_close_keyboard(move || s(UiAction::CloseKeyboard));
    let s = send.clone();
    window.on_set_search_filter(move |i| {
        s(UiAction::SetSearchFilter(
            crate::app::SearchFilter::from_index(i),
        ))
    });
    let s = send.clone();
    window.on_close_search(move || s(UiAction::CloseSearch));
    let s = send.clone();
    window.on_open_artist(move |uri| s(UiAction::OpenArtist(uri.to_string())));
    let s = send.clone();
    window.on_play_artist(move |uri| s(UiAction::PlayArtist(uri.to_string())));
    let s = send.clone();
    window.on_play_song(move |uri| s(UiAction::PlaySong(uri.to_string())));
    let s = send.clone();
    window.on_toggle_save_album(move |uri| s(UiAction::ToggleSaveAlbum(uri.to_string())));
    let s = send.clone();
    window.on_toggle_follow(move |uri| s(UiAction::ToggleFollow(uri.to_string())));
    let s = send.clone();
    window.on_pick_liked(move || s(UiAction::PickLiked));
    let s = send.clone();
    window.on_rename_device(move || s(UiAction::RenameDevice));
    let s = send.clone();
    window.on_find_speakers(move || s(UiAction::FindSpeakers));
    let s = send.clone();
    window.on_power_off(move || s(UiAction::PowerOff));
    let s = send.clone();
    window.on_connect_speaker(move |address| s(UiAction::ConnectSpeaker(address.to_string())));
    let s = send.clone();
    window.on_choose_sonos(move |uuid| s(UiAction::ChooseSonos(uuid.to_string())));
    let s = send.clone();
    window.on_use_jack(move || s(UiAction::UseJack));
    let s = send.clone();
    window.on_forget_speaker(move || s(UiAction::ForgetSpeaker));
    let s = send.clone();
    window.on_toggle_list_view(move || s(UiAction::ToggleListView));
    let s = send.clone();
    window.on_open_book(move |id| s(UiAction::OpenBook(id.to_string())));
    let s = send.clone();
    window.on_set_books_enabled(move |b| s(UiAction::SetBooksEnabled(b)));
    let s = send.clone();
    window.on_edit_books_server(move || s(UiAction::EditBooksServer));
    let s = send.clone();
    window.on_books_sign_in(move || s(UiAction::BooksSignIn));
    let s = send.clone();
    window.on_books_sign_out(move || s(UiAction::BooksSignOut));
    let s = send.clone();
    window.on_listen(move || s(UiAction::Listen));
    let s = send.clone();
    window.on_create_playlist(move || s(UiAction::NewEmptyPlaylist));
    let s = send.clone();
    window.on_set_recent_songs(move |songs| s(UiAction::SetRecentSongs(songs)));
    let s = send.clone();
    window.on_play_recent(move |i| s(UiAction::PlayRecentSong(i.max(0) as usize)));
    let s = send.clone();
    window.on_set_theme(move |i| s(UiAction::SetTheme(i.max(0) as u32)));
    let s = send.clone();
    window.on_open_sleep_timer(move || s(UiAction::TapSleepTimer));
    let s = send.clone();
    window.on_set_sleep_length(move |minutes| s(UiAction::SetSleepLength(minutes.max(1) as u32)));
    let s = send.clone();
    window.on_open_album_artist(move |uri| s(UiAction::OpenAlbumArtist(uri.to_string())));
    let s = send.clone();
    window.on_open_playing_artist(move || s(UiAction::OpenPlayingArtist));
    let s = send.clone();
    window.on_toggle_like(move || s(UiAction::ToggleLike));
    let s = send.clone();
    window.on_keyboard_done(move || s(UiAction::KeyboardDone));
    let s = send.clone();
    window.on_open_picker(move |uri| s(UiAction::OpenPicker(uri.to_string())));
    let s = send.clone();
    window.on_close_picker(move || s(UiAction::ClosePicker));
    let s = send.clone();
    window.on_pick_playlist(move |uri| s(UiAction::PickPlaylist(uri.to_string())));
    let s = send.clone();
    window.on_new_playlist(move || s(UiAction::NewPlaylist));
    let s = send.clone();
    window.on_cancel_name(move || s(UiAction::CancelText));
    let s = send.clone();
    window.on_edit_playlist(move |uri| s(UiAction::EditPlaylist(uri.to_string())));
    let s = send.clone();
    window.on_finish_editing(move || s(UiAction::FinishEditing));
    let s = send.clone();
    window.on_rename_playlist(move || s(UiAction::RenamePlaylist));
    let s = send.clone();
    window.on_ask_delete(move || s(UiAction::AskDelete));
    let s = send.clone();
    window.on_confirm_delete_playlist(move || s(UiAction::ConfirmDelete));
    let s = send.clone();
    window.on_cancel_delete(move || s(UiAction::CancelDelete));
    let s = send.clone();
    window.on_remove_track(move |i| s(UiAction::RemoveTrack(i.max(0) as usize)));
    let s = send;
    window.on_move_track(move |from, to| {
        s(UiAction::MoveTrack {
            from: from.max(0) as usize,
            to: to.max(0) as usize,
        })
    });
}

impl Bridge {
    fn render(&mut self) {
        let Some(state) = self.last.as_ref() else {
            return;
        };
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let v = view::build(state);
        self.images.begin_render();

        w.set_screen(match v.screen {
            ScreenView::Grid => ScreenKind::Grid,
            ScreenView::Detail => ScreenKind::Detail,
            ScreenView::NowPlaying => ScreenKind::NowPlaying,
            ScreenView::Search => ScreenKind::Search,
            ScreenView::Artist => ScreenKind::Artist,
            ScreenView::Settings => ScreenKind::Settings,
            ScreenView::Books => ScreenKind::Books,
            ScreenView::Book => ScreenKind::Book,
        });
        w.set_section(v.section.index());
        w.set_grid_title(v.grid_title.as_str().into());
        let tiles: Vec<TileData> = v
            .grid
            .iter()
            .map(|t| tile_data(&mut self.images, t))
            .collect();
        sync(&self.tiles, tiles);
        w.set_grid_state(load_state(v.grid_status));

        if let Some(detail) = &v.detail {
            w.set_detail(tile_data(&mut self.images, &detail.header));
            sync(&self.tracks, detail.tracks.iter().map(track_data).collect());
            w.set_detail_state(load_state(detail.status));
            w.set_detail_editable(detail.editable);
            w.set_detail_editing(detail.editing);
            w.set_detail_can_add(detail.can_add);
            w.set_detail_summary(detail.summary.as_str().into());
            w.set_detail_saveable(detail.saveable);
            w.set_detail_saved(detail.saved);
            w.set_detail_artist_link(detail.artist_link);
        }

        let art = self.images.get(&large(&v.now.image_url));
        w.set_mini_visible(v.mini_visible);
        w.set_track_title(v.now.title.as_str().into());
        w.set_track_artists(v.now.artists.as_str().into());
        w.set_track_has_art(art.is_some());
        w.set_track_art(art.unwrap_or_default());
        w.set_playing(v.now.playing);
        w.set_loading(v.now.loading);
        w.set_progress(v.now.progress);
        w.set_position_text(v.now.position.as_str().into());
        w.set_duration_text(v.now.duration.as_str().into());
        w.set_volume(v.now.volume as i32);
        w.set_shuffle(v.now.shuffle);
        w.set_repeat(v.now.repeat.index());
        w.set_banner_at_bottom(v.banner_at_bottom);
        w.set_banner(v.banner.unwrap_or_default().into());
        w.set_voice_status(v.voice_status.as_str().into());
        w.set_voice_listening(v.voice_listening);
        w.set_voice_enabled(v.voice_enabled);
        w.set_display_mode(match v.display {
            DisplayMode::Active => 0,
            DisplayMode::Dim => 1,
            DisplayMode::Off => 2,
        });
        w.set_clock(chrono::Local::now().format("%-I:%M").to_string().into());
        w.set_show_clock(v.show_clock);
        w.set_auth_needed(v.auth_needed);
        w.set_pairing_needed(v.pairing_needed);

        // Search rows and artist albums are built only while shown, so their covers are
        // requested only then.
        w.set_keyboard_open(v.keyboard);
        w.set_now_track_uri(v.now.track_uri.as_str().into());
        w.set_now_liked(v.now.liked);
        w.set_now_has_artist(v.now.has_artist);
        w.set_sleep_left(v.now.sleep_left.as_str().into());
        w.set_list_view(v.list_view);
        w.set_recent_chips(v.recent_chips);
        w.set_can_create_playlist(v.can_create_playlist);
        w.set_books_enabled(v.books_enabled);
        w.set_books_query(v.books.query.as_str().into());
        w.set_books_state(load_state(v.books.status));
        w.set_books_message(v.books.message.as_str().into());
        if v.screen == ScreenView::Books {
            let rows = v
                .books
                .rows
                .iter()
                .map(|r| search_row_data(&mut self.images, r))
                .collect();
            sync(&self.books_rows, rows);
        }
        if let Some(book) = &v.book {
            let cover = self.images.get(&large(&book.cover_url));
            w.set_book_title(book.title.as_str().into());
            w.set_book_author(book.author.as_str().into());
            w.set_book_narrator(book.narrator.as_str().into());
            w.set_book_has_cover(cover.is_some());
            w.set_book_cover(cover.unwrap_or_default());
            w.set_book_info(book.info.as_str().into());
            w.set_book_state(load_state(book.status));
            let chapters: Vec<ChapterData> = book
                .chapters
                .iter()
                .map(|(title, start)| ChapterData {
                    title: title.as_str().into(),
                    start: start.as_str().into(),
                })
                .collect();
            w.set_book_chapters(ModelRc::new(VecModel::from(chapters)));
        }
        let s = &v.settings;
        w.set_books_url(s.books_url.as_str().into());
        w.set_books_user(s.books_user.as_str().into());
        w.set_books_settings_message(s.books_message.as_str().into());
        w.set_recent_songs(v.recent_songs);
        if v.recent_songs && v.recent_chips {
            let now = chrono::Local::now();
            let rows = v
                .recent_song_rows
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    let image = self.images.get(&r.image_url);
                    TileData {
                        // The index into history, for PlayRecentSong.
                        uri: i.to_string().into(),
                        title: r.title.as_str().into(),
                        subtitle: format!("{} · {}", r.artists, played_ago(r.played_at, now))
                            .into(),
                        has_image: image.is_some(),
                        image: image.unwrap_or_default(),
                    }
                })
                .collect();
            sync(&self.recent_songs, rows);
        }
        w.global::<Theme>().set_scheme(v.theme as i32);
        w.set_picker_open(v.picker.is_some());
        if let Some(picker) = &v.picker {
            let rows = picker
                .playlists
                .iter()
                .map(|t| tile_data(&mut self.images, t))
                .collect();
            sync(&self.picker_playlists, rows);
        }
        w.set_name_open(v.text_entry.is_some());
        if let Some(entry) = &v.text_entry {
            w.set_name_title(entry.title.as_str().into());
            w.set_name_text(entry.text.as_str().into());
        }
        w.set_confirm_delete(
            v.confirm_delete
                .as_deref()
                .map(|name| {
                    if name.is_empty() {
                        "this playlist"
                    } else {
                        name
                    }
                })
                .unwrap_or_default()
                .into(),
        );
        w.set_search_query(v.search.query.as_str().into());
        w.set_search_state(load_state(v.search.status));
        w.set_search_note(v.search.note.as_str().into());
        w.set_search_filter(v.search.filter.index());
        if v.screen == ScreenView::Search {
            let rows = v
                .search
                .rows
                .iter()
                .map(|r| search_row_data(&mut self.images, r))
                .collect();
            sync(&self.search_rows, rows);
        }
        if let Some(artist) = v.artist.as_ref().filter(|_| v.screen == ScreenView::Artist) {
            w.set_artist(tile_data(&mut self.images, &artist.header));
            let albums = artist
                .albums
                .iter()
                .map(|t| tile_data(&mut self.images, t))
                .collect();
            sync(&self.artist_albums, albums);
            w.set_artist_state(load_state(artist.status));
            w.set_artist_followed(artist.followed);
            w.set_artist_back_label(artist.back_label.as_str().into());
        }
        let s = &v.settings;
        w.set_account_name(s.account_name.as_str().into());
        w.set_account_id(s.account_id.as_str().into());
        w.set_device_name(s.device_name.as_str().into());
        w.set_output_name(s.output.as_str().into());
        w.set_on_speaker(s.on_speaker);
        w.set_speaker_connected(s.speaker_connected);
        w.set_bluetooth(s.bluetooth);
        w.set_scanning(s.scanning);
        w.set_settings_message(s.message.as_str().into());
        w.set_sleep_minutes(s.sleep_minutes as i32);
        w.set_bluetooth_note(s.bluetooth_note.as_str().into());
        w.set_restarting(s.restarting);
        w.set_temperature(s.temperature.as_str().into());
        w.set_temperature_hot(s.temperature_hot);
        w.set_wifi(s.wifi.as_str().into());
        w.set_wifi_weak(s.wifi_weak);
        w.set_powering_off(s.powering_off);
        let speakers: Vec<SpeakerRowData> = s
            .speakers
            .iter()
            .map(|r| SpeakerRowData {
                address: r.address.as_str().into(),
                name: r.name.as_str().into(),
                status: r.status.as_str().into(),
            })
            .collect();
        w.set_speakers(ModelRc::new(VecModel::from(speakers)));
        w.set_on_sonos(s.on_sonos);
        let rooms: Vec<SpeakerRowData> = s
            .sonos_rooms
            .iter()
            .map(|r| SpeakerRowData {
                address: r.address.as_str().into(),
                name: r.name.as_str().into(),
                status: r.status.as_str().into(),
            })
            .collect();
        w.set_sonos_rooms(ModelRc::new(VecModel::from(rooms)));
    }
}

/// Every image URL the view shows: grid tiles, the detail header and the now-playing art.
/// "just now", "12 min ago", "3 h ago", "Yesterday", or a date like "Oct 2".
fn played_ago(played_at: i64, now: chrono::DateTime<chrono::Local>) -> String {
    use chrono::TimeZone;
    let Some(then) = chrono::Local.timestamp_opt(played_at, 0).single() else {
        return String::new();
    };
    let minutes = (now - then).num_minutes();
    if minutes < 1 {
        "just now".into()
    } else if minutes < 60 {
        format!("{minutes} min ago")
    } else if minutes < 24 * 60 && then.date_naive() == now.date_naive() {
        format!("{} h ago", minutes / 60)
    } else if now.date_naive().pred_opt() == Some(then.date_naive()) {
        "Yesterday".into()
    } else {
        then.format("%b %-d").to_string()
    }
}

fn search_row_data(images: &mut Images, r: &SearchRowView) -> SearchRowData {
    let image = images.get(&r.image_url);
    SearchRowData {
        kind: match r.kind {
            RowKind::Header => 0,
            RowKind::Song => 1,
            RowKind::Album => 2,
            RowKind::Artist => 3,
            RowKind::Playlist => 4,
            RowKind::Book => 5,
        },
        title: r.title.as_str().into(),
        subtitle: r.subtitle.as_str().into(),
        has_image: image.is_some(),
        image: image.unwrap_or_default(),
        uri: r.uri.as_str().into(),
    }
}

fn tile_data(images: &mut Images, t: &TileView) -> TileData {
    let image = images.get(&t.image_url);
    TileData {
        uri: t.uri.as_str().into(),
        title: t.title.as_str().into(),
        subtitle: t.subtitle.as_str().into(),
        has_image: image.is_some(),
        image: image.unwrap_or_default(),
    }
}

fn track_data(t: &TrackRowView) -> TrackData {
    TrackData {
        uri: t.uri.as_str().into(),
        title: t.title.as_str().into(),
        artists: t.artists.as_str().into(),
        duration: t.duration.as_str().into(),
        current: t.current,
    }
}

fn load_state(status: LoadStatus) -> LoadState {
    match status {
        LoadStatus::Loading => LoadState::Loading,
        LoadStatus::Empty => LoadState::Empty,
        LoadStatus::Failed => LoadState::Failed,
        LoadStatus::Forbidden => LoadState::Forbidden,
        LoadStatus::Limited => LoadState::Limited,
        LoadStatus::Ready => LoadState::Ready,
    }
}

/// Updates only changed rows so scroll positions and images don't reset.
fn sync<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
    if model.row_count() != rows.len() {
        model.set_vec(rows);
        return;
    }
    for (i, row) in rows.into_iter().enumerate() {
        if model.row_data(i).as_ref() != Some(&row) {
            model.set_row_data(i, row);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(range: std::ops::Range<usize>) -> Vec<String> {
        range
            .map(|i| format!("https://i.scdn.co/image/{i}"))
            .collect()
    }

    #[test]
    fn eviction_drops_oldest_but_skips_live() {
        let mut order: VecDeque<String> = urls(0..5).into();
        let live: HashSet<String> = urls(0..2).into_iter().collect();
        let evicted = evict(&mut order, &live, 3);
        assert_eq!(evicted, urls(2..4));
        assert_eq!(Vec::from(order), [urls(0..2), urls(4..5)].concat());
    }

    #[test]
    fn eviction_keeps_everything_when_all_live() {
        let mut order: VecDeque<String> = urls(0..5).into();
        let live: HashSet<String> = urls(0..5).into_iter().collect();
        assert!(evict(&mut order, &live, 3).is_empty());
        assert_eq!(order.len(), 5);
    }

    // A grid with more covers than the cache holds must not reload covers in a loop.
    #[test]
    fn live_covers_over_the_limit_are_not_requested_again() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut images = Images {
            ready: HashMap::new(),
            order: VecDeque::new(),
            requested: HashSet::new(),
            live: HashSet::new(),
            requests: tx,
        };
        // Any screen, the add-to-playlist sheet included, that shows more covers than
        // the cache holds: whatever a render asks for is live, with no list to forget.
        let all = urls(0..IMAGE_CACHE_LIMIT + 1);
        images.begin_render();
        for url in &all {
            assert!(images.get(&Some(url.clone())).is_none());
        }
        for url in &all {
            images.insert(url.clone(), Image::default());
            // Each delivery re-renders the view.
            images.begin_render();
            for u in &all {
                images.get(&Some(u.clone()));
            }
        }
        let mut sent = 0;
        while rx.try_recv().is_ok() {
            sent += 1;
        }
        assert_eq!(sent, all.len(), "each cover requested once");
        assert!(all.iter().all(|u| images.ready.contains_key(u)));
    }
}
