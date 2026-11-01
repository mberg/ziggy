//! AirPlay: shairport-sync makes the player an AirPlay speaker. Ziggy runs it, passes its
//! sound to PipeWire, reads what's playing from its metadata, and controls the phone through
//! its D-Bus interface. Nothing is written to the SD card: its settings file lives in the
//! session's runtime folder, which is in memory.

pub mod metadata;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UdpSocket;
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::app::{AirPlayCommand, AirPlayUpdate, Input};
use metadata::Event;

/// Covers are asked for as "airplay-cover:<hash of the image>".
pub const COVER: &str = "airplay-cover:";

/// The cover of the song playing now, kept in memory for the image loader.
pub type CoverStore = Arc<Mutex<Option<(String, Vec<u8>)>>>;

/// The image bytes for a cover URL, if it's the one playing now.
pub fn cover(store: &CoverStore, url: &str) -> Option<Vec<u8>> {
    match &*store.lock().unwrap() {
        Some((key, bytes)) if key == url => Some(bytes.clone()),
        _ => None,
    }
}

/// Keeps a new cover and returns its URL; the same picture sent again keeps its URL.
fn keep_cover(store: &CoverStore, bytes: Vec<u8>) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    let url = format!("{COVER}{:016x}", hasher.finish());
    *store.lock().unwrap() = Some((url.clone(), bytes));
    Some(url)
}

pub struct AirPlaySettings {
    /// The name phones show.
    pub name: String,
    /// The PipeWire speaker; the headphone jack when None.
    pub output: Option<String>,
    pub enabled: bool,
}

pub enum Backend {
    Real,
    /// `--fake`: a pretend phone starts playing a few seconds after AirPlay is turned on.
    Fake,
}

pub fn spawn(
    backend: Backend,
    settings: AirPlaySettings,
    covers: CoverStore,
    inputs: UnboundedSender<Input>,
) -> UnboundedSender<AirPlayCommand> {
    let (tx, rx) = mpsc::unbounded_channel();
    match backend {
        Backend::Real => tokio::spawn(run(settings, covers, inputs, rx)),
        Backend::Fake => tokio::spawn(fake::run(settings, covers, inputs, rx)),
    };
    tx
}

/// After shairport-sync stops, wait this long before starting it again, doubling up to the most.
const RESTART_MIN: Duration = Duration::from_secs(2);
const RESTART_MAX: Duration = Duration::from_secs(60);
/// The speaker stream closes after this long without sound, so a Bluetooth speaker can sleep.
const QUIET_CLOSE: Duration = Duration::from_secs(3);

enum Stopped {
    TurnedOff,
    /// The core has gone.
    Closed,
}

async fn run(
    mut settings: AirPlaySettings,
    covers: CoverStore,
    inputs: UnboundedSender<Input>,
    mut commands: UnboundedReceiver<AirPlayCommand>,
) {
    let mut wait = RESTART_MIN;
    loop {
        if !settings.enabled {
            match commands.recv().await {
                None => return,
                Some(AirPlayCommand::Enable(on)) => settings.enabled = on,
                Some(_) => {}
            }
            continue;
        }
        let started = Instant::now();
        let result = receive(&settings, &covers, &inputs, &mut commands).await;
        let _ = inputs.send(Input::AirPlay(AirPlayUpdate::Ended));
        match result {
            Ok(Stopped::Closed) => return,
            Ok(Stopped::TurnedOff) => {
                tracing::info!("AirPlay off");
                settings.enabled = false;
                wait = RESTART_MIN;
                continue;
            }
            Err(e) => tracing::warn!("AirPlay stopped: {e:#}"),
        }
        if started.elapsed() > RESTART_MAX {
            wait = RESTART_MIN;
        }
        // Wait before trying again, but still hear "turn off".
        let pause = tokio::time::sleep(wait);
        tokio::pin!(pause);
        loop {
            tokio::select! {
                _ = &mut pause => break,
                command = commands.recv() => match command {
                    None => return,
                    Some(AirPlayCommand::Enable(on)) => settings.enabled = on,
                    Some(_) => {}
                },
            }
        }
        wait = (wait * 2).min(RESTART_MAX);
    }
}

