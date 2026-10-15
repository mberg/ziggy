//! Plays Spotify through Spotify Soloist, Spotify's own headless Connect player, which Ziggy
//! starts, keeps up to date and controls over Soloist's local WebSocket API.
//!
//! Soloist replaced librespot when Spotify stopped giving librespot the keys to unlock songs for
//! newer accounts. It plays through PipeWire, signs in when the device is picked in a Spotify
//! app, and its builds stop working 90 days after they're made, so Ziggy downloads fresh ones.

pub mod protocol;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio_tungstenite::tungstenite::Message;

use self::protocol::Event;
use crate::app::{Input, PlayerCommand, PlayerUpdate};
use crate::library::cache::DiskCache;
use crate::model::{Account, LIKED_URI};

/// Where Spotify publishes Soloist builds.
const DOWNLOAD: &str = "https://soloist-builds.spotifycdn.com/soloist_release_arm64.tar.gz";
/// Builds stop working 90 days after they're made; fetch a new one well before.
const REFRESH_AFTER_DAYS: i64 = 45;
/// Soloist exits with this when its build has expired.
const EXIT_EXPIRED: i32 = 10;
/// Asking for this file makes the next start pair afresh (`ziggy signin`).
pub const PAIR_REQUEST: &str = "soloist-pair";
/// The account's ID, which the library caches; Liked Songs needs it.
const ACCOUNT_KEY: &str = "account";
/// Soloist's local WebSocket, on a fixed port so `ziggy` can ask it too (`soloist ctl -w`).
pub const WS_PORT: u16 = 47321;
/// How long a list may take to skip ahead to the tapped song before the sound comes back anyway.
const SKIP_TIMEOUT: Duration = Duration::from_secs(20);

/// Starts a list at a song on this device through Spotify's Web API, which is instant. Gets the
/// list's URI and where to start; false when that didn't work and the engine should skip ahead
/// itself.
pub type StartFn = Arc<
    dyn Fn(
            String,
            crate::library::web_api::StartAt,
        ) -> futures_util::future::BoxFuture<'static, bool>
        + Send
        + Sync,
>;

#[derive(Debug, Clone)]
pub struct SoloistSettings {
    /// The name in Spotify's device list.
    pub device_name: String,
    /// A file holding the Spotify Soloist API key.
    pub api_key_file: PathBuf,
    /// Ziggy's state folder: Soloist's binary, sign-in and data live under it.
    pub state_dir: PathBuf,
    pub initial_volume: u8,
    /// A PipeWire output node; the default output when None.
    pub output: Option<String>,
}

impl SoloistSettings {
    fn bin(&self) -> PathBuf {
        self.state_dir.join("bin/soloist")
    }
    fn data_dir(&self) -> PathBuf {
        self.state_dir.join("soloist")
    }
    fn cache_dir(&self) -> PathBuf {
        self.state_dir.join("cache/soloist")
    }
}

pub fn spawn(
    settings: SoloistSettings,
    cache: Arc<DiskCache>,
    start: Option<StartFn>,
    inputs: UnboundedSender<Input>,
) -> UnboundedSender<PlayerCommand> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(run(settings, cache, start, inputs, rx));
    tx
}

async fn run(
    settings: SoloistSettings,
    cache: Arc<DiskCache>,
    start: Option<StartFn>,
    inputs: UnboundedSender<Input>,
    mut commands: UnboundedReceiver<PlayerCommand>,
) {
    let mut backoff = Duration::from_secs(2);
    let mut force_download = false;
    loop {
        let started = std::time::Instant::now();
        let result = serve(
            &settings,
            &cache,
            start.as_ref(),
            &inputs,
            &mut commands,
            force_download,
        )
        .await;
        force_download = false;
        let _ = inputs.send(Input::Player(PlayerUpdate::Disconnected));
        match result {
            Ok(Exit::CommandsClosed) => return,
            Ok(Exit::Expired) => {
                tracing::warn!("Soloist's build expired; fetching a new one");
                force_download = true;
                continue;
            }
            Ok(Exit::Stopped(status)) => tracing::warn!("Soloist stopped ({status})"),
            Err(e) => tracing::warn!("Soloist: {e:#}"),
        }
        if started.elapsed() > Duration::from_secs(60) {
            backoff = Duration::from_secs(2);
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(60));
    }
}

