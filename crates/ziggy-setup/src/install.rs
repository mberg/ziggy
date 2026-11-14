//! Putting the player on a Pi: system setup, the player itself, models, config and sign-ins.

use std::path::{Path, PathBuf};

use anyhow::Context;

use crate::devconfig::{self, CONFIG_PATH, MUSIC_DIR, VERTEX_KEY_PATH, WAKE_MODEL};
use crate::remote::Remote;

pub const REPO: &str = "mberg/ziggy";
pub const PLAYER_ASSET: &str = "ziggy-player-linux-arm64.tar.gz";
const PROVISION: &str = include_str!("../../../deploy/provision-remote.sh");
const SERVICE: &str = include_str!("../../../deploy/ziggy-player.service");
const WIFI_WATCH: &str = include_str!("../../../deploy/ziggy-wifi-watch");
const WIFI_WATCH_SERVICE: &str = include_str!("../../../deploy/ziggy-wifi-watch.service");
const SMB_CONF: &str = include_str!("../../../deploy/smb.conf");
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
    /// A Home Assistant long-lived access token, straight from the prompt; it's never saved on
    /// this computer.
    pub ha_token: Option<&'a str>,
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
    if let Some(token) = secrets.ha_token {
        r.upload(
            token.as_bytes(),
            devconfig::HA_TOKEN_PATH,
            "600",
            Some("ziggy-player"),
        )?;
    }
    Ok(())
}

/// True when the player already has a Home Assistant token. The token itself stays on the Pi.
pub fn has_ha_token(r: &dyn Remote) -> bool {
    r.run(&format!(
        "sudo test -s {} && echo yes || true",
        devconfig::HA_TOKEN_PATH
    ))
    .is_ok_and(|out| out.trim() == "yes")
}

