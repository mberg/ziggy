//! Owns the tokio runtime: feeds inputs to the core, carries out effects, publishes state.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::app::{AppState, Core, CoreConfig, Effect, Input, LibraryRequest, PlayerCommand};
use crate::config::Config;
use crate::images::{ImageLoader, ImageSink, spawn_image_loader};
use crate::library::cache::DiskCache;
use crate::library::fake::{FakeCatalog, FakeSource};
use crate::library::service::spawn_library;
use crate::player::fake::spawn_fake_player;

pub type Publish = Box<dyn Fn(AppState) + Send>;

/// Starts the background runtime on its own thread and returns the input channel.
pub fn start(
    config: Config,
    fake: bool,
    image_requests: UnboundedReceiver<String>,
    publish: Publish,
    images: ImageSink,
) -> anyhow::Result<UnboundedSender<Input>> {
    let (inputs_tx, inputs_rx) = mpsc::unbounded_channel();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let inputs = inputs_tx.clone();
    std::thread::Builder::new()
        .name("ziggy-runtime".into())
        .spawn(move || {
            runtime.block_on(async move {
                if let Err(e) = run(
                    config,
                    fake,
                    inputs,
                    inputs_rx,
                    image_requests,
                    publish,
                    images,
                )
                .await
                {
                    // Without the runtime the UI would show "Loading…" forever. Exit so systemd
                    // restarts the player and the journal shows why.
                    tracing::error!("runtime stopped: {e:#}");
                    std::process::exit(1);
                }
            });
        })?;
    Ok(inputs_tx)
}

