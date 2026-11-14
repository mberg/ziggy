//! Where the local player's sound goes: `pw-cat` into PipeWire on the Pi, as AirPlay does,
//! so the headphone jack and Bluetooth speakers both work; the computer's own speakers
//! elsewhere, for trying the player on a Mac.

use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;

/// Takes 16-bit samples, channels interleaved. Writing waits while the output is full, which
/// keeps decoding in step with playing.
pub trait Sink {
    fn write(&mut self, samples: &[i16]) -> anyhow::Result<()>;
}

/// The PipeWire node to play to: a Bluetooth speaker's, or None for the headphone jack.
pub fn open(rate: u32, channels: u16, target: Option<&str>) -> anyhow::Result<Box<dyn Sink>> {
    if cfg!(target_os = "linux") && which("pw-cat").is_some() {
        Ok(Box::new(PwCat::open(rate, channels, target)?))
    } else {
        Ok(Box::new(Speakers::open(rate, channels)?))
    }
}

fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|p| p.is_file())
}

fn runtime_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(|| crate::player::soloist::own_uid().map(|uid| format!("/run/user/{uid}").into()))
}

/// The headphone jack's node, found the way Soloist's is.
fn jack_node(dir: &Option<PathBuf>) -> Option<String> {
    let mut cmd = Command::new("pw-dump");
    if let Some(dir) = dir {
        cmd.env("XDG_RUNTIME_DIR", dir);
    }
    let out = cmd.stderr(Stdio::null()).output().ok()?;
    crate::player::soloist::protocol::jack_node(&String::from_utf8_lossy(&out.stdout))
}

struct PwCat {
    child: Child,
    stdin: ChildStdin,
    bytes: Vec<u8>,
}

impl PwCat {
    fn open(rate: u32, channels: u16, target: Option<&str>) -> anyhow::Result<PwCat> {
        let dir = runtime_dir();
        let target = target.map(str::to_string).or_else(|| jack_node(&dir));
        let mut cmd = Command::new("pw-cat");
        if let Some(dir) = &dir {
            cmd.env("XDG_RUNTIME_DIR", dir).env(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}/bus", dir.display()),
            );
        }
        cmd.args(["--playback", "--raw", "--format", "s16"])
            .args(["--rate", &rate.to_string()])
            .args(["--channels", &channels.to_string()])
            .args(["-P", "node.name=ziggy-music,application.name=My music"]);
        if let Some(target) = &target {
            cmd.args(["--target", target]);
        }
        let mut child = cmd
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("starting pw-cat")?;
        let stdin = child.stdin.take().context("no pw-cat input")?;
        Ok(PwCat {
            child,
            stdin,
            bytes: Vec::new(),
        })
    }
}

impl Sink for PwCat {
    fn write(&mut self, samples: &[i16]) -> anyhow::Result<()> {
        self.bytes.clear();
        self.bytes
            .extend(samples.iter().flat_map(|s| s.to_le_bytes()));
        self.stdin.write_all(&self.bytes).context("pw-cat stopped")
    }
}

impl Drop for PwCat {
    /// Stops the sound at once, rather than playing out what's buffered.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The computer's default output, through cpal. Converts to the output's rate and channels.
struct Speakers {
    _stream: cpal::Stream,
    buffer: Arc<Mutex<VecDeque<f32>>>,
    /// Samples (all channels) the buffer may hold before writing waits: a quarter second.
    limit: usize,
    from_rate: u32,
    from_channels: usize,
    to_rate: u32,
    to_channels: usize,
    /// Where between input frames the next output frame falls.
    phase: f64,
    last: Vec<f32>,
}

impl Speakers {
    fn open(rate: u32, channels: u16) -> anyhow::Result<Speakers> {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
        let device = cpal::default_host()
            .default_output_device()
            .context("no sound output")?;
        let config = device.default_output_config()?;
        let to_rate = config.sample_rate().0;
        let to_channels = config.channels() as usize;
        let buffer = Arc::new(Mutex::new(VecDeque::<f32>::new()));
        let feed = buffer.clone();
        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => device.build_output_stream(
                &config.into(),
                move |out: &mut [f32], _| {
                    let mut buffer = feed.lock().unwrap();
                    for s in out.iter_mut() {
                        *s = buffer.pop_front().unwrap_or(0.0);
                    }
                },
                |e| tracing::warn!("sound output: {e}"),
                None,
            )?,
            cpal::SampleFormat::I16 => device.build_output_stream(
                &config.into(),
                move |out: &mut [i16], _| {
                    let mut buffer = feed.lock().unwrap();
                    for s in out.iter_mut() {
                        *s = (buffer.pop_front().unwrap_or(0.0) * i16::MAX as f32) as i16;
                    }
                },
                |e| tracing::warn!("sound output: {e}"),
                None,
            )?,
            other => anyhow::bail!("unsupported output format {other:?}"),
        };
        stream.play()?;
        Ok(Speakers {
            _stream: stream,
            buffer,
            limit: (to_rate as usize * to_channels) / 4,
            from_rate: rate,
            from_channels: channels.max(1) as usize,
            to_rate,
            to_channels,
            phase: 0.0,
            last: vec![0.0; channels.max(1) as usize],
        })
    }
}

impl Sink for Speakers {
    fn write(&mut self, samples: &[i16]) -> anyhow::Result<()> {
        let frames: Vec<&[i16]> = samples.chunks(self.from_channels).collect();
        let step = self.from_rate as f64 / self.to_rate as f64;
        let mut out = Vec::with_capacity(
            (frames.len() as f64 / step) as usize * self.to_channels + self.to_channels,
        );
        // Linear interpolation between the previous frame and the next.
        let mut prev = self.last.clone();
        for frame in &frames {
            let cur: Vec<f32> = frame.iter().map(|&s| s as f32 / 32768.0).collect();
            while self.phase < 1.0 {
                let t = self.phase as f32;
                for ch in 0..self.to_channels {
                    let src = ch.min(self.from_channels - 1);
                    let v = if ch < self.from_channels || self.from_channels == 1 {
                        prev[src] + (cur[src] - prev[src]) * t
                    } else {
                        0.0
                    };
                    out.push(v);
                }
                self.phase += step;
            }
            self.phase -= 1.0;
            prev = cur;
        }
        self.last = prev;
        loop {
            {
                let mut buffer = self.buffer.lock().unwrap();
                if buffer.len() < self.limit {
                    buffer.extend(out.drain(..));
                    return Ok(());
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
