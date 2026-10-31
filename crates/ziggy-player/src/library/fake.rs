//! In-memory catalog for `--fake` mode: UI work without a Spotify account.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::{FetchError, LibrarySource};
use crate::model::{
    Account, Collection, CollectionKind, EditOutcome, LIKED_URI, PlaylistEdit, SearchResults,
    Section, Track, liked_collection,
};

/// The fake account's Spotify ID; it owns every fake playlist but the last.
pub const FAKE_ACCOUNT_ID: &str = "fake";

/// Shared by the fake library and the fake player, so playlist edits are heard.
pub struct FakeCatalog {
    data: Mutex<Data>,
}

struct Data {
    playlists: Vec<Collection>,
    albums: Vec<Collection>,
    tracks: HashMap<String, Vec<Track>>,
    created: usize,
    /// Whether the user follows the fake artist.
    following: bool,
    /// Album URIs in the fake library; every fake album starts saved.
    saved_albums: std::collections::HashSet<String>,
}

impl FakeCatalog {
    pub fn sample() -> Self {
        let playlist_names = [
            "Dance Party",
            "Bedtime",
            "Car Songs",
            "Frozen Favourites",
            "Morning Mix",
            "A Very Long Playlist Name That Should Be Cut Off Nicely",
        ];
        let album_names = [
            ("Moana", "Various Artists", 2016),
            ("Encanto", "Various Artists", 2021),
            ("Abbey Road", "The Beatles", 1969),
            ("Rumours", "Fleetwood Mac", 1977),
        ];
        let playlists: Vec<Collection> = playlist_names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                // The last playlist belongs to someone else, so it can't be edited.
                let mine = i + 1 < playlist_names.len();
                Collection {
                    uri: format!("spotify:playlist:fake{i}"),
                    kind: CollectionKind::Playlist,
                    name: name.to_string(),
                    subtitle: if mine { "Fake Account" } else { "Someone Else" }.into(),
                    image_url: None,
                    owner_id: Some(if mine { FAKE_ACCOUNT_ID } else { "someone" }.into()),
                    snapshot_id: None,
                    artist_uri: None,
                    year: None,
                }
            })
            .collect();
        let albums: Vec<Collection> = album_names
            .iter()
            .enumerate()
            .map(|(i, (name, artist, year))| Collection {
                uri: format!("spotify:album:fake{i}"),
                kind: CollectionKind::Album,
                name: name.to_string(),
                subtitle: artist.to_string(),
                image_url: None,
                artist_uri: Some(fake_artist().uri),
                year: Some(*year),
                ..Default::default()
            })
            .collect();
        let mut tracks = HashMap::new();
        for c in playlists.iter().chain(albums.iter()) {
            tracks.insert(c.uri.clone(), fake_tracks(&c.uri, &c.name, 12));
        }
        tracks.insert(LIKED_URI.to_string(), fake_tracks(LIKED_URI, "Liked", 20));
        let saved_albums = albums.iter().map(|a| a.uri.clone()).collect();
        Self {
            data: Mutex::new(Data {
                following: true,
                saved_albums,
                playlists,
                albums,
                tracks,
                created: 0,
            }),
        }
    }

    pub fn playlists(&self) -> Vec<Collection> {
        self.data.lock().unwrap().playlists.clone()
    }

    /// The albums in the fake library.
    pub fn albums(&self) -> Vec<Collection> {
        let data = self.data.lock().unwrap();
        data.albums
            .iter()
            .filter(|a| data.saved_albums.contains(&a.uri))
            .cloned()
            .collect()
    }

    /// Followed artists: the fake band, unless unfollowed.
    pub fn followed(&self) -> Vec<Collection> {
        if self.data.lock().unwrap().following {
            vec![fake_artist()]
        } else {
            Vec::new()
        }
    }

    /// Every fake album, saved or not, as search and artists see them.
    pub fn all_albums(&self) -> Vec<Collection> {
        self.data.lock().unwrap().albums.clone()
    }

    pub fn tracks_for(&self, uri: &str) -> Vec<Track> {
        let data = self.data.lock().unwrap();
        if uri == fake_artist().uri {
            return data
                .albums
                .iter()
                .flat_map(|a| data.tracks.get(&a.uri).cloned().unwrap_or_default())
                .collect();
        }
        data.tracks.get(uri).cloned().unwrap_or_default()
    }

    /// Every song in the catalog, for looking up a track URI.
    fn find_track(data: &Data, uri: &str) -> Option<Track> {
        data.tracks
            .values()
            .flatten()
            .find(|t| t.uri == uri)
            .cloned()
    }

    pub fn apply(&self, edit: PlaylistEdit) -> Result<EditOutcome, FetchError> {
        let mut data = self.data.lock().unwrap();
        let owned = |data: &Data, uri: &str| {
            data.playlists
                .iter()
                .any(|p| p.uri == uri && p.owner_id.as_deref() == Some(FAKE_ACCOUNT_ID))
        };
        let check = |data: &Data, uri: &str| {
            if owned(data, uri) {
                Ok(())
            } else {
                Err(FetchError::Forbidden)
            }
        };
        match edit {
            PlaylistEdit::Add {
                playlist_uri,
                track_uri,
            } => {
                check(&data, &playlist_uri)?;
                let track = Self::find_track(&data, &track_uri).ok_or(FetchError::NotFound)?;
                data.tracks.entry(playlist_uri).or_default().push(track);
                Ok(EditOutcome::Done)
            }
            PlaylistEdit::Create { name, track_uri } => {
                let tracks = match track_uri {
                    Some(uri) => vec![Self::find_track(&data, &uri).ok_or(FetchError::NotFound)?],
                    None => Vec::new(),
                };
                data.created += 1;
                let created = Collection {
                    uri: format!("spotify:playlist:fake-new{}", data.created),
                    kind: CollectionKind::Playlist,
                    name,
                    subtitle: "Fake Account".into(),
                    image_url: None,
                    owner_id: Some(FAKE_ACCOUNT_ID.into()),
                    snapshot_id: None,
                    artist_uri: None,
                    year: None,
                };
                data.playlists.insert(0, created.clone());
                data.tracks.insert(created.uri.clone(), tracks);
                Ok(EditOutcome::Created(created))
            }
            PlaylistEdit::Remove {
                playlist_uri,
                track_uri,
                ..
            } => {
                check(&data, &playlist_uri)?;
                if let Some(list) = data.tracks.get_mut(&playlist_uri) {
                    list.retain(|t| t.uri != track_uri);
                }
                Ok(EditOutcome::Done)
            }
            PlaylistEdit::Move {
                playlist_uri,
                from,
                to,
                ..
            } => {
                check(&data, &playlist_uri)?;
                let list = data.tracks.entry(playlist_uri).or_default();
                if from < list.len() && to < list.len() {
                    let track = list.remove(from);
                    list.insert(to, track);
                }
                Ok(EditOutcome::Done)
            }
            PlaylistEdit::Rename { playlist_uri, name } => {
                check(&data, &playlist_uri)?;
                for p in data.playlists.iter_mut().filter(|p| p.uri == playlist_uri) {
                    p.name = name.clone();
                }
                Ok(EditOutcome::Done)
            }
            PlaylistEdit::Like { track_uri } => {
                let track = Self::find_track(&data, &track_uri).ok_or(FetchError::NotFound)?;
                let liked = data.tracks.entry(LIKED_URI.to_string()).or_default();
                if liked.iter().all(|t| t.uri != track_uri) {
                    liked.insert(0, track);
                }
                Ok(EditOutcome::Done)
            }
            PlaylistEdit::Unlike { track_uri } => {
                if let Some(liked) = data.tracks.get_mut(LIKED_URI) {
                    liked.retain(|t| t.uri != track_uri);
                }
                Ok(EditOutcome::Done)
            }
            PlaylistEdit::SaveAlbum { album_uri } => {
                data.saved_albums.insert(album_uri);
                Ok(EditOutcome::Done)
            }
            PlaylistEdit::UnsaveAlbum { album_uri } => {
                data.saved_albums.remove(&album_uri);
                Ok(EditOutcome::Done)
            }
            PlaylistEdit::Follow { .. } => {
                data.following = true;
                Ok(EditOutcome::Done)
            }
            PlaylistEdit::Unfollow { .. } => {
                data.following = false;
                Ok(EditOutcome::Done)
            }
            PlaylistEdit::Delete { playlist_uri } => {
                data.playlists.retain(|p| p.uri != playlist_uri);
                Ok(EditOutcome::Done)
            }
        }
    }
}