enum Exit {
    CommandsClosed,
    Expired,
    Stopped(String),
}

async fn serve(
    settings: &SoloistSettings,
    cache: &DiskCache,
    start: Option<&StartFn>,
    inputs: &UnboundedSender<Input>,
    commands: &mut UnboundedReceiver<PlayerCommand>,
    force_download: bool,
) -> anyhow::Result<Exit> {
    let bin = ensure_binary(&settings.bin(), force_download).await?;
    let key = std::fs::read_to_string(&settings.api_key_file)
        .map(|k| k.trim().to_string())
        .map_err(|e| {
            anyhow::anyhow!(
                "no Soloist API key at {} ({e}); run `ziggy setup`",
                settings.api_key_file.display()
            )
        })?;
    std::fs::create_dir_all(settings.data_dir())?;
    std::fs::create_dir_all(settings.cache_dir())?;

    // `ziggy signin` asks for a fresh pairing: forget the stored sign-in and wait for one.
    let pair_request = settings.state_dir.join(PAIR_REQUEST);
    if pair_request.exists() {
        tracing::info!("waiting to be picked in a Spotify app to sign in");
        let _ = inputs.send(Input::PairingNeeded(true));
        let status = soloist(&bin, settings, &key).arg("--pair").status().await?;
        if status.code() == Some(EXIT_EXPIRED) {
            return Ok(Exit::Expired);
        }
        anyhow::ensure!(status.success(), "pairing failed ({status})");
        let _ = std::fs::remove_file(&pair_request);
    }

    let mut cmd = soloist(&bin, settings, &key);
    cmd.args(["--cache-size", "200"])
        .args(["--initial-volume", &settings.initial_volume.to_string()])
        .args(["--ws", &format!("127.0.0.1:{WS_PORT}")]);
    // A speaker, or the headphone jack: never whatever PipeWire made the default, which is a
    // Bluetooth speaker as soon as one connects.
    let output = match &settings.output {
        Some(node) => Some(node.clone()),
        None => jack_node().await,
    };
    if let Some(node) = &output {
        tracing::info!("playing through {node}");
        cmd.args(["--pipewire-device", node]);
    }
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    forward_logs(&mut child);

    let ws = match connect(&mut child).await? {
        Ok(ws) => ws,
        Err(exit) => return Ok(exit),
    };
    let (mut tx, mut rx) = ws.split();
    tracing::info!("Soloist is running as {:?}", settings.device_name);

    let mut session = Session::default();
    let mut clock = tokio::time::interval(Duration::from_secs(1));
    clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let mut frames: Vec<Value> = Vec::new();
        tokio::select! {
            _ = clock.tick() => {
                let now = std::time::Instant::now();
                session.give_up_skipping(now);
                if session.skipping.is_none()
                    && let Some(position_ms) = session.position_now(now)
                {
                    let _ = inputs.send(Input::Player(PlayerUpdate::Position { position_ms }));
                }
            }
            status = child.wait() => return Ok(exit_of(status?)),
            command = commands.recv() => {
                let Some(command) = command else { return Ok(Exit::CommandsClosed) };
                if !session.logged_in {
                    // The core sends a pending Load again once the player is Connected.
                    continue;
                }
                frames = session.frames_for(command, cache, std::time::Instant::now());
            }
            message = rx.next() => {
                let Some(message) = message else {
                    anyhow::bail!("Soloist's WebSocket closed");
                };
                let Message::Text(text) = message? else { continue };
                match protocol::parse(&text) {
                    Event::Auth { logged_in, .. } => {
                        session.logged_in = logged_in;
                        let _ = inputs.send(Input::PairingNeeded(!logged_in));
                        if logged_in {
                            let _ = inputs.send(Input::Player(PlayerUpdate::Connected));
                            frames.push(protocol::command("get_state"));
                        } else {
                            tracing::info!(
                                "not signed in: pick {:?} in a Spotify app", settings.device_name
                            );
                        }
                    }
                    Event::Updates(updates) => {
                        let now = std::time::Instant::now();
                        for update in updates {
                            if session.forward(&update, now) {
                                let _ = inputs.send(Input::Player(update));
                            }
                        }
                    }
                    Event::Position { position_ms, speed } => {
                        session.anchor = Some((position_ms, std::time::Instant::now(), speed));
                        if session.skipping.is_none() {
                            let _ = inputs.send(Input::Player(PlayerUpdate::Position { position_ms }));
                        }
                    }
                    Event::Queue { upcoming, .. } => {
                        frames = session.start_from_queue(&upcoming, std::time::Instant::now());
                    }
                    Event::Error(message) => tracing::warn!("Soloist refused a command: {message}"),
                    Event::Other => {}
                }
            }
        }
        frames.append(&mut session.outbox);
        for frame in frames {
            tx.send(Message::text(frame.to_string())).await?;
        }
        if let Some(load) = session.load.take() {
            // Spotify's Web API starts a list at a song directly; skipping is the fallback.
            let started = match (start, load.start.clone()) {
                (Some(start), Some(at)) => {
                    tokio::time::timeout(Duration::from_secs(6), start(load.uri.clone(), at))
                        .await
                        .unwrap_or(false)
                }
                _ => false,
            };
            if !started {
                tx.send(Message::text(
                    json!({ "type": "command", "command": "play", "uri": load.uri }).to_string(),
                ))
                .await?;
                if let Some(at) = load.start {
                    session.start = Some(at);
                    session.after_load = Some(Duration::from_millis(800));
                }
            }
        }
        if let Some(after) = session.after_load.take() {
            // Let Soloist load the list before asking where it starts.
            tokio::time::sleep(after).await;
            tx.send(Message::text(
                json!({ "type": "command", "command": "get_queue", "limit": 0 }).to_string(),
            ))
            .await?;
        }
    }
}

