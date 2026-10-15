use super::*;
use crate::model::{
    Account, Collection, CollectionKind, EditOutcome, LIKED_URI, PlayRecord, PlaylistEdit,
    SearchResults, Section, Track,
};
use crate::settings::{Output, Speaker};

pub(crate) fn config() -> CoreConfig {
    CoreConfig {
        dim_after_ms: 60_000,
        off_after_ms: 120_000,
        initial_volume: 50,
        device_name: "Ziggy Test".into(),
        speaker: None,
        saved: Default::default(),
        bluetooth: true,
        books_user: None,
        books_config_url: None,
        voice: true,
    }
}

pub(crate) fn core() -> Core {
    Core::new(config(), 0).0
}

pub(crate) fn playlist(n: u32) -> Collection {
    Collection {
        uri: format!("spotify:playlist:p{n}"),
        kind: CollectionKind::Playlist,
        name: format!("Playlist {n}"),
        subtitle: "Mum".into(),
        image_url: Some(format!("https://i.scdn.co/image/p{n}")),
        ..Default::default()
    }
}

pub(crate) fn track(n: u32) -> Track {
    Track {
        uri: format!("spotify:track:t{n}"),
        name: format!("Song {n}"),
        artists: "Band".into(),
        album: "Album".into(),
        image_url: None,
        duration_ms: 180_000,
        album_uri: Some("spotify:album:a1".into()),
        artist_uri: Some("spotify:artist:r1".into()),
    }
}

pub(crate) fn ui(action: UiAction) -> Input {
    Input::Ui(action)
}

pub(crate) fn with_tracks(core: &mut Core, uri: &str, n: u32) {
    core.handle(
        Input::Library(LibraryUpdate::Tracks {
            collection_uri: uri.into(),
            tracks: (0..n).map(track).collect(),
        }),
        0,
    );
}

#[test]
fn new_requests_initial_sections_and_shows_albums() {
    let (core, effects) = Core::new(config(), 0);
    assert_eq!(core.state().screen, Screen::Grid(Section::Albums));
    assert_eq!(
        effects,
        vec![
            Effect::Library(LibraryRequest::Section(Section::Playlists)),
            Effect::Library(LibraryRequest::Section(Section::Albums)),
            Effect::History(HistoryCommand::LoadRecent),
            Effect::Library(LibraryRequest::Tracks {
                collection_uri: LIKED_URI.into()
            }),
            Effect::Library(LibraryRequest::Account),
            Effect::Display(DisplayMode::Active),
        ]
    );
    assert!(core.state().sections[&Section::Playlists].loading);
    assert_eq!(core.state().playback.volume, 50);
}

#[test]
fn show_section_switches_grid_and_clears_back_stack() {
    let mut c = core();
    c.handle(
        ui(UiAction::OpenCollection("spotify:playlist:p1".into())),
        0,
    );
    let fx = c.handle(ui(UiAction::ShowSection(Section::Albums)), 0);
    assert_eq!(c.state().screen, Screen::Grid(Section::Albums));
    assert_eq!(c.state().section, Section::Albums);
    assert!(c.state().back_stack.is_empty());
    assert_eq!(
        fx,
        vec![Effect::Library(LibraryRequest::Section(Section::Albums))]
    );
}

#[test]
fn show_liked_opens_liked_detail() {
    let mut c = core();
    let fx = c.handle(ui(UiAction::ShowSection(Section::Liked)), 0);
    assert_eq!(c.state().screen, Screen::Detail(LIKED_URI.into()));
    assert_eq!(c.state().section, Section::Liked);
    assert_eq!(
        fx,
        vec![Effect::Library(LibraryRequest::Tracks {
            collection_uri: LIKED_URI.into()
        })]
    );
}

#[test]
fn open_collection_then_back() {
    let mut c = core();
    let fx = c.handle(
        ui(UiAction::OpenCollection("spotify:playlist:p1".into())),
        0,
    );
    assert_eq!(
        c.state().screen,
        Screen::Detail("spotify:playlist:p1".into())
    );
    assert!(c.state().tracks["spotify:playlist:p1"].loading);
    assert_eq!(
        fx,
        vec![Effect::Library(LibraryRequest::Tracks {
            collection_uri: "spotify:playlist:p1".into()
        })]
    );
    c.handle(ui(UiAction::Back), 0);
    assert_eq!(c.state().screen, Screen::Grid(Section::Albums));
}

#[test]
fn back_on_empty_stack_is_noop() {
    let mut c = core();
    let fx = c.handle(ui(UiAction::Back), 0);
    assert!(fx.is_empty());
    assert_eq!(c.state().screen, Screen::Grid(Section::Albums));
}

#[test]
fn section_update_stores_data_and_marks_online() {
    let mut c = core();
    c.handle(
        Input::Library(LibraryUpdate::SectionFailed {
            section: Section::Albums,
            reason: FailReason::Offline,
        }),
        0,
    );
    assert!(!c.state().online);
    c.handle(
        Input::Library(LibraryUpdate::Section {
            section: Section::Playlists,
            items: vec![playlist(1)],
        }),
        0,
    );
    let slot = &c.state().sections[&Section::Playlists];
    assert_eq!(slot.data.as_deref(), Some(&vec![playlist(1)]));
    assert!(!slot.loading && !slot.failed);
    assert!(c.state().online);
}

#[test]
fn failure_keeps_cached_data_and_only_marks_failed_without_data() {
    let mut c = core();
    c.handle(
        Input::Library(LibraryUpdate::Section {
            section: Section::Playlists,
            items: vec![playlist(1)],
        }),
        0,
    );
    c.handle(
        Input::Library(LibraryUpdate::SectionFailed {
            section: Section::Playlists,
            reason: FailReason::Other,
        }),
        0,
    );
    let slot = &c.state().sections[&Section::Playlists];
    assert!(slot.data.is_some());
    assert!(!slot.failed);

    c.handle(
        Input::Library(LibraryUpdate::SectionFailed {
            section: Section::Albums,
            reason: FailReason::Other,
        }),
        0,
    );
    assert!(c.state().sections[&Section::Albums].failed);
}

// Ruling R11: only the player's login rejection shows the sign-in screen.
#[test]
fn only_player_auth_invalid_sets_auth_needed() {
    let mut c = core();
    c.handle(
        Input::Library(LibraryUpdate::TracksFailed {
            collection_uri: "x".into(),
            reason: FailReason::Auth,
        }),
        0,
    );
    assert!(!c.state().auth_needed);
    assert!(c.state().tracks["x"].failed, "shows Can't load this");
    c.handle(
        Input::Library(LibraryUpdate::SectionFailed {
            section: Section::Albums,
            reason: FailReason::Auth,
        }),
        0,
    );
    assert!(!c.state().auth_needed);
    assert!(c.state().online);
    let mut c = core();
    c.handle(Input::AuthInvalid, 0);
    assert!(c.state().auth_needed);
}

#[test]
fn tracks_update_stores_data() {
    let mut c = core();
    with_tracks(&mut c, "spotify:playlist:p1", 3);
    let slot = &c.state().tracks["spotify:playlist:p1"];
    assert_eq!(slot.data.as_ref().map(|t| t.len()), Some(3));
}

fn load(uri: &str, start: Option<u32>, shuffle: bool) -> Effect {
    Effect::Player(PlayerCommand::Load {
        context_uri: uri.into(),
        start_index: start,
        start_uri: None,
        shuffle,
    })
}

#[test]
fn play_collection_is_optimistic() {
    let mut c = core();
    with_tracks(&mut c, "spotify:playlist:p1", 3);
    c.handle(
        ui(UiAction::OpenCollection("spotify:playlist:p1".into())),
        0,
    );
    let fx = c.handle(
        ui(UiAction::PlayCollection {
            uri: "spotify:playlist:p1".into(),
            shuffle: false,
        }),
        0,
    );
    assert_eq!(fx, vec![load("spotify:playlist:p1", Some(0), false)]);
    let pb = &c.state().playback;
    assert_eq!(pb.status, PlayStatus::Loading);
    assert_eq!(pb.track, Some(track(0)));
    assert_eq!(pb.context_uri.as_deref(), Some("spotify:playlist:p1"));
    assert_eq!(c.state().screen, Screen::NowPlaying);
    c.handle(ui(UiAction::Back), 0);
    assert_eq!(
        c.state().screen,
        Screen::Detail("spotify:playlist:p1".into())
    );
}

#[test]
fn play_collection_shuffled_has_no_known_first_track() {
    let mut c = core();
    with_tracks(&mut c, "spotify:playlist:p1", 3);
    let fx = c.handle(
        ui(UiAction::PlayCollection {
            uri: "spotify:playlist:p1".into(),
            shuffle: true,
        }),
        0,
    );
    assert_eq!(fx, vec![load("spotify:playlist:p1", None, true)]);
    assert_eq!(c.state().playback.track, None);
    assert!(c.state().playback.shuffle);
}

// Review focus 1: double taps.
#[test]
fn double_tap_play_sends_one_load() {
    let mut c = core();
    let play = UiAction::PlayCollection {
        uri: "spotify:playlist:p1".into(),
        shuffle: false,
    };
    let first = c.handle(ui(play.clone()), 10_000);
    let second = c.handle(ui(play.clone()), 10_400);
    assert_eq!(first, vec![load("spotify:playlist:p1", Some(0), false)]);
    assert!(second.is_empty());
    assert_eq!(c.state().back_stack.len(), 1, "no duplicate navigation");
    let third = c.handle(ui(play), 11_500);
    assert_eq!(third, vec![load("spotify:playlist:p1", Some(0), false)]);
}

#[test]
fn play_track_uses_index_and_current_shuffle() {
    let mut c = core();
    with_tracks(&mut c, "spotify:album:a1", 5);
    c.handle(Input::Player(PlayerUpdate::Shuffle(true)), 0);
    let fx = c.handle(
        ui(UiAction::PlayTrack {
            collection_uri: "spotify:album:a1".into(),
            index: 3,
        }),
        0,
    );
    assert_eq!(fx, vec![load("spotify:album:a1", Some(3), true)]);
    assert_eq!(c.state().playback.track, Some(track(3)));
}

