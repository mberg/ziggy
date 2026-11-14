//! The local player: plays music files from the card, answering the same commands as
//! Spotify's player. Decoding runs on its own thread; writing to the output paces it.

pub mod output;
pub mod queue;

use std::fs::File;
use std::time::{Duration, Instant};

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions};
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::{Time, TimeBase};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender, error::TryRecvError};

use crate::app::{Input, PlayerCommand, PlayerUpdate};
use crate::music::SharedIndex;
use crate::music::index::{ALBUM_PREFIX, ARTIST_PREFIX, TRACK_PREFIX};
use queue::{Queue, Rng};

/// How often the position is reported while playing.
const POSITION_EVERY: Duration = Duration::from_secs(1);
/// Previous goes back to the start of the song after this much of it.
const RESTART_AFTER_MS: u32 = 3_000;

pub struct LocalSettings {
    /// A Bluetooth speaker's PipeWire node; None plays to the headphone jack.
    pub output: Option<String>,
    pub initial_volume: u8,
}

pub fn spawn(
    index: SharedIndex,
    settings: LocalSettings,
    updates: UnboundedSender<Input>,
) -> UnboundedSender<PlayerCommand> {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::Builder::new()
        .name("ziggy-local-player".into())
        .spawn(move || {
            Player {
                index,
                output: settings.output,
                volume: settings.initial_volume.min(100),
                updates,
                queue: Queue::default(),
                rng: Rng::seeded(),
                track: None,
                playing: false,
                position_ms: 0,
                reported: Instant::now(),
            }
            .run(rx)
        })
        .expect("starting the local player");
    tx
}

/// The song being played.
struct Track {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    id: u32,
    time_base: Option<TimeBase>,
    sink: Option<Box<dyn output::Sink>>,
    /// The sink's rate and channels; a song that changes them midway gets a new sink.
    sink_spec: (u32, u16),
}

struct Player {
    index: SharedIndex,
    output: Option<String>,
    volume: u8,
    updates: UnboundedSender<Input>,
    queue: Queue,
    rng: Rng,
    track: Option<Track>,
    playing: bool,
    position_ms: u32,
    reported: Instant,
}

fn open_file(path: &std::path::Path) -> anyhow::Result<Track> {
    let file = File::open(path)?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe().format(
        &hint,
        stream,
        &FormatOptions {
            enable_gapless: true,
            ..Default::default()
        },
        &MetadataOptions::default(),
    )?;
    let format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| anyhow::anyhow!("no audio in the file"))?;
    let decoder =
        symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())?;
    Ok(Track {
        id: track.id,
        time_base: track.codec_params.time_base,
        format,
        decoder,
        sink: None,
        sink_spec: (0, 0),
    })
}

impl Player {
    fn send(&self, update: PlayerUpdate) {
        let _ = self.updates.send(Input::Player(update));
    }

    fn run(mut self, mut rx: UnboundedReceiver<PlayerCommand>) {
        self.send(PlayerUpdate::Connected);
        loop {
            let command = if self.playing && self.track.is_some() {
                match rx.try_recv() {
                    Ok(command) => Some(command),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => return,
                }
            } else {
                match rx.blocking_recv() {
                    Some(command) => Some(command),
                    None => return,
                }
            };
            match command {
                Some(command) => self.handle(command),
                None => self.play_some(),
            }
        }
    }

    fn handle(&mut self, command: PlayerCommand) {
        match command {
            PlayerCommand::Load {
                context_uri,
                start_index,
                start_uri,
                shuffle,
            } => self.load(&context_uri, start_index, start_uri.as_deref(), shuffle),
            PlayerCommand::Play => {
                if self.track.is_some() {
                    self.playing = true;
                    self.send(PlayerUpdate::Playing {
                        position_ms: self.position_ms,
                    });
                } else if self.queue.current().is_some() {
                    self.start_current();
                }
            }
            PlayerCommand::Pause => {
                self.playing = false;
                // Dropping the output stops the sound now and lets a Bluetooth speaker sleep.
                if let Some(t) = &mut self.track {
                    t.sink = None;
                }
                self.send(PlayerUpdate::Paused {
                    position_ms: self.position_ms,
                });
            }
            PlayerCommand::Next => self.advance(false),
            PlayerCommand::Previous => {
                if self.position_ms > RESTART_AFTER_MS || self.queue.previous().is_none() {
                    self.seek(0);
                } else {
                    self.start_current();
                }
            }
            PlayerCommand::Seek { position_ms } => self.seek(position_ms),
            PlayerCommand::SetVolume { percent } => {
                self.volume = percent.min(100);
                self.send(PlayerUpdate::Volume {
                    percent: self.volume,
                });
            }
            PlayerCommand::SetShuffle(shuffle) => {
                self.queue.set_shuffle(shuffle, &mut self.rng);
                self.send(PlayerUpdate::Shuffle(shuffle));
            }
            PlayerCommand::SetRepeat(repeat) => {
                self.queue.repeat = repeat;
                self.send(PlayerUpdate::Repeat(repeat));
            }
        }
    }