/// Runs shairport-sync until it stops or AirPlay is turned off.
async fn receive(
    settings: &AirPlaySettings,
    covers: &CoverStore,
    inputs: &UnboundedSender<Input>,
    commands: &mut UnboundedReceiver<AirPlayCommand>,
) -> anyhow::Result<Stopped> {
    let dir = runtime_dir().context("no runtime folder for the settings")?;
    let socket = UdpSocket::bind("127.0.0.1:0").await?;
    let port = socket.local_addr()?.port();
    let conf = dir.join("ziggy-airplay.conf");
    std::fs::write(&conf, config_file(&settings.name, port))?;
    let mut child = session(Command::new("shairport-sync"), &dir)
        .arg("-c")
        .arg(&conf)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("starting shairport-sync")?;
    tracing::info!("AirPlay on as {:?}", settings.name);
    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                // It can't take the system bus's names; the session bus is the one in use.
                if !line.contains("on the system bus") {
                    tracing::info!("shairport-sync: {line}");
                }
            }
        });
    }
    let mut audio = child
        .stdout
        .take()
        .context("no sound from shairport-sync")?;
    let mut speaker = Speaker::new(settings.output.clone(), dir.clone());
    let mut reader = metadata::Reader::default();
    let mut sound = vec![0u8; 16 * 1024];
    let mut packet = vec![0u8; 70 * 1024];
    let mut check = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            read = audio.read(&mut sound) => {
                let n = read?;
                anyhow::ensure!(n > 0, "shairport-sync closed its sound output");
                speaker.play(&sound[..n]).await;
            }
            got = socket.recv(&mut packet) => {
                let update = match reader.packet(&packet[..got?]) {
                    Some(Event::Update(update)) => update,
                    Some(Event::Cover(bytes)) => AirPlayUpdate::Cover(keep_cover(covers, bytes)),
                    None => continue,
                };
                let _ = inputs.send(Input::AirPlay(update));
            }
            _ = check.tick() => speaker.close_if_quiet(),
            status = child.wait() => anyhow::bail!("shairport-sync exited ({})", status?),
            command = commands.recv() => match command {
                None => return Ok(Stopped::Closed),
                Some(AirPlayCommand::Enable(false)) => return Ok(Stopped::TurnedOff),
                Some(AirPlayCommand::Enable(true)) => {}
                Some(command) => remote(command, &dir),
            },
        }
    }
}

/// shairport-sync's settings: sound on its output, metadata to Ziggy's port, controls on
/// the session bus (a system service can't reach the system bus's names).
fn config_file(name: &str, port: u16) -> String {
    let name = name.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        "general = {{\n  name = \"{name}\";\n  output_backend = \"stdout\";\n  \
         dbus_service_bus = \"session\";\n  mpris_service_bus = \"session\";\n}};\n\
         sessioncontrol = {{\n  allow_session_interruption = \"yes\";\n}};\n\
         metadata = {{\n  enabled = \"yes\";\n  include_cover_art = \"yes\";\n  \
         socket_address = \"127.0.0.1\";\n  socket_port = {port};\n  \
         socket_msglength = 65000;\n  progress_interval = 1.0;\n}};\n"
    )
}

/// The player's session folder, where PipeWire and the session bus live.
fn runtime_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(|| crate::player::soloist::own_uid().map(|uid| format!("/run/user/{uid}").into()))
}

/// A command run in the player's session: PipeWire and the session bus.
fn session(mut cmd: Command, dir: &Path) -> Command {
    cmd.env("XDG_RUNTIME_DIR", dir).env(
        "DBUS_SESSION_BUS_ADDRESS",
        format!("unix:path={}/bus", dir.display()),
    );
    cmd
}