#[test]
fn toggle_play_flips_status_optimistically() {
    let mut c = core();
    assert!(
        c.handle(ui(UiAction::TogglePlay), 0).is_empty(),
        "nothing loaded"
    );
    c.handle(
        ui(UiAction::PlayCollection {
            uri: "spotify:playlist:p1".into(),
            shuffle: false,
        }),
        0,
    );
    c.handle(Input::Player(PlayerUpdate::Playing { position_ms: 0 }), 0);
    let fx = c.handle(ui(UiAction::TogglePlay), 0);
    assert_eq!(fx, vec![Effect::Player(PlayerCommand::Pause)]);
    assert_eq!(c.state().playback.status, PlayStatus::Paused);
    let fx = c.handle(ui(UiAction::TogglePlay), 0);
    assert_eq!(fx, vec![Effect::Player(PlayerCommand::Play)]);
    assert_eq!(c.state().playback.status, PlayStatus::Playing);
}

#[test]
fn player_updates_change_playback() {
    let mut c = core();
    c.handle(Input::Player(PlayerUpdate::TrackChanged(track(7))), 0);
    c.handle(
        Input::Player(PlayerUpdate::Playing { position_ms: 1_000 }),
        0,
    );
    c.handle(
        Input::Player(PlayerUpdate::Position { position_ms: 5_000 }),
        0,
    );
    c.handle(Input::Player(PlayerUpdate::Volume { percent: 120 }), 0);
    c.handle(
        Input::Player(PlayerUpdate::Repeat(crate::model::Repeat::Track)),
        0,
    );
    let pb = &c.state().playback;
    assert_eq!(pb.track, Some(track(7)));
    assert_eq!(pb.status, PlayStatus::Playing);
    assert_eq!(pb.position_ms, 5_000);
    assert_eq!(pb.volume, 100);
    assert_eq!(pb.repeat, crate::model::Repeat::Track);
    c.handle(
        Input::Player(PlayerUpdate::Paused { position_ms: 6_000 }),
        0,
    );
    assert_eq!(c.state().playback.status, PlayStatus::Paused);
    c.handle(Input::Player(PlayerUpdate::Stopped), 0);
    assert_eq!(c.state().playback.status, PlayStatus::Stopped);
}

#[test]
fn controls_emit_commands() {
    let mut c = core();
    c.handle(Input::Player(PlayerUpdate::TrackChanged(track(1))), 0);
    assert_eq!(
        c.handle(ui(UiAction::Next), 0),
        vec![Effect::Player(PlayerCommand::Next)]
    );
    assert_eq!(
        c.handle(ui(UiAction::Previous), 0),
        vec![Effect::Player(PlayerCommand::Previous)]
    );
    assert_eq!(
        c.handle(
            ui(UiAction::Seek {
                position_ms: 999_999
            }),
            0
        ),
        vec![Effect::Player(PlayerCommand::Seek {
            position_ms: 180_000
        })],
        "seek is clamped to the track length"
    );
    assert_eq!(
        c.handle(ui(UiAction::SetVolume { percent: 150 }), 0),
        vec![Effect::Player(PlayerCommand::SetVolume { percent: 100 })]
    );
    assert_eq!(
        c.handle(ui(UiAction::ToggleShuffle), 0),
        vec![Effect::Player(PlayerCommand::SetShuffle(true))]
    );
    assert_eq!(
        c.handle(ui(UiAction::CycleRepeat), 0),
        vec![Effect::Player(PlayerCommand::SetRepeat(
            crate::model::Repeat::Context
        ))]
    );
}

#[test]
fn open_now_playing_needs_something_playing() {
    let mut c = core();
    c.handle(ui(UiAction::OpenNowPlaying), 0);
    assert_eq!(c.state().screen, Screen::Grid(Section::Albums));
    c.handle(Input::Player(PlayerUpdate::TrackChanged(track(1))), 0);
    c.handle(ui(UiAction::OpenNowPlaying), 0);
    assert_eq!(c.state().screen, Screen::NowPlaying);
}

#[test]
fn unavailable_shows_notice_that_expires() {
    let mut c = core();
    c.handle(Input::Player(PlayerUpdate::Unavailable), 1_000);
    assert_eq!(c.state().notice, Some(Notice::TrackUnavailable));
    c.handle(Input::Tick, 3_000);
    assert_eq!(c.state().notice, Some(Notice::TrackUnavailable));
    c.handle(Input::Tick, 5_100);
    assert_eq!(c.state().notice, None);
}

#[test]
fn play_while_offline_shows_notice_but_still_loads() {
    let mut c = core();
    c.handle(Input::Player(PlayerUpdate::Disconnected), 0);
    let fx = c.handle(
        ui(UiAction::PlayCollection {
            uri: "spotify:playlist:p1".into(),
            shuffle: false,
        }),
        0,
    );
    assert_eq!(fx, vec![load("spotify:playlist:p1", Some(0), false)]);
    assert_eq!(c.state().notice, Some(Notice::NoInternet));
}

#[test]
fn idle_dims_then_turns_off_and_touch_wakes() {
    let mut c = core();
    assert!(c.handle(Input::Tick, 59_000).is_empty());
    assert_eq!(
        c.handle(Input::Tick, 60_000),
        vec![Effect::Display(DisplayMode::Dim)]
    );
    assert_eq!(
        c.handle(Input::Tick, 120_000),
        vec![Effect::Display(DisplayMode::Off)]
    );
    assert_eq!(c.state().display, DisplayMode::Off);
    let fx = c.handle(ui(UiAction::Touch), 121_000);
    assert_eq!(fx, vec![Effect::Display(DisplayMode::Active)]);
    assert!(c.handle(Input::Tick, 122_000).is_empty());
}

#[test]
fn playing_keeps_screen_awake_and_wakes_it() {
    let mut c = core();
    c.handle(Input::Tick, 60_000);
    assert_eq!(c.state().display, DisplayMode::Dim);
    let fx = c.handle(
        Input::Player(PlayerUpdate::Playing { position_ms: 0 }),
        61_000,
    );
    assert_eq!(fx, vec![Effect::Display(DisplayMode::Active)]);
    assert!(c.handle(Input::Tick, 500_000).is_empty());
    assert_eq!(c.state().display, DisplayMode::Active);
}

// Review focus 5: Wi-Fi down at boot.
#[test]
fn reconnect_rerequests_library_and_open_detail() {
    let mut c = core();
    c.handle(
        Input::Library(LibraryUpdate::SectionFailed {
            section: Section::Playlists,
            reason: FailReason::Offline,
        }),
        0,
    );
    c.handle(ui(UiAction::OpenCollection("spotify:album:a1".into())), 0);
    let fx = c.handle(Input::Player(PlayerUpdate::Connected), 0);
    assert!(c.state().online);
    assert_eq!(
        fx,
        vec![
            Effect::Library(LibraryRequest::Section(Section::Playlists)),
            Effect::Library(LibraryRequest::Section(Section::Albums)),
            Effect::History(HistoryCommand::LoadRecent),
            Effect::Library(LibraryRequest::Tracks {
                collection_uri: "spotify:album:a1".into()
            }),
            Effect::Library(LibraryRequest::Account),
        ]
    );
}

#[test]
fn account_is_stored_and_not_rerequested_on_reconnect() {
    let mut c = core();
    let account = Account {
        id: "1255644".into(),
        name: "Sam".into(),
    };
    c.handle(Input::Library(LibraryUpdate::Account(account.clone())), 0);
    assert_eq!(c.state().account, Some(account));
    let fx = c.handle(Input::Player(PlayerUpdate::Connected), 0);
    assert!(!fx.contains(&Effect::Library(LibraryRequest::Account)));
}

#[test]
fn disconnect_pauses_and_marks_offline() {
    let mut c = core();
    c.handle(Input::Player(PlayerUpdate::Playing { position_ms: 0 }), 0);
    c.handle(Input::Player(PlayerUpdate::Disconnected), 0);
    assert!(!c.state().online);
    assert_eq!(c.state().playback.status, PlayStatus::Paused);
}

#[test]
fn speaker_status_is_tracked() {
    let mut c = core();
    c.handle(Input::Speaker { connected: false }, 0);
    assert!(!c.state().speaker_connected);
}

#[test]
fn reconnect_reissues_a_pending_load() {
    let mut c = core();
    c.handle(Input::Player(PlayerUpdate::Disconnected), 0);
    c.handle(
        ui(UiAction::PlayCollection {
            uri: "spotify:playlist:p1".into(),
            shuffle: true,
        }),
        0,
    );
    assert_eq!(c.state().playback.status, PlayStatus::Loading);
    let fx = c.handle(Input::Player(PlayerUpdate::Connected), 5_000);
    assert!(fx.contains(&load("spotify:playlist:p1", None, true)));

    // Nothing pending once playback started.
    c.handle(
        Input::Player(PlayerUpdate::Playing { position_ms: 0 }),
        6_000,
    );
    c.handle(Input::Player(PlayerUpdate::Disconnected), 7_000);
    let fx = c.handle(Input::Player(PlayerUpdate::Connected), 8_000);
    assert!(
        !fx.iter()
            .any(|e| matches!(e, Effect::Player(PlayerCommand::Load { .. })))
    );
}

// ---- Search and artists ----

fn searching() -> Core {
    let mut c = core();
    c.handle(ui(UiAction::ShowSection(Section::Search)), 0);
    c
}

fn type_text(c: &mut Core, text: &str, at_ms: u64) {
    c.handle(ui(UiAction::KeyPressed(text.into())), at_ms);
}

