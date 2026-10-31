//! Asks Gemini what a spoken request means, as one call to a fixed set of functions.

use std::time::Duration;

use base64::Engine;
use serde_json::{Value, json};

use crate::app::{HomeAction, SearchKind, VoiceCommand, VoiceContext};

/// Fastest of the models tested (about 1.1–1.6 s through Vertex) and as accurate on requests.
pub const DEFAULT_MODEL: &str = "gemini-3.5-flash-lite";
const ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta/models";
const VERTEX_HOST: &str = "https://aiplatform.googleapis.com/v1";

/// 16 kHz mono 16-bit PCM in a WAV container.
pub fn wav(samples: &[f32]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&16_000u32.to_le_bytes());
    out.extend_from_slice(&32_000u32.to_le_bytes()); // bytes per second
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    out
}

fn instructions(context: &VoiceContext) -> String {
    let mut text = String::from(
        "You control a music player on a small touchscreen at home. The audio is one spoken \
         request. It may begin with the player's wake word (such as \"ziggy\"); ignore that. \
         Choose exactly one function.\n\
         - If the request names something in the library below, call play_item with its id.\n\
         - Otherwise, to play music, call search_and_play with a precise Spotify search query. \
         Use what you know: for \"Paul Simon's first album\" search for that album's real title \
         and artist.\n\
         - For pause, resume, skipping, going back, louder or quieter, call player_control.\n\
         - \"Add\", \"heart\", \"like\" and \"save\" all mean the same: for the playing \
         song (\"heart this\", \"add this song\") call save_playing with song; for its album \
         (\"save the album\", \"add this album\") call save_playing with album. To put the \
         playing song in one of the listener's playlists (\"add this to Road Trip\"), call \
         add_to_playlist with that playlist's id, but only when a playlist is named; with no \
         playlist named, \"add this\" means save_playing.\n\
         - If the audio isn't a request for the player, or you can't tell what was asked, \
         call not_understood.\n\n",
    );
    if !context.home.is_empty() {
        text.push_str(
            "The player also controls lights, plugs and scenes in the house, listed under Home \
             below. For a request to switch, dim or brighten them, or to run a scene, call \
             home_control with their ids from that list, never others.\n\
             - A light group is all the lights of a room: for the whole room (\"turn off the \
             mud room\") use the group; for one light use that light.\n\
             - \"All the lights\" or \"everything\" means every light, plug and fan in the \
             list.\n\
             - With no light named, \"brighter\" or \"dimmer\" means the lights that are on.\n\
             - A scene is run with action run; its name may leave out the room (\"nightlight \
             in the bedroom\" is the bedroom's Nightlight scene).\n\
             - If you can't tell which controls were meant, call not_understood.\n\n",
        );
    }
    match &context.now_playing {
        Some(track) if context.playing => text.push_str(&format!("Playing now: {track}.\n")),
        Some(track) => text.push_str(&format!("Paused: {track}.\n")),
        None => text.push_str("Nothing is playing.\n"),
    }
    text.push_str(&format!(
        "Volume: {}%.\n\nLibrary (id | kind | name | by):\n",
        context.volume
    ));
    for item in &context.items {
        text.push_str(&format!(
            "{} | {} | {} | {}\n",
            item.uri, item.kind, item.name, item.by
        ));
    }
    if !context.home.is_empty() {
        text.push_str("\nHome (id | kind | name | room | now):\n");
        for c in &context.home {
            text.push_str(&format!(
                "{} | {} | {} | {} | {}\n",
                c.entity_id, c.kind, c.name, c.room, c.now
            ));
        }
    }
    text
}

