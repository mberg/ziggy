//! The guided commands: `ziggy setup` walks through everything, one question at a time;
//! `ziggy config`, `update`, `status`, `logs` and `signin` look after a player afterwards.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use console::style;
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Input, Password, Select};

use crate::devconfig::{self, Gemini, Setting, Voice};
use crate::install::{self, PlayerSource, Secrets};
use crate::remote::{Remote, Ssh};
use crate::{spotify, store};

fn theme() -> ColorfulTheme {
    ColorfulTheme::default()
}

fn step(n: usize, total: usize, title: &str) {
    println!(
        "\n{} {}",
        style(format!("Step {n} of {total}")).dim(),
        style(title).bold().cyan()
    );
}

fn say(text: &str) {
    println!("{text}");
}

fn ok(text: &str) {
    println!("{} {text}", style("✓").green());
}

fn ask(prompt: &str, default: &str) -> anyhow::Result<String> {
    Ok(Input::<String>::with_theme(&theme())
        .with_prompt(prompt)
        .default(default.to_string())
        .interact_text()?)
}

fn yes(prompt: &str, default: bool) -> anyhow::Result<bool> {
    Ok(Confirm::with_theme(&theme())
        .with_prompt(prompt)
        .default(default)
        .interact()?)
}

fn choose(prompt: &str, items: &[&str]) -> anyhow::Result<usize> {
    Ok(Select::with_theme(&theme())
        .with_prompt(prompt)
        .items(items)
        .default(0)
        .interact()?)
}

/// "ziggy-den.local" → "Den".
fn name_from_host(host: &str) -> String {
    let base = host.rsplit('@').next().unwrap_or(host);
    let base = base.split('.').next().unwrap_or(base);
    let base = base.strip_prefix("ziggy-").unwrap_or(base);
    let mut chars = base.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => "Ziggy".into(),
    }
}

/// Asks for the Pi's address until SSH works.
fn find_pi(host: Option<String>) -> anyhow::Result<Ssh> {
    let mut host = match host {
        Some(h) => h,
        None => ask(
            "The Pi's address (the hostname you set in Raspberry Pi Imager)",
            "ziggy.local",
        )?,
    };
    loop {
        say(&format!("Connecting to {host}…"));
        let ssh = Ssh::new(&host);
        if ssh.reachable() {
            ok(&format!("Connected to {host}"));
            if ssh.run("sudo -n true").is_ok() {
                return Ok(ssh);
            }
            // Newer Raspberry Pi OS images ask the Imager user for a password with sudo.
            let user = ssh
                .run("whoami")
                .map(|u| u.trim().to_string())
                .unwrap_or_else(|_| "pi".into());
            say(&format!(
                "{} On this Pi, sudo asks for a password, and setup needs to install without\n\
                 one. Run this once in another terminal and enter the Pi's password:\n\n  \
                 ssh -t {host} 'echo \"{user} ALL=(ALL) NOPASSWD:ALL\" | sudo tee /etc/sudoers.d/010_{user}-nopasswd >/dev/null && sudo chmod 440 /etc/sudoers.d/010_{user}-nopasswd'\n",
                style("!").yellow()
            ));
            if !yes("Done? Check again", true)? {
                bail!("stopped");
            }
            continue;
        }
        say(&format!(
            "{} Couldn't connect to {host} over SSH without a password.\n\
             Check that:\n\
             • the Pi is on and has been up for a minute or two;\n\
             • the address matches the hostname set in Raspberry Pi Imager (add .local);\n\
             • your SSH key was added in Imager. Try `ssh {host}` in another terminal.\n\
             If the user isn't the same as on this computer, use user@address, e.g. pi@{host}.",
            style("!").yellow()
        ));
        match choose(
            "What next?",
            &["Try again", "Use a different address", "Stop"],
        )? {
            0 => {}
            1 => host = ask("The Pi's address", &host)?,
            _ => bail!("stopped"),
        }
    }
}