fn search_requests(fx: &[Effect]) -> Vec<String> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Library(LibraryRequest::Search(q)) => Some(q.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn search_section_opens_with_keyboard() {
    let c = searching();
    assert_eq!(c.state().screen, Screen::Search);
    assert_eq!(c.state().section, Section::Search);
    assert!(c.state().keyboard_open);
    assert!(c.wants_search_tick());
}

#[test]
fn search_waits_for_a_pause_in_typing_and_sends_once() {
    let mut c = searching();
    type_text(&mut c, "a", 1_000);
    type_text(&mut c, "b", 1_100);
    assert!(search_requests(&c.handle(Input::SearchTick, 1_499)).is_empty());
    assert_eq!(search_requests(&c.handle(Input::SearchTick, 1_500)), ["ab"]);
    assert!(search_requests(&c.handle(Input::SearchTick, 2_000)).is_empty());
    assert!(c.state().search.results.loading);
}

#[test]
fn query_is_trimmed_and_whitespace_only_sends_nothing() {
    let mut c = searching();
    type_text(&mut c, " ", 0);
    assert!(search_requests(&c.handle(Input::SearchTick, 1_000)).is_empty());
    type_text(&mut c, "abba ", 1_000);
    assert_eq!(
        search_requests(&c.handle(Input::SearchTick, 2_000)),
        ["abba"]
    );
}

#[test]
fn clearing_drops_results_without_a_request() {
    let mut c = searching();
    type_text(&mut c, "ab", 0);
    c.handle(Input::SearchTick, 1_000);
    let fx = c.handle(ui(UiAction::ClearSearch), 1_100);
    assert!(search_requests(&fx).is_empty());
    assert_eq!(c.state().search.query, "");
    assert_eq!(c.state().search.results, Slot::default());
}

#[test]
fn stale_results_are_dropped_and_current_ones_kept() {
    let mut c = searching();
    type_text(&mut c, "ab", 0);
    c.handle(Input::SearchTick, 1_000);
    let results = |name: &str| {
        Input::Library(LibraryUpdate::SearchResults {
            query: name.into(),
            results: SearchResults {
                playlists: vec![playlist(1)],
                ..Default::default()
            },
        })
    };
    c.handle(results("a"), 1_000);
    assert!(c.state().search.results.data.is_none());
    c.handle(results("ab"), 1_000);
    assert!(c.state().search.results.data.is_some());
    assert!(!c.state().search.results.loading);
}

#[test]
fn typing_is_ignored_off_the_search_screen() {
    let mut c = core();
    type_text(&mut c, "x", 0);
    assert_eq!(c.state().search.query, "");
}

#[test]
fn leaving_search_closes_the_keyboard() {
    let mut c = searching();
    c.handle(ui(UiAction::OpenCollection("spotify:album:a1".into())), 0);
    assert!(!c.state().keyboard_open);
    c.handle(ui(UiAction::Back), 0);
    assert_eq!(c.state().screen, Screen::Search);
}

#[test]
fn open_artist_requests_albums_and_back_returns_to_search() {
    let mut c = searching();
    let fx = c.handle(ui(UiAction::OpenArtist("spotify:artist:r1".into())), 0);
    assert_eq!(c.state().screen, Screen::Artist("spotify:artist:r1".into()));
    assert!(fx.contains(&Effect::Library(LibraryRequest::ArtistAlbums {
        artist_uri: "spotify:artist:r1".into()
    })));
    c.handle(ui(UiAction::Back), 0);
    assert_eq!(c.state().screen, Screen::Search);
}

#[test]
fn play_song_plays_it_within_its_album() {
    let mut c = searching();
    type_text(&mut c, "song", 0);
    c.handle(Input::SearchTick, 1_000);
    c.handle(
        Input::Library(LibraryUpdate::SearchResults {
            query: "song".into(),
            results: SearchResults {
                tracks: vec![track(7)],
                ..Default::default()
            },
        }),
        1_000,
    );
    let fx = c.handle(ui(UiAction::PlaySong("spotify:track:t7".into())), 2_000);
    assert!(fx.contains(&Effect::Player(PlayerCommand::Load {
        context_uri: "spotify:album:a1".into(),
        start_index: None,
        start_uri: Some("spotify:track:t7".into()),
        shuffle: false,
    })));
    assert_eq!(c.state().playback.track, Some(track(7)));
    assert_eq!(c.state().screen, Screen::NowPlaying);
}

#[test]
fn play_song_without_an_album_plays_the_track_itself() {
    let mut c = searching();
    let mut lonely = track(8);
    lonely.album_uri = None;
    c.state.search.results.data = Some(Arc::new(SearchResults {
        tracks: vec![lonely],
        ..Default::default()
    }));
    let fx = c.handle(ui(UiAction::PlaySong("spotify:track:t8".into())), 0);
    assert!(fx.iter().any(|e| matches!(
        e,
        Effect::Player(PlayerCommand::Load { context_uri, .. }) if context_uri == "spotify:track:t8"
    )));
}

#[test]
fn forbidden_track_list_is_marked_forbidden_not_failed() {
    let mut c = core();
    c.handle(
        Input::Library(LibraryUpdate::TracksFailed {
            collection_uri: "spotify:playlist:p9".into(),
            reason: FailReason::Forbidden,
        }),
        0,
    );
    let slot = &c.state().tracks["spotify:playlist:p9"];
    assert!(slot.forbidden && !slot.failed);
}

#[test]
fn closing_search_returns_to_the_previous_section() {
    let mut c = core();
    c.handle(ui(UiAction::ShowSection(Section::Albums)), 0);
    c.handle(ui(UiAction::ShowSection(Section::Search)), 0);
    c.handle(ui(UiAction::OpenArtist("spotify:artist:r1".into())), 0);
    c.handle(ui(UiAction::Back), 0);
    c.handle(ui(UiAction::CloseSearch), 0);
    assert_eq!(c.state().screen, Screen::Grid(Section::Albums));
    assert_eq!(c.state().section, Section::Albums);
    assert!(!c.state().keyboard_open);
}

// ---- Playlist editing ----

fn me() -> Account {
    Account {
        id: "me".into(),
        name: "Sam".into(),
    }
}

fn mine(n: u32) -> Collection {
    Collection {
        owner_id: Some("me".into()),
        snapshot_id: Some(format!("s{n}")),
        ..playlist(n)
    }
}

/// A core signed in as `me` with playlist 1 (owned, 3 songs) and playlist 2 (someone else's).
fn editor() -> Core {
    let mut c = core();
    c.handle(Input::Library(LibraryUpdate::Account(me())), 0);
    c.handle(
        Input::Library(LibraryUpdate::Section {
            section: Section::Playlists,
            items: vec![mine(1), playlist(2)],
        }),
        0,
    );
    with_tracks(&mut c, "spotify:playlist:p1", 3);
    c
}

fn edit_of(fx: &[Effect]) -> (u64, PlaylistEdit) {
    fx.iter()
        .find_map(|e| match e {
            Effect::Library(LibraryRequest::Edit { id, edit }) => Some((*id, edit.clone())),
            _ => None,
        })
        .expect("an edit")
}

fn track_names(c: &Core, uri: &str) -> Vec<String> {
    c.state().tracks[uri]
        .data
        .as_ref()
        .unwrap()
        .iter()
        .map(|t| t.name.clone())
        .collect()
}

#[test]
fn adding_a_song_shows_at_once_and_reloads_after_success() {
    let mut c = editor();
    c.state.search.results.data = Some(Arc::new(SearchResults {
        tracks: vec![track(9)],
        ..Default::default()
    }));
    c.handle(ui(UiAction::OpenPicker("spotify:track:t9".into())), 0);
    assert_eq!(c.state().picker.as_deref(), Some("spotify:track:t9"));
    let fx = c.handle(ui(UiAction::PickPlaylist("spotify:playlist:p1".into())), 0);
    let (id, edit) = edit_of(&fx);
    assert_eq!(
        edit,
        PlaylistEdit::Add {
            playlist_uri: "spotify:playlist:p1".into(),
            track_uri: "spotify:track:t9".into()
        }
    );
    assert_eq!(c.state().picker, None);
    assert_eq!(
        track_names(&c, "spotify:playlist:p1").last().unwrap(),
        "Song 9"
    );
    assert_eq!(c.state().notice, Some(Notice::AddedTo("Playlist 1".into())));
    let fx = c.handle(
        Input::Library(LibraryUpdate::EditDone {
            id,
            outcome: EditOutcome::Done,
        }),
        0,
    );
    assert!(fx.contains(&Effect::Library(LibraryRequest::ReloadTracks {
        collection_uri: "spotify:playlist:p1".into()
    })));
}

#[test]
fn a_refused_edit_is_undone_with_a_notice() {
    let mut c = editor();
    c.handle(
        ui(UiAction::OpenCollection("spotify:playlist:p1".into())),
        0,
    );
    c.handle(ui(UiAction::EditPlaylist("spotify:playlist:p1".into())), 0);
    let fx = c.handle(ui(UiAction::RemoveTrack(0)), 0);
    let (id, edit) = edit_of(&fx);
    assert_eq!(
        edit,
        PlaylistEdit::Remove {
            playlist_uri: "spotify:playlist:p1".into(),
            track_uri: "spotify:track:t0".into(),
            snapshot_id: Some("s1".into()),
        }
    );
    assert_eq!(track_names(&c, "spotify:playlist:p1"), ["Song 1", "Song 2"]);
    c.handle(
        Input::Library(LibraryUpdate::EditFailed {
            id,
            reason: FailReason::Forbidden,
        }),
        0,
    );
    assert_eq!(
        track_names(&c, "spotify:playlist:p1"),
        ["Song 0", "Song 1", "Song 2"]
    );
    assert_eq!(c.state().notice, Some(Notice::CouldntSave));
}

#[test]
fn duplicate_add_is_refused_with_a_notice() {
    let mut c = editor();
    c.handle(ui(UiAction::OpenPicker("spotify:track:t1".into())), 0);
    let fx = c.handle(ui(UiAction::PickPlaylist("spotify:playlist:p1".into())), 0);
    assert!(fx.is_empty());
    assert_eq!(
        c.state().notice,
        Some(Notice::AlreadyIn("Playlist 1".into()))
    );
}

#[test]
fn edits_are_refused_offline() {
    let mut c = editor();
    c.handle(Input::Player(PlayerUpdate::Disconnected), 0);
    c.handle(ui(UiAction::OpenPicker("spotify:track:t9".into())), 0);
    let fx = c.handle(ui(UiAction::PickPlaylist("spotify:playlist:p1".into())), 0);
    assert!(fx.is_empty());
    assert_eq!(c.state().notice, Some(Notice::NoInternet));
}

#[test]
fn only_owned_playlists_can_enter_edit_mode() {
    let mut c = editor();
    c.handle(
        ui(UiAction::OpenCollection("spotify:playlist:p2".into())),
        0,
    );
    c.handle(ui(UiAction::EditPlaylist("spotify:playlist:p2".into())), 0);
    assert_eq!(c.state().editing, None);
    c.handle(ui(UiAction::Back), 0);
    c.handle(
        ui(UiAction::OpenCollection("spotify:playlist:p1".into())),
        0,
    );
    c.handle(ui(UiAction::EditPlaylist("spotify:playlist:p1".into())), 0);
    assert_eq!(c.state().editing.as_deref(), Some("spotify:playlist:p1"));
    c.handle(ui(UiAction::Back), 0);
    assert_eq!(c.state().editing, None);
}

#[test]
fn creating_a_playlist_shows_a_placeholder_then_the_real_one() {
    let mut c = editor();
    c.handle(ui(UiAction::OpenPicker("spotify:track:t1".into())), 0);
    c.handle(ui(UiAction::NewPlaylist), 0);
    assert_eq!(c.state().picker, None);
    for key in ["R", "o", "a", "d"] {
        c.handle(ui(UiAction::KeyPressed(key.into())), 0);
    }
    c.handle(ui(UiAction::Backspace), 0);
    c.handle(ui(UiAction::KeyPressed("d".into())), 0);
    let fx = c.handle(ui(UiAction::KeyboardDone), 0);
    let (id, edit) = edit_of(&fx);
    assert_eq!(
        edit,
        PlaylistEdit::Create {
            name: "Road".into(),
            track_uri: Some("spotify:track:t1".into())
        }
    );
    assert_eq!(c.state().text_entry, None);
    let placeholder = c.state().sections[&Section::Playlists]
        .data
        .as_ref()
        .unwrap()[0]
        .clone();
    assert_eq!(placeholder.name, "Road");
    assert_eq!(track_names(&c, &placeholder.uri), ["Song 1"]);

    let real = mine(7);
    c.handle(
        Input::Library(LibraryUpdate::EditDone {
            id,
            outcome: EditOutcome::Created(real.clone()),
        }),
        0,
    );
    let lists = c.state().sections[&Section::Playlists]
        .data
        .clone()
        .unwrap();
    assert_eq!(lists[0], real);
    assert!(!c.state().tracks.contains_key(&placeholder.uri));
    assert_eq!(track_names(&c, &real.uri), ["Song 1"]);
}

#[test]
fn empty_names_are_not_saved() {
    let mut c = editor();
    c.handle(ui(UiAction::OpenPicker("spotify:track:t1".into())), 0);
    c.handle(ui(UiAction::NewPlaylist), 0);
    c.handle(ui(UiAction::KeyPressed("  ".into())), 0);
    let fx = c.handle(ui(UiAction::KeyboardDone), 0);
    assert!(fx.is_empty());
    assert!(c.state().text_entry.is_some());
    c.handle(ui(UiAction::CancelText), 0);
    assert_eq!(c.state().text_entry, None);
}

#[test]
fn rename_starts_with_the_current_name() {
    let mut c = editor();
    c.handle(
        ui(UiAction::OpenCollection("spotify:playlist:p1".into())),
        0,
    );
    c.handle(ui(UiAction::EditPlaylist("spotify:playlist:p1".into())), 0);
    c.handle(ui(UiAction::RenamePlaylist), 0);
    assert_eq!(c.state().text_entry.as_ref().unwrap().text, "Playlist 1");
    c.handle(ui(UiAction::KeyPressed("!".into())), 0);
    let fx = c.handle(ui(UiAction::KeyboardDone), 0);
    assert!(
        matches!(edit_of(&fx).1, PlaylistEdit::Rename { ref name, .. } if name == "Playlist 1!")
    );
    assert_eq!(
        c.state().sections[&Section::Playlists]
            .data
            .as_ref()
            .unwrap()[0]
            .name,
        "Playlist 1!"
    );
}

#[test]
fn move_reorders_and_sends_the_snapshot() {
    let mut c = editor();
    c.handle(
        ui(UiAction::OpenCollection("spotify:playlist:p1".into())),
        0,
    );
    c.handle(ui(UiAction::EditPlaylist("spotify:playlist:p1".into())), 0);
    let fx = c.handle(ui(UiAction::MoveTrack { from: 0, to: 2 }), 0);
    assert_eq!(
        edit_of(&fx).1,
        PlaylistEdit::Move {
            playlist_uri: "spotify:playlist:p1".into(),
            from: 0,
            to: 2,
            snapshot_id: Some("s1".into()),
        }
    );
    assert_eq!(
        track_names(&c, "spotify:playlist:p1"),
        ["Song 1", "Song 2", "Song 0"]
    );
    assert!(
        c.handle(ui(UiAction::MoveTrack { from: 0, to: 9 }), 0)
            .is_empty()
    );
}

#[test]
fn delete_asks_first_then_removes_and_returns_to_playlists() {
    let mut c = editor();
    c.handle(
        ui(UiAction::OpenCollection("spotify:playlist:p1".into())),
        0,
    );
    c.handle(ui(UiAction::EditPlaylist("spotify:playlist:p1".into())), 0);
    c.handle(ui(UiAction::AskDelete), 0);
    assert_eq!(
        c.state().confirm_delete.as_deref(),
        Some("spotify:playlist:p1")
    );
    c.handle(ui(UiAction::CancelDelete), 0);
    assert_eq!(c.state().confirm_delete, None);
    c.handle(ui(UiAction::AskDelete), 0);
    let fx = c.handle(ui(UiAction::ConfirmDelete), 0);
    assert!(matches!(edit_of(&fx).1, PlaylistEdit::Delete { .. }));
    assert_eq!(c.state().screen, Screen::Grid(Section::Playlists));
    assert_eq!(c.state().editing, None);
    let lists = c.state().sections[&Section::Playlists]
        .data
        .clone()
        .unwrap();
    assert!(lists.iter().all(|p| p.uri != "spotify:playlist:p1"));
}

#[test]
fn keys_go_to_the_name_field_not_search_while_it_is_open() {
    let mut c = editor();
    c.handle(ui(UiAction::ShowSection(Section::Search)), 0);
    c.handle(ui(UiAction::OpenPicker("spotify:track:t1".into())), 0);
    c.handle(ui(UiAction::NewPlaylist), 0);
    c.handle(ui(UiAction::KeyPressed("x".into())), 0);
    assert_eq!(c.state().search.query, "");
    assert_eq!(c.state().text_entry.as_ref().unwrap().text, "x");
}

#[test]
fn heart_likes_the_playing_song_and_undoes_on_failure() {
    let mut c = editor();
    with_tracks(&mut c, LIKED_URI, 2);
    // Liked Songs is loaded, so whether song 5 is liked is known without asking.
    let fx = c.handle(Input::Player(PlayerUpdate::TrackChanged(track(5))), 0);
    assert!(
        !fx.iter()
            .any(|e| matches!(e, Effect::Library(LibraryRequest::IsLiked { .. })))
    );
    c.handle(
        Input::Library(LibraryUpdate::Liked {
            track_uri: "spotify:track:t5".into(),
            liked: false,
        }),
        0,
    );
    let fx = c.handle(ui(UiAction::ToggleLike), 0);
    let (id, edit) = edit_of(&fx);
    assert_eq!(
        edit,
        PlaylistEdit::Like {
            track_uri: "spotify:track:t5".into()
        }
    );
    assert!(c.state().liked["spotify:track:t5"]);
    assert_eq!(track_names(&c, LIKED_URI)[0], "Song 5");
    c.handle(
        Input::Library(LibraryUpdate::EditFailed {
            id,
            reason: FailReason::Other,
        }),
        0,
    );
    assert!(!c.state().liked["spotify:track:t5"]);
    assert_eq!(track_names(&c, LIKED_URI), ["Song 0", "Song 1"]);
}

#[test]
fn list_view_toggles() {
    let mut c = core();
    assert!(!c.state().list_view);
    c.handle(ui(UiAction::ToggleListView), 0);
    assert!(c.state().list_view);
}

// ---- Settings ----

fn settings_effects(fx: &[Effect]) -> Vec<crate::settings::Settings> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::ApplySettings(s) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

fn found(address: &str, name: &str) -> Input {
    Input::Bluetooth(BtUpdate::Found(FoundSpeaker {
        address: address.into(),
        name: name.into(),
        paired: false,
    }))
}

#[test]
fn settings_opens_from_the_rail() {
    let mut c = core();
    c.handle(ui(UiAction::ShowSection(Section::Settings)), 0);
    assert_eq!(c.state().screen, Screen::Settings);
    assert_eq!(c.state().device.device_name, "Ziggy Test");
}

#[test]
fn renaming_the_device_saves_and_restarts() {
    let mut c = core();
    c.handle(ui(UiAction::ShowSection(Section::Settings)), 0);
    c.handle(ui(UiAction::RenameDevice), 0);
    assert_eq!(c.state().text_entry.as_ref().unwrap().text, "Ziggy Test");
    for _ in 0.."Ziggy Test".len() {
        c.handle(ui(UiAction::Backspace), 0);
    }
    c.handle(ui(UiAction::KeyPressed("Kitchen".into())), 0);
    let fx = c.handle(ui(UiAction::KeyboardDone), 0);
    let saved = settings_effects(&fx);
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].device_name.as_deref(), Some("Kitchen"));
    assert!(c.state().device.restarting);
}

