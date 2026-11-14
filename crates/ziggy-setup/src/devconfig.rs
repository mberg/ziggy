//! The device's config file (/etc/ziggy/config.toml): made by setup, changed by `ziggy config`.

use toml::{Table, Value};

pub const CONFIG_PATH: &str = "/etc/ziggy/config.toml";
pub const STATE_DIR: &str = "/var/lib/ziggy";
pub const VERTEX_KEY_PATH: &str = "/var/lib/ziggy/vertex-key.json";
/// Where the player reads the Home Assistant token (its default in the player's config).
pub const HA_TOKEN_PATH: &str = "/var/lib/ziggy/ha-token";
/// The wake-word model, downloaded onto the device when voice is turned on.
pub const WAKE_MODEL: &str = "sherpa-onnx-kws-zipformer-gigaspeech-3.3M-2024-01-01";
/// The music on the card: `ziggy music` and the shared folder copy into it. It's the player's
/// default, so the config doesn't name it.
pub const MUSIC_DIR: &str = "/var/lib/ziggy-music";

pub fn wake_model_dir() -> String {
    format!("{STATE_DIR}/models/{WAKE_MODEL}")
}

/// How voice requests reach Gemini.
#[derive(Debug, Clone, PartialEq)]
pub enum Gemini {
    /// A Google Cloud service account key file (Vertex AI, paid by Cloud credits).
    Vertex,
    /// A Gemini API key from Google AI Studio.
    ApiKey(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Voice {
    pub wake_phrase: String,
    pub gemini: Gemini,
}

/// What setup asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct Choices {
    pub device_name: String,
    pub audiobookshelf_url: Option<String>,
    pub voice: Option<Voice>,
    /// False for a card-only player, with no Spotify account.
    pub spotify: bool,
    /// My music: music files on the player's card. Always on for a card-only player.
    pub my_music: bool,
    /// The music folder is shared on the network.
    pub music_share: bool,
}

pub fn new_config(choices: &Choices) -> Table {
    let mut t = Table::new();
    t.insert("device_name".into(), choices.device_name.clone().into());
    t.insert("state_dir".into(), STATE_DIR.into());
    t.insert("audio_backend".into(), "alsa".into());
    t.insert("audio_device".into(), "plughw:CARD=Headphones".into());
    t.insert("initial_volume".into(), Value::Integer(60));
    t.insert("dim_after_secs".into(), Value::Integer(180));
    t.insert("off_after_secs".into(), Value::Integer(600));
    if let Some(url) = &choices.audiobookshelf_url {
        t.insert("audiobookshelf_url".into(), url.clone().into());
    }
    if let Some(voice) = &choices.voice {
        set_voice(&mut t, voice);
    }
    if !choices.spotify {
        t.insert("spotify".into(), Value::Boolean(false));
    }
    t.insert(
        "my_music".into(),
        Value::Boolean(choices.my_music || !choices.spotify),
    );
    t.insert("music_share".into(), Value::Boolean(choices.music_share));
    t
}

/// Whether My music is on: when the config says so, and always on a card-only player.
pub fn my_music_on(t: &Table) -> bool {
    !spotify_on(t) || t.get("my_music").and_then(Value::as_bool).unwrap_or(false)
}

/// False on a card-only player. Spotify is on unless the config says otherwise.
pub fn spotify_on(t: &Table) -> bool {
    t.get("spotify").and_then(Value::as_bool).unwrap_or(true)
}

/// Whether the music folder is shared on the network. Off unless the config says so.
pub fn music_share_on(t: &Table) -> bool {
    t.get("music_share")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

pub fn set_voice(t: &mut Table, voice: &Voice) {
    t.insert("voice_model_dir".into(), wake_model_dir().into());
    t.insert("wake_phrase".into(), voice.wake_phrase.clone().into());
    match &voice.gemini {
        Gemini::Vertex => {
            t.insert("vertex_key_file".into(), VERTEX_KEY_PATH.into());
            t.remove("gemini_api_key");
        }
        Gemini::ApiKey(key) => {
            t.insert("gemini_api_key".into(), key.clone().into());
            t.remove("vertex_key_file");
        }
    }
}

pub fn voice_off(t: &mut Table) {
    for key in [
        "voice_model_dir",
        "wake_phrase",
        "wake_threshold",
        "microphone",
        "gemini_api_key",
        "vertex_key_file",
        "vertex_location",
        "gemini_model",
    ] {
        t.remove(key);
    }
}

pub fn render(t: &Table) -> String {
    format!(
        "# Ziggy device config, written by `ziggy setup`; change it with `ziggy config`.\n{}",
        toml::to_string(t).expect("a table renders")
    )
}

pub fn parse(text: &str) -> anyhow::Result<Table> {
    Ok(text.parse::<Table>()?)
}

/// One line per setting for the `ziggy config` menu.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Setting {
    DeviceName,
    Volume,
    DimAfter,
    OffAfter,
    Audiobookshelf,
    HomeAssistant,
    Voice,
    WakePhrase,
    WakeSensitivity,
    Microphone,
    GeminiModel,
    MyMusic,
    MusicShare,
}

impl Setting {
    pub const ALL: [Setting; 13] = [
        Setting::DeviceName,
        Setting::Volume,
        Setting::DimAfter,
        Setting::OffAfter,
        Setting::Audiobookshelf,
        Setting::HomeAssistant,
        Setting::Voice,
        Setting::WakePhrase,
        Setting::WakeSensitivity,
        Setting::Microphone,
        Setting::GeminiModel,
        Setting::MyMusic,
        Setting::MusicShare,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Setting::DeviceName => "Device name",
            Setting::Volume => "Starting volume",
            Setting::DimAfter => "Dim the screen after",
            Setting::OffAfter => "Turn the screen off after",
            Setting::Audiobookshelf => "Audiobookshelf server",
            Setting::HomeAssistant => "Home Assistant",
            Setting::Voice => "Voice control",
            Setting::WakePhrase => "Wake word",
            Setting::WakeSensitivity => "Wake word sensitivity",
            Setting::Microphone => "Microphone",
            Setting::GeminiModel => "Gemini model",
            Setting::MyMusic => "My music (files on the card)",
            Setting::MusicShare => "Shared music folder",
        }
    }

