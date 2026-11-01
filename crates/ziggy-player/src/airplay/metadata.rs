//! shairport-sync's metadata, as it sends it over UDP: each packet is a 4-byte type ("core" for
//! the song's details, "ssnc" for shairport-sync's own), a 4-byte code, then the data. A cover
//! too big for one packet comes in numbered "chnk" pieces.

use crate::app::AirPlayUpdate;

/// AirPlay counts audio in frames, 44,100 a second.
const FRAMES_PER_SEC: u64 = 44_100;
/// AirPlay's volume runs from -30 (quietest) to 0 (loudest); -144 is muted.
const QUIETEST: f32 = -30.0;
const MUTED: f32 = -144.0;

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Update(AirPlayUpdate),
    /// The cover's image file; empty when the song has none.
    Cover(Vec<u8>),
}

#[derive(Default)]
pub struct Reader {
    /// The details of the song being described, between "mdst" and "mden".
    title: String,
    artist: String,
    album: String,
    pieces: Option<Pieces>,
}

/// A packet arriving in pieces.
struct Pieces {
    kind: [u8; 8],
    parts: Vec<Option<Vec<u8>>>,
}

impl Reader {
    pub fn packet(&mut self, packet: &[u8]) -> Option<Event> {
        if packet.len() < 8 {
            return None;
        }
        let (kind, data) = packet.split_at(8);
        let update = |u| Some(Event::Update(u));
        match kind {
            b"ssncchnk" => self.piece(data),
            b"ssncpbeg" => update(AirPlayUpdate::Started),
            b"ssncprsm" => update(AirPlayUpdate::Playing),
            // A flush is a pause, or a skip or seek just before playing resumes.
            b"ssncpfls" | b"ssncpend" => update(AirPlayUpdate::Paused),
            // Some seconds after playback ends, the phone has gone.
            b"ssncaend" => update(AirPlayUpdate::Ended),
            b"ssncmdst" => {
                self.title.clear();
                self.artist.clear();
                self.album.clear();
                None
            }
            b"coreminm" => {
                self.title = text(data);
                None
            }
            b"coreasar" => {
                self.artist = text(data);
                None
            }
            b"coreasal" => {
                self.album = text(data);
                None
            }
            b"ssncmden" => update(AirPlayUpdate::Song {
                title: self.title.clone(),
                artist: self.artist.clone(),
                album: self.album.clone(),
            }),
            b"ssncPICT" => Some(Event::Cover(data.to_vec())),
            b"ssncprgr" => progress(&text(data)).map(Event::Update),
            b"ssncpvol" => {
                volume(&text(data)).map(|percent| Event::Update(AirPlayUpdate::Volume { percent }))
            }
            _ => None,
        }
    }

    /// A piece: its number, how many there are, then the packet's type, code and part of its data.
    fn piece(&mut self, data: &[u8]) -> Option<Event> {
        if data.len() < 16 {
            return None;
        }
        let index = u32::from_be_bytes(data[0..4].try_into().ok()?) as usize;
        let total = u32::from_be_bytes(data[4..8].try_into().ok()?) as usize;
        let kind: [u8; 8] = data[8..16].try_into().ok()?;
        if total == 0 || index >= total || total > 1000 {
            return None;
        }
        let pieces = match &mut self.pieces {
            Some(p) if p.kind == kind && p.parts.len() == total => p,
            _ => self.pieces.insert(Pieces {
                kind,
                parts: vec![None; total],
            }),
        };
        pieces.parts[index] = Some(data[16..].to_vec());
        if pieces.parts.iter().any(Option::is_none) {
            return None;
        }
        let pieces = self.pieces.take()?;
        let mut whole = pieces.kind.to_vec();
        whole.extend(pieces.parts.into_iter().flatten().flatten());
        self.packet(&whole)
    }
}

fn text(data: &[u8]) -> String {
    String::from_utf8_lossy(data).trim().to_string()
}

/// "start/now/end", in frames: where the song is and how long it runs.
fn progress(s: &str) -> Option<AirPlayUpdate> {
    let mut parts = s.split('/').map(|p| p.trim().parse::<u32>().ok());
    let (start, now, end) = (parts.next()??, parts.next()??, parts.next()??);
    let ms = |frames: u32| (u64::from(frames) * 1000 / FRAMES_PER_SEC) as u32;
    Some(AirPlayUpdate::Progress {
        position_ms: ms(now.wrapping_sub(start)),
        duration_ms: ms(end.wrapping_sub(start)),
    })
}

/// "airplay volume,dB,lowest,highest": the phone's volume as 0-100.
fn volume(s: &str) -> Option<u8> {
    let airplay: f32 = s.split(',').next()?.trim().parse().ok()?;
    Some(if airplay <= QUIETEST {
        0
    } else {
        ((airplay - QUIETEST) / -QUIETEST * 100.0)
            .round()
            .clamp(0.0, 100.0) as u8
    })
}