#[test]
fn unchanged_device_name_is_not_saved() {
    let mut c = core();
    c.handle(ui(UiAction::RenameDevice), 0);
    let fx = c.handle(ui(UiAction::KeyboardDone), 0);
    assert!(settings_effects(&fx).is_empty());
    assert!(!c.state().device.restarting);
}

#[test]
fn scanning_then_connecting_saves_the_speaker() {
    let mut c = core();
    let fx = c.handle(ui(UiAction::FindSpeakers), 0);
    assert_eq!(
        fx,
        vec![Effect::ScanSonos, Effect::Bluetooth(BtCommand::Scan)]
    );
    assert!(c.state().device.scanning);
    // A second tap while scanning does nothing.
    assert!(c.handle(ui(UiAction::FindSpeakers), 0).is_empty());
    c.handle(found("AA", "Boombox"), 0);
    c.handle(found("BB", "Kitchen"), 0);
    c.handle(found("AA", "Boombox 2"), 0);
    assert_eq!(c.state().device.found.len(), 2);
    c.handle(Input::Bluetooth(BtUpdate::ScanFinished), 0);
    assert!(!c.state().device.scanning);

    let fx = c.handle(ui(UiAction::ConnectSpeaker("AA".into())), 0);
    assert_eq!(fx, vec![Effect::Bluetooth(BtCommand::Connect("AA".into()))]);
    let fx = c.handle(Input::Bluetooth(BtUpdate::Connected("AA".into())), 0);
    assert_eq!(
        settings_effects(&fx)[0].output,
        Some(Output::Bluetooth(Speaker {
            address: "AA".into(),
            name: "Boombox 2".into()
        }))
    );
}