    /// The setting's current value, for the menu.
    pub fn show(self, t: &Table) -> String {
        let s = |k: &str| t.get(k).and_then(Value::as_str).map(str::to_string);
        let n = |k: &str| t.get(k).and_then(Value::as_integer);
        let voice_on = t.contains_key("voice_model_dir");
        match self {
            Setting::DeviceName => s("device_name").unwrap_or_default(),
            Setting::Volume => format!("{}%", n("initial_volume").unwrap_or(50)),
            Setting::DimAfter => minutes(n("dim_after_secs").unwrap_or(180)),
            Setting::OffAfter => minutes(n("off_after_secs").unwrap_or(600)),
            Setting::Audiobookshelf => s("audiobookshelf_url").unwrap_or_else(|| "off".into()),
            Setting::HomeAssistant => match t.get("home_assistant").and_then(Value::as_table) {
                None => "off".into(),
                Some(ha) => {
                    let pages = ha.get("page").and_then(Value::as_array).map_or(0, Vec::len);
                    let url = ha.get("url").and_then(Value::as_str).unwrap_or_default();
                    format!("{url} ({pages} page{})", if pages == 1 { "" } else { "s" })
                }
            },
            Setting::Voice if !voice_on => "off".into(),
            Setting::Voice if t.contains_key("vertex_key_file") => "on (Vertex AI)".into(),
            Setting::Voice => "on (Gemini API key)".into(),
            Setting::MyMusic if !spotify_on(t) => "on (no Spotify)".into(),
            Setting::MyMusic if my_music_on(t) => "on".into(),
            Setting::MyMusic => "off".into(),
            Setting::MusicShare if !my_music_on(t) => "(My music is off)".into(),
            Setting::MusicShare if music_share_on(t) => "on".into(),
            Setting::MusicShare => "off".into(),
            _ if !voice_on => "(voice is off)".into(),
            Setting::WakePhrase => s("wake_phrase").unwrap_or_else(|| "ziggy".into()),
            Setting::WakeSensitivity => match t.get("wake_threshold").and_then(Value::as_float) {
                Some(v) => format!("{v}"),
                None => "0.25 (default)".into(),
            },
            Setting::Microphone => s("microphone").unwrap_or_else(|| "the default input".into()),
            Setting::GeminiModel => {
                s("gemini_model").unwrap_or_else(|| "gemini-3.5-flash-lite (default)".into())
            }
        }
    }

    pub fn key(self) -> Option<&'static str> {
        Some(match self {
            Setting::DeviceName => "device_name",
            Setting::Volume => "initial_volume",
            Setting::DimAfter => "dim_after_secs",
            Setting::OffAfter => "off_after_secs",
            Setting::Audiobookshelf => "audiobookshelf_url",
            Setting::WakePhrase => "wake_phrase",
            Setting::WakeSensitivity => "wake_threshold",
            Setting::Microphone => "microphone",
            Setting::GeminiModel => "gemini_model",
            Setting::Voice | Setting::HomeAssistant | Setting::MyMusic | Setting::MusicShare => {
                return None;
            }
        })
    }
}

fn minutes(secs: i64) -> String {
    if secs % 60 == 0 {
        format!("{} min", secs / 60)
    } else {
        format!("{secs} s")
    }
}