/// How long Soloist's echoes of a volume Ziggy just set are ignored. A dragged slider sends a
/// stream of volumes, and late echoes would pull it back.
const VOLUME_ECHO: Duration = Duration::from_millis(1500);

/// What the engine remembers between frames.
#[derive(Default)]
struct Session {
    logged_in: bool,
    current_uri: Option<String>,
    volume: u8,
    /// When Ziggy last set the volume itself.
    volume_set_at: Option<std::time::Instant>,
    /// The last position Soloist reported, when, and how fast it moves (0 when paused).
    anchor: Option<(u32, std::time::Instant, f64)>,
    /// A list to start once the frames before it (shuffle) are sent.
    load: Option<Load>,
    /// A list was just started and should skip ahead to this song once the queue is known.
    start: Option<crate::library::web_api::StartAt>,
    after_load: Option<Duration>,
    /// Skipping ahead quietly: the song to reach, the volume to put back, and when to give up.
    skipping: Option<(Option<String>, u8, std::time::Instant)>,
    /// Frames to send after an event, such as the volume put back once the song is reached.
    outbox: Vec<Value>,
}

/// Starting a list.
#[derive(Debug, Clone, PartialEq)]
struct Load {
    uri: String,
    /// Where to start, unless at the top.
    start: Option<crate::library::web_api::StartAt>,
}

