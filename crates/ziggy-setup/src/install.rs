//! Putting the player on a Pi: system setup, the player itself, models, config and sign-ins.

use std::path::{Path, PathBuf};

use anyhow::Context;

use crate::devconfig::{self, CONFIG_PATH, VERTEX_KEY_PATH, WAKE_MODEL};
use crate::remote::Remote;

pub const REPO: &str = "mberg/ziggy";
pub const PLAYER_ASSET: &str = "ziggy-player-linux-arm64.tar.gz";
const PROVISION: &str = include_str!("../../../deploy/provision-remote.sh");
const SERVICE: &str = include_str!("../../../deploy/ziggy-player.service");
const WAKE_MODEL_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/kws-models";

/// Where the player binary comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum PlayerSource {
    /// A GitHub release: the latest, or a tag such as "v0.2.0".
    Release(Option<String>),
    /// A binary built on this computer (scripts/build-pi.sh).
    Local(PathBuf),
}

pub fn release_url(tag: Option<&str>) -> String {
    match tag {
        Some(tag) => format!("https://github.com/{REPO}/releases/download/{tag}/{PLAYER_ASSET}"),
        None => format!("https://github.com/{REPO}/releases/latest/download/{PLAYER_ASSET}"),
    }
}

/// Installs packages, the ziggy-player user, the service, and display settings. Safe to run again.
/// Returns true when the Pi must reboot for display settings to apply.
pub fn provision(r: &dyn Remote) -> anyhow::Result<bool> {
    r.upload(
        PROVISION.as_bytes(),
        "/tmp/ziggy-provision/provision-remote.sh",
        "755",
        None,
    )?;
    r.upload(
        SERVICE.as_bytes(),
        "/tmp/ziggy-provision/ziggy-player.service",
        "644",
        None,
    )?;
    let out = r
        .run("sudo bash /tmp/ziggy-provision/provision-remote.sh; rc=$?; sudo rm -rf /tmp/ziggy-provision; exit $rc")
        .context("preparing the Pi")?;
    Ok(out.contains("ZIGGY_REBOOT_NEEDED"))
}

pub fn install_player(r: &dyn Remote, source: &PlayerSource) -> anyhow::Result<()> {
    match source {
        PlayerSource::Release(tag) => {
            let url = release_url(tag.as_deref());
            r.run(&format!(
                "tmp=$(mktemp -d)\n\
                 curl -fsSL '{url}' -o \"$tmp/player.tar.gz\"\n\
                 tar xzf \"$tmp/player.tar.gz\" -C \"$tmp\"\n\
                 sudo install -m 755 \"$tmp/ziggy-player\" /usr/local/bin/ziggy-player.new\n\
                 sudo sync /usr/local/bin/ziggy-player.new\n\
                 sudo mv -f /usr/local/bin/ziggy-player.new /usr/local/bin/ziggy-player\n\
                 sudo sync\n\
                 rm -rf \"$tmp\""
            ))
            .with_context(|| format!("downloading the player from {url}"))?;
        }
        PlayerSource::Local(path) => {
            let bytes =
                std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
            r.upload(&bytes, "/usr/local/bin/ziggy-player", "755", None)?;
        }
    }
    Ok(())
}

/// Downloads the wake-word model onto the Pi unless it's already there.
pub fn install_wake_model(r: &dyn Remote) -> anyhow::Result<()> {
    let dir = devconfig::wake_model_dir();
    r.run(&format!(
        "if [ ! -d '{dir}' ]; then\n\
         curl -fsSL '{WAKE_MODEL_URL}/{WAKE_MODEL}.tar.bz2' | sudo tar xj -C '{state}/models'\n\
         sudo chown -R ziggy-player:ziggy-player '{state}/models'\n\
         fi",
        state = devconfig::STATE_DIR
    ))
    .context("downloading the wake-word model")?;
    Ok(())
}