fn fake_tracks(collection_uri: &str, collection: &str, n: u32) -> Vec<Track> {
    (1..=n)
        .map(|i| Track {
            uri: format!(
                "spotify:track:fake-{}-{i}",
                collection_uri.replace(':', "-")
            ),
            name: format!("{collection} Song {i}"),
            artists: "The Fake Band".into(),
            album: collection.to_string(),
            image_url: None,
            // Short tracks so auto-advance is visible during UI work.
            duration_ms: 20_000 + i * 1_000,
            // The fake player only knows collections, so a single song plays in its own one.
            album_uri: Some(collection_uri.to_string()),
            artist_uri: Some(fake_artist().uri),
        })
        .collect()
}

/// The one artist in the fake catalog; every fake song is by it.
pub fn fake_artist() -> Collection {
    Collection {
        uri: "spotify:artist:fake".into(),
        kind: CollectionKind::Artist,
        name: "The Fake Band".into(),
        subtitle: "Artist".into(),
        image_url: None,
        ..Default::default()
    }
}

pub struct FakeSource {
    catalog: Arc<FakeCatalog>,
}

impl FakeSource {
    pub fn new(catalog: Arc<FakeCatalog>) -> Self {
        Self { catalog }
    }
}

impl LibrarySource for FakeSource {
    async fn section(&self, section: Section) -> Result<Vec<Collection>, FetchError> {
        tokio::time::sleep(Duration::from_millis(300)).await;
        Ok(match section {
            Section::Playlists => self.catalog.playlists(),
            Section::Albums => self.catalog.albums(),
            Section::Artists => self.catalog.followed(),
            Section::Recent => {
                let mut recent = self.catalog.playlists()[..2].to_vec();
                recent.push(self.catalog.albums()[0].clone());
                recent.push(liked_collection());
                recent
            }
            Section::Liked => vec![liked_collection()],
            Section::Search | Section::Settings | Section::Books | Section::Home => Vec::new(),
        })
    }

