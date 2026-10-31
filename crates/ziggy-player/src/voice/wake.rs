//! The wake word, spotted on the device with a sherpa-onnx keyword model.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use sherpa_onnx::{
    KeywordSpotter, KeywordSpotterConfig, OnlineModelConfig, OnlineStream,
    OnlineTransducerModelConfig,
};

use super::bpe::Bpe;
use super::listen::{RATE, WakeWord};

/// In testing with four voices it woke 16 times in 24 and never on other words; "hey ziggy"
/// woke more readily but also on "hey music". The phrase can be changed in the config.
pub const DEFAULT_PHRASE: &str = "ziggy";
pub const DEFAULT_THRESHOLD: f32 = 0.25;

pub struct SherpaWake {
    spotter: KeywordSpotter,
    stream: OnlineStream,
}

/// The model file whose name starts with `part`, preferring the smaller int8 version.
fn model_file(dir: &Path, part: &str) -> anyhow::Result<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(part) && n.ends_with(".onnx"))
        })
        .collect();
    found.sort_by_key(|p| !p.to_string_lossy().contains(".int8."));
    found
        .into_iter()
        .next()
        .with_context(|| format!("no {part}*.onnx in {}", dir.display()))
}

impl SherpaWake {
    /// Loads a sherpa-onnx keyword model (e.g. sherpa-onnx-kws-zipformer-gigaspeech-3.3M).
    pub fn load(dir: &Path, phrase: &str, threshold: f32) -> anyhow::Result<Self> {
        let bpe = std::fs::read(dir.join("bpe.model"))
            .ok()
            .and_then(|b| Bpe::from_model(&b))
            .with_context(|| format!("reading {}/bpe.model", dir.display()))?;
        let Some(keywords) = bpe.keyword_line(phrase, "wake") else {
            bail!("the wake phrase {phrase:?} can't be spelled with this model's English letters");
        };
        let path = |p: PathBuf| Some(p.to_string_lossy().into_owned());
        let config = KeywordSpotterConfig {
            model_config: OnlineModelConfig {
                transducer: OnlineTransducerModelConfig {
                    encoder: path(model_file(dir, "encoder")?),
                    decoder: path(model_file(dir, "decoder")?),
                    joiner: path(model_file(dir, "joiner")?),
                },
                tokens: path(dir.join("tokens.txt")),
                num_threads: 1,
                ..Default::default()
            },
            keywords_threshold: threshold,
            keywords_buf: Some(keywords),
            ..Default::default()
        };
        let spotter = KeywordSpotter::create(&config).context("loading the wake word model")?;
        let stream = spotter.create_stream();
        Ok(Self { spotter, stream })
    }
}

impl WakeWord for SherpaWake {
    fn feed(&mut self, samples: &[f32]) -> bool {
        self.stream.accept_waveform(RATE as i32, samples);
        let mut heard = false;
        while self.spotter.is_ready(&self.stream) {
            self.spotter.decode(&self.stream);
            if let Some(result) = self.spotter.get_result(&self.stream)
                && !result.keyword.is_empty()
            {
                heard = true;
                self.spotter.reset(&self.stream);
            }
        }
        heard
    }

    fn reset(&mut self) {
        self.stream = self.spotter.create_stream();
    }
}

#[cfg(test)]
mod tests_support {
    use super::*;