#[test]
fn a_failed_connection_names_the_speaker() {
    let mut c = core();
    c.handle(found("AA", "Boombox"), 0);
    c.handle(ui(UiAction::ConnectSpeaker("AA".into())), 0);
    let fx = c.handle(Input::Bluetooth(BtUpdate::ConnectFailed("AA".into())), 0);
    assert!(settings_effects(&fx).is_empty());
    assert_eq!(c.state().device.failed.as_deref(), Some("Boombox"));
    assert_eq!(c.state().device.connecting, None);
}

#[test]
fn forgetting_the_speaker_switches_to_the_jack() {
    let mut cfg = config();
    cfg.speaker = Some(Speaker {
        address: "AA".into(),
        name: "Boombox".into(),
    });
    let mut c = Core::new(cfg, 0).0;
    let fx = c.handle(ui(UiAction::ForgetSpeaker), 0);
    assert_eq!(fx, vec![Effect::Bluetooth(BtCommand::Forget("AA".into()))]);
    let fx = c.handle(Input::Bluetooth(BtUpdate::Forgotten("AA".into())), 0);
    assert_eq!(settings_effects(&fx)[0].output, Some(Output::Jack));
}

#[test]
fn no_bluetooth_means_no_scan() {
    let mut cfg = config();
    cfg.bluetooth = false;
    let mut c = Core::new(cfg, 0).0;
    // Only the Sonos scan: there's no Bluetooth here.
    assert_eq!(
        c.handle(ui(UiAction::FindSpeakers), 0),
        vec![Effect::ScanSonos]
    );
}

// ---- Library saves ----

#[test]
fn liked_songs_in_the_add_sheet_likes_the_song() {
    let mut c = editor();
    with_tracks(&mut c, LIKED_URI, 2);
    c.state.search.results.data = Some(Arc::new(SearchResults {
        tracks: vec![track(9)],
        ..Default::default()
    }));
    c.handle(ui(UiAction::OpenPicker("spotify:track:t9".into())), 0);
    let fx = c.handle(ui(UiAction::PickLiked), 0);
    assert_eq!(
        edit_of(&fx).1,
        PlaylistEdit::Like {
            track_uri: "spotify:track:t9".into()
        }
    );
    assert_eq!(track_names(&c, LIKED_URI)[0], "Song 9");
    assert_eq!(
        c.state().notice,
        Some(Notice::AddedTo("Liked Songs".into()))
    );
    // Already liked: nothing to do.
    c.handle(ui(UiAction::OpenPicker("spotify:track:t0".into())), 0);
    assert!(c.handle(ui(UiAction::PickLiked), 0).is_empty());
    assert_eq!(
        c.state().notice,
        Some(Notice::AlreadyIn("Liked Songs".into()))
    );
}

#[test]
fn opening_an_album_asks_whether_it_is_saved_and_the_heart_saves_it() {
    let mut c = core();
    let album = Collection {
        uri: "spotify:album:x".into(),
        kind: CollectionKind::Album,
        name: "New Album".into(),
        ..Default::default()
    };
    c.state.search.results.data = Some(Arc::new(SearchResults {
        albums: vec![album.clone()],
        ..Default::default()
    }));
    let fx = c.handle(ui(UiAction::OpenCollection(album.uri.clone())), 0);
    assert!(fx.contains(&Effect::Library(LibraryRequest::IsLiked {
        track_uri: album.uri.clone()
    })));
    c.handle(
        Input::Library(LibraryUpdate::Liked {
            track_uri: album.uri.clone(),
            liked: false,
        }),
        0,
    );
    let fx = c.handle(ui(UiAction::ToggleSaveAlbum(album.uri.clone())), 0);
    let (id, edit) = edit_of(&fx);
    assert_eq!(
        edit,
        PlaylistEdit::SaveAlbum {
            album_uri: album.uri.clone()
        }
    );
    let albums = c.state().sections[&Section::Albums].data.clone().unwrap();
    assert_eq!(albums[0], album);
    // Spotify refuses: the album leaves the list again.
    c.handle(
        Input::Library(LibraryUpdate::EditFailed {
            id,
            reason: FailReason::Other,
        }),
        0,
    );
    assert!(!c.state().liked[&album.uri]);
    let albums = c
        .state()
        .sections
        .get(&Section::Albums)
        .and_then(|s| s.data.clone());
    assert!(albums.is_none_or(|a| a.iter().all(|x| x.uri != album.uri)));
}

// ---- Artists ----

#[test]
fn artists_drill_down_to_albums_and_back() {
    let mut c = core();
    c.handle(ui(UiAction::ShowSection(Section::Artists)), 0);
    assert_eq!(c.state().screen, Screen::Grid(Section::Artists));
    let fx = c.handle(ui(UiAction::OpenCollection("spotify:artist:r1".into())), 0);
    assert_eq!(c.state().screen, Screen::Artist("spotify:artist:r1".into()));
    assert!(fx.contains(&Effect::Library(LibraryRequest::ArtistAlbums {
        artist_uri: "spotify:artist:r1".into()
    })));
    c.handle(ui(UiAction::OpenCollection("spotify:album:a1".into())), 0);
    c.handle(ui(UiAction::Back), 0);
    assert_eq!(c.state().screen, Screen::Artist("spotify:artist:r1".into()));
    c.handle(ui(UiAction::Back), 0);
    assert_eq!(c.state().screen, Screen::Grid(Section::Artists));
}