    async fn tracks(&self, collection_uri: &str) -> Result<Vec<Track>, FetchError> {
        tokio::time::sleep(Duration::from_millis(300)).await;
        Ok(self.catalog.tracks_for(collection_uri))
    }

    async fn search(&self, query: &str) -> Result<SearchResults, FetchError> {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let q = query.to_lowercase();
        let hit = |name: &str| name.to_lowercase().contains(&q);
        let (playlists, albums) = (self.catalog.playlists(), self.catalog.all_albums());
        let collections = playlists.iter().chain(&albums);
        let tracks = collections
            .clone()
            .flat_map(|c| self.catalog.tracks_for(&c.uri))
            .filter(|t| hit(&t.name))
            .take(20)
            .collect();
        let artist = fake_artist();
        Ok(SearchResults {
            tracks,
            artists: if hit(&artist.name) {
                vec![artist]
            } else {
                vec![]
            },
            albums: albums.iter().filter(|c| hit(&c.name)).cloned().collect(),
            playlists: playlists.iter().filter(|c| hit(&c.name)).cloned().collect(),
        })
    }

    async fn artist_albums(&self, _artist_uri: &str) -> Result<Vec<Collection>, FetchError> {
        tokio::time::sleep(Duration::from_millis(200)).await;
        Ok(self.catalog.all_albums())
    }