/// Asks the phone, through shairport-sync, to do something.
fn remote(command: AirPlayCommand, dir: &Path) {
    const REMOTE: &str = "org.gnome.ShairportSync.RemoteControl";
    let (interface, method, args): (&str, &str, Vec<String>) = match command {
        AirPlayCommand::Play => (REMOTE, "Play", vec![]),
        AirPlayCommand::Pause => (REMOTE, "Pause", vec![]),
        AirPlayCommand::Next => (REMOTE, "Next", vec![]),
        AirPlayCommand::Previous => (REMOTE, "Previous", vec![]),
        AirPlayCommand::SetVolume { percent } => (
            REMOTE,
            "SetAirplayVolume",
            vec![
                "d".into(),
                format!("{:.2}", metadata::airplay_volume(percent)),
            ],
        ),
        AirPlayCommand::End => ("org.gnome.ShairportSync", "DropSession", vec![]),
        AirPlayCommand::Enable(_) => return,
    };
    let mut cmd = session(Command::new("busctl"), dir);
    cmd.args([
        "--user",
        "call",
        "org.gnome.ShairportSync",
        "/org/gnome/ShairportSync",
        interface,
        method,
    ])
    .args(&args)
    .stdin(Stdio::null());
    tokio::spawn(async move {
        match cmd.output().await {
            Ok(out) if out.status.success() => {}
            Ok(out) => tracing::warn!(
                "AirPlay {method} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            Err(e) => tracing::warn!("AirPlay {method} failed: {e}"),
        }
    });
}

/// The stream to the speaker, open only while there's sound: an open stream keeps a
/// Bluetooth speaker awake.
struct Speaker {
    output: Option<String>,
    dir: PathBuf,
    open: Option<(Child, ChildStdin)>,
    last_sound: Instant,
}

impl Speaker {
    fn new(output: Option<String>, dir: PathBuf) -> Self {
        Speaker {
            output,
            dir,
            open: None,
            last_sound: Instant::now(),
        }
    }

    async fn play(&mut self, sound: &[u8]) {
        self.last_sound = Instant::now();
        if self.open.is_none() {
            match self.connect().await {
                Ok(open) => self.open = Some(open),
                Err(e) => {
                    tracing::warn!("AirPlay can't reach the speaker: {e:#}");
                    return;
                }
            }
        }
        if let Some((_, stdin)) = &mut self.open
            && let Err(e) = stdin.write_all(sound).await
        {
            tracing::warn!("AirPlay sound stopped: {e}");
            self.open = None;
        }
    }

    fn close_if_quiet(&mut self) {
        if self.open.is_some() && self.last_sound.elapsed() >= QUIET_CLOSE {
            self.open = None;
        }
    }

    /// pw-cat plays what it's given: 16-bit stereo at 44.1 kHz, as shairport-sync sends it.
    async fn connect(&self) -> anyhow::Result<(Child, ChildStdin)> {
        let target = match &self.output {
            Some(node) => Some(node.clone()),
            None => crate::player::soloist::jack_node().await,
        };
        let mut cmd = session(Command::new("pw-cat"), &self.dir);
        cmd.args(["--playback", "--raw", "--format", "s16", "--rate", "44100"])
            .args(["--channels", "2"])
            .args(["-P", "node.name=airplay,application.name=AirPlay"]);
        if let Some(target) = &target {
            cmd.args(["--target", target]);
        }
        let mut child = cmd
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("starting pw-cat")?;
        let stdin = child.stdin.take().context("no pw-cat input")?;
        Ok((child, stdin))
    }
}

mod fake {
    use super::*;

    /// A pretend phone, for trying the screen with `--fake`.
    pub async fn run(
        mut settings: AirPlaySettings,
        covers: CoverStore,
        inputs: UnboundedSender<Input>,
        mut commands: UnboundedReceiver<AirPlayCommand>,
    ) {
        let send = |u| {
            let _ = inputs.send(Input::AirPlay(u));
        };
        let mut playing = None::<(u32, bool)>;
        let mut song = 0;
        let mut starts_in = settings.enabled.then_some(8);
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    if let Some(left) = &mut starts_in {
                        *left -= 1;
                        if *left == 0 {
                            starts_in = None;
                            playing = Some((0, true));
                            send(AirPlayUpdate::Started);
                            send(AirPlayUpdate::Volume { percent: 70 });
                            describe(song, &covers, &send);
                        }
                    }
                    if let Some((position, true)) = &mut playing {
                        *position += 1000;
                        send(AirPlayUpdate::Progress { position_ms: *position, duration_ms: 200_000 });
                    }
                }
                command = commands.recv() => match command {
                    None => return,
                    Some(AirPlayCommand::Enable(on)) => {
                        settings.enabled = on;
                        starts_in = on.then_some(8);
                        if !on && playing.take().is_some() {
                            send(AirPlayUpdate::Ended);
                        }
                    }
                    Some(AirPlayCommand::Play | AirPlayCommand::Pause) => {
                        if let Some((_, on)) = &mut playing {
                            *on = !*on;
                            send(if *on { AirPlayUpdate::Playing } else { AirPlayUpdate::Paused });
                        }
                    }
                    Some(AirPlayCommand::Next | AirPlayCommand::Previous) => {
                        if playing.is_some() {
                            song += 1;
                            playing = Some((0, true));
                            describe(song, &covers, &send);
                        }
                    }
                    Some(AirPlayCommand::SetVolume { percent }) => send(AirPlayUpdate::Volume { percent }),
                    Some(AirPlayCommand::End) => {
                        if playing.take().is_some() {
                            send(AirPlayUpdate::Paused);
                            send(AirPlayUpdate::Ended);
                        }
                    }
                }
            }
        }
    }

    fn describe(song: u32, covers: &CoverStore, send: &impl Fn(AirPlayUpdate)) {
        send(AirPlayUpdate::Song {
            title: format!("Song from a phone {}", song + 1),
            artist: "The AirPlay Band".into(),
            album: "Over the Air".into(),
        });
        let shade = (song * 70 % 255) as u8;
        let img = image::RgbaImage::from_pixel(64, 64, image::Rgba([40, 120, shade, 255]));
        let mut png = std::io::Cursor::new(Vec::new());
        if img.write_to(&mut png, image::ImageFormat::Png).is_ok() {
            send(AirPlayUpdate::Cover(keep_cover(covers, png.into_inner())));
        }
        send(AirPlayUpdate::Progress {
            position_ms: 0,
            duration_ms: 200_000,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_cover_keeps_its_url_and_only_the_latest_is_kept() {
        let store = CoverStore::default();
        let a = keep_cover(&store, b"one".to_vec()).unwrap();
        assert!(a.starts_with(COVER));
        assert_eq!(keep_cover(&store, b"one".to_vec()).unwrap(), a);
        assert_eq!(cover(&store, &a).unwrap(), b"one");
        let b = keep_cover(&store, b"two".to_vec()).unwrap();
        assert_ne!(a, b);
        assert_eq!(cover(&store, &a), None, "an old cover has gone");
        assert_eq!(keep_cover(&store, Vec::new()), None, "no picture");
    }

    #[test]
    fn the_name_is_quoted_for_shairport_sync() {
        let conf = config_file("Matt's \"Ziggy\"", 5555);
        assert!(conf.contains(r#"name = "Matt's \"Ziggy\"";"#), "{conf}");
        assert!(conf.contains("socket_port = 5555;"));
        assert!(conf.contains("output_backend = \"stdout\";"));
    }
}
