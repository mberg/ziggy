//! The music on the card as albums, artists and songs, built from each file's tags.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::tags::FileTags;
use crate::model::{Collection, CollectionKind, SearchResults, Track};

pub const TRACK_PREFIX: &str = "local:track:";
pub const ALBUM_PREFIX: &str = "local:album:";
pub const ARTIST_PREFIX: &str = "local:artist:";
/// Covers the image loader draws from the card.
pub const COVER_PREFIX: &str = "local-cover:";
pub const VARIOUS_ARTISTS: &str = "Various artists";
const UNKNOWN_ARTIST: &str = "Unknown artist";

/// Whether a URI is for music on the card.
pub fn is_local(uri: &str) -> bool {
    uri.starts_with("local:")
}

/// A short ID that stays the same across restarts and updates (64-bit FNV-1a).
pub fn short_id(text: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in text.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

/// One file as last scanned, kept between restarts so unchanged files aren't read again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Path inside the music folder, with `/` between folders.
    pub path: String,
    pub size: u64,
    /// Seconds since 1970.
    pub modified: i64,
    /// None when the file couldn't be read; it's tried again once it changes.
    pub tags: Option<FileTags>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LocalTrack {
    pub id: String,
    pub path: String,
    pub title: String,
    pub artist: String,
    pub album_id: String,
    pub disc: u32,
    pub number: u32,
    pub duration_ms: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LocalAlbum {
    pub id: String,
    pub name: String,
    pub artist: String,
    pub artist_id: String,
    pub year: Option<u16>,
    /// Track IDs in playing order.
    pub tracks: Vec<String>,
    pub cover: Option<Cover>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cover {
    /// A picture inside this song's file.
    Embedded(String),
    /// A picture file in the album's folder.
    File(String),
}

/// Everything on the card, ready to show and play.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Index {
    pub root: PathBuf,
    pub tracks: HashMap<String, LocalTrack>,
    /// Sorted by artist, then name.
    pub albums: Vec<LocalAlbum>,
}

/// Picture files that count as an album's cover, best first.
const COVER_NAMES: [&str; 6] = [
    "cover.jpg",
    "folder.jpg",
    "front.jpg",
    "cover.png",
    "folder.png",
    "front.png",
];

fn sort_key(name: &str) -> String {
    let lower = name.to_lowercase();
    lower
        .strip_prefix("the ")
        .map(str::to_string)
        .unwrap_or(lower)
}

/// "01 - Song", "01. Song" and "1 Song" are all "Song".
fn title_from_file(path: &str) -> String {
    let stem = Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(path);
    let rest = stem.trim_start_matches(|c: char| c.is_ascii_digit());
    let rest = rest.trim_start_matches([' ', '-', '.', '_']);
    if rest.is_empty() || rest.len() == stem.len() {
        stem.to_string()
    } else {
        rest.to_string()
    }
}

/// The folder a file is in, and that folder's parent, as names.
fn folders(path: &str) -> (Option<&str>, Option<&str>) {
    let mut parts = path.rsplit('/').skip(1);
    (parts.next(), parts.next())
}

fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

impl Index {
    /// Builds the albums from scanned files. `pictures` lists the picture files found, by path
    /// inside the music folder.
    pub fn build(root: &Path, entries: &[Entry], pictures: &[String]) -> Index {
        struct Song<'a> {
            entry: &'a Entry,
            tags: &'a FileTags,
            artist: String,
            album: String,
        }
        // First group by folder and album name, so an album's artist can be worked out from
        // all its songs.
        let mut groups: BTreeMap<(String, String), Vec<Song>> = BTreeMap::new();
        for entry in entries {
            let Some(tags) = &entry.tags else { continue };
            let (folder, grandparent) = folders(&entry.path);
            let artist = tags
                .artist
                .clone()
                .or_else(|| tags.album_artist.clone())
                .or_else(|| grandparent.map(str::to_string))
                .unwrap_or_else(|| UNKNOWN_ARTIST.into());
            let album = tags
                .album
                .clone()
                .or_else(|| folder.map(str::to_string))
                .unwrap_or_else(|| "Songs".into());
            groups
                .entry((parent_dir(&entry.path).to_string(), album.to_lowercase()))
                .or_default()
                .push(Song {
                    entry,
                    tags,
                    artist,
                    album,
                });
        }
        let mut albums: BTreeMap<String, LocalAlbum> = BTreeMap::new();
        let mut tracks = HashMap::new();
        let mut covers: HashMap<String, Cover> = HashMap::new();
        for ((dir, _), songs) in groups {
            let album_artist = songs
                .iter()
                .find_map(|s| s.tags.album_artist.clone())
                .unwrap_or_else(|| {
                    let first = &songs[0].artist;
                    if songs.iter().all(|s| &s.artist == first) {
                        first.clone()
                    } else {
                        VARIOUS_ARTISTS.into()
                    }
                });
            let name = songs[0].album.clone();
            // Albums split over disc folders share an ID, so they join up.
            let id = short_id(&format!(
                "{}\u{1f}{}",
                album_artist.to_lowercase(),
                name.to_lowercase()
            ));
            let album = albums.entry(id.clone()).or_insert_with(|| LocalAlbum {
                id: id.clone(),
                name: name.clone(),
                artist: album_artist.clone(),
                artist_id: short_id(&album_artist.to_lowercase()),
                year: None,
                tracks: Vec::new(),
                cover: None,
            });
            let folder_cover = COVER_NAMES.iter().find_map(|cover| {
                let want = if dir.is_empty() {
                    cover.to_string()
                } else {
                    format!("{dir}/{cover}")
                };
                pictures
                    .iter()
                    .find(|p| p.eq_ignore_ascii_case(&want))
                    .cloned()
            });
            for song in songs {
                let track_id = short_id(&song.entry.path);
                album.year = album.year.or(song.tags.year);
                if album.cover.is_none() && song.tags.picture {
                    album.cover = Some(Cover::Embedded(song.entry.path.clone()));
                }
                album.tracks.push(track_id.clone());
                tracks.insert(
                    track_id.clone(),
                    LocalTrack {
                        id: track_id,
                        path: song.entry.path.clone(),
                        title: song
                            .tags
                            .title
                            .clone()
                            .unwrap_or_else(|| title_from_file(&song.entry.path)),
                        artist: song.artist,
                        album_id: id.clone(),
                        disc: song.tags.disc.unwrap_or(1),
                        number: song.tags.track.unwrap_or(0),
                        duration_ms: song.tags.duration_ms,
                    },
                );
            }
            if let Some(file) = folder_cover {
                covers.entry(id.clone()).or_insert(Cover::File(file));
            }
        }
        let mut albums: Vec<LocalAlbum> = albums
            .into_values()
            .map(|mut album| {
                // A picture in the folder beats one inside a file: it's usually bigger.
                if let Some(cover) = covers.remove(&album.id) {
                    album.cover = Some(cover);
                }
                album.tracks.sort_by(|a, b| {
                    let (a, b) = (&tracks[a], &tracks[b]);
                    (a.disc, a.number, &a.path).cmp(&(b.disc, b.number, &b.path))
                });
                album
            })
            .collect();
        albums.sort_by(|a, b| {
            (sort_key(&a.artist), sort_key(&a.name)).cmp(&(sort_key(&b.artist), sort_key(&b.name)))
        });
        Index {
            root: root.to_path_buf(),
            tracks,
            albums,
        }
    }

    pub fn album(&self, id: &str) -> Option<&LocalAlbum> {
        self.albums.iter().find(|a| a.id == id)
    }

    pub fn file(&self, track_id: &str) -> Option<PathBuf> {
        self.tracks.get(track_id).map(|t| self.root.join(&t.path))
    }

    fn cover_url(album: &LocalAlbum) -> Option<String> {
        album
            .cover
            .as_ref()
            .map(|_| format!("{COVER_PREFIX}{}", album.id))
    }

    pub fn album_collection(&self, album: &LocalAlbum) -> Collection {
        Collection {
            uri: format!("{ALBUM_PREFIX}{}", album.id),
            kind: CollectionKind::Album,
            name: album.name.clone(),
            subtitle: album.artist.clone(),
            image_url: Self::cover_url(album),
            artist_uri: Some(format!("{ARTIST_PREFIX}{}", album.artist_id)),
            year: album.year,
            ..Default::default()
        }
    }

    pub fn artist_collection(&self, album: &LocalAlbum) -> Collection {
        Collection {
            uri: format!("{ARTIST_PREFIX}{}", album.artist_id),
            kind: CollectionKind::Artist,
            name: album.artist.clone(),
            image_url: Self::cover_url(album),
            ..Default::default()
        }
    }

    pub fn track(&self, id: &str) -> Option<Track> {
        let t = self.tracks.get(id)?;
        let album = self.album(&t.album_id)?;
        Some(Track {
            uri: format!("{TRACK_PREFIX}{id}"),
            name: t.title.clone(),
            artists: t.artist.clone(),
            album: album.name.clone(),
            image_url: Self::cover_url(album),
            duration_ms: t.duration_ms,
            album_uri: Some(format!("{ALBUM_PREFIX}{}", album.id)),
            artist_uri: Some(format!("{ARTIST_PREFIX}{}", album.artist_id)),
        })
    }

    /// Every album, as My music shows them.
    pub fn albums(&self) -> Vec<Collection> {
        self.albums
            .iter()
            .map(|a| self.album_collection(a))
            .collect()
    }

    /// The songs of an album, or of every album by an artist.
    pub fn tracks_of(&self, uri: &str) -> Option<Vec<Track>> {
        if let Some(id) = uri.strip_prefix(ALBUM_PREFIX) {
            let album = self.album(id)?;
            return Some(album.tracks.iter().filter_map(|t| self.track(t)).collect());
        }
        if let Some(id) = uri.strip_prefix(ARTIST_PREFIX) {
            let tracks: Vec<Track> = self
                .albums
                .iter()
                .filter(|a| a.artist_id == id)
                .flat_map(|a| a.tracks.iter())
                .filter_map(|t| self.track(t))
                .collect();
            return (!tracks.is_empty()).then_some(tracks);
        }
        None
    }

    /// An artist's albums, newest first.
    pub fn artist_albums(&self, uri: &str) -> Option<Vec<Collection>> {
        let id = uri.strip_prefix(ARTIST_PREFIX)?;
        let mut albums: Vec<&LocalAlbum> =
            self.albums.iter().filter(|a| a.artist_id == id).collect();
        if albums.is_empty() {
            return None;
        }
        albums.sort_by_key(|a| std::cmp::Reverse(a.year));
        Some(
            albums
                .into_iter()
                .map(|a| self.album_collection(a))
                .collect(),
        )
    }

    /// Songs, albums and artists whose names have every word typed.
    pub fn search(&self, query: &str, limit: usize) -> SearchResults {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        if words.is_empty() {
            return SearchResults::default();
        }
        let hit = |text: &str| {
            let text = text.to_lowercase();
            words.iter().all(|w| text.contains(w.as_str()))
        };
        let mut found = SearchResults::default();
        let mut artists_seen = std::collections::HashSet::new();
        for album in &self.albums {
            if found.artists.len() < limit
                && hit(&album.artist)
                && artists_seen.insert(album.artist_id.clone())
            {
                found.artists.push(self.artist_collection(album));
            }
            if found.albums.len() < limit && hit(&format!("{} {}", album.name, album.artist)) {
                found.albums.push(self.album_collection(album));
            }
            for id in &album.tracks {
                if found.tracks.len() >= limit {
                    break;
                }
                let t = &self.tracks[id];
                if hit(&format!("{} {}", t.title, t.artist)) {
                    found.tracks.extend(self.track(id));
                }
            }
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, tags: FileTags) -> Entry {
        Entry {
            path: path.into(),
            size: 1,
            modified: 0,
            tags: Some(tags),
        }
    }

    fn song(title: &str, artist: &str, album: &str, track: u32) -> FileTags {
        FileTags {
            title: Some(title.into()),
            artist: Some(artist.into()),
            album: Some(album.into()),
            track: Some(track),
            duration_ms: 180_000,
            ..Default::default()
        }
    }

    #[test]
    fn songs_group_into_albums_in_track_order() {
        let index = Index::build(
            Path::new("/music"),
            &[
                entry("Abba/Gold/02.mp3", song("SOS", "ABBA", "Gold", 2)),
                entry("Abba/Gold/01.mp3", song("Dancing Queen", "ABBA", "Gold", 1)),
                entry(
                    "The Beatles/Help/01.mp3",
                    song("Help!", "The Beatles", "Help!", 1),
                ),
            ],
            &["Abba/Gold/Cover.JPG".into()],
        );
        let albums = index.albums();
        assert_eq!(
            albums.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            vec!["Gold", "Help!"],
            "sorted by artist, ignoring \"The\""
        );
        assert_eq!(albums[0].subtitle, "ABBA");
        assert!(
            albums[0]
                .image_url
                .as_deref()
                .unwrap()
                .starts_with(COVER_PREFIX)
        );
        assert_eq!(albums[1].image_url, None);
        let tracks = index.tracks_of(&albums[0].uri).unwrap();
        assert_eq!(
            tracks.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            vec!["Dancing Queen", "SOS"]
        );
        assert_eq!(tracks[0].album_uri.as_deref(), Some(albums[0].uri.as_str()));
        assert_eq!(
            index.file(tracks[0].uri.strip_prefix(TRACK_PREFIX).unwrap()),
            Some(PathBuf::from("/music/Abba/Gold/01.mp3"))
        );
    }

    #[test]
    fn missing_tags_come_from_the_folders_and_file_name() {
        let index = Index::build(
            Path::new("/m"),
            &[entry(
                "Raffi/Baby Beluga/03 - Day O.mp3",
                FileTags::default(),
            )],
            &[],
        );
        let album = &index.albums()[0];
        assert_eq!(album.name, "Baby Beluga");
        assert_eq!(album.subtitle, "Raffi");
        assert_eq!(index.tracks_of(&album.uri).unwrap()[0].name, "Day O");
    }

    #[test]
    fn mixed_artists_without_an_album_artist_are_various_artists() {
        let index = Index::build(
            Path::new("/m"),
            &[
                entry(
                    "Moana/01.mp3",
                    song("Where You Are", "Christopher Jackson", "Moana", 1),
                ),
                entry(
                    "Moana/02.mp3",
                    song("How Far I'll Go", "Auli'i Cravalho", "Moana", 2),
                ),
            ],
            &[],
        );
        assert_eq!(index.albums().len(), 1);
        assert_eq!(index.albums()[0].subtitle, VARIOUS_ARTISTS);
    }

    #[test]
    fn disc_folders_join_into_one_album() {
        let mut one = song("A", "X", "Big", 1);
        one.disc = Some(1);
        let mut two = song("B", "X", "Big", 1);
        two.disc = Some(2);
        let index = Index::build(
            Path::new("/m"),
            &[
                entry("X/Big/CD2/01.mp3", two),
                entry("X/Big/CD1/01.mp3", one),
            ],
            &[],
        );
        assert_eq!(index.albums().len(), 1);
        let names: Vec<_> = index
            .tracks_of(&index.albums()[0].uri)
            .unwrap()
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(names, vec!["A", "B"]);
    }

    #[test]
    fn search_matches_every_word() {
        let index = Index::build(
            Path::new("/m"),
            &[
                entry("a/1.mp3", song("Baby Beluga", "Raffi", "Baby Beluga", 1)),
                entry("a/2.mp3", song("Day O", "Raffi", "Baby Beluga", 2)),
            ],
            &[],
        );
        let found = index.search("raffi day", 5);
        assert_eq!(found.tracks.len(), 1);
        assert_eq!(found.tracks[0].name, "Day O");
        assert!(found.albums.is_empty());
        let found = index.search("beluga", 5);
        assert_eq!(found.albums.len(), 1);
        assert_eq!(found.tracks.len(), 1);
        assert_eq!(index.search("raffi", 5).artists.len(), 1);
        assert!(index.search("  ", 5).tracks.is_empty());
    }

    #[test]
    fn ids_stay_the_same() {
        assert_eq!(short_id("abc"), short_id("abc"));
        assert_ne!(short_id("abc"), short_id("abd"));
        assert_eq!(short_id("").len(), 16);
    }

    #[test]
    fn titles_lose_their_track_numbers() {
        assert_eq!(title_from_file("x/01 - Song.mp3"), "Song");
        assert_eq!(title_from_file("x/1. Song.mp3"), "Song");
        assert_eq!(title_from_file("x/Song.mp3"), "Song");
        assert_eq!(title_from_file("x/1999.mp3"), "1999");
    }
}