impl Session {
    /// Notes what an update says; false when it shouldn't reach the app.
    fn forward(&mut self, update: &PlayerUpdate, now: std::time::Instant) -> bool {
        // While skipping ahead, the songs passed on the way don't reach the screen.
        if let Some((target, volume, _)) = &self.skipping {
            match update {
                PlayerUpdate::TrackChanged(t)
                    if target.is_none() || target.as_deref() == Some(&t.uri) =>
                {
                    let volume = *volume;
                    self.finish_skipping(volume, now);
                }
                PlayerUpdate::TrackChanged(t) => {
                    self.current_uri = Some(t.uri.clone());
                    return false;
                }
                PlayerUpdate::Volume { .. }
                | PlayerUpdate::Playing { .. }
                | PlayerUpdate::Position { .. } => {
                    return false;
                }
                _ => {}
            }
        }
        match update {
            PlayerUpdate::TrackChanged(t) => {
                self.current_uri = Some(t.uri.clone());
                self.anchor = Some((0, now, self.speed()));
            }
            PlayerUpdate::Volume { percent } => {
                if self
                    .volume_set_at
                    .is_some_and(|at| now.duration_since(at) < VOLUME_ECHO)
                {
                    return false;
                }
                self.volume = *percent;
            }
            PlayerUpdate::Playing { .. } => self.anchor = Some((self.position_at(now), now, 1.0)),
            PlayerUpdate::Paused { .. } | PlayerUpdate::Stopped => {
                self.anchor = Some((self.position_at(now), now, 0.0));
            }
            _ => {}
        }
        true
    }

    fn speed(&self) -> f64 {
        self.anchor.map_or(0.0, |(_, _, speed)| speed)
    }

    /// Where playback is, counted on from Soloist's last report.
    fn position_at(&self, now: std::time::Instant) -> u32 {
        self.anchor.map_or(0, |(position, at, speed)| {
            position.saturating_add((now.duration_since(at).as_millis() as f64 * speed) as u32)
        })
    }

    /// The same, but only while playing: the clock the screen shows then.
    fn position_now(&self, now: std::time::Instant) -> Option<u32> {
        (self.speed() > 0.0).then(|| self.position_at(now))
    }

    fn frames_for(
        &mut self,
        command: PlayerCommand,
        cache: &DiskCache,
        now: std::time::Instant,
    ) -> Vec<Value> {
        if let PlayerCommand::SetVolume { percent } = command {
            self.volume = percent;
            self.volume_set_at = Some(now);
        }
        let PlayerCommand::Load {
            context_uri,
            start_index,
            start_uri,
            shuffle,
        } = command
        else {
            return protocol::frames(&command);
        };
        let username = cache
            .read::<Account>(ACCOUNT_KEY)
            .map(|a| a.id)
            .unwrap_or_default();
        if context_uri == LIKED_URI && username.is_empty() {
            tracing::warn!("Liked Songs needs the account, which hasn't loaded yet");
        }
        let uri = protocol::context_uri_for(&context_uri, &username);
        use crate::library::web_api::StartAt;
        let start = match (start_uri, start_index) {
            (Some(uri), _) => Some(StartAt::Uri(uri)),
            (None, Some(i)) if i > 0 => Some(StartAt::Position(i)),
            _ => None,
        };
        self.load = Some(Load { uri, start });
        // Shuffle first, so the list plays in the right order; the list itself starts next.
        protocol::frames(&PlayerCommand::SetShuffle(shuffle))
    }