fn functions(home: bool) -> Value {
    let mut list = json!([
        {
            "name": "play_item",
            "description": "Play a playlist, album, artist or Liked Songs from the library list.",
            "parameters": {
                "type": "object",
                "properties": {"id": {"type": "string", "description": "The id from the library list."}},
                "required": ["id"]
            }
        },
        {
            "name": "search_and_play",
            "description": "Search Spotify and play the best match. For music not in the library.",
            "parameters": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Spotify search words, e.g. \"Graceland Paul Simon\"."},
                    "kind": {"type": "string", "enum": ["song", "album", "artist", "playlist"]}
                },
                "required": ["query", "kind"]
            }
        },
        {
            "name": "player_control",
            "description": "Pause, resume, skip to the next song, go back, or change the volume a step.",
            "parameters": {
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["pause", "resume", "next", "previous", "louder", "quieter"]}
                },
                "required": ["action"]
            }
        },
        {
            "name": "set_volume",
            "description": "Set the volume to a percentage.",
            "parameters": {
                "type": "object",
                "properties": {"percent": {"type": "integer", "description": "0 to 100."}},
                "required": ["percent"]
            }
        },
        {
            "name": "sleep_timer",
            "description": "Stop the music after some minutes; 0 turns the sleep timer off.",
            "parameters": {
                "type": "object",
                "properties": {"minutes": {"type": "integer"}},
                "required": ["minutes"]
            }
        },
        {
            "name": "save_playing",
            "description": "Heart the playing song (add it to Liked Songs), or save its album to the library.",
            "parameters": {
                "type": "object",
                "properties": {"what": {"type": "string", "enum": ["song", "album"]}},
                "required": ["what"]
            }
        },
        {
            "name": "add_to_playlist",
            "description": "Add the playing song to a playlist the listener named. Never guess a playlist.",
            "parameters": {
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "The playlist's id from the library list."},
                    "heard_name": {"type": "string", "description": "The playlist name exactly as the listener said it; empty if they didn't say one."}
                },
                "required": ["id", "heard_name"]
            }
        },
        {
            "name": "not_understood",
            "description": "The audio isn't a request for the player, or it's unclear.",
            "parameters": {"type": "object", "properties": {}}
        }
    ]);
    if home {
        list.as_array_mut().expect("a list").push(json!({
            "name": "home_control",
            "description": "Switch, dim or brighten lights, plugs and fans in the Home list, or run its scenes.",
            "parameters": {
                "type": "object",
                "properties": {
                    "ids": {"type": "array", "items": {"type": "string"}, "description": "Ids from the Home list."},
                    "action": {"type": "string", "enum": ["on", "off", "brightness", "brighter", "dimmer", "run"]},
                    "percent": {"type": "integer", "description": "For brightness: 0 to 100."}
                },
                "required": ["ids", "action"]
            }
        }));
    }
    list
}

/// The generateContent request body.
pub fn request(context: &VoiceContext, audio: &[f32]) -> Value {
    json!({
        "systemInstruction": {"parts": [{"text": instructions(context)}]},
        "contents": [{
            "role": "user",
            "parts": [{
                "inlineData": {
                    "mimeType": "audio/wav",
                    "data": base64::engine::general_purpose::STANDARD.encode(wav(audio))
                }
            }]
        }],
        "tools": [{"functionDeclarations": functions(!context.home.is_empty())}],
        "toolConfig": {"functionCallingConfig": {"mode": "ANY"}},
        "generationConfig": {"temperature": 0}
    })
}