    /// Speaks `text` with a Mac voice as 16 kHz audio, padded with quiet.
    pub fn say(voice: &str, text: &str) -> Vec<f32> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("say.wav");
        let ok = std::process::Command::new("say")
            .args([
                "-v",
                voice,
                "-o",
                path.to_str().unwrap(),
                "--data-format=LEI16@16000",
                text,
            ])
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let bytes = std::fs::read(&path).unwrap();
        let mut audio = vec![0.0; RATE / 2];
        audio.extend(
            bytes[44..]
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0),
        );
        audio.extend(vec![0.0; RATE]);
        // A real microphone always hisses a little; perfect silence confuses the model.
        let mut state = 12_345u32;
        for s in &mut audio {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            *s += ((state >> 16) as f32 / 32_768.0 - 1.0) * 0.002;
        }
        audio
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::say;
    use super::*;
    use crate::voice::listen::{Heard, Listener};

    /// Adds rumbly noise about 10 dB below the speech, roughly like music in the room.
    fn noisy(audio: &[f32], seed: u32) -> Vec<f32> {
        let loud = (audio.iter().map(|s| s * s).sum::<f32>() / audio.len() as f32).sqrt();
        let mut state = seed.wrapping_mul(2_654_435_761) | 1;
        let mut brown = 0.0f32;
        audio
            .iter()
            .map(|s| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                let white = (state as f32 / u32::MAX as f32) * 2.0 - 1.0;
                brown = 0.97 * brown + white;
                s + brown * loud * 0.316 / 3.4
            })
            .collect()
    }

    /// With the keyword model: `VOICE_MODEL_DIR=… cargo test -- --ignored live_wake --nocapture`.
    #[test]
    #[ignore]
    fn live_wake() {
        let Ok(dir) = std::env::var("VOICE_MODEL_DIR") else {
            return;
        };
        let voices = ["Samantha", "Daniel", "Karen", "Fred"];
        let phrase = std::env::var("WAKE_PHRASE").unwrap_or_else(|_| DEFAULT_PHRASE.to_string());
        let should_wake = [
            phrase.clone(),
            format!("{phrase}, play graceland"),
            format!("{phrase} next song"),
        ];
        let should_not = [
            "hey music",
            "hello music",
            "hello there, how are you",
            "hey jude",
            "okay google",
            "skip this song",
            "give me a tip",
            "pick it up",
            "pippa is here",
            "hey mom",
            "the ziggy in the elevator",
            "what's the weather like today",
            "hey max, can you pass the salt",
        ];
        for threshold in [0.25] {
            let (mut woke, mut missed, mut false_wakes) = (0, 0, 0);
            for (i, voice) in voices.iter().enumerate() {
                for (said, want) in should_wake
                    .iter()
                    .map(|p| (p.as_str(), true))
                    .chain(should_not.iter().map(|p| (*p, false)))
                {
                    let clean = say(voice, said);
                    for (kind, audio) in [
                        ("clean", clean.clone()),
                        ("noise", noisy(&clean, i as u32 + 1)),
                    ] {
                        let mut listener = Listener::new(
                            SherpaWake::load(Path::new(&dir), &phrase, threshold).unwrap(),
                        );
                        let heard: Vec<Heard> =
                            audio.chunks(1_600).flat_map(|c| listener.push(c)).collect();
                        let got = heard.first() == Some(&Heard::Woke);
                        match (want, got) {
                            (true, true) => woke += 1,
                            (true, false) => {
                                missed += 1;
                                println!("  missed: {voice} {kind} {said:?}");
                            }
                            (false, true) => {
                                false_wakes += 1;
                                println!("  false wake: {voice} {kind} {said:?}");
                            }
                            (false, false) => {}
                        }
                        if want && got && std::env::var("SHOW_EVENTS").is_ok() {
                            println!(
                                "  {voice} {kind} {said:?}: {heard:?}",
                                heard = heard
                                    .iter()
                                    .map(|h| match h {
                                        Heard::Woke => "woke".to_string(),
                                        Heard::Request(a) =>
                                            format!("request {:.1}s", a.len() as f32 / RATE as f32),
                                        Heard::Nothing => "nothing".to_string(),
                                    })
                                    .collect::<Vec<_>>()
                            );
                        }
                        // After the wake word, the rest of the request is recorded.
                        if want && got && said.contains(',') {
                            assert!(
                                heard.iter().any(|h| matches!(h, Heard::Request(_))),
                                "{voice} {kind}: no request after the wake word: {heard:?}"
                            );
                        }
                    }
                }
            }
            println!(
                "threshold {threshold}: woke {woke}, missed {missed}, false wakes {false_wakes}"
            );
        }
    }
}

#[cfg(test)]
mod recordings {
    use super::*;
    use crate::voice::listen::{Heard, Listener};

