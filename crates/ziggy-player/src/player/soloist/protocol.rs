//! Soloist's local WebSocket API: the commands Ziggy sends and the events it reads back.
//! Pure, so it's tested without a Soloist running.
//! <https://developer.spotify.com/documentation/soloist/reference/websocket-api>

use serde_json::{Value, json};

use crate::app::{PlayerCommand, PlayerUpdate};
use crate::model::{LIKED_URI, Repeat, Track};

/// A command frame.
pub fn command(name: &str) -> Value {
    json!({ "type": "command", "command": name })
}

fn with(mut frame: Value, key: &str, value: Value) -> Value {
    frame[key] = value;
    frame
}

/// Liked Songs is the user's collection to Spotify.
pub fn context_uri_for(uri: &str, username: &str) -> String {
    if uri == LIKED_URI && !username.is_empty() {
        format!("spotify:user:{username}:collection")
    } else {
        uri.to_string()
    }
}

/// The frames for everything except Load, which needs the queue to find its starting song.
pub fn frames(command: &PlayerCommand) -> Vec<Value> {
    match command {
        PlayerCommand::Load { .. } => Vec::new(),
        PlayerCommand::Play => vec![self::command("play")],
        PlayerCommand::Pause => vec![self::command("pause")],
        PlayerCommand::Next => vec![self::command("skip_next")],
        PlayerCommand::Previous => vec![self::command("skip_prev")],
        PlayerCommand::Seek { position_ms } => {
            vec![with(
                self::command("seek"),
                "position_ms",
                json!(position_ms),
            )]
        }
        PlayerCommand::SetVolume { percent } => {
            vec![with(
                self::command("set_volume"),
                "volume",
                json!(percent.min(&100)),
            )]
        }
        PlayerCommand::SetShuffle(on) => {
            vec![with(self::command("set_shuffle"), "enabled", json!(on))]
        }
        // Soloist sets the two repeat flags separately; turn one off before the other on.
        PlayerCommand::SetRepeat(repeat) => {
            let track = with(
                self::command("set_repeat_track"),
                "enabled",
                json!(*repeat == Repeat::Track),
            );
            let context = with(
                self::command("set_repeat_context"),
                "enabled",
                json!(*repeat == Repeat::Context),
            );
            if *repeat == Repeat::Track {
                vec![context, track]
            } else {
                vec![track, context]
            }
        }
    }
}

/// Starting a list: shuffle first, so the list starts in the right order, then play it.
pub fn load_frames(context_uri: &str, shuffle: bool) -> Vec<Value> {
    vec![
        with(self::command("set_shuffle"), "enabled", json!(shuffle)),
        with(self::command("play"), "uri", json!(context_uri)),
    ]
}

/// Soloist can't start a list at a given song, so the list starts and skips ahead. How many
/// skips reach `start_uri` (by the queue Soloist reports) or `start_index`; None when the song
/// isn't in reach, and the list plays from the top.
pub fn skips_to(
    upcoming: &[String],
    current: Option<&str>,
    start_uri: Option<&str>,
    start_index: Option<u32>,
) -> Option<usize> {
    if let Some(uri) = start_uri {
        if current == Some(uri) {
            return Some(0);
        }
        return upcoming.iter().position(|u| u == uri).map(|i| i + 1);
    }
    start_index.map(|i| i as usize)
}

/// What a frame from Soloist means for the app.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Signed in to Spotify or not; while not, the device waits to be picked in a Spotify app.
    Auth {
        logged_in: bool,
        username: Option<String>,
    },
    Updates(Vec<PlayerUpdate>),
    /// Where playback is, and how fast it moves (0 when paused). Soloist sends this only when
    /// it changes; the engine counts on from it.
    Position {
        position_ms: u32,
        speed: f64,
    },
    /// The upcoming tracks, as asked for with `get_queue`.
    Queue {
        current: Option<String>,
        upcoming: Vec<String>,
    },
    Error(String),
    Other,
}