#[test]
fn following_an_artist_adds_it_to_artists() {
    let mut c = core();
    let artist = Collection {
        uri: "spotify:artist:r1".into(),
        kind: CollectionKind::Artist,
        name: "Band".into(),
        ..Default::default()
    };
    c.state.search.results.data = Some(Arc::new(SearchResults {
        artists: vec![artist.clone()],
        ..Default::default()
    }));
    let fx = c.handle(ui(UiAction::ToggleFollow(artist.uri.clone())), 0);
    assert_eq!(
        edit_of(&fx).1,
        PlaylistEdit::Follow {
            artist_uri: artist.uri.clone()
        }
    );
    assert_eq!(
        c.state().sections[&Section::Artists].data.as_ref().unwrap()[0],
        artist
    );
    c.handle(ui(UiAction::ToggleFollow(artist.uri.clone())), 0);
    assert!(
        c.state().sections[&Section::Artists]
            .data
            .as_ref()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn the_playing_songs_artist_opens_their_page_with_a_name() {
    let mut c = core();
    c.handle(Input::Player(PlayerUpdate::TrackChanged(track(1))), 0);
    c.handle(ui(UiAction::OpenNowPlaying), 0);
    let fx = c.handle(ui(UiAction::OpenPlayingArtist), 0);
    assert_eq!(c.state().screen, Screen::Artist("spotify:artist:r1".into()));
    assert!(fx.contains(&Effect::Library(LibraryRequest::ArtistAlbums {
        artist_uri: "spotify:artist:r1".into()
    })));
    let v = crate::view::build(c.state());
    assert_eq!(v.artist.unwrap().header.title, "Band");
    c.handle(ui(UiAction::Back), 0);
    assert_eq!(c.state().screen, Screen::NowPlaying);
}

#[test]
fn album_artist_opens_the_artist_or_steps_back_to_them() {
    let mut c = core();
    let album = Collection {
        uri: "spotify:album:x".into(),
        kind: CollectionKind::Album,
        name: "Graceland".into(),
        subtitle: "Paul Simon".into(),
        artist_uri: Some("spotify:artist:ps".into()),
        ..Default::default()
    };
    c.handle(
        Input::Library(LibraryUpdate::Section {
            section: Section::Albums,
            items: vec![album.clone()],
        }),
        0,
    );
    // From Albums: opens the artist page, titled with their name.
    c.handle(ui(UiAction::OpenCollection(album.uri.clone())), 0);
    c.handle(ui(UiAction::OpenAlbumArtist(album.uri.clone())), 0);
    assert_eq!(c.state().screen, Screen::Artist("spotify:artist:ps".into()));
    assert_eq!(
        crate::view::build(c.state()).artist.unwrap().header.title,
        "Paul Simon"
    );
    // From that artist page into the album and back up: no second artist page.
    c.handle(ui(UiAction::OpenCollection(album.uri.clone())), 0);
    let depth = c.state().back_stack.len();
    c.handle(ui(UiAction::OpenAlbumArtist(album.uri.clone())), 0);
    assert_eq!(c.state().screen, Screen::Artist("spotify:artist:ps".into()));
    assert_eq!(c.state().back_stack.len(), depth - 1);
}

#[test]
fn an_album_cached_without_its_artist_finds_them_by_name() {
    let mut c = core();
    let album = Collection {
        uri: "spotify:album:old".into(),
        kind: CollectionKind::Album,
        name: "Graceland".into(),
        subtitle: "Paul Simon, Ladysmith".into(),
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
    let fx = c.handle(ui(UiAction::OpenAlbumArtist(album.uri.clone())), 0);
    assert!(fx.contains(&Effect::Library(LibraryRequest::FindArtist {
        album_uri: album.uri.clone(),
        name: "Paul Simon".into()
    })));
    let artist = Collection {
        uri: "spotify:artist:ps".into(),
        kind: CollectionKind::Artist,
        name: "Paul Simon".into(),
        ..Default::default()
    };
    c.handle(
        Input::Library(LibraryUpdate::ArtistFound {
            album_uri: album.uri.clone(),
            artist,
        }),
        0,
    );
    assert_eq!(c.state().screen, Screen::Artist("spotify:artist:ps".into()));
    let albums = c.state().sections[&Section::Albums].data.clone().unwrap();
    assert_eq!(albums[0].artist_uri.as_deref(), Some("spotify:artist:ps"));
}

// ---- Sleep timer ----

fn playing_at(volume: u8) -> Core {
    let mut c = core();
    c.handle(ui(UiAction::SetVolume { percent: volume }), 0);
    c.handle(Input::Player(PlayerUpdate::TrackChanged(track(1))), 0);
    c.handle(Input::Player(PlayerUpdate::Playing { position_ms: 0 }), 0);
    c
}

#[test]
fn sleep_timer_fades_then_pauses_and_restores_volume() {
    let mut c = playing_at(60);
    // The moon starts it straight away, for the default 30 minutes.
    c.handle(ui(UiAction::TapSleepTimer), 0);
    let ends = 30 * 60_000;
    assert_eq!(c.state().sleep_ends_ms, Some(ends));
    assert_eq!(crate::view::build(c.state()).now.sleep_left, "30 min");
    let player = |fx: Vec<Effect>| -> Vec<Effect> {
        fx.into_iter()
            .filter(|e| matches!(e, Effect::Player(_)))
            .collect()
    };
    // Halfway through the fade the volume is about half.
    assert!(!c.wants_search_tick());
    c.handle(Input::Tick, ends - 5_500);
    assert!(c.wants_search_tick(), "fast ticks for a smooth fade");
    let fx = player(c.handle(Input::SearchTick, ends - 2_500));
    assert_eq!(
        fx,
        vec![Effect::Player(PlayerCommand::SetVolume { percent: 30 })]
    );
    // At the end: pause, then the original volume comes back for next time.
    let fx = player(c.handle(Input::Tick, ends));
    assert_eq!(
        fx,
        vec![
            Effect::Player(PlayerCommand::Pause),
            Effect::Player(PlayerCommand::SetVolume { percent: 60 }),
        ]
    );
    assert_eq!(c.state().playback.status, PlayStatus::Paused);
    assert_eq!(c.state().playback.volume, 60);
    assert_eq!(c.state().sleep_ends_ms, None);
}

#[test]
fn tapping_the_moon_while_running_turns_it_off_and_restores_volume() {
    let mut c = playing_at(60);
    c.handle(ui(UiAction::SetSleepTimer(Some(30))), 0);
    c.handle(Input::Tick, 30 * 60_000 - 3_000);
    let fx = c.handle(ui(UiAction::TapSleepTimer), 30 * 60_000 - 2_000);

    assert!(fx.contains(&Effect::Player(PlayerCommand::SetVolume { percent: 60 })));
    assert_eq!(c.state().sleep_ends_ms, None);
}

#[test]
fn during_a_sleep_timer_the_screen_dims_in_seconds_then_goes_dark_after_a_minute() {
    let mut c = playing_at(60);
    c.handle(ui(UiAction::SetSleepTimer(Some(60))), 10_000);
    c.handle(Input::Tick, 12_000);
    assert_eq!(c.state().display, DisplayMode::Active);
    c.handle(Input::Tick, 13_000);
    assert_eq!(c.state().display, DisplayMode::Dim);
    // A new song doesn't light it up again.
    c.handle(
        Input::Player(PlayerUpdate::Playing { position_ms: 0 }),
        14_000,
    );
    assert_eq!(c.state().display, DisplayMode::Dim);
    c.handle(Input::Tick, 70_000);
    assert_eq!(c.state().display, DisplayMode::Off);
    // A touch wakes it, and the cycle starts again.
    c.handle(ui(UiAction::Touch), 80_000);
    assert_eq!(c.state().display, DisplayMode::Active);
    c.handle(Input::Tick, 83_000);
    assert_eq!(c.state().display, DisplayMode::Dim);
}

#[test]
fn the_sleep_length_comes_from_settings_and_saves_without_a_restart() {
    let mut c = playing_at(60);
    let fx = c.handle(ui(UiAction::SetSleepLength(60)), 0);
    assert!(matches!(&fx[..], [Effect::SaveSettings(s)] if s.sleep_minutes == Some(60)));
    assert!(!c.state().device.restarting);
    c.handle(ui(UiAction::TapSleepTimer), 1_000);
    assert_eq!(c.state().sleep_ends_ms, Some(1_000 + 60 * 60_000));
    c.handle(ui(UiAction::TapSleepTimer), 2_000);
    assert_eq!(c.state().sleep_ends_ms, None);
}

#[test]
fn settings_show_the_temperature_when_there_is_one() {
    let mut c = core();
    assert_eq!(crate::view::build(c.state()).settings.temperature, "");
    c.handle(Input::Temperature(Some(52)), 0);
    let v = crate::view::build(c.state()).settings;
    assert_eq!(v.temperature, "52°C");
    assert!(!v.temperature_hot);
    c.handle(Input::Temperature(Some(71)), 0);
    assert!(crate::view::build(c.state()).settings.temperature_hot);
    c.handle(Input::Temperature(None), 0);
    assert_eq!(crate::view::build(c.state()).settings.temperature, "");
}

#[test]
fn three_songs_failing_quickly_pauses_instead_of_skipping_on() {
    let mut c = playing_at(60);
    let fail = Input::Player(PlayerUpdate::Unavailable);
    assert!(c.handle(fail.clone(), 1_000).is_empty());
    assert_eq!(
        banner(&c).as_deref(),
        Some("That song can't play, skipping")
    );
    assert!(c.handle(fail.clone(), 2_000).is_empty());
    let fx = c.handle(fail.clone(), 3_000);
    assert!(fx.contains(&Effect::Player(PlayerCommand::Pause)), "{fx:?}");
    assert!(banner(&c).unwrap().contains("Ziggy stopped"));
    // Failures spread out (a bad song now and then) don't stop the music.
    let mut c = playing_at(60);
    for t in [0, 40_000, 80_000, 120_000] {
        assert!(c.handle(fail.clone(), t).is_empty());
    }
}

#[test]
fn turn_off_pauses_then_shuts_down_once() {
    let mut c = playing_at(60);
    let fx = c.handle(ui(UiAction::PowerOff), 0);
    assert_eq!(fx, [Effect::Player(PlayerCommand::Pause), Effect::PowerOff]);
    assert!(crate::view::build(c.state()).settings.powering_off);
    assert!(c.handle(ui(UiAction::PowerOff), 10).is_empty(), "only once");
}

#[test]
fn settings_show_the_wifi_and_flag_a_weak_signal() {
    let mut c = core();
    assert_eq!(crate::view::build(c.state()).settings.wifi, "");
    let connected = |signal| {
        Input::Wifi(Some(crate::app::WifiStatus::Connected {
            network: "46Brewer".into(),
            band: "5 GHz".into(),
            signal,
        }))
    };
    c.handle(connected(66), 0);
    let v = crate::view::build(c.state()).settings;
    assert_eq!(v.wifi, "Wi-Fi 46Brewer · 5 GHz · 66%");
    assert!(!v.wifi_weak);
    c.handle(connected(30), 0);
    assert!(crate::view::build(c.state()).settings.wifi_weak);
    c.handle(Input::Wifi(Some(crate::app::WifiStatus::Disconnected)), 0);
    let v = crate::view::build(c.state()).settings;
    assert_eq!(v.wifi, "No Wi-Fi");
    assert!(v.wifi_weak);
}

#[test]
fn picking_a_colour_scheme_saves_without_a_restart() {
    let mut c = core();
    let fx = c.handle(ui(UiAction::SetTheme(1)), 0);
    assert!(matches!(&fx[..], [Effect::SaveSettings(s)] if s.theme == Some(1)));
    assert!(!c.state().device.restarting);
    assert_eq!(crate::view::build(c.state()).theme, 1);
    c.handle(ui(UiAction::SetTheme(9)), 0);
    assert_eq!(crate::view::build(c.state()).theme, 4);
}

// ---- Play history ----

fn history_effects(fx: &[Effect]) -> Vec<HistoryCommand> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::History(c) => Some(c.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn each_song_is_logged_with_where_it_played_from_and_how_long() {
    let mut c = core();
    with_tracks(&mut c, "spotify:album:a1", 3);
    c.handle(
        ui(UiAction::PlayCollection {
            uri: "spotify:album:a1".into(),
            shuffle: false,
        }),
        0,
    );
    let fx = c.handle(Input::Player(PlayerUpdate::TrackChanged(track(0))), 0);
    let logged = history_effects(&fx);
    assert!(matches!(&logged[..], [HistoryCommand::Start(r)]
        if r.track.uri == "spotify:track:t0" && r.context_uri.as_deref() == Some("spotify:album:a1")));
    // The same song announced again while loading is still one play.
    let fx = c.handle(Input::Player(PlayerUpdate::TrackChanged(track(0))), 100);
    assert!(history_effects(&fx).is_empty());

    c.handle(
        Input::Player(PlayerUpdate::Playing { position_ms: 0 }),
        1_000,
    );
    // Listening time is saved every 10 seconds of actual play.
    let fx = c.handle(Input::Tick, 11_000);
    assert_eq!(history_effects(&fx), [HistoryCommand::Listened(10_000)]);
    // Paused time doesn't count.
    let fx = c.handle(
        Input::Player(PlayerUpdate::Paused {
            position_ms: 12_000,
        }),
        13_000,
    );
    assert_eq!(history_effects(&fx), [HistoryCommand::Listened(12_000)]);
    c.handle(Input::Tick, 60_000);
    c.handle(
        Input::Player(PlayerUpdate::Playing {
            position_ms: 12_000,
        }),
        60_000,
    );
    // The next song closes the first one's time and starts its own row.
    let fx = c.handle(Input::Player(PlayerUpdate::TrackChanged(track(1))), 65_000);
    let logged = history_effects(&fx);
    assert_eq!(logged[0], HistoryCommand::Listened(17_000));
    assert!(matches!(&logged[1], HistoryCommand::Start(r) if r.track.uri == "spotify:track:t1"));
}

fn play(n: u32, context: Option<&str>, name: &str) -> PlayRecord {
    PlayRecord {
        track: track(n),
        context_uri: context.map(str::to_string),
        context_name: name.into(),
        context_image: None,
        played_at: 1_000,
        listened_ms: 60_000,
    }
}

#[test]
fn recent_comes_from_history_grouped_by_where_songs_played_from() {
    let mut c = core();
    c.handle(
        Input::HistoryRecent(vec![
            play(3, Some("spotify:playlist:p1"), "Road"),
            play(2, Some("spotify:playlist:p1"), "Road"),
            play(1, Some(LIKED_URI), ""),
            play(0, None, ""),
        ]),
        0,
    );
    let recent = c.state().sections[&Section::Recent].data.clone().unwrap();
    let names: Vec<&str> = recent.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["Road", "Liked Songs", "Album"]);
    assert_eq!(recent[2].uri, "spotify:album:a1");
}

#[test]
fn a_new_device_borrows_spotifys_recent_list_once() {
    let mut c = core();
    let fx = c.handle(Input::HistoryRecent(vec![]), 0);
    assert!(fx.contains(&Effect::Library(LibraryRequest::Section(Section::Recent))));
    let fx = c.handle(Input::HistoryRecent(vec![]), 0);
    assert!(fx.is_empty());
}

#[test]
fn a_recent_song_plays_from_where_it_was_heard() {
    let mut c = core();
    c.handle(
        Input::HistoryRecent(vec![play(4, Some("spotify:playlist:p1"), "Road")]),
        0,
    );
    c.handle(ui(UiAction::SetRecentSongs(true)), 0);
    assert!(c.state().recent_songs);
    let fx = c.handle(ui(UiAction::PlayRecentSong(0)), 0);
    assert!(fx.contains(&Effect::Player(PlayerCommand::Load {
        context_uri: "spotify:playlist:p1".into(),
        start_index: None,
        start_uri: Some("spotify:track:t4".into()),
        shuffle: false,
    })));
}

// ---- Sonos ----

fn kitchen() -> crate::settings::SonosRoom {
    crate::settings::SonosRoom {
        uuid: "RINCON_K".into(),
        name: "Kitchen".into(),
    }
}

#[test]
fn find_speakers_also_scans_for_sonos_rooms_on_any_device() {
    let mut cfg = config();
    cfg.bluetooth = false;
    let mut c = Core::new(cfg, 0).0;
    let fx = c.handle(ui(UiAction::FindSpeakers), 0);
    assert_eq!(fx, vec![Effect::ScanSonos]);
    assert!(crate::view::build(c.state()).settings.scanning);
    c.handle(Input::SonosRooms(vec![kitchen()]), 0);
    let v = crate::view::build(c.state());
    assert!(!v.settings.scanning);
    assert_eq!(v.settings.sonos_rooms[0].name, "Kitchen");
}

#[test]
fn choosing_a_room_saves_it_and_restarts_into_sonos() {
    let mut c = core();
    c.handle(Input::SonosRooms(vec![kitchen()]), 0);
    let fx = c.handle(ui(UiAction::ChooseSonos("RINCON_K".into())), 0);
    assert_eq!(
        settings_effects(&fx)[0].output,
        Some(Output::Sonos(kitchen()))
    );
    assert!(c.state().device.restarting);
}

#[test]
fn playing_on_sonos_shows_the_room_and_can_switch_back() {
    let mut cfg = config();
    cfg.saved.output = Some(Output::Sonos(kitchen()));
    let mut c = Core::new(cfg, 0).0;
    c.handle(Input::SonosRooms(vec![kitchen()]), 0);
    let v = crate::view::build(c.state());
    assert_eq!(v.settings.output, "Sonos: Kitchen");
    assert!(v.settings.on_sonos);
    assert_eq!(v.settings.sonos_rooms[0].status, "In use");
    let fx = c.handle(ui(UiAction::UseJack), 0);
    assert_eq!(settings_effects(&fx)[0].output, Some(Output::Jack));
}

#[test]
fn an_empty_playlist_can_be_made_from_playlists() {
    let mut c = editor();
    c.handle(ui(UiAction::NewEmptyPlaylist), 0);
    for key in ["G", "y", "m"] {
        c.handle(ui(UiAction::KeyPressed(key.into())), 0);
    }
    let fx = c.handle(ui(UiAction::KeyboardDone), 0);
    assert_eq!(
        edit_of(&fx).1,
        PlaylistEdit::Create {
            name: "Gym".into(),
            track_uri: None
        }
    );
    let placeholder = c.state().sections[&Section::Playlists]
        .data
        .as_ref()
        .unwrap()[0]
        .clone();
    assert_eq!(placeholder.name, "Gym");
    assert!(track_names(&c, &placeholder.uri).is_empty());
    assert_eq!(c.state().notice, Some(Notice::Created("Gym".into())));
}

// ---- Audiobooks ----

fn books_library() -> crate::audiobooks::types::BooksLibrary {
    use crate::audiobooks::types::{BookProgress, BookSummary, BooksLibrary};
    let book = |id: &str, title: &str, author: &str| BookSummary {
        id: id.into(),
        title: title.into(),
        author: author.into(),
        duration_secs: 7_200.0,
        ..Default::default()
    };
    BooksLibrary {
        books: vec![
            book("b1", "The Hobbit", "Tolkien"),
            book("b2", "Matilda", "Roald Dahl"),
        ],
        progress: [(
            "b2".to_string(),
            BookProgress {
                current_secs: 3_600.0,
                progress: 0.5,
                finished: false,
            },
        )]
        .into_iter()
        .collect(),
        continue_ids: vec!["b2".into()],
    }
}

fn books_core() -> Core {
    let mut cfg = config();
    cfg.books_config_url = Some("http://nas.local:13378".into());
    cfg.books_user = Some("sam".into());
    Core::new(cfg, 0).0
}

#[test]
fn a_config_file_address_turns_books_on_and_opening_books_loads_them() {
    let mut c = books_core();
    assert!(c.state().books.enabled);
    assert!(crate::view::build(c.state()).books_enabled);
    let fx = c.handle(ui(UiAction::ShowSection(Section::Books)), 0);
    assert_eq!(fx, vec![Effect::Books(BooksRequest::Load)]);
    assert_eq!(c.state().screen, Screen::Books);
}

#[test]
fn signing_in_asks_for_username_then_a_hidden_password() {
    let mut cfg = config();
    cfg.books_config_url = Some("http://nas.local:13378".into());
    let mut c = Core::new(cfg, 0).0;
    c.handle(ui(UiAction::BooksSignIn), 0);
    c.handle(ui(UiAction::KeyPressed("sam".into())), 0);
    c.handle(ui(UiAction::KeyboardDone), 0);
    for key in ["p", "w", " ", "1"] {
        c.handle(ui(UiAction::KeyPressed(key.into())), 0);
    }
    let v = crate::view::build(c.state());
    let entry = v.text_entry.unwrap();
    assert_eq!(entry.title, "Password for sam");
    assert_eq!(entry.text, "••••");
    let fx = c.handle(ui(UiAction::KeyboardDone), 0);
    assert_eq!(
        fx,
        vec![Effect::Books(BooksRequest::SignIn {
            url: "http://nas.local:13378".into(),
            username: "sam".into(),
            password: "pw 1".into(),
        })]
    );
    assert!(c.state().books.signing_in);
    let fx = c.handle(
        Input::Books(BooksUpdate::SignedIn {
            username: "sam".into(),
        }),
        0,
    );
    assert_eq!(fx, vec![Effect::Books(BooksRequest::Load)]);
}

#[test]
fn the_books_screen_lists_continue_listening_then_filters_as_you_type() {
    let mut c = books_core();
    c.handle(ui(UiAction::ShowSection(Section::Books)), 0);
    c.handle(Input::Books(BooksUpdate::Library(books_library())), 0);
    let titles = |c: &Core| -> Vec<String> {
        crate::view::build(c.state())
            .books
            .rows
            .into_iter()
            .map(|r| r.title)
            .collect()
    };
    assert_eq!(
        titles(&c),
        [
            "Continue listening",
            "Matilda",
            "All books",
            "The Hobbit",
            "Matilda"
        ]
    );
    let v = crate::view::build(c.state());
    assert_eq!(v.books.rows[1].subtitle, "Roald Dahl · 1 h left");
    c.handle(ui(UiAction::OpenKeyboard), 0);
    assert!(c.state().keyboard_open);
    c.handle(ui(UiAction::KeyPressed("tolk".into())), 0);
    assert_eq!(titles(&c), ["The Hobbit"]);
}

#[test]
fn opening_a_book_loads_its_page() {
    use crate::audiobooks::types::{BookDetail, Chapter};
    let mut c = books_core();
    c.handle(Input::Books(BooksUpdate::Library(books_library())), 0);
    let fx = c.handle(ui(UiAction::OpenBook("b2".into())), 0);
    assert_eq!(fx, vec![Effect::Books(BooksRequest::Detail("b2".into()))]);
    c.handle(
        Input::Books(BooksUpdate::Detail {
            id: "b2".into(),
            detail: BookDetail {
                summary: books_library().books[1].clone(),
                description: String::new(),
                chapters: vec![Chapter {
                    title: "One".into(),
                    start_secs: 3_725.0,
                    end_secs: 4_000.0,
                }],
            },
            progress: None,
        }),
        0,
    );
    let book = crate::view::build(c.state()).book.unwrap();
    assert_eq!(book.title, "Matilda");
    assert_eq!(book.info, "2 h · 1 h left");
    assert_eq!(book.chapters, [("One".to_string(), "1:02:05".to_string())]);
}

// ---- Voice ----

fn voice(update: VoiceUpdate) -> Input {
    Input::Voice(update)
}

fn volumes(fx: &[Effect]) -> Vec<u8> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Player(PlayerCommand::SetVolume { percent }) => Some(*percent),
            _ => None,
        })
        .collect()
}

