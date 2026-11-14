//! My music: music files on the player's own card, shown alongside Spotify or without it.
//!
//! The folder is scanned at start and every few seconds after, so music copied in by
//! `ziggy music add` or the shared folder shows up by itself.

pub mod index;
pub mod tags;

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

use tokio::sync::mpsc::{self, UnboundedSender};

use crate::app::{Input, LibraryRequest, LibraryUpdate, MusicUpdate};
use crate::model::Section;
use index::{Entry, Index};

pub type SharedIndex = Arc<RwLock<Index>>;

/// How often the folder is checked for new, changed or removed files.
const RESCAN: Duration = Duration::from_secs(15);
/// A file changed this recently may still be copying; it waits for the next scan.
const SETTLE_SECS: i64 = 10;
/// Search results of each kind.
const SEARCH_LIMIT: usize = 8;
const CACHE_FILE: &str = "music-index.json";

fn modified_secs(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64)
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Every music and picture file under `root`, as paths inside it. Hidden files are left out,
/// which includes the `._` files a Mac leaves on a shared folder.
fn walk(root: &Path) -> (Vec<(String, std::fs::Metadata)>, Vec<String>) {
    let mut music = Vec::new();
    let mut pictures = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for item in read.flatten() {
            let name = item.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with('.') {
                continue;
            }
            let path = item.path();
            let Ok(meta) = item.metadata() else { continue };
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            if tags::is_music(&path) {
                music.push((rel, meta));
            } else if matches!(
                path.extension()
                    .and_then(|e| e.to_str())
                    .map(str::to_ascii_lowercase)
                    .as_deref(),
                Some("jpg" | "jpeg" | "png")
            ) {
                pictures.push(rel);
            }
        }
    }
    music.sort_by(|a, b| a.0.cmp(&b.0));
    (music, pictures)
}

/// Scans the folder, reading tags only for files that are new or changed since `known`.
/// Files still being copied are left for the next scan. Returns the entries and pictures.
pub fn scan(root: &Path, known: &[Entry], now: i64) -> (Vec<Entry>, Vec<String>) {
    let known: std::collections::HashMap<&str, &Entry> =
        known.iter().map(|e| (e.path.as_str(), e)).collect();
    let (files, pictures) = walk(root);
    let mut entries = Vec::with_capacity(files.len());
    for (path, meta) in files {
        let (size, modified) = (meta.len(), modified_secs(&meta));
        if let Some(old) = known.get(path.as_str())
            && old.size == size
            && old.modified == modified
        {
            entries.push((*old).clone());
            continue;
        }
        if now - modified < SETTLE_SECS {
            continue;
        }
        let tags = match tags::read_tags(&root.join(&path)) {
            Ok(tags) => Some(tags),
            Err(e) => {
                tracing::warn!("can't read {path}: {e}");
                None
            }
        };
        entries.push(Entry {
            path,
            size,
            modified,
            tags,
        });
    }
    (entries, pictures)
}

fn load_cache(file: &Path) -> Vec<Entry> {
    std::fs::read(file)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_cache(file: &Path, entries: &[Entry]) {
    let write = || -> anyhow::Result<()> {
        let tmp = file.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec(entries)?)?;
        std::fs::rename(&tmp, file)?;
        Ok(())
    };
    if let Err(e) = write() {
        tracing::warn!("saving the music index failed: {e:#}");
    }
}

/// Starts watching the music folder. Library requests for music on the card go to the
/// returned sender; the shared index is what the player and image loader read.
pub fn spawn_music(
    root: PathBuf,
    state_dir: &Path,
    inputs: UnboundedSender<Input>,
) -> (SharedIndex, UnboundedSender<LibraryRequest>) {
    let index: SharedIndex = Arc::new(RwLock::new(Index {
        root: root.clone(),
        ..Default::default()
    }));
    let cache_file = state_dir.join(CACHE_FILE);
    // Last scan's files, so the albums show at once after a restart.
    let mut entries = load_cache(&cache_file);
    let mut pictures: Vec<String> = Vec::new();
    if !entries.is_empty() {
        *index.write().unwrap() = Index::build(&root, &entries, &pictures);
    }
    let (tx, mut rx) = mpsc::unbounded_channel::<LibraryRequest>();
    let scanned = index.clone();
    let scan_inputs = inputs.clone();
    tokio::spawn(async move {
        let mut first = true;
        loop {
            let (root2, known) = (root.clone(), entries.clone());
            let result =
                tokio::task::spawn_blocking(move || scan(&root2, &known, now_secs())).await;
            if let Ok((found, found_pictures)) = result {
                // Pictures are only listed, not cached, so the first scan always rebuilds.
                let changed = first || found != entries || found_pictures != pictures;
                if changed {
                    entries = found;
                    pictures = found_pictures;
                    let built = Index::build(&root, &entries, &pictures);
                    let albums = built.albums();
                    if !first || !entries.is_empty() {
                        tracing::info!(
                            "My music: {} songs in {} albums",
                            built.tracks.len(),
                            albums.len()
                        );
                    }
                    *scanned.write().unwrap() = built;
                    save_cache(&cache_file, &entries);
                    let _ = scan_inputs.send(Input::Library(LibraryUpdate::Section {
                        section: Section::MyMusic,
                        items: albums,
                    }));
                }
            }
            first = false;
            tokio::time::sleep(RESCAN).await;
        }
    });
    let served = index.clone();
    tokio::spawn(async move {
        while let Some(request) = rx.recv().await {
            let reply = answer(&served.read().unwrap(), request);
            if let Some(reply) = reply {
                let _ = inputs.send(reply);
            }
        }
    });
    (index, tx)
}