/// The sign-ins and keys the player needs, from this computer's copy.
pub struct Secrets<'a> {
    /// Holds the library sign-in, web-auth.json.
    pub spotify_dir: &'a Path,
    /// The Spotify Soloist API key file.
    pub soloist_key: Option<&'a Path>,
    pub vertex_key: Option<&'a Path>,
}

/// Where the player reads the Soloist API key (its default in the player's config).
pub const SOLOIST_KEY_PATH: &str = "/var/lib/ziggy/soloist-api-key";
/// The player pairs afresh on its next start when this exists.
const PAIR_REQUEST: &str = "/var/lib/ziggy/soloist-pair";

pub fn install_secrets(r: &dyn Remote, secrets: &Secrets) -> anyhow::Result<()> {
    let state = devconfig::STATE_DIR;
    let web_auth = secrets.spotify_dir.join(crate::spotify::WEB_AUTH_FILE);
    if web_auth.is_file() {
        r.upload(
            &std::fs::read(&web_auth)?,
            &format!("{state}/{}", crate::spotify::WEB_AUTH_FILE),
            "600",
            Some("ziggy-player"),
        )?;
    }
    if let Some(key) = secrets.soloist_key {
        let bytes = std::fs::read(key).with_context(|| format!("reading {}", key.display()))?;
        r.upload(&bytes, SOLOIST_KEY_PATH, "600", Some("ziggy-player"))?;
    }
    if let Some(key) = secrets.vertex_key {
        let bytes = std::fs::read(key).with_context(|| format!("reading {}", key.display()))?;
        r.upload(&bytes, VERTEX_KEY_PATH, "600", Some("ziggy-player"))?;
    }
    Ok(())
}

pub fn write_config(r: &dyn Remote, config: &toml::Table) -> anyhow::Result<()> {
    devconfig::check(config)?;
    r.upload(
        devconfig::render(config).as_bytes(),
        CONFIG_PATH,
        "644",
        None,
    )
}

pub fn read_config(r: &dyn Remote) -> anyhow::Result<Option<toml::Table>> {
    r.read(CONFIG_PATH)?
        .map(|text| devconfig::parse(&text))
        .transpose()
}

/// Restarts the player; returns whether it's running and its last log lines.
pub fn restart(r: &dyn Remote) -> anyhow::Result<(bool, String)> {
    let out = r.run(
        "sudo systemctl restart ziggy-player\n\
         sleep 4\n\
         echo \"STATE=$(systemctl is-active ziggy-player)\"\n\
         sudo journalctl -u ziggy-player -n 12 --no-pager -o cat || true",
    )?;
    Ok((
        out.contains("STATE=active"),
        out.replace("STATE=active\n", ""),
    ))
}

/// Installs the player's systemd unit as this version of `ziggy` has it, so an update also
/// brings changes to how the service runs.
pub fn install_service(r: &dyn Remote) -> anyhow::Result<()> {
    r.upload(
        SERVICE.as_bytes(),
        "/etc/systemd/system/ziggy-player.service",
        "644",
        None,
    )?;
    r.run("sudo systemctl daemon-reload")?;
    Ok(())
}

/// Keeps the Pi's logs across reboots (capped), so a crash can be read afterwards. Safe to run
/// again; the same settings are in the provisioning script.
pub fn keep_logs(r: &dyn Remote) -> anyhow::Result<()> {
    r.run(
        "sudo mkdir -p /etc/systemd/journald.conf.d /var/log/journal\n\
         printf '[Journal]\\nStorage=persistent\\nSystemMaxUse=50M\\n' \
           | sudo tee /etc/systemd/journald.conf.d/ziggy.conf >/dev/null\n\
         sudo systemctl restart systemd-journald || true\n\
         sudo journalctl --flush || true",
    )?;
    Ok(())
}

