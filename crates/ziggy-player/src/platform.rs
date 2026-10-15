//! Hardware bits: the DSI backlight, and restarting to apply settings. Bluetooth lives in
//! `crate::bluetooth`.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use crate::app::{DisplayMode, Input, WifiStatus};

pub struct Platform {
    backlight: Option<Arc<Backlight>>,
}

/// Dimming and going dark fade over this long; waking is instant.
const FADE: Duration = Duration::from_secs(5);
const FADE_STEPS: u32 = 25;

impl Platform {
    /// Must be called inside the tokio runtime.
    pub fn start() -> Self {
        let backlight = Backlight::detect();
        match &backlight {
            Some(b) => tracing::info!("backlight at {}", b.brightness.display()),
            None => tracing::info!("no backlight control; the UI overlay handles dimming"),
        }
        Self {
            backlight: backlight.map(Arc::new),
        }
    }

    /// Turns the backlight off at once, cancelling any fade; for shutting down.
    pub fn screen_off_now(&self) {
        if let Some(backlight) = &self.backlight {
            backlight.generation.fetch_add(1, Ordering::SeqCst);
            backlight.write(0);
        }
    }

    pub fn set_display(&self, mode: DisplayMode) {
        let Some(backlight) = &self.backlight else {
            return;
        };
        let target = brightness_for(mode, backlight.max);
        // A newer change cancels a fade still running.
        let generation = backlight.generation.fetch_add(1, Ordering::SeqCst) + 1;
        if mode == DisplayMode::Active {
            backlight.write(target);
            return;
        }
        let backlight = backlight.clone();
        tokio::spawn(async move {
            let from = backlight.current.load(Ordering::SeqCst);
            for step in 1..=FADE_STEPS {
                tokio::time::sleep(FADE / FADE_STEPS).await;
                if backlight.generation.load(Ordering::SeqCst) != generation {
                    return;
                }
                let value = from as i64
                    + (target as i64 - from as i64) * i64::from(step) / i64::from(FADE_STEPS);
                backlight.write(value as u32);
            }
        });
    }
}

/// The kernel's CPU temperature, in thousandths of a degree.
const THERMAL: &str = "/sys/class/thermal/thermal_zone0/temp";

/// Whole °C from the kernel's reading, e.g. "47236" is 47.
pub fn parse_temperature(text: &str) -> Option<i32> {
    let millidegrees: i64 = text.trim().parse().ok()?;
    Some((millidegrees as f64 / 1000.0).round() as i32)
}