/// Sets a text setting; empty removes it, back to the default.
pub fn set_text(t: &mut Table, key: &str, value: &str) {
    let value = value.trim();
    if value.is_empty() {
        t.remove(key);
    } else {
        t.insert(key.into(), value.into());
    }
}

/// Sets the Home Assistant address, keeping any pages already written.
pub fn set_home_assistant(t: &mut Table, url: &str) {
    let ha = t
        .entry("home_assistant")
        .or_insert_with(|| Value::Table(Table::new()));
    if let Some(ha) = ha.as_table_mut() {
        ha.insert("url".into(), url.trim().trim_end_matches('/').into());
    }
}

pub fn home_assistant_off(t: &mut Table) {
    t.remove("home_assistant");
}

/// Its pages are written by hand, so a new setup keeps the old config's Home Assistant section.
pub fn keep_home_assistant(t: &mut Table, old: Option<&Table>) {
    if let Some(ha) = old.and_then(|o| o.get("home_assistant")) {
        t.insert("home_assistant".into(), ha.clone());
    }
}

/// Checks the rules the player checks when it starts, so a bad edit isn't pushed.
pub fn check(t: &Table) -> anyhow::Result<()> {
    let n = |k: &str, d: i64| t.get(k).and_then(Value::as_integer).unwrap_or(d);
    let name = t
        .get("device_name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    anyhow::ensure!(!name.trim().is_empty(), "the device name can't be blank");
    anyhow::ensure!(
        (0..=100).contains(&n("initial_volume", 50)),
        "volume must be 0-100"
    );
    anyhow::ensure!(
        n("off_after_secs", 600) > n("dim_after_secs", 180),
        "the screen must dim before it turns off"
    );
    if let Some(v) = t.get("wake_threshold").and_then(Value::as_float) {
        anyhow::ensure!(
            (0.0..=1.0).contains(&v),
            "wake word sensitivity must be 0-1"
        );
    }
    // Pages are written by hand on the Pi; the player checks those itself.
    if let Some(ha) = t.get("home_assistant") {
        let url = ha.get("url").and_then(Value::as_str).unwrap_or_default();
        anyhow::ensure!(
            url.strip_prefix("http://")
                .is_some_and(|host| !host.trim_matches('/').is_empty()),
            "Home Assistant needs its local http:// address, like http://bergmini.local:8123"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choices() -> Choices {
        Choices {
            device_name: "Kitchen".into(),
            audiobookshelf_url: Some("http://nas.local:13378".into()),
            voice: Some(Voice {
                wake_phrase: "ziggy".into(),
                gemini: Gemini::Vertex,
            }),
            spotify: true,
            my_music: false,
            music_share: false,
        }
    }

    /// The config setup writes must load in the player itself.
    #[test]
    fn a_new_config_is_one_the_player_accepts() {
        let text = render(&new_config(&choices()));
        let config = ziggy_player_config(&text);
        assert!(config.contains("device_name = \"Kitchen\""));
        assert!(config.contains("vertex_key_file = \"/var/lib/ziggy/vertex-key.json\""));
        assert!(config.contains(&format!("voice_model_dir = \"{}\"", wake_model_dir())));
        check(&parse(&text).unwrap()).unwrap();
        let player = ziggy_player::config::Config::parse(&text).unwrap();
        assert!(player.spotify);
        assert!(!player.music_share);
        assert!(!player.my_music, "My music is off on a Spotify player");
        assert!(!config.contains("spotify ="), "Spotify is on by default");
        assert!(!my_music_on(&parse(&text).unwrap()));
        assert_eq!(Setting::MyMusic.show(&new_config(&choices())), "off");
        assert_eq!(
            Setting::MusicShare.show(&new_config(&choices())),
            "(My music is off)"
        );
        let with_music = new_config(&Choices {
            my_music: true,
            ..choices()
        });
        assert!(my_music_on(&with_music));
        assert!(
            ziggy_player::config::Config::parse(&render(&with_music))
                .unwrap()
                .my_music
        );
    }

    /// A card-only player, with the shared folder on.
    #[test]
    fn a_card_only_config_is_one_the_player_accepts() {
        let mut t = new_config(&Choices {
            spotify: false,
            music_share: true,
            ..choices()
        });
        let text = render(&t);
        assert!(text.contains("spotify = false"), "{text}");
        assert!(text.contains("music_share = true"), "{text}");
        assert!(
            text.contains("my_music = true"),
            "always on without Spotify"
        );
        let player = ziggy_player::config::Config::parse(&text).unwrap();
        assert!(!player.spotify);
        assert!(player.music_share);
        assert_eq!(
            player.music_dir(),
            std::path::PathBuf::from(MUSIC_DIR),
            "setup and the player agree on the music folder"
        );
        assert!(!spotify_on(&t));
        assert!(my_music_on(&t));
        assert_eq!(Setting::MyMusic.show(&t), "on (no Spotify)");
        assert_eq!(Setting::MusicShare.show(&t), "on");
        t.insert("music_share".into(), Value::Boolean(false));
        assert_eq!(Setting::MusicShare.show(&t), "off");
        // Setup again with Spotify: the key goes, so Spotify is on.
        assert!(spotify_on(&new_config(&choices())));
    }

    /// Loads it with the player's own config parser, which rejects unknown keys.
    fn ziggy_player_config(text: &str) -> String {
        ziggy_player::config::Config::parse(text).expect("the player accepts the config");
        text.to_string()
    }

    #[test]
    fn voice_can_switch_backends_and_turn_off() {
        let mut t = new_config(&choices());
        set_voice(
            &mut t,
            &Voice {
                wake_phrase: "hey ziggy".into(),
                gemini: Gemini::ApiKey("abc".into()),
            },
        );
        assert_eq!(Setting::Voice.show(&t), "on (Gemini API key)");
        assert!(!t.contains_key("vertex_key_file"));
        voice_off(&mut t);
        assert_eq!(Setting::Voice.show(&t), "off");
        assert_eq!(Setting::WakePhrase.show(&t), "(voice is off)");
        assert!(!t.contains_key("gemini_api_key"));
    }

    #[test]
    fn the_menu_shows_values_and_bad_edits_are_caught() {
        let mut t = new_config(&choices());
        assert_eq!(Setting::DimAfter.show(&t), "3 min");
        assert_eq!(Setting::Audiobookshelf.show(&t), "http://nas.local:13378");
        set_text(&mut t, "audiobookshelf_url", " ");
        assert_eq!(Setting::Audiobookshelf.show(&t), "off");
        t.insert("dim_after_secs".into(), Value::Integer(900));
        assert!(check(&t).is_err());
        t.insert("dim_after_secs".into(), Value::Integer(60));
        t.insert("device_name".into(), "  ".into());
        assert!(check(&t).is_err());
    }

    #[test]
    fn home_assistant_url_is_set_and_shown_without_touching_pages() {
        let mut t: Table = toml::from_str(
            "device_name = \"Leo\"\n[home_assistant]\nurl = \"http://old:8123\"\n[[home_assistant.page]]\nname = \"Room\"\ncontrols = [\"light.desk\"]\n",
        )
        .unwrap();
        assert_eq!(Setting::HomeAssistant.show(&t), "http://old:8123 (1 page)");
        set_home_assistant(&mut t, "http://192.168.1.69:8123/ ");
        assert_eq!(
            t["home_assistant"]["url"].as_str(),
            Some("http://192.168.1.69:8123")
        );
        assert_eq!(
            t["home_assistant"]["page"].as_array().map(Vec::len),
            Some(1)
        );
        home_assistant_off(&mut t);
        assert!(!t.contains_key("home_assistant"));
        assert_eq!(Setting::HomeAssistant.show(&t), "off");
    }

    #[test]
    fn home_assistant_must_be_http_with_a_host() {
        let mut t: Table = toml::from_str("device_name = \"Leo\"\n").unwrap();
        set_home_assistant(&mut t, "https://ha.example.com");
        assert!(check(&t).is_err());
        set_home_assistant(&mut t, "http://");
        assert!(check(&t).is_err());
        set_home_assistant(&mut t, "http://ha.local:8123");
        check(&t).unwrap();
    }

    #[test]
    fn setup_again_keeps_home_assistant() {
        let mut old = new_config(&choices());
        set_home_assistant(&mut old, "http://ha.local:8123");
        let mut t = new_config(&choices());
        keep_home_assistant(&mut t, Some(&old));
        assert_eq!(t["home_assistant"], old["home_assistant"]);
        let mut fresh = new_config(&choices());
        keep_home_assistant(&mut fresh, None);
        assert!(!fresh.contains_key("home_assistant"));
    }

    /// Pages written by hand, named controls included, survive `ziggy config` and still load.
    #[test]
    fn a_config_with_home_assistant_is_one_the_player_accepts() {
        let mut t = parse(&format!(
            "{}[[home_assistant.page]]\nname = \"Hall\"\n\
             controls = [\"light.ceiling\", {{ entity = \"light.desk\", name = \"Desk lamp\" }}]\n",
            render(&new_config(&choices()))
        ))
        .unwrap();
        set_home_assistant(&mut t, "http://192.168.1.69:8123");
        check(&t).unwrap();
        let config = ziggy_player::config::Config::parse(&render(&t))
            .expect("the player accepts the config");
        let ha = config.home_assistant.unwrap();
        assert_eq!(ha.url, "http://192.168.1.69:8123");
        assert_eq!(ha.pages[0].controls[1].name(), Some("Desk lamp"));
    }
}