fn banner(c: &Core) -> Option<String> {
    crate::view::build(c.state()).banner
}

#[test]
fn the_wake_word_lowers_the_music_and_passes_the_library_to_gemini() {
    let mut c = playing_at(60);
    c.handle(
        Input::Library(LibraryUpdate::Section {
            section: Section::Playlists,
            items: vec![playlist(1)],
        }),
        0,
    );
    let fx = c.handle(voice(VoiceUpdate::Woke), 1_000);
    assert_eq!(volumes(&fx), [15]);
    assert_eq!(c.state().voice.phase, VoicePhase::Listening);
    let Some(Effect::VoiceContext(context)) =
        fx.iter().find(|e| matches!(e, Effect::VoiceContext(_)))
    else {
        panic!("no context in {fx:?}")
    };
    assert_eq!(
        context.volume, 60,
        "Gemini hears the real volume, not the lowered one"
    );
    assert_eq!(context.now_playing.as_deref(), Some("Song 1 by Band"));
    assert_eq!(context.items[0].uri, "spotify:playlist:p1");
    assert_eq!(context.items[0].kind, "playlist");
    c.handle(voice(VoiceUpdate::Thinking), 2_000);
    assert_eq!(c.state().voice.phase, VoicePhase::Thinking);
    // Not understood: the music comes back.
    let fx = c.handle(voice(VoiceUpdate::NotUnderstood), 3_000);
    assert_eq!(volumes(&fx), [60]);
    assert_eq!(c.state().voice.phase, VoicePhase::Idle);
    assert_eq!(banner(&c).as_deref(), Some("Didn't catch that"));
    assert!(
        crate::view::build(c.state()).banner_at_bottom,
        "voice replies show at the bottom"
    );
}