/// Tells the app the CPU temperature every few seconds, when it changes. Does nothing on a
/// computer that doesn't report one. Must be called inside the tokio runtime.
pub fn spawn_temperature(inputs: tokio::sync::mpsc::UnboundedSender<Input>) {
    if !std::path::Path::new(THERMAL).exists() {
        return;
    }
    tokio::spawn(async move {
        let mut last = None;
        loop {
            let now = std::fs::read_to_string(THERMAL)
                .ok()
                .and_then(|t| parse_temperature(&t));
            if now != last {
                last = now;
                if inputs.send(Input::Temperature(now)).is_err() {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
}

/// The Wi-Fi network in use, from NetworkManager's last scan: one `IN-USE:SSID:FREQ` line per
/// access point, the one in use marked `*`. Colons inside a name are escaped as `\:`.
pub fn parse_network(nmcli: &str) -> Option<(String, String)> {
    nmcli.lines().find_map(|line| {
        let fields = split_terse(line);
        let [in_use, ssid, freq] = fields.as_slice() else {
            return None;
        };
        if in_use != "*" {
            return None;
        }
        let mhz: u32 = freq.trim_end_matches(" MHz").trim().parse().ok()?;
        let band = if mhz < 3000 {
            "2.4 GHz"
        } else if mhz < 5925 {
            "5 GHz"
        } else {
            "6 GHz"
        };
        Some((ssid.clone(), band.to_string()))
    })
}

/// Splits one line of `nmcli -t` output on unescaped colons.
fn split_terse(line: &str) -> Vec<String> {
    let mut fields = vec![String::new()];
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(next) = chars.next() {
                    fields.last_mut().unwrap().push(next);
                }
            }
            ':' => fields.push(String::new()),
            c => fields.last_mut().unwrap().push(c),
        }
    }
    fields
}

/// The live signal of wlan0 from `/proc/net/wireless`, as NetworkManager's 0-100: -100 dBm or
/// weaker is 0, -40 dBm or stronger is 100.
pub fn parse_signal(proc_net_wireless: &str) -> Option<u8> {
    let line = proc_net_wireless
        .lines()
        .find(|l| l.trim_start().starts_with("wlan0:"))?;
    let level: f32 = line
        .split_whitespace()
        .nth(3)?
        .trim_end_matches('.')
        .parse()
        .ok()?;
    // Some drivers report an unsigned byte.
    let dbm = if level > 0.0 { level - 256.0 } else { level };
    Some(((dbm.clamp(-100.0, -40.0) + 100.0) * 100.0 / 60.0).round() as u8)
}

/// Tells the app about the Wi-Fi every 10 s, when it changes. Does nothing on a computer
/// without NetworkManager. Must be called inside the tokio runtime.
pub fn spawn_wifi(inputs: tokio::sync::mpsc::UnboundedSender<Input>) {
    if !std::path::Path::new("/proc/net/wireless").exists() {
        return;
    }
    tokio::spawn(async move {
        let mut last = None;
        loop {
            // `--rescan no` reads NetworkManager's last scan; a scan would interrupt playback.
            let Ok(out) = tokio::process::Command::new("nmcli")
                .args([
                    "-t",
                    "-f",
                    "IN-USE,SSID,FREQ",
                    "dev",
                    "wifi",
                    "list",
                    "--rescan",
                    "no",
                ])
                .output()
                .await
            else {
                return;
            };
            let now = Some(
                match (
                    parse_network(&String::from_utf8_lossy(&out.stdout)),
                    std::fs::read_to_string("/proc/net/wireless")
                        .ok()
                        .and_then(|t| parse_signal(&t)),
                ) {
                    (Some((network, band)), Some(signal)) => WifiStatus::Connected {
                        network,
                        band,
                        signal,
                    },
                    _ => WifiStatus::Disconnected,
                },
            );
            if now != last {
                last = now.clone();
                if inputs.send(Input::Wifi(now)).is_err() {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    });
}

pub fn brightness_for(mode: DisplayMode, max: u32) -> u32 {
    match mode {
        DisplayMode::Active => max,
        // 80% darker.
        DisplayMode::Dim => (max / 5).max(1),
        DisplayMode::Off => 0,
    }
}

struct Backlight {
    brightness: PathBuf,
    max: u32,
    /// The value last written.
    current: AtomicU32,
    /// Bumped by every display change, so an older fade stops.
    generation: AtomicU64,
}

impl Backlight {
    fn detect() -> Option<Self> {
        let dir = std::fs::read_dir("/sys/class/backlight")
            .ok()?
            .flatten()
            .next()?
            .path();
        let max = std::fs::read_to_string(dir.join("max_brightness"))
            .ok()?
            .trim()
            .parse()
            .ok()?;
        Some(Self {
            brightness: dir.join("brightness"),
            max,
            current: AtomicU32::new(max),
            generation: AtomicU64::new(0),
        })
    }

    fn write(&self, value: u32) {
        self.current.store(value, Ordering::SeqCst);
        if let Err(e) = std::fs::write(&self.brightness, value.to_string()) {
            tracing::warn!("setting backlight failed: {e}");
        }
    }
}

/// Shuts the Pi down cleanly, so the SD card is never cut off mid-write. The service user may
/// run exactly this through sudo (set up by `ziggy setup` and `ziggy update`).
pub async fn power_off() {
    tracing::info!("turning off");
    match tokio::process::Command::new("sudo")
        .args(["-n", "/usr/bin/systemctl", "poweroff"])
        .status()
        .await
    {
        Ok(status) if status.success() => {}
        Ok(status) => tracing::error!("turning off failed: sudo exited with {status}"),
        Err(e) => tracing::error!("turning off failed: {e}"),
    }
}

/// Replaces this process with a fresh copy of itself, with the same arguments, so new
/// settings take effect. Under systemd the service keeps its PID; on a Mac the window reopens.
pub fn restart() -> ! {
    use std::os::unix::process::CommandExt;
    let error = match std::env::current_exe() {
        Ok(exe) => std::process::Command::new(exe)
            .args(std::env::args_os().skip(1))
            .exec(),
        Err(e) => e,
    };
    tracing::error!("restarting failed: {error}");
    // systemd restarts the service on exit; on a Mac the user starts it again.
    std::process::exit(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_network_in_use_is_found_with_its_band() {
        let nmcli = " :46Brewer:2422 MHz\n*:46Brewer:5240 MHz\n :Other:5785 MHz\n";
        assert_eq!(
            parse_network(nmcli),
            Some(("46Brewer".into(), "5 GHz".into()))
        );
        assert_eq!(
            parse_network("*:Cafe\\: Guest:2437 MHz\n"),
            Some(("Cafe: Guest".into(), "2.4 GHz".into()))
        );
        assert_eq!(
            parse_network(" :46Brewer:2422 MHz\n"),
            None,
            "nothing in use"
        );
    }

    #[test]
    fn the_signal_level_becomes_a_percentage() {
        let proc = "Inter-| sta-|   Quality        |   Discarded packets               | Missed | WE\n face | tus | link level noise |  nwid  crypt   frag  retry   misc | beacon | 22\n wlan0: 0000   54.  -56.  -256        0      0      0     10      0        0\n";
        assert_eq!(parse_signal(proc), Some(73));
        assert_eq!(parse_signal(&proc.replace("-56.", "-30.")), Some(100));
        assert_eq!(parse_signal(&proc.replace("-56.", "-95.")), Some(8));
        assert_eq!(
            parse_signal(&proc.replace("-56.", "200.")),
            Some(73),
            "200 as a byte is -56 dBm"
        );
        assert_eq!(parse_signal("no wifi here"), None);
    }

    #[test]
    fn the_kernel_reading_is_whole_degrees() {
        assert_eq!(parse_temperature("47236\n"), Some(47));
        assert_eq!(parse_temperature("47500"), Some(48));
        assert_eq!(parse_temperature("not a number"), None);
    }

    #[test]
    fn brightness_levels() {
        assert_eq!(brightness_for(DisplayMode::Active, 255), 255);
        assert_eq!(brightness_for(DisplayMode::Dim, 255), 51);
        assert_eq!(brightness_for(DisplayMode::Dim, 5), 1);
        assert_eq!(brightness_for(DisplayMode::Off, 255), 0);
    }
}
