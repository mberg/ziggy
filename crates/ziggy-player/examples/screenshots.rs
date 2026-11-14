//! Saves screenshots of the player with the made-up library, for the README and the website.
//!
//!     cargo run -p ziggy-player --example screenshots -- docs/screenshots
//!
//! A window opens, steps through the screens by itself, and closes when it's done.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use slint::ComponentHandle;
use ziggy_player::app::{Input, UiAction, VoiceUpdate};
use ziggy_player::config::Config;
use ziggy_player::model::Section;
use ziggy_player::{AppWindow, runtime, ui_bridge};

/// One screenshot: what happens first, and the file it's saved as.
struct Shot {
    name: &'static str,
    inputs: Vec<Input>,
}

fn shots() -> Vec<Shot> {
    let shot = |name, actions: Vec<UiAction>| Shot {
        name,
        inputs: actions.into_iter().map(Input::Ui).collect(),
    };
    vec![
        shot("playlists", vec![UiAction::ShowSection(Section::Playlists)]),
        shot(
            "playlist",
            vec![UiAction::OpenCollection("spotify:playlist:fake0".into())],
        ),
        shot(
            "now-playing",
            vec![
                UiAction::PlayTrack {
                    collection_uri: "spotify:playlist:fake0".into(),
                    index: 2,
                },
                UiAction::OpenNowPlaying,
            ],
        ),
        shot("albums", vec![UiAction::ShowSection(Section::Albums)]),
        shot("home", vec![UiAction::ShowSection(Section::Home)]),
        shot("settings", vec![UiAction::ShowSection(Section::Settings)]),
        // Last, because the player keeps listening until a reply comes, and none does here.
        Shot {
            name: "voice",
            inputs: vec![
                Input::Ui(UiAction::ShowSection(Section::Albums)),
                Input::Voice(VoiceUpdate::Woke),
            ],
        },
    ]
}

fn main() -> anyhow::Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("rustls crypto provider already installed"))?;
    let out = PathBuf::from(std::env::args().nth(1).unwrap_or("docs/screenshots".into()));
    std::fs::create_dir_all(&out)?;

    let mut config = Config::load(std::path::Path::new("dev/config.toml"))?;
    // A fresh state folder, so a library cached by an earlier run doesn't change the pictures.
    let state = tempfile::tempdir()?;
    config.state_dir = state.path().to_path_buf();
    let window = AppWindow::new()?;
    let (image_tx, image_rx) = tokio::sync::mpsc::unbounded_channel();
    ui_bridge::install(&window, image_tx);
    let inputs = runtime::start(
        config,
        true,
        image_rx,
        Box::new(ui_bridge::publish),
        Arc::new(ui_bridge::deliver_image),
    )?;
    ui_bridge::wire_callbacks(&window, inputs.clone());

    let shots = Rc::new(shots());
    let step = Rc::new(std::cell::Cell::new(0usize));
    let timer = Rc::new(slint::Timer::default());
    let weak = window.as_weak();
    // Each tick saves the screen the previous tick asked for, then asks for the next one.
    // The first tick only waits for the made-up library to load.
    let t = timer.clone();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(1500),
        move || {
            let i = step.get();
            let window = weak.unwrap();
            if i > 0 {
                let shot = &shots[i - 1];
                match window.window().take_snapshot() {
                    Ok(buffer) => {
                        let path = out.join(format!("{}.png", shot.name));
                        image::RgbaImage::from_raw(
                            buffer.width(),
                            buffer.height(),
                            buffer.as_bytes().to_vec(),
                        )
                        .expect("snapshot size")
                        .save(&path)
                        .expect("save screenshot");
                        println!("saved {}", path.display());
                    }
                    Err(e) => eprintln!("couldn't take {}: {e}", shot.name),
                }
            }
            match shots.get(i) {
                Some(shot) => {
                    for input in &shot.inputs {
                        let _ = inputs.send(input.clone());
                    }
                    step.set(i + 1);
                }
                None => {
                    t.stop();
                    let _ = slint::quit_event_loop();
                }
            }
        },
    );
    window.run()?;
    Ok(())
}
