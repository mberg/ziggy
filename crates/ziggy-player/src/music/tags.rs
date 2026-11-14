//! Reading a music file's tags, length and cover with Symphonia.

use std::fs::File;
use std::path::Path;

use serde::{Deserialize, Serialize};
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, MetadataRevision, StandardTagKey, StandardVisualKey};
use symphonia::core::probe::Hint;

/// What a file says about itself. Every field may be missing.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FileTags {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album_artist: Option<String>,
    pub album: Option<String>,
    pub track: Option<u32>,
    pub disc: Option<u32>,
    pub year: Option<u16>,
    pub duration_ms: u32,
    /// The file carries a cover picture.
    pub picture: bool,
}

/// The file extensions My music plays.
pub const EXTENSIONS: [&str; 4] = ["mp3", "m4a", "aac", "flac"];

pub fn is_music(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

fn probe(path: &Path) -> anyhow::Result<symphonia::core::probe::ProbeResult> {
    let file = File::open(path)?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    Ok(symphonia::default::get_probe().format(
        &hint,
        stream,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    )?)
}

/// Every metadata revision: ID3 tags come with the probe, MP4 and FLAC tags with the format.
fn revisions(probed: &mut symphonia::core::probe::ProbeResult) -> Vec<MetadataRevision> {
    let mut out = Vec::new();
    if let Some(m) = probed.metadata.get()
        && let Some(rev) = m.current()
    {
        out.push(rev.clone());
    }
    if let Some(rev) = probed.format.metadata().current() {
        out.push(rev.clone());
    }
    out
}

/// "3/12" and "3" are both track 3.
fn number(value: &str) -> Option<u32> {
    value.split('/').next()?.trim().parse().ok()
}

/// The year from "1986", "1986-03-01" or "1986-03-01T00:00:00".
fn year(value: &str) -> Option<u16> {
    let digits: String = value.trim().chars().take(4).collect();
    digits.parse().ok().filter(|y| (1000..=9999).contains(y))
}

pub fn read_tags(path: &Path) -> anyhow::Result<FileTags> {
    let mut probed = probe(path)?;
    let mut tags = FileTags::default();
    for rev in revisions(&mut probed) {
        for tag in rev.tags() {
            let value = tag.value.to_string();
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            let text = || Some(value.to_string());
            match tag.std_key {
                Some(StandardTagKey::TrackTitle) => tags.title = tags.title.take().or_else(text),
                Some(StandardTagKey::Artist) => tags.artist = tags.artist.take().or_else(text),
                Some(StandardTagKey::AlbumArtist) => {
                    tags.album_artist = tags.album_artist.take().or_else(text)
                }
                Some(StandardTagKey::Album) => tags.album = tags.album.take().or_else(text),
                Some(StandardTagKey::TrackNumber) => tags.track = tags.track.or(number(value)),
                Some(StandardTagKey::DiscNumber) => tags.disc = tags.disc.or(number(value)),
                Some(StandardTagKey::Date | StandardTagKey::OriginalDate) => {
                    tags.year = tags.year.or(year(value))
                }
                _ => {}
            }
        }
        tags.picture |= !rev.visuals().is_empty();
    }
    if let Some(track) = probed.format.default_track() {
        let p = &track.codec_params;
        if let (Some(frames), Some(rate)) = (p.n_frames, p.sample_rate)
            && rate > 0
        {
            tags.duration_ms =
                (frames.saturating_mul(1000) / rate as u64).min(u32::MAX as u64) as u32;
        }
    }
    Ok(tags)
}

/// The file's own cover picture, front cover first.
pub fn read_picture(path: &Path) -> Option<Vec<u8>> {
    let mut probed = probe(path).ok()?;
    let visuals: Vec<_> = revisions(&mut probed)
        .into_iter()
        .flat_map(|rev| rev.visuals().to_vec())
        .collect();
    visuals
        .iter()
        .find(|v| v.usage == Some(StandardVisualKey::FrontCover))
        .or(visuals.first())
        .map(|v| v.data.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_years_are_read_loosely() {
        assert_eq!(number("3/12"), Some(3));
        assert_eq!(number(" 07 "), Some(7));
        assert_eq!(number("A"), None);
        assert_eq!(year("1986-03-01"), Some(1986));
        assert_eq!(year("86"), None);
    }

    #[test]
    fn only_music_files_count() {
        assert!(is_music(Path::new("a/b/01 Song.MP3")));
        assert!(is_music(Path::new("x.flac")));
        assert!(!is_music(Path::new("cover.jpg")));
        assert!(!is_music(Path::new("notes")));
    }
}
