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
}

/// The image URL for an Audiobookshelf book's cover.
pub fn book_cover_url(id: &str) -> String {
    format!("{BOOK_COVER}{id}")
}

const BOOK_COVER: &str = "abs-cover:";

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
        })
    }

    pub fn with_books(mut self, books: crate::audiobooks::service::AbsHandle) -> Self {
        self.books = Some(books);
        self
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