/// The developer app's Client ID, asked for once and remembered.
fn spotify_client_id() -> anyhow::Result<String> {
    let mut saved = store::load();
    if let Some(id) = &saved.spotify_client_id {
        return Ok(id.clone());
    }
    say(&format!(
        "Ziggy reads your Spotify library through a Spotify developer app. One app serves every\n\
         player and every person, so this is only needed once:\n\
         1. Open https://developer.spotify.com/dashboard and create an app.\n\
         2. Redirect URI: {}\n\
         3. Tick \"Web API\".\n\
         4. Under \"User Management\", add the email of each Spotify account that will use Ziggy.",
        spotify::REDIRECT_URI
    ));
    let id: String = Input::with_theme(&theme())
        .with_prompt("The app's Client ID")
        .validate_with(|s: &String| {
            if s.trim().len() == 32 && s.trim().chars().all(|c| c.is_ascii_hexdigit()) {
                Ok(())
            } else {
                Err("a Client ID is 32 letters and numbers")
            }
        })
        .interact_text()?;
    saved.spotify_client_id = Some(id.trim().to_string());
    store::save(&saved)?;
    Ok(id.trim().to_string())
}

/// Signs a Spotify account in for a player's library (the developer app) and checks it.
/// Playback signs in on the Pi itself; see `pair`.
async fn library_sign_in(dir: &Path, client_id: &str) -> anyhow::Result<()> {
    if dir.join(spotify::WEB_AUTH_FILE).is_file()
        && yes(
            "Use the Spotify library sign-in saved for this player?",
            true,
        )?
    {
        return Ok(());
    }
    loop {
        say("A browser opens. Sign in with the Spotify account this player is for.");
        spotify::auth_web(dir, client_id).await?;
        say("Checking access…");
        match spotify::probe(dir).await {
            Ok(()) => {
                ok("The library works");
                return Ok(());
            }
            Err(e) if e.to_string().contains("429") => {
                say(&format!(
                    "{} Spotify is limiting requests from the developer app right now, so the\n\
                     check is skipped. The sign-in is saved; the library loads once the limit\n\
                     ends (usually within a day). Playing music isn't affected.",
                    style("!").yellow()
                ));
                return Ok(());
            }
            Err(e) => {
                say(&format!(
                    "{} Spotify refused some requests: {e}\n\
                     Usually the account's email isn't under \"User Management\" in the developer\n\
                     app.",
                    style("!").yellow()
                ));
                if !yes("Sign in again?", true)? {
                    bail!("Spotify isn't working for this player yet");
                }
            }
        }
    }
}

/// The Spotify Soloist API key, asked for once and kept privately on this computer.
fn soloist_key() -> anyhow::Result<PathBuf> {
    let path = store::soloist_key_file();
    if std::fs::read_to_string(&path).is_ok_and(|k| !k.trim().is_empty()) {
        return Ok(path);
    }
    say(
        "Ziggy plays music with Spotify Soloist, Spotify's own player for Raspberry Pi. It needs an\n\
         API key, made once with a Spotify Premium account:\n\
         1. Open https://developer.spotify.com/dashboard and choose \"Spotify Soloist API Key\".\n\
         2. Accept the terms and generate a key.\n\
         The key is yours: don't share or publish it.",
    );
    let key = Password::with_theme(&theme())
        .with_prompt("The Soloist API key")
        .validate_with(|s: &String| {
            if s.trim().len() >= 16 && !s.trim().contains(char::is_whitespace) {
                Ok(())
            } else {
                Err("that doesn't look like an API key")
            }
        })
        .interact()?;
    store::save_soloist_key(&key)
}

/// Playback signs in on the Pi: the player waits to be picked in a Spotify app. Waits up to
/// ten minutes for that, then leaves it to the player's screen.
fn pair(ssh: &Ssh, device_name: &str) -> anyhow::Result<()> {
    if install::is_paired(ssh) {
        ok("Signed in to Spotify for playback");
        return Ok(());
    }
    say(&format!(
        "Now sign it in for playback. On a phone or computer on this Wi-Fi, open the Spotify app,\n\
         signed in as the person this player is for. Tap the devices button, choose\n\
         \"{device_name}\" and play anything. (The player's screen says the same.)"
    ));
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(600) {
        std::thread::sleep(Duration::from_secs(5));
        if install::is_paired(ssh) {
            ok("Signed in. Music plays on the player");
            return Ok(());
        }
    }
    say(&format!(
        "{} Not picked yet. Do it any time: the player's screen shows how.",
        style("!").yellow()
    ));
    Ok(())
}

fn audiobookshelf(current: Option<&str>) -> anyhow::Result<Option<String>> {
    if !yes(
        "Do you have an Audiobookshelf server for audiobooks?",
        current.is_some(),
    )? {
        return Ok(None);
    }
    let url = ask("Its address", current.unwrap_or("http://nas.local:13378"))?;
    let url = url.trim().trim_end_matches('/').to_string();
    Ok(Some(if url.starts_with("http") {
        url
    } else {
        format!("http://{url}")
    }))
}