    /// The songs a URI stands for: an album's, an artist's, or one song in its album.
    fn load(
        &mut self,
        uri: &str,
        start_index: Option<u32>,
        start_uri: Option<&str>,
        shuffle: bool,
    ) {
        let (ids, start) = {
            let index = self.index.read().unwrap();
            let ids: Vec<String> = if let Some(id) = uri.strip_prefix(TRACK_PREFIX) {
                index
                    .tracks
                    .get(id)
                    .and_then(|t| index.album(&t.album_id))
                    .map(|a| a.tracks.clone())
                    .unwrap_or_default()
            } else if uri.starts_with(ALBUM_PREFIX) || uri.starts_with(ARTIST_PREFIX) {
                index
                    .tracks_of(uri)
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|t| t.uri.strip_prefix(TRACK_PREFIX).map(str::to_string))
                    .collect()
            } else {
                Vec::new()
            };
            let wanted = start_uri
                .or(uri.starts_with(TRACK_PREFIX).then_some(uri))
                .and_then(|u| u.strip_prefix(TRACK_PREFIX));
            let start = wanted
                .and_then(|id| ids.iter().position(|t| t == id))
                .or(start_index.map(|i| i as usize));
            (ids, start)
        };
        self.track = None;
        if ids.is_empty() {
            tracing::warn!("nothing on the card for {uri}");
            self.playing = false;
            self.send(PlayerUpdate::Stopped);
            return;
        }
        self.queue.load(ids, start, shuffle, &mut self.rng);
        self.send(PlayerUpdate::Shuffle(self.queue.shuffle));
        self.send(PlayerUpdate::Repeat(self.queue.repeat));
        self.start_current();
    }

    /// Opens the current song and plays it; a song that won't open is skipped.
    fn start_current(&mut self) {
        let mut tries = self.queue.ids.len();
        loop {
            let Some(id) = self.queue.current().map(str::to_string) else {
                return self.stop();
            };
            let (file, track) = {
                let index = self.index.read().unwrap();
                (index.file(&id), index.track(&id))
            };
            match file.zip(track) {
                Some((file, track)) => match open_file(&file) {
                    Ok(opened) => {
                        self.track = Some(opened);
                        self.playing = true;
                        self.position_ms = 0;
                        self.reported = Instant::now();
                        self.send(PlayerUpdate::Loading);
                        self.send(PlayerUpdate::TrackChanged(track));
                        self.send(PlayerUpdate::Playing { position_ms: 0 });
                        return;
                    }
                    Err(e) => tracing::warn!("can't play {}: {e}", file.display()),
                },
                None => tracing::warn!("song {id} is no longer on the card"),
            }
            self.send(PlayerUpdate::Unavailable);
            tries = tries.saturating_sub(1);
            if tries == 0 || self.queue.next(false).is_none() {
                return self.stop();
            }
        }
    }

    fn stop(&mut self) {
        self.track = None;
        self.playing = false;
        self.position_ms = 0;
        self.send(PlayerUpdate::Stopped);
    }

    fn advance(&mut self, finished: bool) {
        if self.queue.next(finished).is_some() {
            self.start_current();
        } else {
            self.stop();
        }
    }

    fn seek(&mut self, position_ms: u32) {
        let Some(t) = &mut self.track else { return };
        let to = SeekTo::Time {
            time: Time::new(
                (position_ms / 1000) as u64,
                (position_ms % 1000) as f64 / 1000.0,
            ),
            track_id: Some(t.id),
        };
        match t.format.seek(SeekMode::Coarse, to) {
            Ok(_) => {
                t.decoder.reset();
                self.position_ms = position_ms;
            }
            Err(e) => tracing::warn!("seeking failed: {e}"),
        }
        self.send(PlayerUpdate::Position {
            position_ms: self.position_ms,
        });
    }

    /// Decodes and plays one packet of the current song.
    fn play_some(&mut self) {
        let Some(t) = &mut self.track else { return };
        let packet = match t.format.next_packet() {
            Ok(packet) => packet,
            // The end of the file, or a file that broke partway: either way, the next song.
            Err(_) => return self.advance(true),
        };
        if packet.track_id() != t.id {
            return;
        }
        let decoded = match t.decoder.decode(&packet) {
            Ok(decoded) => decoded,
            // A damaged packet; carry on with the next.
            Err(symphonia::core::errors::Error::DecodeError(_)) => return,
            Err(e) => {
                tracing::warn!("decoding failed: {e}");
                return self.advance(true);
            }
        };
        let spec = *decoded.spec();
        let mut samples = SampleBuffer::<i16>::new(decoded.capacity() as u64, spec);
        samples.copy_interleaved_ref(decoded);
        // A curve, so the low half of the slider is still usable.
        let gain = (self.volume as f32 / 100.0).powi(2);
        let scaled: Vec<i16> = samples
            .samples()
            .iter()
            .map(|&s| (s as f32 * gain) as i16)
            .collect();
        let wanted = (spec.rate, spec.channels.count() as u16);
        if t.sink.is_none() || t.sink_spec != wanted {
            match output::open(wanted.0, wanted.1, self.output.as_deref()) {
                Ok(sink) => {
                    t.sink = Some(sink);
                    t.sink_spec = wanted;
                }
                Err(e) => {
                    tracing::error!("can't open the sound output: {e:#}");
                    self.playing = false;
                    self.send(PlayerUpdate::Paused {
                        position_ms: self.position_ms,
                    });
                    return;
                }
            }
        }
        if let Some(sink) = &mut t.sink
            && let Err(e) = sink.write(&scaled)
        {
            tracing::warn!("{e:#}");
            t.sink = None;
        }
        if let Some(tb) = t.time_base {
            let time = tb.calc_time(packet.ts());
            self.position_ms = (time.seconds * 1000 + (time.frac * 1000.0) as u64) as u32;
        }
        if self.reported.elapsed() >= POSITION_EVERY {
            self.reported = Instant::now();
            self.send(PlayerUpdate::Position {
                position_ms: self.position_ms,
            });
        }
    }
}