/// The command in Gemini's reply; None when it didn't understand.
pub fn parse_reply(reply: &Value) -> Option<VoiceCommand> {
    let call = reply["candidates"][0]["content"]["parts"]
        .as_array()?
        .iter()
        .find_map(|part| part.get("functionCall"))?;
    let args = &call["args"];
    let text = |key: &str| args[key].as_str().map(str::trim).filter(|s| !s.is_empty());
    let number = |key: &str| {
        args[key]
            .as_f64()
            .map(|n| n.round().clamp(0.0, 1000.0) as u32)
    };
    match call["name"].as_str()? {
        "play_item" => Some(VoiceCommand::Play(text("id")?.to_string())),
        "search_and_play" => Some(VoiceCommand::Search {
            query: text("query")?.to_string(),
            kind: match text("kind")? {
                "song" => SearchKind::Song,
                "album" => SearchKind::Album,
                "artist" => SearchKind::Artist,
                "playlist" => SearchKind::Playlist,
                _ => return None,
            },
        }),
        "player_control" => Some(match text("action")? {
            "pause" => VoiceCommand::Pause,
            "resume" => VoiceCommand::Resume,
            "next" => VoiceCommand::Next,
            "previous" => VoiceCommand::Previous,
            "louder" => VoiceCommand::Louder,
            "quieter" => VoiceCommand::Quieter,
            _ => return None,
        }),
        "set_volume" => Some(VoiceCommand::SetVolume(number("percent")?.min(100) as u8)),
        "sleep_timer" => Some(VoiceCommand::SleepTimer(number("minutes")?.min(240))),
        "save_playing" => match text("what")? {
            "song" => Some(VoiceCommand::LikeSong),
            "album" => Some(VoiceCommand::SaveAlbum),
            _ => None,
        },
        "add_to_playlist" => Some(match text("heard_name") {
            Some(heard) => VoiceCommand::AddToPlaylist {
                uri: text("id")?.to_string(),
                heard: heard.to_string(),
            },
            // "Add this" with no playlist named means heart it.
            None => VoiceCommand::LikeSong,
        }),
        "home_control" => {
            let ids: Vec<String> = args["ids"]
                .as_array()?
                .iter()
                .filter_map(Value::as_str)
                .map(|id| id.trim().to_string())
                .filter(|id| !id.is_empty())
                .collect();
            if ids.is_empty() {
                return None;
            }
            let action = match text("action")? {
                "on" => HomeAction::On,
                "off" => HomeAction::Off,
                "brightness" => HomeAction::Brightness(number("percent")?.min(100) as u8),
                "brighter" => HomeAction::Brighter,
                "dimmer" => HomeAction::Dimmer,
                "run" => HomeAction::Run,
                _ => return None,
            };
            Some(VoiceCommand::Home { ids, action })
        }
        _ => None,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AskError {
    #[error("No internet right now")]
    Offline,
    #[error("Voice isn't set up: the Gemini key was refused")]
    BadKey,
    #[error("Gemini is busy, try again")]
    Busy,
    #[error("Couldn't ask Gemini (HTTP {0})")]
    Http(u16),
}

/// How requests reach Gemini.
pub enum Backend {
    /// The Gemini API with a key from Google AI Studio.
    ApiKey(String),
    /// Vertex AI on Google Cloud, as a service account; Cloud credits pay for it.
    Vertex {
        account: Box<super::google_auth::ServiceAccount>,
        /// "global", or a region such as "europe-west4".
        location: String,
    },
}

pub struct Gemini {
    http: reqwest::Client,
    backend: Backend,
    model: String,
}

impl Gemini {
    pub fn new(backend: Backend, model: Option<String>) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .expect("HTTP client"),
            backend,
            model: model.unwrap_or_else(|| DEFAULT_MODEL.to_string()),
        }
    }

    fn post(&self, url: String) -> reqwest::RequestBuilder {
        self.http.post(url)
    }

    pub async fn ask(
        &self,
        context: &VoiceContext,
        audio: &[f32],
    ) -> Result<Option<VoiceCommand>, AskError> {
        let model = &self.model;
        let builder = match &self.backend {
            Backend::ApiKey(key) => self
                .post(format!("{ENDPOINT}/{model}:generateContent"))
                .header("x-goog-api-key", key),
            Backend::Vertex { account, location } => {
                let token = account.token(&self.http).await.map_err(|e| {
                    tracing::warn!("Vertex AI sign-in failed: {e:#}");
                    AskError::Offline
                })?;
                let host = if location == "global" {
                    VERTEX_HOST.to_string()
                } else {
                    format!("https://{location}-aiplatform.googleapis.com/v1")
                };
                self.post(format!(
                    "{host}/projects/{}/locations/{location}/publishers/google/models/{model}:generateContent",
                    account.project_id
                ))
                .bearer_auth(token)
            }
        };
        let response = builder
            .json(&request(context, audio))
            .send()
            .await
            .map_err(|_| AskError::Offline)?;
        match response.status().as_u16() {
            200 => {}
            400 | 401 | 403 => {
                let body = response.text().await.unwrap_or_default();
                tracing::warn!("Gemini refused the request: {body}");
                return Err(if body.contains("API_KEY") || body.contains("PERMISSION") {
                    AskError::BadKey
                } else {
                    AskError::Http(400)
                });
            }
            429 | 503 => return Err(AskError::Busy),
            status => return Err(AskError::Http(status)),
        }
        let reply: Value = response.json().await.map_err(|_| AskError::Http(200))?;
        Ok(parse_reply(&reply))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::VoiceItem;

    fn context() -> VoiceContext {
        VoiceContext {
            now_playing: Some("Graceland by Paul Simon".into()),
            playing: true,
            volume: 60,
            items: vec![VoiceItem {
                uri: "spotify:playlist:road".into(),
                kind: "playlist",
                name: "Road Trip".into(),
                by: "sam".into(),
            }],
            ..VoiceContext::default()
        }
    }

    fn reply(name: &str, args: Value) -> Value {
        json!({"candidates": [{"content": {"role": "model", "parts": [
            {"functionCall": {"name": name, "args": args}}
        ]}}]})
    }

    #[test]
    fn the_request_carries_audio_library_and_functions() {
        let body = request(&context(), &[0.0; 1600]);
        let instructions = body["systemInstruction"]["parts"][0]["text"]
            .as_str()
            .unwrap();
        assert!(instructions.contains("spotify:playlist:road | playlist | Road Trip | sam"));
        assert!(instructions.contains("Playing now: Graceland by Paul Simon."));
        let audio = &body["contents"][0]["parts"][0]["inlineData"];
        assert_eq!(audio["mimeType"], "audio/wav");
        let wav = base64::engine::general_purpose::STANDARD
            .decode(audio["data"].as_str().unwrap())
            .unwrap();
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(wav.len(), 44 + 3200);
        assert_eq!(body["toolConfig"]["functionCallingConfig"]["mode"], "ANY");
        assert_eq!(
            body["tools"][0]["functionDeclarations"]
                .as_array()
                .unwrap()
                .len(),
            8
        );
    }

    #[test]
    fn replies_become_commands() {
        assert_eq!(
            parse_reply(&reply("play_item", json!({"id": "spotify:playlist:road"}))),
            Some(VoiceCommand::Play("spotify:playlist:road".into()))
        );
        assert_eq!(
            parse_reply(&reply(
                "search_and_play",
                json!({"query": "The Paul Simon Songbook", "kind": "album"})
            )),
            Some(VoiceCommand::Search {
                query: "The Paul Simon Songbook".into(),
                kind: SearchKind::Album
            })
        );
        assert_eq!(
            parse_reply(&reply("player_control", json!({"action": "next"}))),
            Some(VoiceCommand::Next)
        );
        assert_eq!(
            parse_reply(&reply("set_volume", json!({"percent": 140}))),
            Some(VoiceCommand::SetVolume(100))
        );
        assert_eq!(
            parse_reply(&reply("sleep_timer", json!({"minutes": 30.0}))),
            Some(VoiceCommand::SleepTimer(30))
        );
    }

    #[test]
    fn saving_replies_become_commands() {
        assert_eq!(
            parse_reply(&reply("save_playing", json!({"what": "song"}))),
            Some(VoiceCommand::LikeSong)
        );
        assert_eq!(
            parse_reply(&reply("save_playing", json!({"what": "album"}))),
            Some(VoiceCommand::SaveAlbum)
        );
        assert_eq!(
            parse_reply(&reply(
                "add_to_playlist",
                json!({"id": "spotify:playlist:road", "heard_name": "road trip"})
            )),
            Some(VoiceCommand::AddToPlaylist {
                uri: "spotify:playlist:road".into(),
                heard: "road trip".into()
            })
        );
        assert_eq!(
            parse_reply(&reply(
                "add_to_playlist",
                json!({"id": "spotify:playlist:road", "heard_name": ""})
            )),
            Some(VoiceCommand::LikeSong),
            "no playlist named"
        );
        assert_eq!(
            parse_reply(&reply("save_playing", json!({"what": "artist"}))),
            None
        );
    }

    #[test]
    fn home_controls_are_offered_only_when_the_player_has_them() {
        let mut ctx = context();
        let body = request(&ctx, &[0.0; 1600]);
        let names = |body: &Value| -> Vec<String> {
            body["tools"][0]["functionDeclarations"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| f["name"].as_str().unwrap().to_string())
                .collect()
        };
        assert!(!names(&body).contains(&"home_control".to_string()));
        assert!(
            !body["systemInstruction"]["parts"][0]["text"]
                .as_str()
                .unwrap()
                .contains("Home (")
        );
        ctx.home = vec![crate::app::VoiceHomeControl {
            entity_id: "light.mud_room".into(),
            kind: "light group of 4".into(),
            name: "Mud room".into(),
            room: "Mud room".into(),
            now: "on at 70%".into(),
        }];
        let body = request(&ctx, &[0.0; 1600]);
        assert!(names(&body).contains(&"home_control".to_string()));
        assert!(
            body["systemInstruction"]["parts"][0]["text"]
                .as_str()
                .unwrap()
                .contains("light.mud_room | light group of 4 | Mud room | Mud room | on at 70%")
        );
    }

    #[test]
    fn home_replies_become_commands() {
        let home = |args: Value| parse_reply(&reply("home_control", args));
        assert_eq!(
            home(json!({"ids": ["light.mr", "light.lr"], "action": "off"})),
            Some(VoiceCommand::Home {
                ids: vec!["light.mr".into(), "light.lr".into()],
                action: HomeAction::Off
            })
        );
        assert_eq!(
            home(json!({"ids": ["light.mr"], "action": "brightness", "percent": 140})),
            Some(VoiceCommand::Home {
                ids: vec!["light.mr".into()],
                action: HomeAction::Brightness(100)
            })
        );
        assert_eq!(
            home(json!({"ids": ["scene.nightlight"], "action": "run"})),
            Some(VoiceCommand::Home {
                ids: vec!["scene.nightlight".into()],
                action: HomeAction::Run
            })
        );
        assert_eq!(home(json!({"ids": [], "action": "on"})), None, "no ids");
        assert_eq!(
            home(json!({"ids": ["light.mr"], "action": "brightness"})),
            None,
            "no level"
        );
        assert_eq!(
            home(json!({"ids": ["light.mr"], "action": "explode"})),
            None
        );
    }

    #[test]
    fn unclear_or_broken_replies_are_not_understood() {
        assert_eq!(parse_reply(&reply("not_understood", json!({}))), None);
        assert_eq!(
            parse_reply(&reply("player_control", json!({"action": "dance"}))),
            None
        );
        assert_eq!(
            parse_reply(&reply("search_and_play", json!({"query": " "}))),
            None
        );
        assert_eq!(parse_reply(&reply("format_disk", json!({}))), None);
        assert_eq!(parse_reply(&json!({"candidates": []})), None);
        let text_only = json!({"candidates": [{"content": {"parts": [{"text": "Sure!"}]}}]});
        assert_eq!(parse_reply(&text_only), None);
    }

    /// With a real key: `GEMINI_API_KEY=… cargo test -- --ignored live_gemini --nocapture`,
    /// or `VERTEX_KEY_FILE=…` for Vertex AI. Speaks requests with the Mac's `say` command.
    #[tokio::test]
    #[ignore]
    async fn live_gemini() {
        let backend = if let Ok(path) = std::env::var("VERTEX_KEY_FILE") {
            Backend::Vertex {
                account: Box::new(
                    crate::voice::google_auth::ServiceAccount::load(std::path::Path::new(&path))
                        .unwrap(),
                ),
                location: "global".into(),
            }
        } else if let Ok(key) = std::env::var("GEMINI_API_KEY") {
            Backend::ApiKey(key)
        } else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ask.wav");
        let cases = [
            (
                "play road trip",
                Some(VoiceCommand::Play("spotify:playlist:road".into())),
            ),
            ("skip this song", Some(VoiceCommand::Next)),
            ("heart this", Some(VoiceCommand::LikeSong)),
            ("add this song", Some(VoiceCommand::LikeSong)),
            ("save the album", Some(VoiceCommand::SaveAlbum)),
            (
                "add this to road trip",
                Some(VoiceCommand::AddToPlaylist {
                    uri: "spotify:playlist:road".into(),
                    heard: "road trip".into(),
                }),
            ),
            ("what's the capital of france", None),
        ];
        let gemini = Gemini::new(backend, std::env::var("GEMINI_MODEL").ok());
        for (said, want) in cases {
            let status = std::process::Command::new("say")
                .args([
                    "-o",
                    path.to_str().unwrap(),
                    "--data-format=LEI16@16000",
                    said,
                ])
                .status()
                .unwrap();
            assert!(status.success());
            let bytes = std::fs::read(&path).unwrap();
            // `say` writes a plain 44-byte header for this format.
            let audio: Vec<f32> = bytes[44..]
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
                .collect();
            let started = std::time::Instant::now();
            let got = gemini
                .ask(&context(), &audio)
                .await
                .unwrap()
                .map(|c| match c {
                    // As the player resolves it: a playlist only when its name was said (in
                    // any case), otherwise a heart.
                    VoiceCommand::AddToPlaylist { uri, heard }
                        if heard.to_lowercase().contains("road") =>
                    {
                        VoiceCommand::AddToPlaylist {
                            uri,
                            heard: heard.to_lowercase(),
                        }
                    }
                    VoiceCommand::AddToPlaylist { .. } => VoiceCommand::LikeSong,
                    c => c,
                });
            println!("{said:?} -> {got:?} in {:?}", started.elapsed());
            assert_eq!(got, want, "{said}");
        }
        // Something not in the library becomes a search.
        let status = std::process::Command::new("say")
            .args([
                "-o",
                path.to_str().unwrap(),
                "--data-format=LEI16@16000",
                "play paul simon's first album",
            ])
            .status()
            .unwrap();
        assert!(status.success());
        let bytes = std::fs::read(&path).unwrap();
        let audio: Vec<f32> = bytes[44..]
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
            .collect();
        let got = gemini.ask(&context(), &audio).await.unwrap();
        println!("first album -> {got:?}");
        assert!(matches!(
            got,
            Some(VoiceCommand::Search {
                kind: SearchKind::Album,
                ..
            })
        ));
    }

    /// Home requests, spoken with `say`, against a house like Matt's:
    /// `VERTEX_KEY_FILE=… cargo test -- --ignored live_gemini_home --nocapture`.
    #[tokio::test]
    #[ignore]
    async fn live_gemini_home() {
        let Ok(path) = std::env::var("VERTEX_KEY_FILE") else {
            return;
        };
        let backend = Backend::Vertex {
            account: Box::new(
                crate::voice::google_auth::ServiceAccount::load(std::path::Path::new(&path))
                    .unwrap(),
            ),
            location: "global".into(),
        };
        let row = |id: &str, kind: &str, name: &str, room: &str, now: &str| {
            crate::app::VoiceHomeControl {
                entity_id: id.into(),
                kind: kind.into(),
                name: name.into(),
                room: room.into(),
                now: now.into(),
            }
        };
        let mut ctx = context();
        ctx.home = vec![
            row(
                "light.master_bedroom",
                "light group of 2",
                "Master bedroom",
                "Master bedroom",
                "on at 70%",
            ),
            row("light.mr", "light", "MR", "Master bedroom", "on at 70%"),
            row("light.lr", "light", "LR", "Master bedroom", "on at 70%"),
            row(
                "scene.master_bedroom_nightlight",
                "scene",
                "Master bedroom Nightlight",
                "Master bedroom",
                "",
            ),
            row(
                "scene.master_bedroom_dimmed",
                "scene",
                "Master bedroom Dimmed",
                "Master bedroom",
                "",
            ),
            row(
                "light.mud_room",
                "light group of 4",
                "Mud room",
                "Mud room",
                "off",
            ),
            row("light.mud_3", "light", "Mud 3", "Mud room", "off"),
            row(
                "scene.mud_room_bright",
                "scene",
                "Mud room Bright",
                "Mud room",
                "",
            ),
        ];
        let home = |ids: &[&str], action: HomeAction| {
            Some(VoiceCommand::Home {
                ids: ids.iter().map(|s| s.to_string()).collect(),
                action,
            })
        };
        let cases = [
            (
                "turn off the mud room",
                home(&["light.mud_room"], HomeAction::Off),
            ),
            (
                "turn on the mudroom lights",
                home(&["light.mud_room"], HomeAction::On),
            ),
            (
                "dim m r to thirty percent",
                home(&["light.mr"], HomeAction::Brightness(30)),
            ),
            (
                "bedroom nightlight",
                home(&["scene.master_bedroom_nightlight"], HomeAction::Run),
            ),
            (
                "turn off the bedroom",
                home(&["light.master_bedroom"], HomeAction::Off),
            ),
            (
                "make it brighter in the bedroom",
                home(&["light.master_bedroom"], HomeAction::Brighter),
            ),
            ("skip this song", Some(VoiceCommand::Next)),
        ];
        let dir = tempfile::tempdir().unwrap();
        let wav_path = dir.path().join("ask.wav");
        let gemini = Gemini::new(backend, std::env::var("GEMINI_MODEL").ok());
        let mut wrong = 0;
        for (said, want) in cases {
            let status = std::process::Command::new("say")
                .args([
                    "-o",
                    wav_path.to_str().unwrap(),
                    "--data-format=LEI16@16000",
                    said,
                ])
                .status()
                .unwrap();
            assert!(status.success());
            let bytes = std::fs::read(&wav_path).unwrap();
            let audio: Vec<f32> = bytes[44..]
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
                .collect();
            let started = std::time::Instant::now();
            let got = gemini.ask(&ctx, &audio).await.unwrap();
            let ok = got == want;
            wrong += usize::from(!ok);
            println!(
                "{said:?} -> {got:?} in {:?}{}",
                started.elapsed(),
                if ok { "" } else { "  WRONG" }
            );
        }
        assert_eq!(wrong, 0);
    }
}