    /// The fallback: skips from the top of the list to the song that was tapped, with the
    /// sound down and the songs in between kept off the screen.
    fn start_from_queue(&mut self, upcoming: &[String], now: std::time::Instant) -> Vec<Value> {
        use crate::library::web_api::StartAt;
        let Some(start) = self.start.take() else {
            return Vec::new();
        };
        let (start_uri, start_index) = match &start {
            StartAt::Uri(uri) => (Some(uri.as_str()), None),
            StartAt::Position(i) => (None, Some(*i)),
        };
        let skips = protocol::skips_to(
            upcoming,
            self.current_uri.as_deref(),
            start_uri,
            start_index,
        )
        .unwrap_or(0);
        if skips == 0 {
            return Vec::new();
        }
        let target = start_uri
            .map(str::to_string)
            .or_else(|| upcoming.get(skips - 1).cloned());
        self.skipping = Some((target, self.volume, now + SKIP_TIMEOUT));
        self.volume_set_at = Some(now);
        let mut frames = vec![json!({ "type": "command", "command": "set_volume", "volume": 0 })];
        frames.extend(std::iter::repeat_n(protocol::command("skip_next"), skips));
        frames
    }

    /// The tapped song is playing: the sound comes back.
    fn finish_skipping(&mut self, volume: u8, now: std::time::Instant) {
        self.skipping = None;
        self.volume_set_at = Some(now);
        self.outbox
            .push(json!({ "type": "command", "command": "set_volume", "volume": volume }));
    }

    /// Never leave the sound down: after a while, whatever is playing is heard.
    fn give_up_skipping(&mut self, now: std::time::Instant) {
        if let Some((_, volume, until)) = self.skipping
            && now >= until
        {
            tracing::warn!("didn't reach the tapped song; playing what's there");
            self.finish_skipping(volume, now);
        }
    }
}

fn soloist(bin: &Path, settings: &SoloistSettings, key: &str) -> Command {
    let mut cmd = Command::new(bin);
    cmd.args(["--device-name", &settings.device_name])
        .args(["--api-key", key])
        .arg("--data-dir")
        .arg(settings.data_dir())
        .arg("--cache-dir")
        .arg(settings.cache_dir());
    // PipeWire runs in the service user's session; a system service doesn't get its address.
    if std::env::var_os("XDG_RUNTIME_DIR").is_none()
        && let Some(uid) = own_uid()
    {
        cmd.env("XDG_RUNTIME_DIR", format!("/run/user/{uid}"));
    }
    cmd
}

#[cfg(unix)]
fn own_uid() -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self").ok().map(|m| m.uid())
}

#[cfg(not(unix))]
fn own_uid() -> Option<u32> {
    None
}

/// The PipeWire node for the Pi's headphone jack, e.g.
/// `alsa_output.platform-3f00b840.mailbox.stereo-fallback` on a Pi 3.
async fn jack_node() -> Option<String> {
    let mut cmd = Command::new("pw-dump");
    if std::env::var_os("XDG_RUNTIME_DIR").is_none()
        && let Some(uid) = own_uid()
    {
        cmd.env("XDG_RUNTIME_DIR", format!("/run/user/{uid}"));
    }
    let out = cmd.output().await.ok()?;
    protocol::jack_node(&String::from_utf8_lossy(&out.stdout))
}

/// Soloist's own log goes to the journal; the "playing … [0:10 / 3:03]" line it prints every
/// 10 s only at debug level.
fn forward_logs(child: &mut Child) {
    for stream in [
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
    ]
    .into_iter()
    .flatten()
    {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stream).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.contains(": playing spotify:") || line.contains("RTKit") {
                    tracing::debug!(target: "soloist", "{line}");
                } else {
                    tracing::info!(target: "soloist", "{line}");
                }
            }
        });
    }
}

/// Connects to Soloist's WebSocket once it's listening.
async fn connect(
    child: &mut Child,
) -> anyhow::Result<
    Result<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        Exit,
    >,
