//! Downloads cover art, caches the bytes on disk, decodes off the UI thread.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use image::RgbaImage;
use tokio::sync::Semaphore;
use tokio::sync::mpsc::UnboundedReceiver;

pub type ImageSink = Arc<dyn Fn(String, Option<RgbaImage>) + Send + Sync>;

pub struct ImageLoader {
    client: reqwest::Client,
    dir: PathBuf,
    /// Audiobookshelf covers ("abs-cover:<item id>") need the signed-in client.
    books: Option<crate::audiobooks::service::AbsHandle>,
    /// AirPlay covers ("airplay-cover:…") come from the phone and are only kept in memory.
    airplay: Option<crate::airplay::CoverStore>,
    /// My music covers ("local-cover:<album id>") are read from the card each time.
    music: Option<crate::music::SharedIndex>,
}

/// The image URL for an Audiobookshelf book's cover.
pub fn book_cover_url(id: &str) -> String {
    format!("{BOOK_COVER}{id}")
}

const BOOK_COVER: &str = "abs-cover:";

/// `--fake` covers are drawn, not downloaded: "fake-cover:<collection URI>".
const FAKE_COVER: &str = "fake-cover:";

pub fn fake_cover_url(collection_uri: &str) -> String {
    format!("{FAKE_COVER}{collection_uri}")
}

/// A two-colour gradient with a soft circle, picked from the collection URI so each
/// fake playlist and album looks different but always the same.
fn draw_fake_cover(key: &str, size: u32) -> RgbaImage {
    const PAIRS: [([u8; 3], [u8; 3]); 6] = [
        ([255, 94, 98], [255, 195, 113]),
        ([72, 85, 199], [142, 197, 252]),
        ([17, 153, 142], [56, 239, 125]),
        ([142, 45, 226], [255, 110, 196]),
        ([252, 74, 26], [247, 183, 51]),
        ([30, 60, 114], [42, 157, 143]),
    ];
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    let h = hasher.finish();
    let (a, b) = PAIRS[(h % PAIRS.len() as u64) as usize];
    let (cx, cy) = (
        size as f32 * (0.3 + ((h >> 8) % 40) as f32 / 100.0),
        size as f32 * (0.3 + ((h >> 16) % 40) as f32 / 100.0),
    );
    let radius = size as f32 * 0.28;
    RgbaImage::from_fn(size, size, |x, y| {
        let t = (x + y) as f32 / (2 * size) as f32;
        let mut c = [0f32; 3];
        for k in 0..3 {
            c[k] = a[k] as f32 * (1.0 - t) + b[k] as f32 * t;
        }
        let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
        let glow = (1.0 - (d - radius).max(0.0) / 6.0).clamp(0.0, 1.0) * 0.35;
        image::Rgba([
            (c[0] + (255.0 - c[0]) * glow) as u8,
            (c[1] + (255.0 - c[1]) * glow) as u8,
            (c[2] + (255.0 - c[2]) * glow) as u8,
            255,
        ])
    })
}

pub fn cache_file_name(url: &str) -> String {
    let last = url.rsplit('/').next().unwrap_or("");
    let safe: String = last.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    if safe.is_empty() {
        let mut hasher = DefaultHasher::new();
        url.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    } else {
        safe
    }
}

/// Covers are decoded at the size they're shown, not as downloaded: a grid tile is about 160 px.
pub const SMALL_PX: u32 = 160;
/// The Now Playing and book covers are about 320 px.
pub const LARGE_PX: u32 = 320;
/// A requested URL with this in front wants the large copy, kept separately in memory.
pub const LARGE: &str = "large:";

/// The URL to download, and the size to decode it at.
pub fn sized(request: &str) -> (&str, u32) {
    match request.strip_prefix(LARGE) {
        Some(url) => (url, LARGE_PX),
        None => (request, SMALL_PX),
    }
}

pub fn decode(bytes: &[u8], max_px: u32) -> anyhow::Result<RgbaImage> {
    let img = image::load_from_memory(bytes)?;
    let img = if img.width() > max_px || img.height() > max_px {
        img.thumbnail(max_px, max_px)
    } else {
        img
    };
    Ok(img.to_rgba8())
}