    /// Runs WAVs saved with VOICE_RECORD_DIR through the wake word and listener:
    /// `VOICE_MODEL_DIR=… VOICE_RECORDINGS=… cargo test -- --ignored live_recordings --nocapture`.
    #[test]
    #[ignore]
    fn live_recordings() {
        let (Ok(model), Ok(dir)) = (
            std::env::var("VOICE_MODEL_DIR"),
            std::env::var("VOICE_RECORDINGS"),
        ) else {
            return;
        };
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "wav"))
            .collect();
        files.sort();
        // All files as one stream, as the microphone delivered them.
        let audio: Vec<f32> = files
            .iter()
            .flat_map(|f| {
                let bytes = std::fs::read(f).unwrap();
                bytes[44..]
                    .chunks_exact(2)
                    .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
                    .collect::<Vec<_>>()
            })
            .collect();
        println!("{:.0} s of recordings", audio.len() as f32 / RATE as f32);
        let thresholds: Vec<f32> = std::env::var("THRESHOLDS")
            .unwrap_or_else(|_| "0.1,0.15,0.25".into())
            .split(',')
            .map(|t| t.parse().unwrap())
            .collect();
        for threshold in thresholds {
            let wake = SherpaWake::load(Path::new(&model), DEFAULT_PHRASE, threshold).unwrap();
            let mut listener = Listener::new(wake);
            let mut at = 0usize;
            let mut events = Vec::new();
            for chunk in audio.chunks(1_600) {
                at += chunk.len();
                for heard in listener.push(chunk) {
                    let secs = at as f32 / RATE as f32;
                    events.push(match heard {
                        Heard::Woke => format!("woke at {secs:.1}s"),
                        Heard::Request(a) => {
                            format!("request of {:.1}s", a.len() as f32 / RATE as f32)
                        }
                        Heard::Nothing => "nothing".into(),
                    });
                }
            }
            println!("threshold {threshold}: {}", events.join(", "));
        }
    }
}

#[cfg(test)]
mod end_to_end {
    use super::tests_support::say;
    use super::*;
    use crate::app::{VoiceCommand, VoiceContext, VoiceItem};
    use crate::voice::gemini::{Backend, Gemini};
    use crate::voice::listen::{Heard, Listener};

    /// Spoken requests through the wake word, the listener and Gemini on Vertex AI:
    /// `VOICE_MODEL_DIR=… VERTEX_KEY_FILE=… cargo test -- --ignored live_end_to_end --nocapture`.
    #[tokio::test]
    #[ignore]
    async fn live_end_to_end() {
        let (Ok(model), Ok(key)) = (
            std::env::var("VOICE_MODEL_DIR"),
            std::env::var("VERTEX_KEY_FILE"),
        ) else {
            return;
        };
        let gemini = Gemini::new(
            Backend::Vertex {
                account: Box::new(
                    crate::voice::google_auth::ServiceAccount::load(Path::new(&key)).unwrap(),
                ),
                location: "global".into(),
            },
            None,
        );
        let context = VoiceContext {
            now_playing: Some("Graceland by Paul Simon".into()),
            playing: true,
            volume: 60,
            items: vec![VoiceItem {
                uri: "spotify:playlist:road".into(),
                kind: "playlist",
                name: "Road Trip".into(),
                by: "sam".into(),
            }],
            ..VoiceContext::default()
        };
        let cases = [
            ("ziggy next song", Some(VoiceCommand::Next)),
            ("ziggy stop", Some(VoiceCommand::Pause)),
            (
                "ziggy, play road trip",
                Some(VoiceCommand::Play("spotify:playlist:road".into())),
            ),
        ];
        let (mut right, mut total) = (0, 0);
        for voice in ["Samantha", "Daniel", "Karen"] {
            for (said, want) in &cases {
                total += 1;
                let mut listener = Listener::new(
                    SherpaWake::load(Path::new(&model), "ziggy", DEFAULT_THRESHOLD).unwrap(),
                );
                let mut audio = say(voice, said);
                audio.extend(vec![0.0; RATE * 3]);
                let heard: Vec<Heard> =
                    audio.chunks(1_600).flat_map(|c| listener.push(c)).collect();
                let Some(audio) = heard.iter().find_map(|h| match h {
                    Heard::Request(a) => Some(a.clone()),
                    _ => None,
                }) else {
                    println!("{voice:9} {said:24} -> wake word missed ({heard:?})");
                    continue;
                };
                let got = gemini.ask(&context, &audio).await.unwrap();
                let ok = &got == want;
                right += usize::from(ok);
                println!(
                    "{voice:9} {said:24} -> {got:?} ({:.1} s of audio){}",
                    audio.len() as f32 / RATE as f32,
                    if ok { "" } else { "  WRONG" }
                );
            }
        }
        println!("{right} of {total} right");
    }
}