/// Asks how voice should work; returns the choice and a key file to copy over, if any.
fn voice_choice(current_phrase: Option<&str>) -> anyhow::Result<(Voice, Option<PathBuf>)> {
    say(
        "Voice control needs a USB microphone on the Pi and access to Google's Gemini. Requests\n\
         go to Gemini only after the wake word.",
    );
    let how = choose(
        "How should the player reach Gemini?",
        &[
            "Google Cloud service account key file (Vertex AI, uses Cloud credits)",
            "Gemini API key from Google AI Studio",
        ],
    )?;
    let (gemini, key_file) = if how == 0 {
        let mut saved = store::load();
        let default = saved
            .vertex_key_file
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let path: String = Input::with_theme(&theme())
            .with_prompt("Path to the key file (.json)")
            .with_initial_text(default)
            .validate_with(|s: &String| check_key_file(Path::new(s.trim())))
            .interact_text()?;
        let path = PathBuf::from(path.trim());
        saved.vertex_key_file = Some(path.clone());
        store::save(&saved)?;
        (Gemini::Vertex, Some(path))
    } else {
        let key = Password::with_theme(&theme())
            .with_prompt("Gemini API key")
            .interact()?;
        (Gemini::ApiKey(key.trim().to_string()), None)
    };
    let wake_phrase = ask("Wake word", current_phrase.unwrap_or("ziggy"))?;
    Ok((
        Voice {
            wake_phrase,
            gemini,
        },
        key_file,
    ))
}

fn check_key_file(path: &Path) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|_| "can't read that file".to_string())?;
    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|_| "that isn't a JSON key file".to_string())?;
    if json["type"] == "service_account" && json["private_key"].is_string() {
        Ok(())
    } else {
        Err("that isn't a service account key file".into())
    }
}

/// Reboots the Pi and waits for the player to come back.
fn reboot_and_wait(ssh: &Ssh) -> anyhow::Result<()> {
    say("Restarting the Pi so the display settings take effect…");
    let _ = ssh.run("sudo systemctl reboot");
    std::thread::sleep(Duration::from_secs(15));
    let started = Instant::now();
    while !ssh.reachable() {
        if started.elapsed() > Duration::from_secs(240) {
            bail!("the Pi didn't come back after restarting; check its power and Wi-Fi");
        }
        std::thread::sleep(Duration::from_secs(5));
    }
    std::thread::sleep(Duration::from_secs(8));
    Ok(())
}

fn report_running(ssh: &Ssh) -> anyhow::Result<bool> {
    let out = ssh.run(
        "echo \"STATE=$(systemctl is-active ziggy-player)\"; sudo journalctl -u ziggy-player -n 12 --no-pager -o cat || true",
    )?;
    let running = out.contains("STATE=active");
    if running {
        ok("The player is running");
    } else {
        say(&format!(
            "{} The player isn't running. Its last messages:\n{}",
            style("!").red(),
            out.replace("STATE=", "state: ")
        ));
    }
    Ok(running)
}

pub struct SetupOptions {
    pub host: Option<String>,
    pub player: PlayerSource,
}