> {
    for _ in 0..150 {
        if let Some(status) = child.try_wait()? {
            return Ok(Err(exit_of(status)));
        }
        if let Ok((ws, _)) =
            tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{WS_PORT}")).await
        {
            return Ok(Ok(ws));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    anyhow::bail!("Soloist didn't open its WebSocket")
}

fn exit_of(status: std::process::ExitStatus) -> Exit {
    if status.code() == Some(EXIT_EXPIRED) {
        Exit::Expired
    } else {
        Exit::Stopped(status.to_string())
    }
}

/// Soloist's binary, downloaded from Spotify when missing, aging or expired. Builds can't be
/// shipped with Ziggy, so each Pi fetches its own.
async fn ensure_binary(bin: &Path, force: bool) -> anyhow::Result<PathBuf> {
    if bin.exists() && !force {
        match build_age_days(bin).await {
            Some(age) if age < REFRESH_AFTER_DAYS => return Ok(bin.to_path_buf()),
            Some(age) => tracing::info!("Soloist's build is {age} days old; fetching a new one"),
            None => tracing::warn!("couldn't read Soloist's build date; fetching a new one"),
        }
    }
    let dir = bin.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let result = download(dir).await;
    match (result, bin.exists()) {
        (Ok(()), _) => {
            tracing::info!(
                "installed Soloist {}",
                version(bin).await.unwrap_or_default()
            );
            Ok(bin.to_path_buf())
        }
        // Offline, say: the old build still works until it expires.
        (Err(e), true) => {
            tracing::warn!("couldn't fetch Soloist ({e:#}); using the one already here");
            Ok(bin.to_path_buf())
        }
        (Err(e), false) => Err(e.context("downloading Soloist")),
    }
}

/// Downloads and unpacks into `dir`, then swaps the binary in, so a power cut can't leave a
/// broken one.
async fn download(dir: &Path) -> anyhow::Result<()> {
    let tmp = dir.join("soloist.download");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "curl -fsSL --max-time 300 '{DOWNLOAD}' | tar xz -C '{}' soloist && sync",
            tmp.display()
        ))
        .status()
        .await?;
    anyhow::ensure!(status.success(), "curl or tar failed ({status})");
    std::fs::rename(tmp.join("soloist"), dir.join("soloist"))?;
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(())
}