/// My music is off: an empty index, and requests go nowhere.
pub fn spawn_off() -> (SharedIndex, UnboundedSender<LibraryRequest>) {
    let (tx, mut rx) = mpsc::unbounded_channel::<LibraryRequest>();
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    (SharedIndex::default(), tx)
}

/// Answers a library request from the index.
pub fn answer(index: &Index, request: LibraryRequest) -> Option<Input> {
    use crate::app::FailReason;
    let update = match request {
        LibraryRequest::Section(Section::MyMusic) | LibraryRequest::Reload(Section::MyMusic) => {
            LibraryUpdate::Section {
                section: Section::MyMusic,
                items: index.albums(),
            }
        }
        LibraryRequest::Tracks { collection_uri }
        | LibraryRequest::ReloadTracks { collection_uri } => match index.tracks_of(&collection_uri)
        {
            Some(tracks) => LibraryUpdate::Tracks {
                collection_uri,
                tracks,
            },
            None => LibraryUpdate::TracksFailed {
                collection_uri,
                reason: FailReason::Other,
            },
        },
        LibraryRequest::ArtistAlbums { artist_uri } => match index.artist_albums(&artist_uri) {
            Some(albums) => LibraryUpdate::ArtistAlbums { artist_uri, albums },
            None => LibraryUpdate::ArtistAlbumsFailed {
                artist_uri,
                reason: FailReason::Other,
            },
        },
        LibraryRequest::Search(query) => {
            return Some(Input::Music(MusicUpdate::Search {
                results: index.search(&query, SEARCH_LIMIT),
                query,
            }));
        }
        _ => return None,
    };
    Some(Input::Library(update))
}

/// Whether a library request is for the music on the card.
pub fn wants(request: &LibraryRequest) -> bool {
    match request {
        LibraryRequest::Section(s) | LibraryRequest::Reload(s) => *s == Section::MyMusic,
        LibraryRequest::Tracks { collection_uri }
        | LibraryRequest::ReloadTracks { collection_uri } => index::is_local(collection_uri),
        LibraryRequest::ArtistAlbums { artist_uri } => index::is_local(artist_uri),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scan_skips_hidden_files_and_files_still_copying() {
        let dir = tempfile::tempdir().unwrap();
        let album = dir.path().join("Artist/Album");
        std::fs::create_dir_all(&album).unwrap();
        // Not real MP3s: they're listed, fail to read, and are kept as unreadable.
        std::fs::write(album.join("01 Song.mp3"), b"not audio").unwrap();
        std::fs::write(album.join("._01 Song.mp3"), b"mac junk").unwrap();
        std::fs::write(album.join("cover.jpg"), b"jpg").unwrap();
        std::fs::write(album.join("notes.txt"), b"x").unwrap();

        let (entries, pictures) = scan(dir.path(), &[], now_secs());
        assert!(entries.is_empty(), "just written, so still copying");
        assert_eq!(pictures, vec!["Artist/Album/cover.jpg".to_string()]);

        let (entries, _) = scan(dir.path(), &[], now_secs() + 60);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "Artist/Album/01 Song.mp3");
        assert_eq!(entries[0].tags, None, "unreadable");

        // Unchanged files aren't read again.
        let mut known = entries.clone();
        known[0].tags = Some(tags::FileTags::default());
        let (again, _) = scan(dir.path(), &known, now_secs() + 60);
        assert_eq!(again, known);
    }

    #[test]
    fn requests_for_the_card_are_answered_from_the_index() {
        assert!(wants(&LibraryRequest::Section(Section::MyMusic)));
        assert!(!wants(&LibraryRequest::Section(Section::Albums)));
        assert!(wants(&LibraryRequest::Tracks {
            collection_uri: "local:album:1".into()
        }));
        assert!(!wants(&LibraryRequest::Tracks {
            collection_uri: "spotify:album:1".into()
        }));
        let index = Index::default();
        assert_eq!(
            answer(&index, LibraryRequest::Section(Section::MyMusic)),
            Some(Input::Library(LibraryUpdate::Section {
                section: Section::MyMusic,
                items: vec![]
            }))
        );
        assert!(matches!(
            answer(
                &index,
                LibraryRequest::Tracks {
                    collection_uri: "local:album:x".into()
                }
            ),
            Some(Input::Library(LibraryUpdate::TracksFailed { .. }))
        ));
    }
}
