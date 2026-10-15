//! Clicks through the real Slint UI with Slint's headless testing backend.

use std::cell::Cell;
use std::rc::Rc;

use i_slint_backend_testing::ElementHandle;
use slint::ComponentHandle;
use slint::LogicalPosition;
use slint::platform::{PointerEventButton, WindowEvent};
use ziggy_player::{AppWindow, ScreenKind, TileData};

fn visible_by_label(window: &AppWindow, label: &str) -> ElementHandle {
    let found: Vec<ElementHandle> =
        ElementHandle::find_by_accessible_label(window, label).collect();
    assert!(!found.is_empty(), "no element labelled {label:?}");
    found.into_iter().next().unwrap()
}

#[test]
fn ui_clicks() {
    i_slint_backend_testing::init_integration_test_with_system_time();
    slint::spawn_local(async move {
        let window = AppWindow::new().unwrap();
        window.show().unwrap();

        // Album page: the artist's name under the title opens the artist.
        window.set_screen(ScreenKind::Detail);
        window.set_detail(TileData {
            uri: "spotify:album:a1".into(),
            title: "Graceland".into(),
            subtitle: "Paul Simon".into(),
            ..Default::default()
        });
        window.set_detail_artist_link(true);
        let opened = Rc::new(Cell::new(0));
        let o = opened.clone();
        window.on_open_album_artist(move |uri| {
            assert_eq!(uri, "spotify:album:a1");
            o.set(o.get() + 1);
        });
        visible_by_label(&window, "Paul Simon")
            .single_click(PointerEventButton::Left)
            .await;
        assert_eq!(opened.get(), 1, "tapping the artist name opens the artist");

        // Play sits at the bottom of the album column.
        let play = visible_by_label(&window, "Play");
        let bottom = play.absolute_position().y + play.size().height;
        assert!(
            bottom > 480.0 - 40.0,
            "Play should be at the bottom, ends at y={bottom}"
        );
        let plays = Rc::new(Cell::new(0));
        let p = plays.clone();
        window.on_play_collection(move |_, shuffle| {
            assert!(!shuffle);
            p.set(p.get() + 1);
        });
        play.single_click(PointerEventButton::Left).await;
        assert_eq!(plays.get(), 1, "Play plays the album");

        // Now Playing: the moon starts the sleep timer.
        window.set_screen(ScreenKind::NowPlaying);
        let opened = Rc::new(Cell::new(false));
        let o = opened.clone();
        window.on_open_sleep_timer(move || o.set(true));
        visible_by_label(&window, "Sleep timer")
            .single_click(PointerEventButton::Left)
            .await;
        assert!(opened.get(), "the moon starts the sleep timer");

        // Settings: a colour swatch picks the scheme.
        window.set_screen(ScreenKind::Settings);
        window.set_sleep_minutes(30);
        let theme = Rc::new(Cell::new(-1));
        let th = theme.clone();
        window.on_set_theme(move |i| th.set(i));
        visible_by_label(&window, "Ocean")
            .single_click(PointerEventButton::Left)
            .await;
        assert_eq!(theme.get(), 1);

        // The sleep timer length, also near the top.
        let length = Rc::new(Cell::new(0));
        let l = length.clone();
        window.on_set_sleep_length(move |m| l.set(m));
        visible_by_label(&window, "60 minutes")
            .single_click(PointerEventButton::Left)
            .await;
        assert_eq!(length.get(), 60);

        // Settings, at the bottom: Audiobooks turns on, then Sign in.
        window.set_books_url("http://nas.local:13378".into());
        let books_on = Rc::new(Cell::new(false));
        let bo = books_on.clone();
        window.on_set_books_enabled(move |b| bo.set(b));
        let sign_in = Rc::new(Cell::new(false));
        let si = sign_in.clone();
        window.on_books_sign_in(move || si.set(true));
        window
            .window()
            .dispatch_event(WindowEvent::PointerScrolled {
                position: LogicalPosition::new(400.0, 300.0),
                delta_x: 0.0,
                delta_y: -2000.0,
            });
        slint::platform::update_timers_and_animations();
        visible_by_label(&window, "On")
            .single_click(PointerEventButton::Left)
            .await;
        assert!(books_on.get(), "On turns Books on");
        window.set_books_enabled(true);
        slint::platform::update_timers_and_animations();
        window
            .window()
            .dispatch_event(WindowEvent::PointerScrolled {
                position: LogicalPosition::new(400.0, 300.0),
                delta_x: 0.0,
                delta_y: -2000.0,
            });
        slint::platform::update_timers_and_animations();
        visible_by_label(&window, "Sign in")
            .single_click(PointerEventButton::Left)
            .await;
        assert!(sign_in.get(), "Sign in starts signing in");

        // Books: the search field opens the keyboard and a book opens its page.
        window.set_screen(ScreenKind::Books);
        let section = Rc::new(Cell::new(-1));
        let sc = section.clone();
        window.on_show_section(move |i| sc.set(i));
        visible_by_label(&window, "Books")
            .single_click(PointerEventButton::Left)
            .await;
        assert_eq!(section.get(), 7, "the rail shows Books when it's on");
        let keyboard = Rc::new(Cell::new(false));
        let k = keyboard.clone();
        window.on_open_keyboard(move || k.set(true));
        visible_by_label(&window, "Search books")
            .single_click(PointerEventButton::Left)
            .await;
        assert!(keyboard.get(), "the search field opens the keyboard");
        window.set_books_state(ziggy_player::LoadState::Ready);
        window.set_books_rows(slint::ModelRc::new(slint::VecModel::from(vec![
            ziggy_player::SearchRowData {
                kind: 5,
                title: "The Hobbit".into(),
                subtitle: "J.R.R. Tolkien · 10 h 50 min".into(),
                uri: "book-1".into(),
                ..Default::default()
            },
        ])));
        let book = Rc::new(std::cell::RefCell::new(String::new()));
        let b = book.clone();
        window.on_open_book(move |id| *b.borrow_mut() = id.to_string());
        visible_by_label(&window, "The Hobbit")
            .single_click(PointerEventButton::Left)
            .await;
        assert_eq!(*book.borrow(), "book-1");
        window.set_books_enabled(false);
        // Voice: the pill shows while listening, in place of the message bar.
        window.set_banner("Added to Road Trip".into());
        slint::platform::update_timers_and_animations();
        visible_by_label(&window, "Added to Road Trip");
        window.set_voice_status("Listening…".into());
        window.set_voice_listening(true);
        slint::platform::update_timers_and_animations();
        visible_by_label(&window, "Listening…");
        assert!(
            ElementHandle::find_by_accessible_label(&window, "Added to Road Trip")
                .next()
                .is_none(),
            "the message bar waits while listening"
        );
        window.set_voice_status("".into());
        window.set_banner("".into());

        // Search: the microphone listens without the wake word.
        window.set_screen(ScreenKind::Search);
        window.set_voice_enabled(true);
        let listened = Rc::new(Cell::new(false));
        let li = listened.clone();
        window.on_listen(move || li.set(true));
        visible_by_label(&window, "Ask by voice")
            .single_click(PointerEventButton::Left)
            .await;
        assert!(listened.get(), "the microphone starts listening");
        window.set_voice_enabled(false);

        // Recent: the Songs chip, then a song plays.
        window.set_keyboard_open(false);
        window.set_screen(ScreenKind::Grid);
        window.set_recent_chips(true);
        let songs_on = Rc::new(Cell::new(false));
        let so = songs_on.clone();
        window.on_set_recent_songs(move |b| so.set(b));
        visible_by_label(&window, "Songs")
            .single_click(PointerEventButton::Left)
            .await;
        assert!(songs_on.get(), "the Songs chip switches Recent to songs");
        window.set_recent_songs(true);
        window.set_recent_song_rows(slint::ModelRc::new(slint::VecModel::from(vec![TileData {
            uri: "0".into(),
            title: "Graceland".into(),
            subtitle: "Paul Simon · 12 min ago".into(),
            ..Default::default()
        }])));
        let played = Rc::new(Cell::new(-1));
        let pl = played.clone();
        window.on_play_recent(move |i| pl.set(i));
        visible_by_label(&window, "Graceland")
            .single_click(PointerEventButton::Left)
            .await;
        assert_eq!(played.get(), 0);
        window.set_recent_chips(false);
        window.set_recent_songs(false);

        // Playlists: the plus makes a new, empty playlist.
        window.set_screen(ScreenKind::Grid);
        window.set_can_create_playlist(true);
        let created = Rc::new(Cell::new(false));
        let cr = created.clone();
        window.on_create_playlist(move || cr.set(true));
        visible_by_label(&window, "New playlist")
            .single_click(PointerEventButton::Left)
            .await;
        assert!(created.get(), "the plus on Playlists starts a new playlist");
        window.set_can_create_playlist(false);

        // Keyboard: Shift makes the next letter uppercase, then turns itself off.
        window.set_keyboard_open(true);
        let typed = Rc::new(std::cell::RefCell::new(String::new()));
        let t = typed.clone();
        window.on_key_pressed(move |k| t.borrow_mut().push_str(&k));
        visible_by_label(&window, "Shift")
            .single_click(PointerEventButton::Left)
            .await;
        visible_by_label(&window, "K")
            .single_click(PointerEventButton::Left)
            .await;
        visible_by_label(&window, "i")
            .single_click(PointerEventButton::Left)
            .await;
        assert_eq!(typed.borrow().as_str(), "Ki");

        slint::quit_event_loop().unwrap();
    })
    .unwrap();
    slint::run_event_loop().unwrap();
}