async fn run(
    config: Config,
    fake: bool,
    inputs: UnboundedSender<Input>,
    mut inputs_rx: UnboundedReceiver<Input>,
    image_requests: UnboundedReceiver<String>,
    publish: Publish,
    images: ImageSink,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(&config.state_dir)?;
    // Choices made on the Settings screen override the config file.
    let saved = crate::settings::Settings::load(&config.state_dir);
    let mut config = config;
    saved.apply(&mut config);
    let speaker = match &saved.output {
        Some(crate::settings::Output::Bluetooth(speaker)) => Some(speaker.clone()),
        Some(crate::settings::Output::Jack | crate::settings::Output::Sonos(_)) => None,
        None => config
            .bluetooth_speaker
            .clone()
            .map(|address| crate::settings::Speaker {
                name: address.clone(),
                address,
            }),
    };
    let bluetooth = crate::bluetooth::spawn(
        if fake {
            crate::bluetooth::Backend::Fake
        } else {
            crate::bluetooth::Backend::Real {
                watch: config.bluetooth_speaker.clone(),
            }
        },
        inputs.clone(),
    );
    let cache = Arc::new(DiskCache::new(config.cache_dir())?);
    // Audiobooks: signed in if a sign-in was saved on this device.
    let books_handle: crate::audiobooks::service::AbsHandle = Arc::new(std::sync::RwLock::new(
        crate::audiobooks::client::AbsClient::load(
            &config.state_dir.join(crate::audiobooks::client::AUTH_FILE),
        )
        .map(Arc::new),
    ));
    let books_user = books_handle
        .read()
        .unwrap()
        .as_ref()
        .map(|c| c.username())
        .or_else(|| fake.then(|| "Fake Listener".to_string()));
    let books = crate::audiobooks::service::spawn_books(
        if fake {
            crate::audiobooks::service::Backend::Fake
        } else {
            crate::audiobooks::service::Backend::Real(books_handle.clone())
        },
        config.state_dir.clone(),
        cache.clone(),
        inputs.clone(),
    );
    let sonos_room = match &saved.output {
        Some(crate::settings::Output::Sonos(room)) => Some(room.clone()),
        _ => None,
    };
    let (player, library) = if fake {
        let catalog = Arc::new(FakeCatalog::sample());
        let library = spawn_library(
            Arc::new(FakeSource::new(catalog.clone())),
            cache,
            inputs.clone(),
        );
        (spawn_fake_player(catalog, inputs.clone()), library)
    } else {
        let http = crate::library::web_api::ReqwestHttp::new()?;
        // Endpoints Spotify told us to leave alone, remembered across restarts.
        let block_file = config.cache_dir().join("rate-limits.json");
        let web_auth = config
            .state_dir
            .join(crate::library::refresh_tokens::FILE_NAME);
        // The library uses the developer app `ziggy setup` signed in with.
        if !web_auth.is_file() {
            tracing::error!(
                "no library sign-in at {}; run `ziggy signin`",
                web_auth.display()
            );
            let _ = inputs.send(Input::AuthInvalid);
        }
        let tokens = crate::library::refresh_tokens::RefreshTokens::load(&web_auth)
            .unwrap_or_else(|_| crate::library::refresh_tokens::RefreshTokens::missing(&web_auth));
        let source = Arc::new(
            crate::library::web_api::WebApi::new(http, tokens).with_block_file(block_file.clone()),
        );
        let player = match &sonos_room {
            // A Sonos room plays; this device's own player doesn't start.
            Some(room) => crate::player::sonos::spawn(room.clone(), cache.clone(), inputs.clone()),
            None if cfg!(target_os = "linux") => crate::player::soloist::spawn(
                crate::player::soloist::SoloistSettings {
                    device_name: config.device_name.clone(),
                    api_key_file: config.soloist_api_key_file(),
                    state_dir: config.state_dir.clone(),
                    initial_volume: config.initial_volume,
                    output: speaker
                        .as_ref()
                        .map(|s| crate::player::soloist::protocol::bluetooth_node(&s.address)),
                },
                cache.clone(),
                Some(start_through_web_api(
                    source.clone(),
                    config.device_name.clone(),
                )),
                inputs.clone(),
            ),
            None => crate::player::spawn_unavailable(inputs.clone()),
        };
        let library = spawn_library(source, cache, inputs.clone());
        (player, library)
    };
    spawn_image_loader(
        Arc::new(ImageLoader::new(config.images_dir())?.with_books(books_handle.clone())),
        image_requests,
        images,
    );

    let platform = crate::platform::Platform::start();

    let started = Instant::now();
    let now_ms = || started.elapsed().as_millis() as u64;
    let core_config = CoreConfig {
        dim_after_ms: config.dim_after_secs * 1000,
        off_after_ms: config.off_after_secs * 1000,
        initial_volume: config.initial_volume,
        device_name: config.device_name.clone(),
        speaker,
        saved,
        bluetooth: bluetooth.is_some(),
        books_user,
        books_config_url: config.audiobookshelf_url.clone(),
        voice: config.voice_model_dir.is_some(),
    };
    let history =
        crate::history::spawn_history(config.state_dir.join("history.db"), inputs.clone());
    let voice_context = crate::voice::service::SharedContext::default();
    let listen_now = crate::voice::service::ListenNow::default();
    if let Some(model_dir) = &config.voice_model_dir {
        crate::voice::service::spawn_voice(
            crate::voice::service::VoiceSettings {
                model_dir: model_dir.clone(),
                phrase: config
                    .wake_phrase
                    .clone()
                    .unwrap_or_else(|| crate::voice::wake::DEFAULT_PHRASE.into()),
                threshold: config
                    .wake_threshold
                    .unwrap_or(crate::voice::wake::DEFAULT_THRESHOLD),
                microphone: config.microphone.clone(),
                gemini_key: config
                    .gemini_api_key
                    .clone()
                    .or_else(|| std::env::var("GEMINI_API_KEY").ok())
                    .filter(|k| !k.trim().is_empty()),
                vertex_key_file: config.vertex_key_file.clone(),
                vertex_location: config.vertex_location.clone(),
                gemini_model: config.gemini_model.clone(),
            },
            inputs.clone(),
            voice_context.clone(),
            listen_now.clone(),
            tokio::runtime::Handle::current(),
        );
    }
    let outputs = Outputs {
        inputs: inputs.clone(),
        fake,
        player,
        library,
        history,
        books,
        voice_context,
        listen_now,
        bluetooth,
        state_dir: config.state_dir.clone(),
    };
    let (mut core, effects) = Core::new(core_config, now_ms());
    dispatch(effects, &outputs, &platform);
    crate::platform::spawn_temperature(outputs.inputs.clone());
    crate::platform::spawn_wifi(outputs.inputs.clone());
    publish(core.state().clone());

    let mut tick = tokio::time::interval(Duration::from_secs(1));
    // Only runs while the search screen is open, so a search goes out soon after typing stops.
    let mut search_tick = tokio::time::interval(Duration::from_millis(150));
    search_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let input = tokio::select! {
            Some(input) = inputs_rx.recv() => input,
            _ = tick.tick() => Input::Tick,
            _ = search_tick.tick(), if core.wants_search_tick() => Input::SearchTick,
        };
        let quiet = input == Input::SearchTick;
        let effects = core.handle(input, now_ms());
        // A search tick that sent nothing changed nothing; skip re-rendering.
        if quiet && effects.is_empty() {
            continue;
        }
        dispatch(effects, &outputs, &platform);
        publish(core.state().clone());
    }
}

