//! Runs voice control: the microphone and wake word on a thread, Gemini on the runtime.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;

use super::gemini::{Backend, Gemini};
use super::listen::{Heard, Listener};
use super::wake::SherpaWake;
use crate::app::{Input, VoiceContext, VoiceUpdate};

pub struct VoiceSettings {
    /// A sherpa-onnx keyword model folder.
    pub model_dir: PathBuf,
    pub phrase: String,
    pub threshold: f32,
    pub microphone: Option<String>,
    pub gemini_key: Option<String>,
    /// A Google Cloud service account key file; Vertex AI is used instead of the key.
    pub vertex_key_file: Option<PathBuf>,
    pub vertex_location: Option<String>,
    pub gemini_model: Option<String>,
}

/// The latest library and playback summary, sent with each request.
pub type SharedContext = Arc<Mutex<VoiceContext>>;

/// Set to start listening without the wake word (the microphone button).
pub type ListenNow = Arc<std::sync::atomic::AtomicBool>;

pub fn spawn_voice(
    settings: VoiceSettings,
    inputs: UnboundedSender<Input>,
    context: SharedContext,
    listen_now: ListenNow,
    runtime: tokio::runtime::Handle,
) {
    let backend = match (&settings.vertex_key_file, &settings.gemini_key) {
        (Some(path), _) => match super::google_auth::ServiceAccount::load(path) {
            Ok(account) => {
                tracing::info!("voice: using Vertex AI in project {}", account.project_id);
                Some(Backend::Vertex {
                    account: Box::new(account),
                    location: settings
                        .vertex_location
                        .clone()
                        .unwrap_or_else(|| "global".into()),
                })
            }
            Err(e) => {
                tracing::error!("voice: {e:#}");
                None
            }
        },
        (None, Some(key)) => Some(Backend::ApiKey(key.clone())),
        (None, None) => None,
    };
    let gemini = backend.map(|b| Arc::new(Gemini::new(b, settings.gemini_model.clone())));
    std::thread::Builder::new()
        .name("voice".into())
        .spawn(move || {
            let wake =
                match SherpaWake::load(&settings.model_dir, &settings.phrase, settings.threshold) {
                    Ok(wake) => wake,
                    Err(e) => {
                        tracing::error!("voice is off: {e:#}");
                        return;
                    }
                };
            tracing::info!("voice: listening for {:?}", settings.phrase);
            let mut listener = Listener::new(wake);
            loop {
                let (tx, rx) = std::sync::mpsc::channel();
                let failed = Arc::new(std::sync::atomic::AtomicBool::new(false));
                // The stream records while it's alive.
                let _stream =
                    match super::mic::open(settings.microphone.as_deref(), tx, failed.clone()) {
                        Ok(stream) => stream,
                        Err(e) => {
                            tracing::warn!("voice: {e:#}; trying again soon");
                            std::thread::sleep(Duration::from_secs(10));
                            continue;
                        }
                    };
                // Ends when the microphone goes away: it reports an error and goes quiet, or it
                // sends nothing for 5 s.
                let (mut peak, mut since) = (0.0f32, std::time::Instant::now());
                // VOICE_RECORD_DIR saves what the microphone hears, 10 s per file, for tuning.
                let record_dir = std::env::var("VOICE_RECORD_DIR").ok().map(PathBuf::from);
                let mut recording: Vec<f32> = Vec::new();
                let mut files = 0;
                let mut quiet_secs = 0;
                loop {
                    let chunk = match rx.recv_timeout(Duration::from_secs(1)) {
                        Ok(chunk) => {
                            quiet_secs = 0;
                            chunk
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                            quiet_secs += 1;
                            let errored = failed.load(std::sync::atomic::Ordering::Relaxed);
                            if errored || quiet_secs >= 5 {
                                break;
                            }
                            continue;
                        }
                        Err(_) => break,
                    };
                    if listen_now.swap(false, std::sync::atomic::Ordering::Relaxed)
                        && !listener.is_recording()
                    {
                        tracing::info!("voice: listening from the button");
                        listener.listen_now();
                        let _ = inputs.send(Input::Voice(VoiceUpdate::Woke));
                    }
                    // With RUST_LOG=ziggy_player::voice=debug: is any sound arriving at all?
                    peak = chunk.iter().fold(peak, |p, s| p.max(s.abs()));
                    if let Some(dir) = &record_dir {
                        recording.extend_from_slice(&chunk);
                        if recording.len() >= super::listen::RATE * 10 {
                            files += 1;
                            let path = dir.join(format!("mic-{files:03}.wav"));
                            let _ = std::fs::write(&path, super::gemini::wav(&recording));
                            tracing::info!("voice: saved {}", path.display());
                            recording.clear();
                        }
                    }
                    if since.elapsed() >= Duration::from_secs(3) {
                        tracing::debug!("voice: microphone peak {peak:.4} over 3 s");
                        peak = 0.0;
                        since = std::time::Instant::now();
                    }
                    for heard in listener.push(&chunk) {
                        let send = |update| {
                            let _ = inputs.send(Input::Voice(update));
                        };
                        match heard {
                            Heard::Woke => {
                                tracing::info!("voice: heard the wake word");
                                send(VoiceUpdate::Woke);
                            }
                            Heard::Nothing => {
                                tracing::info!("voice: nothing said after the wake word");
                                send(VoiceUpdate::NotUnderstood);
                            }
                            Heard::Request(audio) => {
                                tracing::info!(
                                    "voice: request of {:.1} s",
                                    audio.len() as f32 / super::listen::RATE as f32
                                );
                                send(VoiceUpdate::Thinking);
                                let Some(gemini) = gemini.clone() else {
                                    tracing::warn!(
                                        "voice: no Gemini key, so the request can't be answered"
                                    );
                                    send(VoiceUpdate::Failed(
                                        "Voice needs a Gemini key or Vertex key file in the config"
                                            .into(),
                                    ));
                                    continue;
                                };
                                let context = context.lock().unwrap().clone();
                                let inputs = inputs.clone();
                                runtime.spawn(async move {
                                    let update = match gemini.ask(&context, &audio).await {
                                        Ok(Some(command)) => VoiceUpdate::Command(command),
                                        Ok(None) => VoiceUpdate::NotUnderstood,
                                        Err(e) => VoiceUpdate::Failed(e.to_string()),
                                    };
                                    tracing::info!("voice: {update:?}");
                                    let _ = inputs.send(Input::Voice(update));
                                });
                            }
                        }
                    }
                }
                tracing::warn!("voice: the microphone stopped; reopening");
            }
        })
        .expect("voice thread");
}