impl ImageLoader {
    pub fn new(dir: PathBuf) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()?;
        Ok(Self {
            client,
            dir,
            books: None,
            airplay: None,
            music: None,
        })
    }

    pub fn with_books(mut self, books: crate::audiobooks::service::AbsHandle) -> Self {
        self.books = Some(books);
        self
    }

    pub fn with_airplay(mut self, covers: crate::airplay::CoverStore) -> Self {
        self.airplay = Some(covers);
        self
    }

    pub fn with_music(mut self, index: crate::music::SharedIndex) -> Self {
        self.music = Some(index);
        self
    }

    /// The picture for an album on the card: a picture file in its folder, or one inside a song.
    fn local_cover(&self, album_id: &str) -> anyhow::Result<Vec<u8>> {
        use crate::music::index::Cover;
        let index = self
            .music
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no music on the card"))?;
        let (root, cover) = {
            let index = index.read().unwrap();
            let cover = index.album(album_id).and_then(|a| a.cover.clone());
            (index.root.clone(), cover)
        };
        match cover {
            Some(Cover::File(path)) => Ok(std::fs::read(root.join(path))?),
            Some(Cover::Embedded(path)) => crate::music::tags::read_picture(&root.join(path))
                .ok_or_else(|| anyhow::anyhow!("the song's picture can't be read")),
            None => anyhow::bail!("that album has no cover"),
        }
    }

    async fn download(&self, url: &str) -> anyhow::Result<Vec<u8>> {
        if let Some(id) = url.strip_prefix(BOOK_COVER) {
            let client = self
                .books
                .as_ref()
                .and_then(|h| h.read().unwrap().clone())
                .ok_or_else(|| anyhow::anyhow!("not signed in to Audiobookshelf"))?;
            return client
                .cover(id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("book has no cover"));
        }
        Ok(self
            .client
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?
            .to_vec())
    }

    /// `request` is a cover URL, or one marked `LARGE` for the big copy. Both sizes share the
    /// file on disk.
    pub async fn load(&self, request: &str) -> anyhow::Result<RgbaImage> {
        let (url, max_px) = sized(request);
        if let Some(key) = url.strip_prefix(FAKE_COVER) {
            return Ok(draw_fake_cover(key, max_px));
        }
        if let Some(id) = url.strip_prefix(crate::music::index::COVER_PREFIX) {
            let bytes = self.local_cover(id)?;
            return tokio::task::spawn_blocking(move || decode(&bytes, max_px)).await?;
        }
        if url.starts_with(crate::airplay::COVER) {
            let bytes = self
                .airplay
                .as_ref()
                .and_then(|covers| crate::airplay::cover(covers, url))
                .ok_or_else(|| anyhow::anyhow!("that AirPlay cover has gone"))?;
            return tokio::task::spawn_blocking(move || decode(&bytes, max_px)).await?;
        }
        let path = self.dir.join(cache_file_name(url));
        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(_) => {
                let bytes = self.download(url).await?;
                let tmp = path.with_extension("tmp");
                tokio::fs::write(&tmp, &bytes).await?;
                tokio::fs::rename(&tmp, &path).await?;
                bytes
            }
        };
        let decoded = tokio::task::spawn_blocking(move || decode(&bytes, max_px)).await?;
        if decoded.is_err() {
            let _ = tokio::fs::remove_file(&path).await;
        }
        decoded
    }
}

/// Loads requested URLs, at most 3 at a time, and hands results to `sink`.
pub fn spawn_image_loader(
    loader: Arc<ImageLoader>,
    mut requests: UnboundedReceiver<String>,
    sink: ImageSink,
) {
    let limit = Arc::new(Semaphore::new(3));
    tokio::spawn(async move {
        while let Some(url) = requests.recv().await {
            let (loader, sink, limit) = (loader.clone(), sink.clone(), limit.clone());
            tokio::spawn(async move {
                let Ok(_permit) = limit.acquire_owned().await else {
                    return;
                };
                let result = loader.load(&url).await;
                if let Err(e) = &result {
                    tracing::warn!("cover {url} failed: {e:#}");
                }
                sink(url, result.ok());
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn covers_are_small_unless_asked_for_large() {
        assert_eq!(
            sized("https://i.scdn.co/a"),
            ("https://i.scdn.co/a", SMALL_PX)
        );
        assert_eq!(
            sized("large:https://i.scdn.co/a"),
            ("https://i.scdn.co/a", LARGE_PX)
        );
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(width, height, image::Rgba([200, 100, 50, 255]));
        let mut out = Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn decode_shrinks_large_images_keeping_aspect() {
        let img = decode(&png(640, 320), 300).unwrap();
        assert_eq!(img.dimensions(), (300, 150));
    }

    #[test]
    fn decode_keeps_small_images() {
        assert_eq!(decode(&png(64, 64), 300).unwrap().dimensions(), (64, 64));
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(decode(b"nope", 300).is_err());
    }

    #[test]
    fn cache_file_name_uses_spotify_image_id() {
        assert_eq!(
            cache_file_name("https://i.scdn.co/image/ab67616d00001e02ff9ca1"),
            "ab67616d00001e02ff9ca1"
        );
        let odd = cache_file_name("https://example.com/");
        assert_eq!(odd.len(), 16, "falls back to a hash: {odd}");
    }
}