/// Where effects go.
struct Outputs {
    inputs: UnboundedSender<Input>,
    /// `--fake`: pretend Sonos rooms instead of scanning the network.
    fake: bool,
    player: UnboundedSender<PlayerCommand>,
    library: UnboundedSender<LibraryRequest>,
    history: UnboundedSender<crate::app::HistoryCommand>,
    books: UnboundedSender<crate::app::BooksRequest>,
    voice_context: crate::voice::service::SharedContext,
    listen_now: crate::voice::service::ListenNow,
    bluetooth: Option<UnboundedSender<crate::app::BtCommand>>,
    state_dir: std::path::PathBuf,
}

fn dispatch(effects: Vec<Effect>, outputs: &Outputs, platform: &crate::platform::Platform) {
    for effect in effects {
        match effect {
            Effect::Player(command) => {
                let _ = outputs.player.send(command);
            }
            Effect::Library(request) => {
                let _ = outputs.library.send(request);
            }
            Effect::Display(mode) => platform.set_display(mode),
            Effect::Books(request) => {
                let _ = outputs.books.send(request);
            }
            Effect::VoiceListen => {
                outputs
                    .listen_now
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
            Effect::VoiceContext(context) => {
                *outputs.voice_context.lock().unwrap() = context;
            }
            Effect::ScanSonos => {
                let inputs = outputs.inputs.clone();
                let fake = outputs.fake;
                tokio::spawn(async move {
                    let rooms = if fake {
                        tokio::time::sleep(Duration::from_millis(800)).await;
                        ["Kitchen", "Living Room", "Office"]
                            .into_iter()
                            .enumerate()
                            .map(|(i, name)| crate::settings::SonosRoom {
                                uuid: format!("RINCON_FAKE{i}"),
                                name: name.into(),
                            })
                            .collect()
                    } else {
                        crate::player::sonos::discover_rooms().await
                    };
                    let _ = inputs.send(Input::SonosRooms(rooms));
                });
            }
            Effect::History(command) => {
                let _ = outputs.history.send(command);
            }
            Effect::Bluetooth(command) => {
                if let Some(bluetooth) = &outputs.bluetooth {
                    let _ = bluetooth.send(command);
                }
            }
            Effect::PowerOff => {
                platform.screen_off_now();
                tokio::spawn(async {
                    // Long enough for the pause to reach the speaker.
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                    crate::platform::power_off().await;
                });
            }
            Effect::SaveSettings(settings) => {
                if let Err(e) = settings.save(&outputs.state_dir) {
                    tracing::error!("saving settings failed: {e:#}");
                }
            }
            Effect::ApplySettings(settings) => {
                let state_dir = outputs.state_dir.clone();
                tokio::spawn(async move {
                    if let Err(e) = settings.save(&state_dir) {
                        tracing::error!("saving settings failed: {e:#}");
                        return;
                    }
                    tracing::info!("settings saved; restarting to apply them");
                    // Long enough for the screen to say it's restarting.
                    tokio::time::sleep(Duration::from_millis(800)).await;
                    crate::platform::restart();
                });
            }
        }
    }
}

/// Starts a list at a song on this player through Spotify's Web API: finds the player in the
/// account's device list by name (once), then asks Spotify to play there. False on any failure,
/// and the player skips ahead itself instead.
fn start_through_web_api(
    api: Arc<
        crate::library::web_api::WebApi<
            crate::library::web_api::ReqwestHttp,
            crate::library::refresh_tokens::RefreshTokens,
        >,
    >,
    device_name: String,
) -> crate::player::soloist::StartFn {
    let device_id = Arc::new(std::sync::Mutex::new(None::<String>));
    Arc::new(move |context_uri, start| {
        let (api, device_name, device_id) = (api.clone(), device_name.clone(), device_id.clone());
        Box::pin(async move {
            let known = device_id.lock().unwrap().clone();
            let id = match known {
                Some(id) => id,
                None => match api.device_id(&device_name).await {
                    Ok(Some(id)) => {
                        *device_id.lock().unwrap() = Some(id.clone());
                        id
                    }
                    Ok(None) => {
                        tracing::warn!("Spotify doesn't list {device_name:?} as a device yet");
                        return false;
                    }
                    Err(e) => {
                        tracing::warn!("couldn't find this player in Spotify's devices: {e}");
                        return false;
                    }
                },
            };
            match api.play_from(&id, &context_uri, &start).await {
                Ok(()) => true,
                Err(e) => {
                    tracing::warn!("Spotify didn't start {context_uri} at {start:?}: {e}");
                    // The ID may be stale (the player was renamed or paired again).
                    *device_id.lock().unwrap() = None;
                    false
                }
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::app::{PlayStatus, UiAction};
    use crate::model::Section;

    #[test]
    fn fake_runtime_loads_library_and_plays() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::parse(&format!(
            "device_name = \"Test\"\nstate_dir = \"{}\"\n",
            dir.path().display()
        ))
        .unwrap();
        let (state_tx, state_rx) = std::sync::mpsc::channel();
        let (_image_tx, image_rx) = tokio::sync::mpsc::unbounded_channel();
        let inputs = start(
            config,
            true,
            image_rx,
            Box::new(move |s| {
                let _ = state_tx.send(s);
            }),
            Arc::new(|_, _| {}),
        )
        .unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);
        let wait_for = |pred: &dyn Fn(&AppState) -> bool| loop {
            let state = state_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("state published");
            if pred(&state) {
                break;
            }
            assert!(Instant::now() < deadline, "timed out");
        };
        wait_for(&|s| {
            s.sections
                .get(&Section::Playlists)
                .and_then(|x| x.data.as_ref())
                .is_some_and(|d| !d.is_empty())
        });
        inputs
            .send(Input::Ui(UiAction::PlayCollection {
                uri: "spotify:playlist:fake0".into(),
                shuffle: false,
            }))
            .unwrap();
        wait_for(&|s| s.playback.status == PlayStatus::Playing && s.playback.track.is_some());
        // The play is logged in this device's history, which Recent then shows.
        wait_for(&|s| s.history.as_ref().is_some_and(|h| !h.is_empty()));
        assert!(dir.path().join("history.db").is_file());

        // Search: typing, a pause, then catalog results arrive without another input.
        inputs
            .send(Input::Ui(UiAction::ShowSection(Section::Search)))
            .unwrap();
        inputs
            .send(Input::Ui(UiAction::KeyPressed("dance".into())))
            .unwrap();
        wait_for(&|s| {
            s.search
                .results
                .data
                .as_ref()
                .is_some_and(|r| !r.playlists.is_empty())
        });

        // Editing: add the playing song to "Bedtime" (fake1) and see Spotify's answer come back.
        wait_for(&|s| s.account.is_some());
        let song = "spotify:track:fake-spotify-playlist-fake0-1";
        inputs
            .send(Input::Ui(UiAction::OpenCollection(
                "spotify:playlist:fake1".into(),
            )))
            .unwrap();
        inputs
            .send(Input::Ui(UiAction::OpenPicker(song.into())))
            .unwrap();
        inputs
            .send(Input::Ui(UiAction::PickPlaylist(
                "spotify:playlist:fake1".into(),
            )))
            .unwrap();
        wait_for(&|s| {
            s.tracks
                .get("spotify:playlist:fake1")
                .and_then(|slot| slot.data.as_ref())
                .is_some_and(|tracks| tracks.len() == 13 && tracks[12].uri == song)
                && !s.tracks["spotify:playlist:fake1"].loading
        });
    }
}