async fn version(bin: &Path) -> Option<String> {
    let out = Command::new(bin).arg("--version").output().await.ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// From `soloist 1.3.8.111 build 1791288124 (20261006) …`: days since the build date.
async fn build_age_days(bin: &Path) -> Option<i64> {
    let built = build_date(&version(bin).await?)?;
    Some((chrono::Local::now().date_naive() - built).num_days())
}

fn build_date(version: &str) -> Option<chrono::NaiveDate> {
    version.split(['(', ')']).find_map(|part| {
        (part.len() == 8 && part.bytes().all(|b| b.is_ascii_digit()))
            .then(|| chrono::NaiveDate::parse_from_str(part, "%Y%m%d").ok())
            .flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Track;

    #[test]
    fn the_build_date_comes_from_the_version_line() {
        assert_eq!(
            build_date(
                "soloist 1.3.8.111 build 1791288124 (20261006) (g5c3a2053ac) (linux/aarch64)"
            ),
            chrono::NaiveDate::from_ymd_opt(2026, 10, 6)
        );
        assert_eq!(build_date("soloist 1.3.8"), None);
    }

    #[test]
    fn a_list_starting_at_a_song_is_shuffled_then_handed_to_the_web_api() {
        use crate::library::web_api::StartAt;
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_path_buf()).unwrap();
        cache
            .write(
                ACCOUNT_KEY,
                &Account {
                    id: "31fz".into(),
                    name: "Ellie".into(),
                },
            )
            .unwrap();
        let mut s = Session {
            logged_in: true,
            volume: 60,
            ..Default::default()
        };
        let frames = s.frames_for(
            PlayerCommand::Load {
                context_uri: LIKED_URI.into(),
                start_index: Some(7),
                start_uri: Some("spotify:track:c".into()),
                shuffle: false,
            },
            &cache,
            std::time::Instant::now(),
        );
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0]["command"], "set_shuffle");
        assert_eq!(
            s.load,
            Some(Load {
                uri: "spotify:user:31fz:collection".into(),
                start: Some(StartAt::Uri("spotify:track:c".into())),
            })
        );
    }

    #[test]
    fn the_fallback_skips_quietly_and_hides_the_songs_in_between() {
        use crate::library::web_api::StartAt;
        let t0 = std::time::Instant::now();
        let mut s = Session {
            logged_in: true,
            volume: 60,
            current_uri: Some("spotify:track:a".into()),
            start: Some(StartAt::Uri("spotify:track:c".into())),
            ..Default::default()
        };
        let skip = s.start_from_queue(&["spotify:track:b".into(), "spotify:track:c".into()], t0);
        let names: Vec<&str> = skip
            .iter()
            .map(|f| f["command"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["set_volume", "skip_next", "skip_next"]);
        assert_eq!(skip[0]["volume"], 0);
        let song = |uri: &str| {
            PlayerUpdate::TrackChanged(Track {
                uri: uri.into(),
                ..Default::default()
            })
        };
        assert!(
            !s.forward(&song("spotify:track:b"), t0),
            "passed on the way: hidden"
        );
        assert!(s.outbox.is_empty(), "still quiet");
        assert!(
            s.forward(&song("spotify:track:c"), t0),
            "the tapped song shows"
        );
        assert_eq!(s.outbox[0]["volume"], 60, "and the sound comes back");
        assert!(s.start_from_queue(&[], t0).is_empty(), "only once");
    }

    #[test]
    fn skipping_never_leaves_the_sound_down() {
        let t0 = std::time::Instant::now();
        let mut s = Session {
            skipping: Some((Some("spotify:track:z".into()), 50, t0 + SKIP_TIMEOUT)),
            ..Default::default()
        };
        s.give_up_skipping(t0 + Duration::from_secs(5));
        assert!(s.outbox.is_empty());
        s.give_up_skipping(t0 + SKIP_TIMEOUT);
        assert_eq!(s.outbox[0]["volume"], 50);
        assert!(s.skipping.is_none());
    }

    #[test]
    fn the_clock_counts_on_while_playing_and_stops_when_paused() {
        let t0 = std::time::Instant::now();
        let mut s = Session {
            anchor: Some((10_000, t0, 1.0)),
            ..Default::default()
        };
        assert_eq!(s.position_now(t0 + Duration::from_secs(3)), Some(13_000));
        s.forward(
            &PlayerUpdate::Paused { position_ms: 0 },
            t0 + Duration::from_secs(5),
        );
        assert_eq!(s.position_now(t0 + Duration::from_secs(9)), None, "paused");
        s.forward(
            &PlayerUpdate::Playing { position_ms: 0 },
            t0 + Duration::from_secs(9),
        );
        assert_eq!(s.position_now(t0 + Duration::from_secs(10)), Some(16_000));
    }

    #[test]
    fn echoes_of_a_volume_just_set_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_path_buf()).unwrap();
        let t0 = std::time::Instant::now();
        let mut s = Session {
            logged_in: true,
            ..Default::default()
        };
        s.frames_for(PlayerCommand::SetVolume { percent: 70 }, &cache, t0);
        assert!(!s.forward(
            &PlayerUpdate::Volume { percent: 64 },
            t0 + Duration::from_millis(500)
        ));
        assert!(s.forward(
            &PlayerUpdate::Volume { percent: 40 },
            t0 + Duration::from_secs(3)
        ));
    }

    #[test]
    fn a_list_from_the_top_needs_no_skipping() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_path_buf()).unwrap();
        let mut s = Session {
            logged_in: true,
            ..Default::default()
        };
        s.frames_for(
            PlayerCommand::Load {
                context_uri: "spotify:album:x".into(),
                start_index: Some(0),
                start_uri: None,
                shuffle: true,
            },
            &cache,
            std::time::Instant::now(),
        );
        assert_eq!(s.load.as_ref().map(|l| l.start.clone()), Some(None));
    }
}