/// A volume of 0-100 as AirPlay's -30 to 0, or muted at 0.
pub fn airplay_volume(percent: u8) -> f32 {
    if percent == 0 {
        MUTED
    } else {
        QUIETEST + f32::from(percent.min(100)) / 100.0 * -QUIETEST
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(kind: &str, data: &[u8]) -> Vec<u8> {
        let mut p = kind.as_bytes().to_vec();
        p.extend(data);
        p
    }

    fn read(reader: &mut Reader, kind: &str, data: &[u8]) -> Option<Event> {
        reader.packet(&packet(kind, data))
    }

    #[test]
    fn a_song_is_described_between_start_and_end_markers() {
        let mut r = Reader::default();
        assert_eq!(read(&mut r, "ssncmdst", b"1163602836"), None);
        assert_eq!(read(&mut r, "coreasal", b"November 1, 2026"), None);
        assert_eq!(read(&mut r, "coreasar", b"The AI Daily Brief"), None);
        assert_eq!(
            read(&mut r, "coreminm", "AI Panic \u{2014} History".as_bytes()),
            None
        );
        assert_eq!(
            read(&mut r, "corecaps", b"\x01"),
            None,
            "other details are ignored"
        );
        assert_eq!(
            read(&mut r, "ssncmden", b"1163602836"),
            Some(Event::Update(AirPlayUpdate::Song {
                title: "AI Panic \u{2014} History".into(),
                artist: "The AI Daily Brief".into(),
                album: "November 1, 2026".into(),
            }))
        );
        // The next song starts empty: a podcast's "Loading…" step has no artist.
        read(&mut r, "ssncmdst", b"2");
        read(&mut r, "coreminm", "Loading\u{2026}".as_bytes());
        assert_eq!(
            read(&mut r, "ssncmden", b"2"),
            Some(Event::Update(AirPlayUpdate::Song {
                title: "Loading\u{2026}".into(),
                artist: String::new(),
                album: String::new(),
            }))
        );
    }

    #[test]
    fn playing_pausing_and_leaving() {
        let mut r = Reader::default();
        let update = |r: &mut Reader, k| read(r, k, b"").unwrap();
        assert_eq!(
            update(&mut r, "ssncpbeg"),
            Event::Update(AirPlayUpdate::Started)
        );
        assert_eq!(
            update(&mut r, "ssncprsm"),
            Event::Update(AirPlayUpdate::Playing)
        );
        assert_eq!(
            update(&mut r, "ssncpfls"),
            Event::Update(AirPlayUpdate::Paused)
        );
        assert_eq!(
            update(&mut r, "ssncpend"),
            Event::Update(AirPlayUpdate::Paused)
        );
        assert_eq!(
            update(&mut r, "ssncaend"),
            Event::Update(AirPlayUpdate::Ended)
        );
        assert_eq!(read(&mut r, "ssncphbt", b"1/2"), None);
        assert_eq!(r.packet(b"ssnc"), None, "too short");
    }

    #[test]
    fn progress_is_in_frames_from_the_song_start() {
        let mut r = Reader::default();
        assert_eq!(
            read(&mut r, "ssncprgr", b"1163559911/1163602836/1234649831"),
            Some(Event::Update(AirPlayUpdate::Progress {
                position_ms: 973,
                duration_ms: 1_612_016,
            }))
        );
        // The frame count wraps around.
        assert_eq!(
            read(&mut r, "ssncprgr", b"4294923196/0/441000"),
            Some(Event::Update(AirPlayUpdate::Progress {
                position_ms: 1000,
                duration_ms: 11_000,
            }))
        );
        assert_eq!(read(&mut r, "ssncprgr", b"nonsense"), None);
    }

    #[test]
    fn volume_is_the_phones_slider() {
        let mut r = Reader::default();
        let percent = |r: &mut Reader, s: &str| match read(r, "ssncpvol", s.as_bytes()) {
            Some(Event::Update(AirPlayUpdate::Volume { percent })) => percent,
            other => panic!("{other:?}"),
        };
        assert_eq!(percent(&mut r, "0.00,0.00,-96.30,0.00"), 100);
        assert_eq!(percent(&mut r, "-3.12,-5.02,-96.30,0.00"), 90);
        assert_eq!(percent(&mut r, "-30.00,-96.30,-96.30,0.00"), 0);
        assert_eq!(percent(&mut r, "-144.00,-144.00,-96.30,0.00"), 0);
        assert_eq!(airplay_volume(100), 0.0);
        assert_eq!(airplay_volume(50), -15.0);
        assert_eq!(airplay_volume(0), MUTED);
    }

    fn piece(index: u32, total: u32, kind: &str, data: &[u8]) -> Vec<u8> {
        let mut p = b"ssncchnk".to_vec();
        p.extend(index.to_be_bytes());
        p.extend(total.to_be_bytes());
        p.extend(kind.as_bytes());
        p.extend(data);
        p
    }

    #[test]
    fn a_cover_in_pieces_is_put_back_together() {
        let mut r = Reader::default();
        assert_eq!(r.packet(&piece(0, 3, "ssncPICT", b"\xff\xd8ab")), None);
        assert_eq!(
            r.packet(&piece(2, 3, "ssncPICT", b"ef")),
            None,
            "out of order is fine"
        );
        assert_eq!(
            r.packet(&piece(1, 3, "ssncPICT", b"cd")),
            Some(Event::Cover(b"\xff\xd8abcdef".to_vec()))
        );
        // A new cover that starts before the last one finished replaces it.
        r.packet(&piece(0, 2, "ssncPICT", b"old"));
        r.packet(&piece(0, 3, "ssncPICT", b"n"));
        r.packet(&piece(1, 3, "ssncPICT", b"e"));
        assert_eq!(
            r.packet(&piece(2, 3, "ssncPICT", b"w")),
            Some(Event::Cover(b"new".to_vec()))
        );
        assert_eq!(
            read(&mut r, "ssncPICT", b""),
            Some(Event::Cover(Vec::new()))
        );
        assert_eq!(
            r.packet(&piece(5, 3, "ssncPICT", b"x")),
            None,
            "a piece past the end"
        );
    }
}