/// Deletes the player's Home Assistant token, once Home Assistant is turned off.
pub fn remove_ha_token(r: &dyn Remote) -> anyhow::Result<()> {
    r.run(&format!("sudo rm -f {}", devconfig::HA_TOKEN_PATH))?;
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

/// AirPlay: installs shairport-sync for a player set up before AirPlay. The player runs it
/// itself when AirPlay is on in Settings, so the package's own service stays off. Safe to run
/// again; the provisioning script does the same.
pub fn install_airplay(r: &dyn Remote) -> anyhow::Result<()> {
    r.run(
        "if ! command -v shairport-sync >/dev/null; then\n\
           sudo apt-get update -qq\n\
           sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
             shairport-sync >/dev/null\n\
         fi\n\
         sudo systemctl disable --now shairport-sync.service 2>/dev/null || true",
    )?;
    Ok(())
}

/// Samba's settings: one share, "Music", for the music folder, and nothing else (no printers,
/// no home folders). The player turns smbd on and off; nmbd (old Windows network browsing)
/// stays off, and the share is announced over Bonjour so Finder lists it under Network.
/// Without a password anyone on the network may use it; with one, only the `ziggy` user.
pub fn smb_conf(password: bool) -> String {
    if password {
        SMB_CONF.replace(
            "   guest ok = yes\n",
            "   guest ok = no\n   valid users = ziggy\n",
        )
    } else {
        SMB_CONF.to_string()
    }
}

pub const SMB_CONF_PATH: &str = "/etc/samba/smb.conf";

/// Music on the card: installs Samba and rsync if missing, makes the `ziggy-music` group and
/// the music folder, and lets the SSH user write there (for `ziggy music add`). Writes Samba's
/// settings, then starts smbd only if the config shares the folder; the player turns it on and
/// off itself after that. Safe to run again; the provisioning script does the same first part.
pub fn install_music(r: &dyn Remote, share: bool) -> anyhow::Result<()> {
    r.run(&format!(
        "if ! dpkg -s samba rsync >/dev/null 2>&1; then\n\
           sudo apt-get update -qq\n\
           sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
             samba rsync >/dev/null\n\
         fi\n\
         getent group ziggy-music >/dev/null || sudo groupadd --system ziggy-music\n\
         sudo usermod -aG ziggy-music ziggy-player\n\
         sudo usermod -aG ziggy-music \"$(id -un)\"\n\
         sudo install -d -m 2775 -o ziggy-player -g ziggy-music {MUSIC_DIR}"
    ))
    .context("setting up the music folder")?;
    let password = has_share_password(r);
    r.upload(smb_conf(password).as_bytes(), SMB_CONF_PATH, "644", None)?;
    r.run(if share {
        "sudo systemctl disable --now nmbd 2>/dev/null || true\n\
         sudo systemctl enable --now smbd\n\
         sudo systemctl try-reload-or-restart smbd"
    } else {
        "sudo systemctl disable --now smbd nmbd 2>/dev/null || true"
    })
    .context("setting up the shared music folder")?;
    Ok(())
}

/// True when the shared folder has a password: the `ziggy` user is in Samba's own list.
pub fn has_share_password(r: &dyn Remote) -> bool {
    r.run("sudo pdbedit -L 2>/dev/null | grep -q '^ziggy:' && echo yes || true")
        .is_ok_and(|out| out.trim() == "yes")
}

/// Sets a password on the shared folder, or removes it (None) so anyone on the network may use
/// it. The password belongs to `ziggy`, a user that can't log in to the Pi; in Finder, connect
/// as "ziggy" with it. Files saved through the share still belong to the player.
pub fn set_share_password(r: &dyn Remote, password: Option<&str>) -> anyhow::Result<()> {
    match password {
        Some(password) => {
            // smbpasswd reads the new password twice from stdin.
            r.upload(
                format!("{password}\n{password}\n").as_bytes(),
                "/tmp/ziggy-share-password",
                "600",
                None,
            )?;
            r.run(
                "trap 'sudo rm -f /tmp/ziggy-share-password' EXIT\n\
                 id ziggy >/dev/null 2>&1 || sudo useradd --system --no-create-home \
                   --shell /usr/sbin/nologin ziggy\n\
                 sudo usermod -aG ziggy-music ziggy\n\
                 sudo sh -c 'smbpasswd -s -a ziggy < /tmp/ziggy-share-password >/dev/null'",
            )
            .context("setting the password")?;
        }
        None => {
            r.run("sudo smbpasswd -x ziggy >/dev/null 2>&1 || true")?;
        }
    }
    r.upload(
        smb_conf(password.is_some()).as_bytes(),
        SMB_CONF_PATH,
        "644",
        None,
    )?;
    r.run("sudo systemctl try-reload-or-restart smbd")?;
    Ok(())
}

/// What the player's service user may run through sudo, and nothing else: shutting the Pi down
/// (Turn off) and rejoining the Wi-Fi (Reconnect), both from Settings, and starting or stopping
/// the shared music folder (Samba's smbd), from Settings and when the player starts.
pub const PLAYER_SUDOERS: &str = "# Ziggy: the Turn off, Reconnect and Shared folder settings.\n\
    ziggy-player ALL=(root) NOPASSWD: /usr/bin/systemctl poweroff\n\
    ziggy-player ALL=(root) NOPASSWD: /usr/bin/nmcli --wait 45 device connect wlan0\n\
    ziggy-player ALL=(root) NOPASSWD: /usr/bin/systemctl enable --now smbd\n\
    ziggy-player ALL=(root) NOPASSWD: /usr/bin/systemctl disable --now smbd\n";

/// Installs the sudo rules for Turn off, Reconnect and the shared folder, checked with visudo
/// first. Safe to run again.
pub fn allow_player_commands(r: &dyn Remote) -> anyhow::Result<()> {
    r.upload(PLAYER_SUDOERS.as_bytes(), "/tmp/ziggy-power", "440", None)?;
    r.run(
        "sudo visudo -cf /tmp/ziggy-power >/dev/null \
         && sudo install -m 440 /tmp/ziggy-power /etc/sudoers.d/020_ziggy-player-power; \
         rc=$?; sudo rm -f /tmp/ziggy-power; exit $rc",
    )
    .context("allowing the Turn off, Reconnect and Shared folder settings")?;
    Ok(())
}

/// NetworkManager settings: Wi-Fi power saving off, since it makes a Pi's Wi-Fi drop out, and
/// warnings only in the log. On a weak signal its routine lines run to tens of thousands a day,
/// which pushes the player's own out of the capped log.
pub const WIFI_CONF: &str = "# Ziggy: Wi-Fi power saving off; it makes a Pi's Wi-Fi drop out.\n\
    [connection]\nwifi.powersave = 2\n\n\
    # Ziggy: warnings only, so the Wi-Fi doesn't push the player's lines out of the log.\n\
    [logging]\nlevel = WARN\n";

/// Keeps the Wi-Fi up: power saving off, quieter Wi-Fi logs, and a service that asks
/// NetworkManager to rejoin once the Wi-Fi has stayed down for 3 minutes. Safe to run again.
pub fn watch_wifi(r: &dyn Remote) -> anyhow::Result<()> {
    r.upload(
        WIFI_WATCH.as_bytes(),
        "/usr/local/lib/ziggy/wifi-watch",
        "755",
        None,
    )?;
    r.upload(
        WIFI_WATCH_SERVICE.as_bytes(),
        "/etc/systemd/system/ziggy-wifi-watch.service",
        "644",
        None,
    )?;
    r.upload(
        WIFI_CONF.as_bytes(),
        "/etc/NetworkManager/conf.d/80-ziggy-wifi.conf",
        "644",
        None,
    )?;
    // Neither step drops the connection: the log level changes now, and power saving is off
    // from the next time the Wi-Fi joins (`iw`, when installed, turns it off now). A restart
    // picks up a newer check.
    r.run(
        "sudo systemctl daemon-reload\n\
         sudo systemctl enable ziggy-wifi-watch.service\n\
         sudo systemctl restart ziggy-wifi-watch.service\n\
         sudo nmcli general reload conf 2>/dev/null || true\n\
         sudo nmcli general logging level WARN domains ALL 2>/dev/null || true\n\
         sudo iw dev wlan0 set power_save off 2>/dev/null || true",
    )
    .context("setting up the Wi-Fi check")?;
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

/// True for a player from before Soloist: BlueALSA instead of PipeWire, or, with Spotify, no
/// API key yet. A card-only player needs no key.
pub fn needs_soloist_migration(r: &dyn Remote, spotify: bool) -> bool {
    let no_key = if spotify {
        format!(" || ! sudo test -s {SOLOIST_KEY_PATH}")
    } else {
        String::new()
    };
    r.run(&format!(
        "if ! command -v pw-dump >/dev/null || systemctl is-enabled bluealsa >/dev/null 2>&1{no_key}; \
         then echo MIGRATE; fi"
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
    fn airplay_installs_only_when_missing_and_its_own_service_stays_off() {
        let r = FakeRemote::default();
        install_airplay(&r).unwrap();
        let scripts = r.all_scripts();
        assert!(
            scripts.contains("if ! command -v shairport-sync"),
            "{scripts}"
        );
        assert!(
            scripts.contains("--no-install-recommends shairport-sync"),
            "{scripts}"
        );
        assert!(scripts.contains("systemctl disable --now shairport-sync.service"));
    }

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
    fn turn_off_reconnect_and_sharing_are_the_only_things_the_player_may_sudo() {
        let r = FakeRemote::default();
        allow_player_commands(&r).unwrap();
        let rule = r.file("/tmp/ziggy-power").unwrap();
        assert!(rule.contains("ziggy-player ALL=(root) NOPASSWD: /usr/bin/systemctl poweroff\n"));
        assert!(rule.contains(
            "ziggy-player ALL=(root) NOPASSWD: /usr/bin/nmcli --wait 45 device connect wlan0\n"
        ));
        for line in [
            "ziggy-player ALL=(root) NOPASSWD: /usr/bin/systemctl enable --now smbd\n",
            "ziggy-player ALL=(root) NOPASSWD: /usr/bin/systemctl disable --now smbd\n",
        ] {
            assert!(rule.contains(line), "{rule}");
        }
        assert_eq!(rule.lines().filter(|l| !l.starts_with('#')).count(), 4);
        assert!(r.all_scripts().contains("visudo -cf /tmp/ziggy-power"));
    }

    #[test]
    fn the_music_folder_is_made_and_sharing_stays_off_unless_asked() {
        let r = FakeRemote::default();
        install_music(&r, false).unwrap();
        let scripts = r.all_scripts();
        for part in [
            "if ! dpkg -s samba rsync",
            "--no-install-recommends samba rsync",
            "groupadd --system ziggy-music",
            "usermod -aG ziggy-music ziggy-player",
            "usermod -aG ziggy-music \"$(id -un)\"",
            "install -d -m 2775 -o ziggy-player -g ziggy-music /var/lib/ziggy-music",
            "systemctl disable --now smbd nmbd",
        ] {
            assert!(scripts.contains(part), "{part}\n{scripts}");
        }
        assert!(!scripts.contains("enable --now smbd"));
        let conf = r.file(SMB_CONF_PATH).unwrap();
        assert!(conf.contains("guest ok = yes"), "{conf}");

        let r = FakeRemote::default();
        install_music(&r, true).unwrap();
        assert!(r.all_scripts().contains("systemctl enable --now smbd"));
        assert!(r.all_scripts().contains("systemctl disable --now nmbd"));
    }

    #[test]
    fn samba_shares_only_the_music_folder() {
        let conf = smb_conf(false);
        for line in [
            "[Music]",
            "path = /var/lib/ziggy-music",
            "map to guest = Bad User",
            "guest ok = yes",
            "force user = ziggy-player",
            "force group = ziggy-music",
            "create mask = 0664",
            "directory mask = 2775",
            "server min protocol = SMB2",
            "multicast dns register = yes",
            "load printers = no",
        ] {
            assert!(conf.lines().any(|l| l.trim() == line), "{line}\n{conf}");
        }
        // Two sections: [global] and [Music]; no printers or home folders.
        assert_eq!(conf.lines().filter(|l| l.starts_with('[')).count(), 2);
        assert!(conf.contains(&format!("path = {MUSIC_DIR}\n")));
        let locked = smb_conf(true);
        assert!(locked.contains("   guest ok = no\n   valid users = ziggy\n"));
        assert!(!locked.contains("guest ok = yes"));
    }

    #[test]
    fn a_share_password_is_set_and_removed() {
        let r = FakeRemote::default();
        set_share_password(&r, Some("tunes")).unwrap();
        assert_eq!(
            r.file("/tmp/ziggy-share-password").as_deref(),
            Some("tunes\ntunes\n")
        );
        let scripts = r.all_scripts();
        assert!(scripts.contains("smbpasswd -s -a ziggy"), "{scripts}");
        assert!(scripts.contains("trap 'sudo rm -f /tmp/ziggy-share-password' EXIT"));
        assert!(
            r.file(SMB_CONF_PATH)
                .unwrap()
                .contains("valid users = ziggy")
        );
        set_share_password(&r, None).unwrap();
        assert!(r.all_scripts().contains("smbpasswd -x ziggy"));
        assert!(r.file(SMB_CONF_PATH).unwrap().contains("guest ok = yes"));
    }

    #[test]
    fn a_card_only_player_needs_no_soloist_key() {
        let r = FakeRemote::default();
        needs_soloist_migration(&r, true);
        needs_soloist_migration(&r, false);
        let scripts = r.scripts.borrow();
        assert!(scripts[0].contains(SOLOIST_KEY_PATH));
        assert!(!scripts[1].contains(SOLOIST_KEY_PATH));
    }

    #[test]
    fn wifi_check_is_installed_and_started() {
        let r = FakeRemote::default();
        watch_wifi(&r).unwrap();
        let script = r.file("/usr/local/lib/ziggy/wifi-watch").unwrap();
        // The same command Reconnect may sudo.
        assert!(script.contains("nmcli --wait 45 device connect wlan0"));
        assert!(PLAYER_SUDOERS.contains("/usr/bin/nmcli --wait 45 device connect wlan0"));
        assert!(
            r.file("/etc/systemd/system/ziggy-wifi-watch.service")
                .is_some()
        );
        assert!(
            r.file("/etc/NetworkManager/conf.d/80-ziggy-wifi.conf")
                .unwrap()
                .contains("wifi.powersave = 2")
        );
        assert!(WIFI_CONF.contains("[logging]\nlevel = WARN\n"));
        assert!(r.all_scripts().contains("enable ziggy-wifi-watch.service"));
        assert!(r.all_scripts().contains("logging level WARN"));
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
                ha_token: Some("eyJ"),
            },
        )
        .unwrap();
        assert_eq!(r.file("/var/lib/ziggy/ha-token").as_deref(), Some("eyJ"));
        for path in [
            "/var/lib/ziggy/soloist-api-key",
            "/var/lib/ziggy/web-auth.json",
            "/var/lib/ziggy/vertex-key.json",
            "/var/lib/ziggy/ha-token",
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
    fn the_home_assistant_token_is_checked_and_removed_on_the_pi() {
        let r = FakeRemote::default();
        assert!(!has_ha_token(&r));
        assert!(
            r.all_scripts()
                .contains("sudo test -s /var/lib/ziggy/ha-token")
        );
        let r = FakeRemote {
            output: "yes\n".into(),
            ..Default::default()
        };
        assert!(has_ha_token(&r));
        remove_ha_token(&r).unwrap();
        assert!(
            r.all_scripts()
                .contains("sudo rm -f /var/lib/ziggy/ha-token")
        );
    }

    #[test]
    fn a_bad_config_is_never_written() {
        let r = FakeRemote::default();
        let mut t = devconfig::new_config(&devconfig::Choices {
            device_name: "Den".into(),
            audiobookshelf_url: None,
            voice: None,
            spotify: true,
            my_music: false,
            music_share: false,
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