#[test]
fn a_voice_request_plays_something_from_the_library() {
    let mut c = playing_at(60);
    c.handle(
        Input::Library(LibraryUpdate::Section {
            section: Section::Playlists,
            items: vec![playlist(1), playlist(2)],
        }),
        0,
    );
    c.handle(voice(VoiceUpdate::Woke), 0);
    let fx = c.handle(
        voice(VoiceUpdate::Command(VoiceCommand::Play(
            "spotify:playlist:p2".into(),
        ))),
        0,
    );
    assert_eq!(volumes(&fx), [60]);
    assert!(fx.iter().any(|e| matches!(
        e,
        Effect::Player(PlayerCommand::Load { context_uri, .. }) if context_uri == "spotify:playlist:p2"
    )));
    assert_eq!(banner(&c).as_deref(), Some("Playing Playlist 2"));
}

#[test]
fn something_gemini_made_up_is_never_played() {
    let mut c = playing_at(60);
    c.handle(voice(VoiceUpdate::Woke), 0);
    let fx = c.handle(
        voice(VoiceUpdate::Command(VoiceCommand::Play(
            "spotify:album:imaginary".into(),
        ))),
        0,
    );
    assert!(
        !fx.iter()
            .any(|e| matches!(e, Effect::Player(PlayerCommand::Load { .. })))
    );
    assert_eq!(volumes(&fx), [60]);
    assert_eq!(banner(&c).as_deref(), Some("Didn't catch that"));
}

#[test]
fn a_voice_search_plays_the_first_result_of_its_kind() {
    let mut c = playing_at(60);
    c.handle(voice(VoiceUpdate::Woke), 0);
    let fx = c.handle(
        voice(VoiceUpdate::Command(VoiceCommand::Search {
            query: "The Paul Simon Songbook".into(),
            kind: SearchKind::Album,
        })),
        0,
    );
    assert!(fx.contains(&Effect::Library(LibraryRequest::Search(
        "The Paul Simon Songbook".into()
    ))));
    assert_eq!(
        banner(&c).as_deref(),
        Some("Looking for The Paul Simon Songbook")
    );
    // The search screen's own timer doesn't search again.
    assert!(c.handle(Input::SearchTick, 5_000).is_empty());
    let album = Collection {
        uri: "spotify:album:songbook".into(),
        kind: CollectionKind::Album,
        name: "The Paul Simon Songbook".into(),
        subtitle: "Paul Simon".into(),
        ..Default::default()
    };
    let fx = c.handle(
        Input::Library(LibraryUpdate::SearchResults {
            query: "The Paul Simon Songbook".into(),
            results: SearchResults {
                tracks: vec![track(1)],
                albums: vec![album],
                ..Default::default()
            },
        }),
        6_000,
    );
    assert!(fx.iter().any(|e| matches!(
        e,
        Effect::Player(PlayerCommand::Load { context_uri, .. }) if context_uri == "spotify:album:songbook"
    )));
    assert_eq!(
        banner(&c).as_deref(),
        Some("Playing The Paul Simon Songbook by Paul Simon")
    );
    assert_eq!(c.state().voice.pending_search, None);
}

#[test]
fn louder_and_quieter_start_from_the_volume_before_listening() {
    let mut c = playing_at(60);
    c.handle(voice(VoiceUpdate::Woke), 0);
    let fx = c.handle(voice(VoiceUpdate::Command(VoiceCommand::Louder)), 0);
    assert_eq!(volumes(&fx), [75]);
    c.handle(voice(VoiceUpdate::Woke), 0);
    let fx = c.handle(voice(VoiceUpdate::Command(VoiceCommand::Quieter)), 0);
    assert_eq!(volumes(&fx), [60]);
}

#[test]
fn pause_and_resume_only_change_what_needs_changing() {
    let mut c = playing_at(60);
    c.handle(voice(VoiceUpdate::Woke), 0);
    let fx = c.handle(voice(VoiceUpdate::Command(VoiceCommand::Resume)), 0);
    assert!(!fx.contains(&Effect::Player(PlayerCommand::Pause)));
    assert!(!fx.contains(&Effect::Player(PlayerCommand::Play)));
    c.handle(voice(VoiceUpdate::Woke), 0);
    let fx = c.handle(voice(VoiceUpdate::Command(VoiceCommand::Pause)), 0);
    assert!(fx.contains(&Effect::Player(PlayerCommand::Pause)));
}

#[test]
fn a_lost_reply_still_brings_the_music_back() {
    let mut c = playing_at(60);
    c.handle(voice(VoiceUpdate::Woke), 0);
    c.handle(voice(VoiceUpdate::Thinking), 1_000);
    assert!(volumes(&c.handle(Input::Tick, 10_000)).is_empty());
    let fx = c.handle(Input::Tick, 21_000);
    assert_eq!(volumes(&fx), [60]);
    assert_eq!(c.state().voice.phase, VoicePhase::Idle);
}

#[test]
fn paused_music_is_not_lowered_or_raised() {
    let mut c = playing_at(60);
    c.handle(ui(UiAction::TogglePlay), 0);
    let fx = c.handle(voice(VoiceUpdate::Woke), 0);
    assert!(volumes(&fx).is_empty());
    let fx = c.handle(
        voice(VoiceUpdate::Failed("No internet right now".into())),
        0,
    );
    assert!(volumes(&fx).is_empty());
    assert_eq!(banner(&c).as_deref(), Some("No internet right now"));
}

#[test]
fn the_microphone_button_starts_listening_once() {
    let mut c = core();
    c.handle(ui(UiAction::OpenKeyboard), 0);
    let fx = c.handle(ui(UiAction::Listen), 0);
    assert_eq!(fx, vec![Effect::VoiceListen]);
    assert!(!c.state().keyboard_open, "the keyboard gets out of the way");
    assert!(crate::view::build(c.state()).voice_enabled);
    // Already listening: a second tap does nothing.
    c.handle(voice(VoiceUpdate::Woke), 0);
    assert!(
        c.handle(ui(UiAction::Listen), 0)
            .iter()
            .all(|e| *e != Effect::VoiceListen)
    );
    // No voice on this device: no button, and a tap does nothing.
    let mut cfg = config();
    cfg.voice = false;
    let mut c = Core::new(cfg, 0).0;
    assert!(!crate::view::build(c.state()).voice_enabled);
    assert!(c.handle(ui(UiAction::Listen), 0).is_empty());
}

#[test]
fn voice_hearts_the_song_saves_the_album_and_adds_to_a_playlist() {
    let mut c = playing_at(60);
    c.handle(
        Input::Library(LibraryUpdate::Account(Account {
            id: "mum".into(),
            name: "Mum".into(),
        })),
        0,
    );
    let mut mine = playlist(1);
    mine.owner_id = Some("mum".into());
    let mut theirs = playlist(2);
    theirs.owner_id = Some("someone-else".into());
    c.handle(
        Input::Library(LibraryUpdate::Section {
            section: Section::Playlists,
            items: vec![mine, theirs],
        }),
        0,
    );
    let edits = |fx: &[Effect]| -> Vec<PlaylistEdit> {
        fx.iter()
            .filter_map(|e| match e {
                Effect::Library(LibraryRequest::Edit { edit, .. }) => Some(edit.clone()),
                _ => None,
            })
            .collect()
    };
    let fx = c.handle(voice(VoiceUpdate::Command(VoiceCommand::LikeSong)), 0);
    assert_eq!(
        edits(&fx),
        [PlaylistEdit::Like {
            track_uri: "spotify:track:t1".into()
        }]
    );
    assert_eq!(banner(&c).as_deref(), Some("Added to Liked Songs"));
    // Hearting it again doesn't unlike it.
    let fx = c.handle(voice(VoiceUpdate::Command(VoiceCommand::LikeSong)), 0);
    assert!(edits(&fx).is_empty());
    assert_eq!(banner(&c).as_deref(), Some("Already in Liked Songs"));

    let fx = c.handle(voice(VoiceUpdate::Command(VoiceCommand::SaveAlbum)), 0);
    assert_eq!(
        edits(&fx),
        [PlaylistEdit::SaveAlbum {
            album_uri: "spotify:album:a1".into()
        }]
    );
    assert_eq!(banner(&c).as_deref(), Some("Added to Albums"));
    let fx = c.handle(voice(VoiceUpdate::Command(VoiceCommand::SaveAlbum)), 0);
    assert!(edits(&fx).is_empty(), "saving it again doesn't remove it");

    let fx = c.handle(
        voice(VoiceUpdate::Command(VoiceCommand::AddToPlaylist {
            uri: "spotify:playlist:p1".into(),
            heard: "playlist 1".into(),
        })),
        0,
    );
    assert_eq!(
        edits(&fx),
        [PlaylistEdit::Add {
            playlist_uri: "spotify:playlist:p1".into(),
            track_uri: "spotify:track:t1".into()
        }]
    );
    // No playlist named ("add this song"): Gemini's guess is ignored and the song is hearted,
    // which it already is.
    let fx = c.handle(
        voice(VoiceUpdate::Command(VoiceCommand::AddToPlaylist {
            uri: "spotify:playlist:p1".into(),
            heard: "song".into(),
        })),
        0,
    );
    assert!(edits(&fx).is_empty());
    assert_eq!(banner(&c).as_deref(), Some("Already in Liked Songs"));
    // Someone else's playlist can't be changed, even by name.
    let fx = c.handle(
        voice(VoiceUpdate::Command(VoiceCommand::AddToPlaylist {
            uri: "spotify:playlist:p2".into(),
            heard: "playlist 2".into(),
        })),
        0,
    );
    assert!(
        !edits(&fx)
            .iter()
            .any(|e| matches!(e, PlaylistEdit::Add { .. }))
    );
}
