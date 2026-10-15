//! What `ziggy` remembers on this computer: the Spotify developer app, and each player's
//! Spotify sign-in (kept so a player can be set up again without signing in).
//! Lives in ~/.config/ziggy (or $ZIGGY_HOME).

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Saved {
    /// The Spotify developer app's Client ID; one app serves every player.
    #[serde(default)]
    pub spotify_client_id: Option<String>,
    /// The last Google Cloud key file used for voice, offered again next time.
    #[serde(default)]
    pub vertex_key_file: Option<PathBuf>,
}

/// The Spotify Soloist API key, kept as a private file beside the saved choices. Spotify says
/// the key is for the person who made it, so it's never shared or published.
pub fn soloist_key_file() -> PathBuf {
    home().join("soloist-api-key")
}

pub fn save_soloist_key(key: &str) -> anyhow::Result<PathBuf> {
    let dir = home();
    private_dir(&dir)?;
    let path = soloist_key_file();
    std::fs::write(&path, key.trim())?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(path)
}

pub fn home() -> PathBuf {
    if let Some(dir) = std::env::var_os("ZIGGY_HOME") {
        return PathBuf::from(dir);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        });
    home_in(&base)
}

/// `<base>/ziggy`. A folder saved before the rename to Ziggy (`<base>/muzak`) is moved there
/// first, so the sign-ins and settings carry across.
fn home_in(base: &Path) -> PathBuf {
    let home = base.join("ziggy");
    let old = base.join("muzak");
    if !home.exists() && old.is_dir() && std::fs::rename(&old, &home).is_ok() {
        let _ = std::fs::rename(home.join("muzak.toml"), home.join("ziggy.toml"));
    }
    home
}

fn private_dir(path: &PathBuf) -> anyhow::Result<()> {
    std::fs::create_dir_all(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

pub fn load() -> Saved {
    std::fs::read_to_string(home().join("ziggy.toml"))
        .ok()
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save(saved: &Saved) -> anyhow::Result<()> {
    let dir = home();
    private_dir(&dir)?;
    std::fs::write(dir.join("ziggy.toml"), toml::to_string(saved)?)?;
    Ok(())
}

/// Where a player's Spotify sign-in is kept on this computer, by its address.
pub fn device_dir(host: &str) -> anyhow::Result<PathBuf> {
    let safe: String = host
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let dir = home().join("devices").join(safe);
    private_dir(&home())?;
    private_dir(&dir)?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_from_before_the_rename_is_moved() {
        let base = tempfile::tempdir().unwrap();
        let old = base.path().join("muzak");
        std::fs::create_dir_all(old.join("devices/pi_den")).unwrap();
        std::fs::write(old.join("muzak.toml"), "spotify_client_id = \"abc\"\n").unwrap();
        let home = home_in(base.path());
        assert_eq!(home, base.path().join("ziggy"));
        assert!(!old.exists());
        assert!(home.join("devices/pi_den").is_dir());
        assert_eq!(
            std::fs::read_to_string(home.join("ziggy.toml")).unwrap(),
            "spotify_client_id = \"abc\"\n"
        );
        // Already moved: nothing more happens.
        assert_eq!(home_in(base.path()), home);
    }

    #[test]
    fn saved_choices_round_trip_in_a_private_folder() {
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: tests in this crate don't read ZIGGY_HOME concurrently.
        unsafe { std::env::set_var("ZIGGY_HOME", dir.path().join("m")) };
        save(&Saved {
            spotify_client_id: Some("abc".into()),
            vertex_key_file: None,
        })
        .unwrap();
        assert_eq!(load().spotify_client_id.as_deref(), Some("abc"));
        let device = device_dir("pi@ziggy-den.local").unwrap();
        assert!(device.ends_with("devices/pi_ziggy-den.local"));
        let mode = std::fs::metadata(&device).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }
}