    async fn apply(&self, edit: PlaylistEdit) -> Result<EditOutcome, FetchError> {
        tokio::time::sleep(Duration::from_millis(200)).await;
        self.catalog.apply(edit)
    }

    async fn is_liked(&self, track_uri: &str) -> Result<bool, FetchError> {
        if track_uri.starts_with("spotify:artist:") {
            return Ok(!self.catalog.followed().is_empty());
        }
        if track_uri.starts_with("spotify:album:") {
            return Ok(self.catalog.albums().iter().any(|a| a.uri == track_uri));
        }
        Ok(self
            .catalog
            .tracks_for(LIKED_URI)
            .iter()
            .any(|t| t.uri == track_uri))
    }

    async fn account(&self) -> Result<Account, FetchError> {
        Ok(Account {
            id: FAKE_ACCOUNT_ID.into(),
            name: "Fake Account".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn track_uris_are_unique_across_the_catalog() {
        let catalog = FakeCatalog::sample();
        let mut seen = HashSet::new();
        let (playlists, albums) = (catalog.playlists(), catalog.albums());
        let uris = playlists
            .iter()
            .chain(albums.iter())
            .map(|c| c.uri.as_str())
            .chain([LIKED_URI]);
        for uri in uris {
            for track in catalog.tracks_for(uri) {
                assert!(seen.insert(track.uri.clone()), "duplicate {}", track.uri);
            }
        }
        assert!(seen.len() > 100);
    }

    fn first_track(catalog: &FakeCatalog, uri: &str) -> String {
        catalog.tracks_for(uri)[0].uri.clone()
    }

    #[test]
    fn edits_change_what_the_catalog_serves() {
        let catalog = FakeCatalog::sample();
        let mine = catalog.playlists()[0].uri.clone();
        let song = first_track(&catalog, &catalog.albums()[0].uri);

        catalog
            .apply(PlaylistEdit::Add {
                playlist_uri: mine.clone(),
                track_uri: song.clone(),
            })
            .unwrap();
        assert_eq!(catalog.tracks_for(&mine).last().unwrap().uri, song);

        let last = catalog.tracks_for(&mine).len() - 1;
        catalog
            .apply(PlaylistEdit::Move {
                playlist_uri: mine.clone(),
                from: last,
                to: 0,
                snapshot_id: None,
            })
            .unwrap();
        assert_eq!(catalog.tracks_for(&mine)[0].uri, song);

        catalog
            .apply(PlaylistEdit::Remove {
                playlist_uri: mine.clone(),
                track_uri: song.clone(),
                snapshot_id: None,
            })
            .unwrap();
        assert!(catalog.tracks_for(&mine).iter().all(|t| t.uri != song));

        catalog
            .apply(PlaylistEdit::Rename {
                playlist_uri: mine.clone(),
                name: "Renamed".into(),
            })
            .unwrap();
        assert_eq!(catalog.playlists()[0].name, "Renamed");

        let EditOutcome::Created(made) = catalog
            .apply(PlaylistEdit::Create {
                name: "New".into(),
                track_uri: Some(song.clone()),
            })
            .unwrap()
        else {
            panic!("expected Created")
        };
        assert_eq!(catalog.playlists()[0].uri, made.uri);
        assert_eq!(catalog.tracks_for(&made.uri)[0].uri, song);

        catalog
            .apply(PlaylistEdit::Delete {
                playlist_uri: made.uri.clone(),
            })
            .unwrap();
        assert!(catalog.playlists().iter().all(|p| p.uri != made.uri));
    }

    #[test]
    fn someone_elses_playlist_is_forbidden() {
        let catalog = FakeCatalog::sample();
        let theirs = catalog.playlists().last().unwrap().uri.clone();
        let result = catalog.apply(PlaylistEdit::Rename {
            playlist_uri: theirs,
            name: "x".into(),
        });
        assert_eq!(result, Err(FetchError::Forbidden));
    }
}