pub async fn setup(options: SetupOptions) -> anyhow::Result<()> {
    const STEPS: usize = 6;
    println!("{}", style("Ziggy setup").bold());
    say(
        "This sets up a Raspberry Pi as a Ziggy player. Before starting, flash its SD card with\n\
         Raspberry Pi Imager: choose Raspberry Pi OS Lite (64-bit), and under the settings set a\n\
         hostname (e.g. ziggy-kitchen), a user, your Wi-Fi and your SSH key. Put the card in the\n\
         Pi, attach the display, and power it on.",
    );
    if !yes("Is the Pi ready and on your Wi-Fi?", true)? {
        say("Run `ziggy setup` again once it is.");
        return Ok(());
    }

    step(1, STEPS, "Find the Pi");
    let ssh = find_pi(options.host)?;
    let existing = install::read_config(&ssh).ok().flatten();

    step(2, STEPS, "Name it");
    let current_name = existing
        .as_ref()
        .and_then(|t| t.get("device_name"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let device_name = ask(
        "What should this player be called? (Spotify shows this name)",
        &current_name.unwrap_or_else(|| name_from_host(&ssh.host)),
    )?;

    step(3, STEPS, "Spotify");
    let client_id = spotify_client_id()?;
    let spotify_dir = store::device_dir(&ssh.host)?;
    library_sign_in(&spotify_dir, &client_id).await?;
    let soloist_key = soloist_key()?;

    step(4, STEPS, "Extras");
    let current = |key: &str| {
        existing
            .as_ref()
            .and_then(|t| t.get(key))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
    let audiobookshelf_url = audiobookshelf(current("audiobookshelf_url").as_deref())?;
    let (voice, vertex_key) = if yes(
        "Turn on voice control? (needs a USB microphone)",
        current("voice_model_dir").is_some(),
    )? {
        let (voice, key) = voice_choice(current("wake_phrase").as_deref())?;
        (Some(voice), key)
    } else {
        (None, None)
    };

    step(5, STEPS, "Install");
    say(
        "Preparing the Pi: packages, the ziggy user and the display. A few minutes the first time…",
    );
    let reboot = install::provision(&ssh)?;
    install::allow_power_off(&ssh)?;
    ok("Pi prepared");
    say("Installing the player…");
    install::install_player(&ssh, &options.player)?;
    ok(&install::installed_version(&ssh).unwrap_or_else(|| "Player installed".into()));
    if voice.is_some() {
        say("Downloading the wake-word model…");
        install::install_wake_model(&ssh)?;
        ok("Wake-word model ready");
    }
    let config = devconfig::new_config(&devconfig::Choices {
        device_name: device_name.clone(),
        audiobookshelf_url: audiobookshelf_url.clone(),
        voice: voice.clone(),
    });
    install::write_config(&ssh, &config)?;
    install::install_secrets(
        &ssh,
        &Secrets {
            spotify_dir: &spotify_dir,
            soloist_key: Some(&soloist_key),
            vertex_key: vertex_key.as_deref(),
        },
    )?;
    ok("Settings and sign-ins copied");

    step(6, STEPS, "Start");
    if reboot {
        reboot_and_wait(&ssh)?;
    } else {
        let _ = install::restart(&ssh)?;
    }
    let running = report_running(&ssh)?;
    if running {
        pair(
            &ssh,
            &install::device_name(&ssh).unwrap_or_else(|| device_name.clone()),
        )?;
    }

    println!(
        "\n{}",
        style(format!("{device_name} is set up.")).bold().green()
    );
    say(&format!(
        "On the touchscreen:\n\
         • Settings → Speaker pairs a Bluetooth speaker or picks a Sonos room.{}\n\n\
         From this computer:\n\
         • ziggy config {host}   change settings\n\
         • ziggy update {host}   install the newest player\n\
         • ziggy status {host}   check it's running\n\
         • ziggy logs {host}     see what it's doing",
        if audiobookshelf_url.is_some() {
            "\n• Settings → Audiobooks signs in to Audiobookshelf."
        } else {
            ""
        },
        host = ssh.host
    ));
    if !running {
        bail!("the player didn't start; see the messages above");
    }
    Ok(())
}

/// `ziggy config <host>`: change the device's settings from a menu, then push and restart.
pub fn config(host: &str) -> anyhow::Result<()> {
    let ssh = connect(host)?;
    let mut t = install::read_config(&ssh)?
        .context("this Pi has no Ziggy settings yet; run `ziggy setup` first")?;
    let mut key_to_copy: Option<PathBuf> = None;
    let mut model_needed = false;
    let mut changed = false;
    loop {
        let mut items: Vec<String> = Setting::ALL
            .iter()
            .map(|s| format!("{:<28} {}", s.label(), s.show(&t)))
            .collect();
        items.push(if changed {
            "Save and restart the player".into()
        } else {
            "Done".into()
        });
        items.push("Quit without saving".into());
        let pick = Select::with_theme(&theme())
            .with_prompt(format!("Settings on {host}"))
            .items(&items)
            .default(0)
            .interact()?;
        if pick == Setting::ALL.len() {
            break;
        }
        if pick > Setting::ALL.len() {
            say("Nothing saved.");
            return Ok(());
        }
        let setting = Setting::ALL[pick];
        let voice_on = t.contains_key("voice_model_dir");
        match setting {
            Setting::Voice => {
                if voice_on && yes("Turn voice control off?", false)? {
                    devconfig::voice_off(&mut t);
                } else if !voice_on || yes("Change how it reaches Gemini?", false)? {
                    let current = t
                        .get("wake_phrase")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    let (voice, key) = voice_choice(current.as_deref())?;
                    devconfig::set_voice(&mut t, &voice);
                    key_to_copy = key;
                    model_needed = true;
                }
            }
            _ if !voice_on
                && setting.key().is_some_and(|k| {
                    [
                        "wake_phrase",
                        "wake_threshold",
                        "microphone",
                        "gemini_model",
                    ]
                    .contains(&k)
                }) =>
            {
                say("Turn voice control on first.");
                continue;
            }
            Setting::Volume | Setting::DimAfter | Setting::OffAfter => {
                let key = setting.key().expect("a key");
                let (prompt, scale) = match setting {
                    Setting::Volume => ("Starting volume, 0-100", 1),
                    Setting::DimAfter => ("Dim the screen after how many minutes", 60),
                    _ => ("Turn the screen off after how many minutes", 60),
                };
                let current = t.get(key).and_then(|v| v.as_integer()).unwrap_or(0) / scale;
                let value: i64 = Input::with_theme(&theme())
                    .with_prompt(prompt)
                    .default(current)
                    .interact_text()?;
                t.insert(key.into(), toml::Value::Integer(value * scale));
            }
            Setting::WakeSensitivity => {
                let value: String = Input::with_theme(&theme())
                    .with_prompt(
                        "Sensitivity, 0-1; lower wakes more readily (empty for the default 0.25)",
                    )
                    .allow_empty(true)
                    .interact_text()?;
                match value.trim().parse::<f64>() {
                    Ok(v) => {
                        t.insert("wake_threshold".into(), toml::Value::Float(v));
                    }
                    Err(_) => {
                        t.remove("wake_threshold");
                    }
                }
            }
            _ => {
                let key = setting.key().expect("a key");
                let current = t
                    .get(key)
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let value: String = Input::with_theme(&theme())
                    .with_prompt(format!("{} (empty for the default)", setting.label()))
                    .with_initial_text(current)
                    .allow_empty(true)
                    .interact_text()?;
                devconfig::set_text(&mut t, key, &value);
            }
        }
        if let Err(e) = devconfig::check(&t) {
            say(&format!("{} {e}", style("!").yellow()));
        }
        changed = true;
    }
    if !changed {
        return Ok(());
    }
    devconfig::check(&t)?;
    if model_needed {
        say("Downloading the wake-word model…");
        install::install_wake_model(&ssh)?;
    }
    if let Some(key) = &key_to_copy {
        let dir = store::device_dir(&ssh.host)?;
        install::install_secrets(
            &ssh,
            &Secrets {
                spotify_dir: &dir.join("none"),
                soloist_key: None,
                vertex_key: Some(key),
            },
        )?;
    }
    install::write_config(&ssh, &t)?;
    say("Saved. Restarting the player…");
    let (running, logs) = install::restart(&ssh)?;
    if running {
        ok("The player is running with the new settings");
        say(
            "Choices made on the touchscreen (Settings) still take priority, e.g. its device name.",
        );
        Ok(())
    } else {
        bail!("the player didn't start after the change:\n{logs}")
    }
}

fn connect(host: &str) -> anyhow::Result<Ssh> {
    let ssh = Ssh::new(host);
    if !ssh.reachable() {
        bail!("can't reach {host} over SSH; is it on? Try `ssh {host}`");
    }
    Ok(ssh)
}

pub fn update(host: &str, player: PlayerSource) -> anyhow::Result<()> {
    let ssh = connect(host)?;
    let before = install::installed_version(&ssh);
    if install::needs_rename_migration(&ssh) {
        say("This player was set up as Muzak. Moving it to the Ziggy names…");
        install::provision(&ssh)?;
        ok("Moved. Settings, sign-in and speaker are kept");
    }
    let migrate = install::needs_soloist_migration(&ssh);
    if migrate {
        say("This player still uses the old Spotify player. Switching it to Spotify Soloist…");
        let key = soloist_key()?;
        install::provision(&ssh)?;
        install::install_secrets(
            &ssh,
            &Secrets {
                spotify_dir: &store::device_dir(&ssh.host)?.join("none"),
                soloist_key: Some(&key),
                vertex_key: None,
            },
        )?;
        ok("Switched to PipeWire, with the Soloist key copied");
    }
    install::keep_logs(&ssh)?;
    install::allow_power_off(&ssh)?;
    install::install_service(&ssh)?;
    say("Installing the player…");
    install::install_player(&ssh, &player)?;
    let after = install::installed_version(&ssh);
    let (running, logs) = install::restart(&ssh)?;
    match (before, after) {
        (Some(b), Some(a)) if a != b => ok(&format!("Updated from {b} to {a}")),
        (_, Some(a)) => ok(&format!("{a} installed")),
        _ => ok("Installed"),
    }
    if !running {
        bail!("the player didn't start:\n{logs}");
    }
    ok("The player is running");
    if migrate {
        let name = install::device_name(&ssh).unwrap_or_else(|| name_from_host(&ssh.host));
        // Soloist downloads and starts on the player's first run; give it a moment.
        std::thread::sleep(Duration::from_secs(20));
        pair(&ssh, &name)?;
    }
    Ok(())
}

pub fn status(host: &str) -> anyhow::Result<()> {
    let ssh = connect(host)?;
    ok(&format!("{host} is reachable"));
    say(&install::installed_version(&ssh).unwrap_or_else(|| "The player isn't installed".into()));
    report_running(&ssh)?;
    let health = install::health(&ssh)?;
    for (label, key) in [
        ("Up", "uptime"),
        ("Player started", "started"),
        ("Memory free", "memory"),
        ("Temperature", "temperature"),
    ] {
        if let Some(v) = health.fact(key).filter(|v| !v.is_empty()) {
            say(&format!("  {label}: {v}"));
        }
    }
    for note in health.power_and_heat() {
        say(&format!("{} {note}", style("!").red()));
    }
    if health.fact("logs") == Some("memory") {
        say("  Logs are only kept until the Pi restarts. `ziggy update` fixes that.");
    }
    if health.restarts() > 0 {
        say(&format!(
            "{} The player has restarted itself {} time(s) since it was last started by hand.",
            style("!").red(),
            health.restarts()
        ));
    }
    if health.problems.is_empty() {
        ok("No crashes or errors in the log");
    } else {
        say("Recent problems (oldest first):");
        for line in &health.problems {
            say(&format!("  {line}"));
        }
    }
    Ok(())
}

pub fn logs(host: &str) -> anyhow::Result<()> {
    let ssh = connect(host)?;
    ssh.stream("sudo journalctl -u ziggy-player -n 50 -f -o short")
}

/// Signs a different (or the same) Spotify account in on a player: the library through the
/// developer app, then playback by picking the player in a Spotify app.
pub async fn signin(host: &str) -> anyhow::Result<()> {
    let ssh = connect(host)?;
    let client_id = spotify_client_id()?;
    let dir = store::device_dir(&ssh.host)?;
    // A fresh sign-in, not the saved one.
    let _ = std::fs::remove_file(dir.join(spotify::WEB_AUTH_FILE));
    library_sign_in(&dir, &client_id).await?;
    install::install_secrets(
        &ssh,
        &Secrets {
            spotify_dir: &dir,
            soloist_key: None,
            vertex_key: None,
        },
    )?;
    // Playback is signed in separately, on the Pi; only a change of account needs it again.
    let repair = yes(
        "Sign playback in again too? (Only needed to change the Spotify account.)",
        false,
    )?;
    if repair {
        install::request_pairing(&ssh)?;
    }
    let (running, logs) = install::restart(&ssh)?;
    if !running {
        bail!("the player didn't start:\n{logs}");
    }
    let name = install::device_name(&ssh).unwrap_or_else(|| name_from_host(&ssh.host));
    pair(&ssh, &name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_player_is_named_after_its_hostname() {
        assert_eq!(name_from_host("ziggy-kitchen.local"), "Kitchen");
        assert_eq!(name_from_host("pi@den.local"), "Den");
        assert_eq!(name_from_host("192.168.1.20"), "192");
    }

    #[test]
    fn only_service_account_key_files_are_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.json");
        std::fs::write(
            &good,
            r#"{"type":"service_account","private_key":"-----BEGIN"}"#,
        )
        .unwrap();
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, r#"{"type":"authorized_user"}"#).unwrap();
        assert!(check_key_file(&good).is_ok());
        assert!(check_key_file(&bad).is_err());
        assert!(check_key_file(&dir.path().join("missing.json")).is_err());
    }
}