pub fn parse(frame: &str) -> Event {
    let Ok(v) = serde_json::from_str::<Value>(frame) else {
        return Event::Other;
    };
    match v["type"].as_str().unwrap_or("") {
        "auth_state" => Event::Auth {
            logged_in: v["logged_in"].as_bool().unwrap_or(false),
            username: v["username"].as_str().map(str::to_string),
        },
        "playback_state" => {
            let mut updates = playback_state(&v);
            // The engine counts on from here, so the screen's clock moves every second.
            updates.push(PlayerUpdate::Position {
                position_ms: position(&v["position"]),
            });
            Event::Updates(updates)
        }
        "track_changed" => Event::Updates(
            track(&v["item"])
                .map(PlayerUpdate::TrackChanged)
                .into_iter()
                .collect(),
        ),
        "playback_changed" => Event::Updates(
            status(v["status"].as_str().unwrap_or(""), None)
                .into_iter()
                .collect(),
        ),
        "position_sync" => Event::Position {
            position_ms: position(&v["position"]),
            speed: v["position"]["speed"].as_f64().unwrap_or(0.0),
        },
        "volume_changed" => Event::Updates(volume(&v["volume"]).into_iter().collect()),
        "options_changed" => Event::Updates(options(&v["options"])),
        // Another device took over playback: this one is no longer playing.
        "device_changed" if v["is_active"].as_bool() == Some(false) => {
            Event::Updates(vec![PlayerUpdate::Paused { position_ms: 0 }])
        }
        "queue_changed" => Event::Queue {
            current: None,
            upcoming: v["upcoming"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|e| e["item"]["uri"].as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        },
        "error" => Event::Error(v["message"].as_str().unwrap_or("").to_string()),
        _ => Event::Other,
    }
}

fn playback_state(v: &Value) -> Vec<PlayerUpdate> {
    let mut out = Vec::new();
    if let Some(t) = track(&v["item"]) {
        out.push(PlayerUpdate::TrackChanged(t));
    }
    out.extend(volume(&v["volume"]));
    out.extend(options(&v["options"]));
    out.extend(status(
        v["status"].as_str().unwrap_or(""),
        Some(position(&v["position"])),
    ));
    out
}

fn status(status: &str, position_ms: Option<u32>) -> Option<PlayerUpdate> {
    Some(match status {
        "playing" => PlayerUpdate::Playing {
            position_ms: position_ms.unwrap_or(0),
        },
        "paused" => PlayerUpdate::Paused {
            position_ms: position_ms.unwrap_or(0),
        },
        "buffering" => PlayerUpdate::Loading,
        "idle" => PlayerUpdate::Stopped,
        _ => return None,
    })
}

/// Where playback was at `timestamp_ms`; good enough for the screen, which counts on itself.
fn position(p: &Value) -> u32 {
    p["position_ms"].as_u64().unwrap_or(0).min(u32::MAX as u64) as u32
}

fn volume(v: &Value) -> Option<PlayerUpdate> {
    v.as_u64().map(|p| PlayerUpdate::Volume {
        percent: p.min(100) as u8,
    })
}

fn options(o: &Value) -> Vec<PlayerUpdate> {
    let mut out = Vec::new();
    if let Some(shuffle) = o["shuffle"].as_bool() {
        out.push(PlayerUpdate::Shuffle(shuffle));
    }
    if let Some(repeat) = o["repeat"].as_str() {
        out.push(PlayerUpdate::Repeat(match repeat {
            "track" => Repeat::Track,
            "context" => Repeat::Context,
            _ => Repeat::Off,
        }));
    }
    out
}

fn name(entity: &Value) -> String {
    entity["decorations"]["identity"]["name"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

/// A song from Soloist's entity shape. None for nothing playing, or an ad.
pub fn track(item: &Value) -> Option<Track> {
    let uri = item["uri"].as_str()?;
    if !matches!(item["entity_type"].as_str(), Some("track" | "episode")) {
        return None;
    }
    let d = &item["decorations"];
    let creators: Vec<&Value> = d["creators"]
        .as_array()
        .map(|c| c.iter().map(|c| &c["entity"]).collect())
        .unwrap_or_default();
    let parent = &d["parent"]["entity"];
    let covers = d["visual_identity"]["cover"].as_array();
    // "default" is about 300 px, right for the screen; fall back to any.
    let image_url = covers.and_then(|c| {
        ["default", "large", "small", "xlarge"]
            .iter()
            .find_map(|size| {
                c.iter()
                    .find(|i| i["size"].as_str() == Some(size))
                    .and_then(|i| i["url"].as_str().map(str::to_string))
            })
    });
    Some(Track {
        uri: uri.to_string(),
        name: name(item),
        artists: creators
            .iter()
            .map(|c| name(c))
            .filter(|n| !n.is_empty())
            .collect::<Vec<_>>()
            .join(", "),
        album: name(parent),
        image_url,
        duration_ms: d["playback"]["duration_ms"].as_u64().unwrap_or(0) as u32,
        album_uri: parent["uri"].as_str().map(str::to_string),
        artist_uri: creators
            .first()
            .and_then(|c| c["uri"].as_str().map(str::to_string)),
    })
}

/// The PipeWire node for a Bluetooth speaker, e.g. `bluez_output.3C_B8_A9_30_65_8E.1`.
pub fn bluetooth_node(address: &str) -> String {
    format!("bluez_output.{}.1", address.replace(':', "_"))
}

/// The headphone jack's node in `pw-dump` output: the Pi's built-in audio, not HDMI or USB.
pub fn jack_node(pw_dump: &str) -> Option<String> {
    let nodes: Vec<Value> = serde_json::from_str(pw_dump).ok()?;
    let sinks: Vec<String> = nodes
        .iter()
        .filter(|n| n["type"] == "PipeWire:Interface:Node")
        .map(|n| &n["info"]["props"])
        .filter(|p| p["media.class"] == "Audio/Sink")
        .filter_map(|p| p["node.name"].as_str().map(str::to_string))
        .filter(|name| name.starts_with("alsa_output."))
        .collect();
    sinks
        .iter()
        .find(|n| n.contains("mailbox") || n.contains("bcm2835") || n.contains("Headphones"))
        .or_else(|| {
            sinks
                .iter()
                .find(|n| !n.contains("hdmi") && !n.contains("usb"))
        })
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real `playback_state` from Soloist 1.3.8 on a Pi 3 A+, trimmed.
    const STATE: &str = r#"{"type":"playback_state","status":"paused",
      "item":{"uri":"spotify:track:4Yxc55NX3tAXC2mHRAhtcW","entity_type":"track","decorations":{
        "identity":{"name":"All I Want"},
        "visual_identity":{"cover":[
          {"url":"https://i.scdn.co/image/small","size":"small"},
          {"url":"https://i.scdn.co/image/default","size":"default"},
          {"url":"https://i.scdn.co/image/large","size":"large"}]},
        "parent":{"entity":{"uri":"spotify:album:5p3gSxNiXeYlPlztVAUjB2","entity_type":"album",
          "decorations":{"identity":{"name":"All I Want (Single)"}}}},
        "creators":[
          {"entity":{"uri":"spotify:artist:1McMsnEElThX1knmY4oliG","entity_type":"artist","decorations":{"identity":{"name":"Olivia Rodrigo"}}}},
          {"entity":{"uri":"spotify:artist:3xvaSlT4xsyk6lY1ESOspO","entity_type":"artist","decorations":{"identity":{"name":"Disney"}}}}],
        "playback":{"duration_ms":177322,"content_ratings":[]}}},
      "context":{"uri":"spotify:user:31fz:collection","entity_type":"unknown","decorations":{}},
      "position":{"position_ms":5572,"timestamp_ms":1791308725444,"speed":0},
      "volume":15,"is_active":true,
      "options":{"shuffle":false,"repeat":"context","playback_speed":1}}"#;

    #[test]
    fn a_playback_state_becomes_the_song_volume_options_and_status() {
        let Event::Updates(updates) = parse(STATE) else {
            panic!("expected updates");
        };
        let PlayerUpdate::TrackChanged(t) = &updates[0] else {
            panic!("{updates:?}");
        };
        assert_eq!(t.name, "All I Want");
        assert_eq!(t.artists, "Olivia Rodrigo, Disney");
        assert_eq!(t.album, "All I Want (Single)");
        assert_eq!(
            t.album_uri.as_deref(),
            Some("spotify:album:5p3gSxNiXeYlPlztVAUjB2")
        );
        assert_eq!(
            t.artist_uri.as_deref(),
            Some("spotify:artist:1McMsnEElThX1knmY4oliG")
        );
        assert_eq!(
            t.image_url.as_deref(),
            Some("https://i.scdn.co/image/default")
        );
        assert_eq!(t.duration_ms, 177_322);
        assert_eq!(
            &updates[1..],
            [
                PlayerUpdate::Volume { percent: 15 },
                PlayerUpdate::Shuffle(false),
                PlayerUpdate::Repeat(Repeat::Context),
                PlayerUpdate::Paused { position_ms: 5572 },
                PlayerUpdate::Position { position_ms: 5572 },
            ]
        );
    }

    #[test]
    fn small_events_map_one_to_one() {
        assert_eq!(
            parse(r#"{"type":"playback_changed","status":"playing"}"#),
            Event::Updates(vec![PlayerUpdate::Playing { position_ms: 0 }])
        );
        assert_eq!(
            parse(
                r#"{"type":"position_sync","position":{"position_ms":45000,"timestamp_ms":1,"speed":1.0}}"#
            ),
            Event::Position {
                position_ms: 45000,
                speed: 1.0
            }
        );
        assert_eq!(
            parse(r#"{"type":"volume_changed","volume":42}"#),
            Event::Updates(vec![PlayerUpdate::Volume { percent: 42 }])
        );
        assert_eq!(
            parse(r#"{"type":"auth_state","logged_in":false,"is_active":false,"device_name":"K"}"#),
            Event::Auth {
                logged_in: false,
                username: None
            }
        );
        assert_eq!(
            parse(r#"{"type":"device_changed","is_active":false,"device_name":"K"}"#),
            Event::Updates(vec![PlayerUpdate::Paused { position_ms: 0 }])
        );
        assert_eq!(parse("not json"), Event::Other);
    }

    #[test]
    fn the_queue_lists_upcoming_songs() {
        let q = r#"{"type":"queue_changed","previous":[],"upcoming":[
          {"uid":"a","source":"context","item":{"uri":"spotify:track:a","entity_type":"track","decorations":{}}},
          {"uid":"b","source":"context","item":{"uri":"spotify:track:b","entity_type":"track","decorations":{}}}]}"#;
        assert_eq!(
            parse(q),
            Event::Queue {
                current: None,
                upcoming: vec!["spotify:track:a".into(), "spotify:track:b".into()],
            }
        );
    }

    #[test]
    fn commands_become_frames() {
        assert_eq!(frames(&PlayerCommand::Next), [command("skip_next")]);
        assert_eq!(
            frames(&PlayerCommand::SetVolume { percent: 150 })[0]["volume"],
            100
        );
        // Repeat one song: context off first, then track on.
        let r = frames(&PlayerCommand::SetRepeat(Repeat::Track));
        assert_eq!(r[0]["command"], "set_repeat_context");
        assert_eq!(r[0]["enabled"], false);
        assert_eq!(r[1]["command"], "set_repeat_track");
        assert_eq!(r[1]["enabled"], true);
        let load = load_frames("spotify:album:x", true);
        assert_eq!(load[0]["command"], "set_shuffle");
        assert_eq!(load[1]["uri"], "spotify:album:x");
    }

    #[test]
    fn starting_partway_through_a_list_counts_skips() {
        let upcoming = vec!["b".to_string(), "c".to_string(), "d".to_string()];
        assert_eq!(skips_to(&upcoming, Some("a"), Some("a"), None), Some(0));
        assert_eq!(skips_to(&upcoming, Some("a"), Some("c"), None), Some(2));
        assert_eq!(skips_to(&upcoming, Some("a"), Some("zz"), None), None);
        assert_eq!(skips_to(&upcoming, Some("a"), None, Some(4)), Some(4));
        assert_eq!(skips_to(&upcoming, Some("a"), None, None), None);
    }

    #[test]
    fn the_jack_is_the_built_in_sink_not_hdmi() {
        let dump = r#"[
          {"type":"PipeWire:Interface:Node","info":{"props":{"node.name":"alsa_output.platform-fef00700.hdmi.hdmi-stereo","media.class":"Audio/Sink"}}},
          {"type":"PipeWire:Interface:Node","info":{"props":{"node.name":"alsa_input.usb-C-Media.mono-fallback","media.class":"Audio/Source"}}},
          {"type":"PipeWire:Interface:Node","info":{"props":{"node.name":"alsa_output.platform-3f00b840.mailbox.stereo-fallback","media.class":"Audio/Sink"}}},
          {"type":"PipeWire:Interface:Node","info":{"props":{"node.name":"bluez_output.3C_B8.1","media.class":"Audio/Sink"}}},
          {"type":"PipeWire:Interface:Link","info":{}}]"#;
        assert_eq!(
            jack_node(dump).as_deref(),
            Some("alsa_output.platform-3f00b840.mailbox.stereo-fallback")
        );
        assert_eq!(jack_node("[]"), None);
    }

    #[test]
    fn liked_songs_and_speakers_get_their_spotify_and_pipewire_names() {
        assert_eq!(
            context_uri_for(LIKED_URI, "31fz"),
            "spotify:user:31fz:collection"
        );
        assert_eq!(
            context_uri_for("spotify:album:x", "31fz"),
            "spotify:album:x"
        );
        assert_eq!(
            bluetooth_node("3C:B8:A9:30:65:8E"),
            "bluez_output.3C_B8_A9_30_65_8E.1"
        );
    }
}