/// Lets the player's service user shut the Pi down (the Turn off button), and nothing else.
pub const POWER_SUDOERS: &str = "# Ziggy: the Turn off button in Settings.\nziggy-player ALL=(root) NOPASSWD: /usr/bin/systemctl poweroff\n";

/// Installs the sudo rule for Turn off, checked with visudo first. Safe to run again.
pub fn allow_power_off(r: &dyn Remote) -> anyhow::Result<()> {
    r.upload(POWER_SUDOERS.as_bytes(), "/tmp/ziggy-power", "440", None)?;
    r.run(
        "sudo visudo -cf /tmp/ziggy-power >/dev/null \
         && sudo install -m 440 /tmp/ziggy-power /etc/sudoers.d/020_ziggy-player-power; \
         rc=$?; sudo rm -f /tmp/ziggy-power; exit $rc",
    )
    .context("allowing the Turn off button")?;
    Ok(())
}

/// How the player is doing, for `ziggy status`: uptime, restarts, power and memory, and the
/// failures in the log. One `key=value` per line, then the failures after `--- problems`.
pub fn health(r: &dyn Remote) -> anyhow::Result<Health> {
    let out = r.run(
        "echo \"uptime=$(uptime -p)\"\n\
         echo \"started=$(systemctl show ziggy-player -p ActiveEnterTimestamp --value)\"\n\
         echo \"restarts=$(systemctl show ziggy-player -p NRestarts --value)\"\n\
         echo \"power=$(vcgencmd get_throttled 2>/dev/null | cut -d= -f2)\"\n\
         echo \"temperature=$(vcgencmd measure_temp 2>/dev/null | cut -d= -f2)\"\n\
         echo \"memory=$(free -m | awk '/Mem:/ {print $7\" MB free of \"$2\" MB\"}')\"\n\
         if [ -d /var/log/journal ]; then echo logs=kept; else echo logs=memory; fi\n\
         echo '--- problems'\n\
         sudo journalctl -u ziggy-player --no-pager -o short-iso 2>/dev/null \\\n\
           | grep -E 'Main process exited|Failed with result|panicked|Error:|FemtoVG:|ERROR ziggy_player|WARN ziggy_player' \\\n\
           | tail -12 | cut -c1-220 || true",
    )?;
    Ok(Health::parse(&out))
}

#[derive(Debug, Default, PartialEq)]
pub struct Health {
    pub facts: Vec<(String, String)>,
    pub problems: Vec<String>,
}

impl Health {
    fn parse(out: &str) -> Self {
        let mut health = Health::default();
        let mut problems = false;
        for line in out.lines().map(str::trim_end).filter(|l| !l.is_empty()) {
            if line == "--- problems" {
                problems = true;
            } else if problems {
                health.problems.push(line.to_string());
            } else if let Some((k, v)) = line.split_once('=') {
                health.facts.push((k.to_string(), v.to_string()));
            }
        }
        health
    }

    pub fn fact(&self, key: &str) -> Option<&str> {
        self.facts
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// `vcgencmd get_throttled` is a hex mask: bits 0-3 are happening now, bits 16-19 have
    /// happened since boot. Under-voltage (0, 16) is a power problem; the rest is heat.
    fn throttle_bits(&self) -> u32 {
        self.fact("power")
            .and_then(|p| u32::from_str_radix(p.trim_start_matches("0x"), 16).ok())
            .unwrap_or(0)
    }

    /// What the throttle flags say, in words; empty when all is well.
    pub fn power_and_heat(&self) -> Vec<&'static str> {
        let bits = self.throttle_bits();
        let mut out = Vec::new();
        if bits & 0x10001 != 0 {
            out.push(
                "The Pi has had too little power (under-voltage). Check the cable and charger.",
            );
        }
        if bits & 0xE000E != 0 {
            out.push("The Pi touched its 60°C heat limit and eased off for a moment. That's normal under load; look at airflow only if it keeps happening.");
        }
        out
    }

