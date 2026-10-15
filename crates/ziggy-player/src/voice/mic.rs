//! The microphone, as 16 kHz mono chunks.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use super::listen::RATE;

/// Turns any rate into 16 kHz by linear interpolation; enough for speech.
struct Resampler {
    step: f64,
    position: f64,
    last: f32,
}

impl Resampler {
    fn new(from: u32) -> Self {
        Self {
            step: f64::from(from) / RATE as f64,
            position: 0.0,
            last: 0.0,
        }
    }

    fn run(&mut self, input: &[f32], out: &mut Vec<f32>) {
        // `position` is relative to the sample before `input` (`last`, at -1).
        while self.position < input.len() as f64 {
            let i = self.position.floor() as isize - 1;
            let frac = (self.position - self.position.floor()) as f32;
            let a = if i < 0 { self.last } else { input[i as usize] };
            let b = input[(i + 1) as usize];
            out.push(a + (b - a) * frac);
            self.position += self.step;
        }
        self.position -= input.len() as f64;
        if let Some(last) = input.last() {
            self.last = *last;
        }
    }
}

/// Lets a repeating message through at most once per `every`. A stream in trouble can report
/// the same error thousands of times a second.
struct Throttle {
    every: Duration,
    last: Option<Instant>,
}

impl Throttle {
    fn new(every: Duration) -> Self {
        Self { every, last: None }
    }

    fn due(&mut self, now: Instant) -> bool {
        let due = self
            .last
            .is_none_or(|t| now.duration_since(t) >= self.every);
        if due {
            self.last = Some(now);
        }
        due
    }
}

/// Opens the microphone (the first input whose name contains `name`, or the default) and sends
/// 16 kHz mono audio. The stream records while it's kept alive. `failed` is set when the
/// stream reports an error, so the caller can open it again.
pub fn open(
    name: Option<&str>,
    chunks: Sender<Vec<f32>>,
    failed: Arc<AtomicBool>,
) -> anyhow::Result<cpal::Stream> {
    let host = cpal::default_host();
    let device = match name {
        Some(name) => host
            .input_devices()?
            .find(|d| {
                d.name()
                    .is_ok_and(|n| n.to_lowercase().contains(&name.to_lowercase()))
            })
            .with_context(|| format!("no microphone named {name:?}"))?,
        None => host.default_input_device().context("no microphone")?,
    };
    let config = device.default_input_config()?;
    tracing::info!(
        "microphone: {} at {} Hz, {} channel(s)",
        device.name().unwrap_or_default(),
        config.sample_rate().0,
        config.channels()
    );
    let channels = usize::from(config.channels());
    let mut resampler = Resampler::new(config.sample_rate().0);
    let mut mono = Vec::new();
    let mut handle = move |samples: &mut dyn Iterator<Item = f32>| {
        mono.clear();
        let all: Vec<f32> = samples.collect();
        mono.extend(
            all.chunks(channels)
                .map(|c| c.iter().sum::<f32>() / c.len() as f32),
        );
        let mut out = Vec::with_capacity(mono.len());
        resampler.run(&mono, &mut out);
        let _ = chunks.send(out);
    };
    let throttle = Mutex::new(Throttle::new(Duration::from_secs(10)));
    let error = move |e: cpal::StreamError| {
        failed.store(true, Ordering::Relaxed);
        if throttle.lock().is_ok_and(|mut t| t.due(Instant::now())) {
            tracing::warn!("microphone: {e}");
        }
    };
    let stream_config = config.config();
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &stream_config,
            move |data: &[f32], _| handle(&mut data.iter().copied()),
            error,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            &stream_config,
            move |data: &[i16], _| handle(&mut data.iter().map(|s| *s as f32 / 32768.0)),
            error,
            None,
        )?,
        cpal::SampleFormat::I32 => device.build_input_stream(
            &stream_config,
            move |data: &[i32], _| handle(&mut data.iter().map(|s| *s as f32 / 2_147_483_648.0)),
            error,
            None,
        )?,
        format => anyhow::bail!("unsupported microphone format {format:?}"),
    };
    stream.play()?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repeating_error_is_logged_once_per_interval() {
        let mut t = Throttle::new(Duration::from_secs(10));
        let start = Instant::now();
        assert!(t.due(start));
        assert!(!t.due(start + Duration::from_millis(1)));
        assert!(!t.due(start + Duration::from_secs(9)));
        assert!(t.due(start + Duration::from_secs(10)));
        assert!(!t.due(start + Duration::from_secs(11)));
    }

    #[test]
    fn resampling_keeps_length_and_shape_across_chunks() {
        let mut r = Resampler::new(48_000);
        let input: Vec<f32> = (0..48_000)
            .map(|i| (i as f32 / 48_000.0 * 440.0 * std::f32::consts::TAU).sin())
            .collect();
        let mut out = Vec::new();
        for chunk in input.chunks(1_000) {
            r.run(chunk, &mut out);
        }
        assert!((15_990..=16_010).contains(&out.len()), "{}", out.len());
        // Every third input sample, roughly.
        assert!((out[300] - input[900]).abs() < 0.05);
        let mut same = Resampler::new(16_000);
        let mut copy = Vec::new();
        same.run(&[0.1, 0.2, 0.3], &mut copy);
        same.run(&[0.4], &mut copy);
        assert_eq!(copy.len(), 4);
    }
}