    pub fn restarts(&self) -> u32 {
        self.fact("restarts")
            .and_then(|n| n.parse().ok())
            .unwrap_or(0)
    }
}

/// The name Spotify shows for the player: one set on the touchscreen wins over the config.
pub fn device_name(r: &dyn Remote) -> Option<String> {
    let on_screen = r
        .run("sudo cat /var/lib/ziggy/settings.json 2>/dev/null || true")
        .ok()
        .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
        .and_then(|v| v["device_name"].as_str().map(str::to_string))
        .filter(|n| !n.trim().is_empty());
    on_screen.or_else(|| {
        read_config(r).ok().flatten().and_then(|t| {
            t.get("device_name")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
    })
}

/// Asks the player to forget its Spotify sign-in and wait to be picked in a Spotify app the
/// next time it starts.
pub fn request_pairing(r: &dyn Remote) -> anyhow::Result<()> {
    r.upload(b"", PAIR_REQUEST, "600", Some("ziggy-player"))
}

/// True once the player's Soloist is signed in to Spotify.
pub fn is_paired(r: &dyn Remote) -> bool {
    r.run(
        "sudo -u ziggy-player /var/lib/ziggy/bin/soloist ctl -w 127.0.0.1:47321 status \
           2>/dev/null | grep -q 'logged in: yes' && echo PAIRED || true",
    )
    .is_ok_and(|out| out.contains("PAIRED"))
}

/// True for a player from before Soloist: BlueALSA instead of PipeWire, or no API key yet.
pub fn needs_soloist_migration(r: &dyn Remote) -> bool {
    r.run(&format!(
        "if ! command -v pw-dump >/dev/null || systemctl is-enabled bluealsa >/dev/null 2>&1 \
           || ! sudo test -s {SOLOIST_KEY_PATH}; then echo MIGRATE; fi"
    ))
    .is_ok_and(|out| out.contains("MIGRATE"))
}

/// True for a player set up before the rename to Ziggy, which still has the muzak names.
pub fn needs_rename_migration(r: &dyn Remote) -> bool {
    r.run("test -d /var/lib/muzak && test ! -d /var/lib/ziggy && echo yes || true")
        .is_ok_and(|out| out.trim() == "yes")
}

/// The installed player's version, e.g. "ziggy-player 0.2.0". A player from before the rename
/// answers as "muzak-player".
pub fn installed_version(r: &dyn Remote) -> Option<String> {
    r.run(
        "/usr/local/bin/ziggy-player --version 2>/dev/null \
         || /usr/local/bin/muzak-player --version 2>/dev/null || true",
    )
    .ok()
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::fake::FakeRemote;

    #[test]
    fn provisioning_uploads_the_scripts_and_reports_a_needed_reboot() {
        let r = FakeRemote {
            output: "Provisioned.\nZIGGY_REBOOT_NEEDED\n".into(),
            ..Default::default()
        };
        assert!(provision(&r).unwrap());
        assert!(
            r.file("/tmp/ziggy-provision/ziggy-player.service")
                .unwrap()
                .contains("ExecStart")
        );
        assert!(r.all_scripts().contains("provision-remote.sh"));
        let again = FakeRemote::default();
        assert!(!provision(&again).unwrap(), "nothing changed, no reboot");
    }

    #[test]
    fn the_player_comes_from_a_release_or_this_computer() {
        let r = FakeRemote::default();
        install_player(&r, &PlayerSource::Release(None)).unwrap();
        assert!(r.all_scripts().contains(
            "https://github.com/mberg/ziggy/releases/latest/download/ziggy-player-linux-arm64.tar.gz"
        ));
        install_player(&r, &PlayerSource::Release(Some("v0.2.0".into()))).unwrap();
        assert!(r.all_scripts().contains("/releases/download/v0.2.0/"));

        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("ziggy-player");
        std::fs::write(&bin, b"\x7fELF").unwrap();
        install_player(&r, &PlayerSource::Local(bin)).unwrap();
        let files = r.files.borrow();
        let (bytes, mode, _) = &files["/usr/local/bin/ziggy-player"];
        assert_eq!((bytes.as_slice(), mode.as_str()), (&b"\x7fELF"[..], "755"));
    }

    #[test]
    fn the_name_set_on_the_screen_wins() {
        let r = FakeRemote {
            output: r#"{"device_name": "Ellie's Ziggy", "output": "Jack"}"#.into(),
            ..Default::default()
        };
        assert_eq!(device_name(&r).as_deref(), Some("Ellie's Ziggy"));
    }

    #[test]
    fn turn_off_is_the_only_thing_the_player_may_sudo() {
        let r = FakeRemote::default();
        allow_power_off(&r).unwrap();
        let rule = r.file("/tmp/ziggy-power").unwrap();
        assert!(rule.contains("ziggy-player ALL=(root) NOPASSWD: /usr/bin/systemctl poweroff"));
        assert_eq!(rule.lines().filter(|l| !l.starts_with('#')).count(), 1);
        assert!(r.all_scripts().contains("visudo -cf /tmp/ziggy-power"));
    }

    #[test]
    fn health_reads_facts_and_problems() {
        let h = Health::parse(
            "uptime=up 18 hours\nrestarts=2\npower=0x0\nlogs=kept\n--- problems\n\
             2026-10-05T14:52:57 ziggy-micah systemd[1]: Main process exited, code=exited\n",
        );
        assert_eq!(h.restarts(), 2);
        assert!(h.power_and_heat().is_empty());
        assert_eq!(h.fact("logs"), Some("kept"));
        assert_eq!(h.problems.len(), 1);
        // Bit 0 is "under-voltage now"; bit 16 is "under-voltage has happened".
        assert_eq!(
            Health::parse("power=0x50005\n").power_and_heat().len(),
            2,
            "low power and throttling"
        );
        assert_eq!(Health::parse("power=0x10000\n").power_and_heat().len(), 1);
        // Bit 19 is "the soft temperature limit has been reached": heat, not power.
        let heat = Health::parse("power=0x80000\n").power_and_heat();
        assert_eq!(heat.len(), 1);
        assert!(heat[0].contains("heat limit"));
        assert!(Health::parse("power=\n").power_and_heat().is_empty());
    }

    #[test]
    fn secrets_go_to_the_ziggy_user_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("web-auth.json"), "{}").unwrap();
        let soloist = dir.path().join("soloist-api-key");
        std::fs::write(&soloist, "spak_x").unwrap();
        let key = dir.path().join("vertex.json");
        std::fs::write(&key, "{\"type\":\"service_account\"}").unwrap();
        let r = FakeRemote::default();
        install_secrets(
            &r,
            &Secrets {
                spotify_dir: dir.path(),
                soloist_key: Some(&soloist),
                vertex_key: Some(&key),
            },
        )
        .unwrap();
        for path in [
            "/var/lib/ziggy/soloist-api-key",
            "/var/lib/ziggy/web-auth.json",
            "/var/lib/ziggy/vertex-key.json",
        ] {
            let files = r.files.borrow();
            let (_, mode, owner) = &files[path];
            assert_eq!(
                (mode.as_str(), owner.as_deref()),
                ("600", Some("ziggy-player")),
                "{path}"
            );
        }
    }

    #[test]
    fn a_bad_config_is_never_written() {
        let r = FakeRemote::default();
        let mut t = devconfig::new_config(&devconfig::Choices {
            device_name: "Den".into(),
            audiobookshelf_url: None,
            voice: None,
        });
        write_config(&r, &t).unwrap();
        assert_eq!(
            read_config(&r).unwrap().unwrap()["device_name"].as_str(),
            Some("Den")
        );
        t.insert("initial_volume".into(), toml::Value::Integer(150));
        assert!(write_config(&r, &t).is_err());
    }
}
