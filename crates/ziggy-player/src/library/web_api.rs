//! Spotify Web API client: only the endpoints the player needs, parsed defensively.

use std::collections::HashMap;
use std::collections::HashSet;
use std::future::Future;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::{FetchError, LibrarySource};
use crate::model::{
    Account, Collection, CollectionKind, EditOutcome, LIKED_URI, PlaylistEdit, SearchResults,
    Section, Track, liked_collection,
};

pub const API_BASE: &str = "https://api.spotify.com/v1";
/// Keep in sync with `crates/ziggy-setup/src/main.rs`.
pub const SCOPES: &str = "playlist-read-private,playlist-read-collaborative,user-library-read,user-read-recently-played,playlist-modify-private,playlist-modify-public,user-library-modify,user-follow-read,user-follow-modify,user-read-playback-state,user-modify-playback-state";
const MAX_ITEMS: usize = 500;
const RECENT_LIMIT: usize = 20;
/// Spotify answers "Invalid limit" above 10 per type (checked 2026-10-03).
const SEARCH_LIMIT: usize = 10;
/// An artist page shows up to this many albums, fetched 10 at a time.
const MAX_ARTIST_ALBUMS: usize = 50;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum HttpError {
    #[error("HTTP status {0}")]
    Status(u16),
    #[error("network error: {0}")]
    Network(String),
    #[error("invalid response: {0}")]
    Decode(String),
    /// HTTP 429, with the seconds Spotify asked us to wait, if it said.
    #[error("rate limited")]
    RateLimited(Option<u64>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Delete,
}

pub trait Http: Send + Sync + 'static {
    fn get_json(
        &self,
        url: &str,
        token: &str,
    ) -> impl Future<Output = Result<Value, HttpError>> + Send;

    /// A write. An empty response body comes back as `Value::Null`.
    fn send_json(
        &self,
        method: Method,
        url: &str,
        token: &str,
        body: Option<Value>,
    ) -> impl Future<Output = Result<Value, HttpError>> + Send;
}

pub trait TokenSource: Send + Sync + 'static {
    fn token(&self) -> impl Future<Output = Result<String, FetchError>> + Send;

    /// Called after the API rejected the last token, so the next `token()` must not reuse it.
    fn invalidate(&self) {}
}

pub struct ReqwestHttp {
    client: reqwest::Client,
}

impl ReqwestHttp {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()?,
        })
    }
}

fn retry_after(response: &reqwest::Response) -> Option<u64> {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

impl Http for ReqwestHttp {
    async fn get_json(&self, url: &str, token: &str) -> Result<Value, HttpError> {
        let response = self
            .client
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| HttpError::Network(e.to_string()))?;
        let status = response.status();
        if status.as_u16() == 429 {
            return Err(HttpError::RateLimited(retry_after(&response)));
        }
        if !status.is_success() {
            return Err(HttpError::Status(status.as_u16()));
        }
        response
            .json::<Value>()
            .await
            .map_err(|e| HttpError::Decode(e.to_string()))
    }

    async fn send_json(
        &self,
        method: Method,
        url: &str,
        token: &str,
        body: Option<Value>,
    ) -> Result<Value, HttpError> {
        let method = match method {
            Method::Get => reqwest::Method::GET,
            Method::Post => reqwest::Method::POST,
            Method::Put => reqwest::Method::PUT,
            Method::Delete => reqwest::Method::DELETE,
        };
        let mut request = self.client.request(method, url).bearer_auth(token);
        request = match &body {
            Some(body) => request.json(body),
            // Spotify answers 411 to a bodiless PUT unless `Content-Length: 0` is sent; an
            // empty body alone doesn't make reqwest send the header.
            None => request.header(reqwest::header::CONTENT_LENGTH, "0"),
        };
        let response = request
            .send()
            .await
            .map_err(|e| HttpError::Network(e.to_string()))?;
        let status = response.status();
        if status.as_u16() == 429 {
            return Err(HttpError::RateLimited(retry_after(&response)));
        }
        if !status.is_success() {
            return Err(HttpError::Status(status.as_u16()));
        }
        let text = response
            .text()
            .await
            .map_err(|e| HttpError::Network(e.to_string()))?;
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|e| HttpError::Decode(e.to_string()))
    }
}

// ---- Response shapes (only the fields we use; everything optional where Spotify is inconsistent) ----

#[derive(Debug, Deserialize)]
struct Page<T> {
    #[serde(default = "Vec::new")]
    items: Vec<Option<T>>,
    next: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ImageObj {
    pub url: String,
    pub width: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct OwnerObj {
    id: Option<String>,
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ArtistObj {
    name: String,
    uri: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PlaylistObj {
    uri: String,
    name: String,
    #[serde(default)]
    images: Option<Vec<ImageObj>>,
    owner: Option<OwnerObj>,
    snapshot_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ArtistFull {
    uri: String,
    name: String,
    #[serde(default)]
    images: Option<Vec<ImageObj>>,
}

#[derive(Debug, Deserialize)]
struct FollowedObj {
    artists: Page<ArtistFull>,
}

/// Every group is optional: Spotify omits groups it has nothing for.
#[derive(Debug, Deserialize)]
struct SearchObj {
    tracks: Option<Page<TrackObj>>,
    artists: Option<Page<ArtistFull>>,
    albums: Option<Page<AlbumObj>>,
    playlists: Option<Page<PlaylistObj>>,
}

#[derive(Debug, Deserialize)]
struct MeObj {
    id: String,
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AlbumObj {
    uri: String,
    name: String,
    #[serde(default)]
    images: Option<Vec<ImageObj>>,
    #[serde(default)]
    artists: Vec<ArtistObj>,
    tracks: Option<Page<TrackObj>>,
    /// "1986-08-25", "1986-08" or "1986".
    release_date: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AlbumRef {
    uri: Option<String>,
    name: Option<String>,
    #[serde(default)]
    images: Option<Vec<ImageObj>>,
    #[serde(default)]
    artists: Vec<ArtistObj>,
}

#[derive(Debug, Deserialize)]
struct TrackObj {
    uri: Option<String>,
    name: Option<String>,
    #[serde(default)]
    duration_ms: u32,
    #[serde(default)]
    artists: Vec<ArtistObj>,
    album: Option<AlbumRef>,
    #[serde(default)]
    is_local: bool,
}

#[derive(Debug, Deserialize)]
struct SavedAlbum {
    album: AlbumObj,
}

#[derive(Debug, Deserialize)]
struct SavedTrack {
    track: Option<TrackObj>,
}

/// Spotify has used both `track` and `item` for playlist entries.
#[derive(Debug, Deserialize)]
struct PlaylistItem {
    track: Option<TrackObj>,
    item: Option<TrackObj>,
}

#[derive(Debug, Deserialize)]
struct RecentItem {
    track: TrackObj,
    context: Option<ContextObj>,
}

#[derive(Debug, Deserialize)]
struct ContextObj {
    uri: String,
    #[serde(rename = "type")]
    kind: String,
}

// ---- Conversions ----

fn images(images: &Option<Vec<ImageObj>>) -> &[ImageObj] {
    images.as_deref().unwrap_or(&[])
}

/// Smallest image that is at least 250px wide, else the first one listed.
pub(crate) fn pick_image(images: &[ImageObj]) -> Option<String> {
    images
        .iter()
        .filter(|i| i.width.is_some_and(|w| w >= 250))
        .min_by_key(|i| i.width)
        .or_else(|| images.first())
        .map(|i| i.url.clone())
}

fn join_artists(artists: &[ArtistObj]) -> String {
    artists
        .iter()
        .map(|a| a.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn playlist_collection(p: PlaylistObj) -> Collection {
    let (owner_id, owner_name) = match p.owner {
        Some(o) => (o.id, o.display_name),
        None => (None, None),
    };
    Collection {
        image_url: pick_image(images(&p.images)),
        uri: p.uri,
        kind: CollectionKind::Playlist,
        name: p.name,
        subtitle: owner_name.unwrap_or_default(),
        owner_id,
        snapshot_id: p.snapshot_id,
        artist_uri: None,
        year: None,
    }
}

fn playlist_id(uri: &str) -> Result<&str, FetchError> {
    uri.strip_prefix("spotify:playlist:")
        .ok_or_else(|| FetchError::Other(format!("not a playlist: {uri}")))
}

fn artist_id(uri: &str) -> Result<&str, FetchError> {
    uri.strip_prefix("spotify:artist:")
        .ok_or_else(|| FetchError::Other(format!("not an artist: {uri}")))
}

fn artist_collection(a: ArtistFull) -> Collection {
    Collection {
        image_url: pick_image(images(&a.images)),
        uri: a.uri,
        kind: CollectionKind::Artist,
        name: a.name,
        subtitle: "Artist".into(),
        ..Default::default()
    }
}

/// Percent-encodes a query parameter value (RFC 3986 unreserved characters pass through).
fn encode_query(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn search_results(found: SearchObj) -> SearchResults {
    fn items<T>(page: Option<Page<T>>) -> impl Iterator<Item = T> {
        page.into_iter()
            .flat_map(|p| p.items.into_iter().flatten())
            .take(SEARCH_LIMIT)
    }
    SearchResults {
        tracks: items(found.tracks)
            .filter_map(|t| to_track(t, None))
            .collect(),
        artists: items(found.artists).map(artist_collection).collect(),
        albums: items(found.albums).map(|a| album_collection(&a)).collect(),
        playlists: items(found.playlists).map(playlist_collection).collect(),
    }
}

fn album_collection(a: &AlbumObj) -> Collection {
    Collection {
        uri: a.uri.clone(),
        kind: CollectionKind::Album,
        name: a.name.clone(),
        subtitle: join_artists(&a.artists),
        image_url: pick_image(images(&a.images)),
        artist_uri: a.artists.first().and_then(|r| r.uri.clone()),
        year: a
            .release_date
            .as_deref()
            .and_then(|d| d.get(..4))
            .and_then(|y| y.parse().ok()),
        ..Default::default()
    }
}

/// Only real Spotify tracks; local files and podcast episodes are skipped.
/// `album_fallback` is (uri, name, image) for tracks listed under an album, which omit it.
fn to_track(t: TrackObj, album_fallback: Option<(&str, &str, Option<&str>)>) -> Option<Track> {
    let uri = t.uri?;
    if t.is_local || !uri.starts_with("spotify:track:") {
        return None;
    }
    let (album_uri, album, image_url) = match &t.album {
        Some(a) => (
            a.uri.clone(),
            a.name.clone().unwrap_or_default(),
            pick_image(images(&a.images)),
        ),
        None => album_fallback
            .map(|(uri, name, image)| {
                (
                    Some(uri.to_string()),
                    name.to_string(),
                    image.map(str::to_string),
                )
            })
            .unwrap_or_default(),
    };
    Some(Track {
        uri,
        name: t.name.unwrap_or_default(),
        artists: join_artists(&t.artists),
        album,
        image_url,
        duration_ms: t.duration_ms,
        album_uri,
        artist_uri: t.artists.first().and_then(|a| a.uri.clone()),
    })
}

fn recent_collections(items: Vec<RecentItem>, known_playlists: &[Collection]) -> Vec<Collection> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for item in items {
        let collection = match &item.context {
            Some(ctx) if ctx.kind == "playlist" => {
                known_playlists.iter().find(|p| p.uri == ctx.uri).cloned()
            }
            Some(ctx) if ctx.kind == "collection" => Some(liked_collection()),
            _ => item.track.album.as_ref().and_then(|a| {
                Some(Collection {
                    uri: a.uri.clone()?,
                    kind: CollectionKind::Album,
                    name: a.name.clone().unwrap_or_default(),
                    subtitle: join_artists(&a.artists),
                    image_url: pick_image(images(&a.images)),
                    ..Default::default()
                })
            }),
        };
        if let Some(c) = collection {
            if seen.insert(c.uri.clone()) {
                out.push(c);
                if out.len() >= RECENT_LIMIT {
                    break;
                }
            }
        }
    }
    out
}

fn decode<D: DeserializeOwned>(value: Value) -> Result<D, FetchError> {
    serde_json::from_value(value)
        .map_err(|e| FetchError::Other(format!("unexpected response: {e}")))
}

/// Where a list should start.
#[derive(Debug, Clone, PartialEq)]
pub enum StartAt {
    Uri(String),
    Position(u32),
}

// ---- Client ----

pub struct WebApi<H, T> {
    http: H,
    tokens: T,
    base: String,
    /// The playlist list, shared by the Playlists and Recent sections for a short while so
    /// a refresh fetches it once. Locked during the fetch so concurrent callers wait for it.
    playlists_memo: tokio::sync::Mutex<Option<(std::time::Instant, Vec<Collection>)>>,
    blocks: std::sync::Mutex<Blocks>,
    /// Requests sent since start, and when counting started.
    requests: std::sync::Mutex<(u64, std::time::Instant)>,
}

/// Endpoints Spotify told us to stay away from, and until when. Kept on disk so a restart
/// doesn't ask again: every refused request risks a longer ban.
#[derive(Default)]
struct Blocks {
    until: HashMap<String, SystemTime>,
    file: Option<std::path::PathBuf>,
}

impl Blocks {
    fn load(file: std::path::PathBuf) -> Self {
        let until = std::fs::read(&file)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<HashMap<String, u64>>(&bytes).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|(family, secs)| (family, UNIX_EPOCH + Duration::from_secs(secs)))
            .filter(|(_, until)| *until > SystemTime::now())
            .collect();
        Self {
            until,
            file: Some(file),
        }
    }

    fn blocked(&self, family: &str) -> Option<Duration> {
        self.until
            .get(family)
            .and_then(|until| until.duration_since(SystemTime::now()).ok())
    }

    fn block(&mut self, family: String, secs: u64) {
        self.until
            .insert(family, SystemTime::now() + Duration::from_secs(secs));
        let Some(file) = &self.file else { return };
        let saved: HashMap<&String, u64> = self
            .until
            .iter()
            .filter_map(|(f, t)| Some((f, t.duration_since(UNIX_EPOCH).ok()?.as_secs())))
            .collect();
        if let Err(e) = serde_json::to_vec(&saved)
            .map_err(std::io::Error::other)
            .and_then(|json| std::fs::write(file, json))
        {
            tracing::warn!("saving rate-limit blocks failed: {e}");
        }
    }
}

/// Groups URLs by endpoint so one block covers it: the path without the query, with
/// Spotify IDs replaced, e.g. "/playlists/{id}/items".
fn endpoint_family(url: &str) -> String {
    let path = url.split('?').next().unwrap_or(url);
    let path = path.split("/v1").nth(1).unwrap_or(path);
    path.split('/')
        .map(|segment| {
            let is_id = segment.len() == 22 && segment.chars().all(|c| c.is_ascii_alphanumeric());
            if is_id { "{id}" } else { segment }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Requests are counted and logged every this many, to spot overuse early.
const REQUEST_LOG_EVERY: u64 = 50;

/// How long one fetch of the playlist list serves both sections.
const PLAYLISTS_MEMO: Duration = Duration::from_secs(30);
/// The longest Retry-After worth waiting for before giving up on a request.
const MAX_RETRY_AFTER_SECS: u64 = 10;

impl<H: Http, T: TokenSource> WebApi<H, T> {
    pub fn new(http: H, tokens: T) -> Self {
        Self::with_base(http, tokens, API_BASE)
    }

    pub fn with_base(http: H, tokens: T, base: &str) -> Self {
        Self {
            http,
            tokens,
            base: base.to_string(),
            playlists_memo: tokio::sync::Mutex::new(None),
            blocks: std::sync::Mutex::new(Blocks::default()),
            requests: std::sync::Mutex::new((0, std::time::Instant::now())),
        }
    }

    /// Remembers rate-limit blocks in `file` across restarts.
    pub fn with_block_file(self, file: std::path::PathBuf) -> Self {
        *self.blocks.lock().unwrap() = Blocks::load(file);
        self
    }

    fn count_request(&self) {
        let mut requests = self.requests.lock().unwrap();
        requests.0 += 1;
        if requests.0.is_multiple_of(REQUEST_LOG_EVERY) {
            let minutes = requests.1.elapsed().as_secs() / 60;
            tracing::info!("{} Spotify API requests in {minutes} min", requests.0);
        }
    }

    async fn get(&self, path_or_url: &str) -> Result<Value, FetchError> {
        self.request(Method::Get, path_or_url, None).await
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, FetchError> {
        self.request(method, path, body).await
    }

    async fn request(
        &self,
        method: Method,
        path_or_url: &str,
        body: Option<Value>,
    ) -> Result<Value, FetchError> {
        let url = if path_or_url.starts_with("http") {
            path_or_url.to_string()
        } else {
            format!("{}{}", self.base, path_or_url)
        };
        let family = endpoint_family(&url);
        if let Some(left) = self.blocks.lock().unwrap().blocked(&family) {
            tracing::debug!("not asking {family}: blocked for {}s more", left.as_secs());
            return Err(FetchError::RateLimited);
        }
        let mut retried = false;
        let mut waited = false;
        loop {
            self.count_request();
            let token = self.tokens.token().await?;
            let result = match method {
                Method::Get => self.http.get_json(&url, &token).await,
                _ => {
                    self.http
                        .send_json(method, &url, &token, body.clone())
                        .await
                }
            };
            match result {
                Ok(value) => return Ok(value),
                Err(HttpError::Status(401)) if !retried => {
                    retried = true;
                    self.tokens.invalidate();
                }
                Err(HttpError::Status(401)) => return Err(FetchError::Auth),
                // Spotify asks clients to slow down; wait once if the wait is short.
                Err(HttpError::RateLimited(secs)) if !waited => {
                    let secs = secs.unwrap_or(2);
                    if secs > MAX_RETRY_AFTER_SECS {
                        tracing::warn!(
                            "rate limited for {secs}s: {family}; not asking again until then"
                        );
                        self.blocks.lock().unwrap().block(family, secs);
                        return Err(FetchError::RateLimited);
                    }
                    tracing::info!("rate limited; retrying {url} in {secs}s");
                    waited = true;
                    tokio::time::sleep(Duration::from_secs(secs)).await;
                }
                Err(HttpError::RateLimited(_)) => {
                    tracing::warn!("still rate limited: {family}");
                    return Err(FetchError::RateLimited);
                }
                Err(HttpError::Status(403)) => return Err(FetchError::Forbidden),
                Err(HttpError::Status(404)) => return Err(FetchError::NotFound),
                Err(HttpError::Status(code)) => {
                    return Err(FetchError::Other(format!("HTTP {code} for {url}")));
                }
                Err(HttpError::Network(e)) => {
                    tracing::warn!("network error for {url}: {e}");
                    return Err(FetchError::Offline);
                }
                Err(HttpError::Decode(e)) => return Err(FetchError::Other(e)),
            }
        }
    }

    /// The Web API's ID for the Spotify Connect device called `name`, if Spotify lists it.
    pub async fn device_id(&self, name: &str) -> Result<Option<String>, FetchError> {
        let devices = self.get("/me/player/devices").await?;
        Ok(devices["devices"].as_array().and_then(|list| {
            list.iter()
                .find(|d| d["name"].as_str() == Some(name))
                .and_then(|d| d["id"].as_str().map(str::to_string))
        }))
    }

    /// Starts a list on a device at one of its songs, by the song's URI or its position.
    pub async fn play_from(
        &self,
        device_id: &str,
        context_uri: &str,
        start: &StartAt,
    ) -> Result<(), FetchError> {
        let offset = match start {
            StartAt::Uri(uri) => json!({ "uri": uri }),
            StartAt::Position(i) => json!({ "position": i }),
        };
        self.send(
            Method::Put,
            &format!("/me/player/play?device_id={device_id}"),
            Some(json!({ "context_uri": context_uri, "offset": offset, "position_ms": 0 })),
        )
        .await
        .map(|_| ())
    }

    async fn pages<I: DeserializeOwned>(&self, first: &str) -> Result<Vec<I>, FetchError> {
        let mut out = Vec::new();
        let mut next = Some(first.to_string());
        while let Some(url) = next.take() {
            let page: Page<I> = decode(self.get(&url).await?)?;
            out.extend(page.items.into_iter().flatten());
            if out.len() < MAX_ITEMS {
                next = page.next;
            }
        }
        out.truncate(MAX_ITEMS);
        Ok(out)
    }

    async fn playlists(&self) -> Result<Vec<Collection>, FetchError> {
        let mut memo = self.playlists_memo.lock().await;
        if let Some((at, lists)) = memo.as_ref()
            && at.elapsed() < PLAYLISTS_MEMO
        {
            return Ok(lists.clone());
        }
        let items = self.pages::<PlaylistObj>("/me/playlists?limit=50").await?;
        let lists: Vec<Collection> = items.into_iter().map(playlist_collection).collect();
        *memo = Some((std::time::Instant::now(), lists.clone()));
        Ok(lists)
    }

    async fn albums(&self) -> Result<Vec<Collection>, FetchError> {
        let items = self.pages::<SavedAlbum>("/me/albums?limit=50").await?;
        Ok(items.iter().map(|s| album_collection(&s.album)).collect())
    }

    /// Artists the user follows, by name.
    async fn artists(&self) -> Result<Vec<Collection>, FetchError> {
        let mut out = Vec::new();
        let mut next = Some("/me/following?type=artist&limit=50".to_string());
        while let Some(url) = next.take() {
            let page: FollowedObj = decode(self.get(&url).await?)?;
            out.extend(
                page.artists
                    .items
                    .into_iter()
                    .flatten()
                    .map(artist_collection),
            );
            if out.len() < MAX_ITEMS {
                next = page.artists.next;
            }
        }
        out.truncate(MAX_ITEMS);
        out.sort_by_key(|a| a.name.to_lowercase());
        Ok(out)
    }

    async fn recent(&self) -> Result<Vec<Collection>, FetchError> {
        // Without the playlist list (Spotify rate-limits it hard) recent playlists are
        // skipped, but albums and Liked Songs still show.
        let known = match self.playlists().await {
            Ok(known) => known,
            Err(e) => {
                tracing::warn!("recent without playlists: {e}");
                Vec::new()
            }
        };
        // Recently played pages use cursors; the first page (50 plays) is plenty.
        let page: Page<RecentItem> =
            decode(self.get("/me/player/recently-played?limit=50").await?)?;
        Ok(recent_collections(
            page.items.into_iter().flatten().collect(),
            &known,
        ))
    }

    async fn liked_tracks(&self) -> Result<Vec<Track>, FetchError> {
        let items = self.pages::<SavedTrack>("/me/tracks?limit=50").await?;
        Ok(items
            .into_iter()
            .filter_map(|s| s.track.and_then(|t| to_track(t, None)))
            .collect())
    }

    async fn playlist_tracks(&self, id: &str) -> Result<Vec<Track>, FetchError> {
        let items = match self
            .pages::<PlaylistItem>(&format!("/playlists/{id}/items?limit=100"))
            .await
        {
            Err(FetchError::NotFound) => {
                self.pages::<PlaylistItem>(&format!("/playlists/{id}/tracks?limit=100"))
                    .await?
            }
            other => other?,
        };
        Ok(items
            .into_iter()
            .filter_map(|i| i.item.or(i.track))
            .filter_map(|t| to_track(t, None))
            .collect())
    }

    async fn add(&self, playlist_uri: &str, track_uri: &str) -> Result<(), FetchError> {
        let id = playlist_id(playlist_uri)?;
        self.send(
            Method::Post,
            &format!("/playlists/{id}/items"),
            Some(json!({"uris": [track_uri]})),
        )
        .await?;
        Ok(())
    }

    async fn album_tracks(&self, id: &str) -> Result<Vec<Track>, FetchError> {
        let album: AlbumObj = decode(self.get(&format!("/albums/{id}")).await?)?;
        let image = pick_image(images(&album.images));
        let mut raw = Vec::new();
        let mut next = None;
        if let Some(page) = album.tracks {
            raw.extend(page.items.into_iter().flatten());
            next = page.next;
        }
        if let Some(url) = next {
            raw.extend(self.pages::<TrackObj>(&url).await?);
        }
        Ok(raw
            .into_iter()
            .filter_map(|t| to_track(t, Some((&album.uri, &album.name, image.as_deref()))))
            .collect())
    }
}

impl<H: Http, T: TokenSource> LibrarySource for WebApi<H, T> {
    async fn section(&self, section: Section) -> Result<Vec<Collection>, FetchError> {
        match section {
            Section::Playlists => self.playlists().await,
            Section::Albums => self.albums().await,
            Section::Artists => self.artists().await,
            Section::Recent => self.recent().await,
            Section::Liked => Ok(vec![liked_collection()]),
            Section::Search | Section::Settings | Section::Books => Ok(Vec::new()),
        }
    }

    async fn search(&self, query: &str) -> Result<SearchResults, FetchError> {
        let path = format!(
            "/search?type=track,artist,album,playlist&limit={SEARCH_LIMIT}&q={}",
            encode_query(query)
        );
        Ok(search_results(decode(self.get(&path).await?)?))
    }

    async fn artist_albums(&self, artist_uri: &str) -> Result<Vec<Collection>, FetchError> {
        let id = artist_id(artist_uri)?;
        // Spotify answers "Invalid limit" above 10 here (checked 2026-10-03), so page.
        let mut raw: Vec<AlbumObj> = Vec::new();
        let mut next = Some(format!(
            "/artists/{id}/albums?include_groups=album,single,compilation&limit=10"
        ));
        while let Some(url) = next.take() {
            let page: Page<AlbumObj> = decode(self.get(&url).await?)?;
            raw.extend(page.items.into_iter().flatten());
            if raw.len() < MAX_ARTIST_ALBUMS {
                next = page.next;
            }
        }
        // Spotify lists regional editions of the same album separately.
        let mut names = HashSet::new();
        Ok(raw
            .into_iter()
            .filter(|a| names.insert(a.name.to_lowercase()))
            .map(|a| album_collection(&a))
            .collect())
    }

    async fn apply(&self, edit: PlaylistEdit) -> Result<EditOutcome, FetchError> {
        // The reload after an edit must see the change.
        *self.playlists_memo.lock().await = None;
        match edit {
            PlaylistEdit::Add {
                playlist_uri,
                track_uri,
            } => {
                self.add(&playlist_uri, &track_uri).await?;
                Ok(EditOutcome::Done)
            }
            PlaylistEdit::Create { name, track_uri } => {
                let created: PlaylistObj = decode(
                    self.send(
                        Method::Post,
                        "/me/playlists",
                        Some(json!({"name": name, "public": false})),
                    )
                    .await?,
                )?;
                let collection = playlist_collection(created);
                if let Some(track_uri) = track_uri {
                    self.add(&collection.uri, &track_uri).await?;
                }
                Ok(EditOutcome::Created(collection))
            }
            PlaylistEdit::Remove {
                playlist_uri,
                track_uri,
                snapshot_id,
            } => {
                let id = playlist_id(&playlist_uri)?;
                let mut body = json!({"items": [{"uri": track_uri}]});
                if let Some(snapshot) = snapshot_id {
                    body["snapshot_id"] = json!(snapshot);
                }
                self.send(
                    Method::Delete,
                    &format!("/playlists/{id}/items"),
                    Some(body),
                )
                .await?;
                Ok(EditOutcome::Done)
            }
            PlaylistEdit::Move {
                playlist_uri,
                from,
                to,
                snapshot_id,
            } => {
                let id = playlist_id(&playlist_uri)?;
                // Spotify inserts before an index counted in the list as it was.
                let insert_before = if to > from { to + 1 } else { to };
                let mut body = json!({
                    "range_start": from,
                    "insert_before": insert_before,
                    "range_length": 1,
                });
                if let Some(snapshot) = snapshot_id {
                    body["snapshot_id"] = json!(snapshot);
                }
                self.send(Method::Put, &format!("/playlists/{id}/items"), Some(body))
                    .await?;
                Ok(EditOutcome::Done)
            }
            PlaylistEdit::Rename { playlist_uri, name } => {
                let id = playlist_id(&playlist_uri)?;
                self.send(
                    Method::Put,
                    &format!("/playlists/{id}"),
                    Some(json!({"name": name})),
                )
                .await?;
                Ok(EditOutcome::Done)
            }
            PlaylistEdit::Like { track_uri: uri } | PlaylistEdit::SaveAlbum { album_uri: uri } => {
                let path = format!("/me/library?uris={}", encode_query(&uri));
                self.send(Method::Put, &path, None).await?;
                Ok(EditOutcome::Done)
            }
            PlaylistEdit::Unlike { track_uri: uri }
            | PlaylistEdit::UnsaveAlbum { album_uri: uri } => {
                let path = format!("/me/library?uris={}", encode_query(&uri));
                self.send(Method::Delete, &path, None).await?;
                Ok(EditOutcome::Done)
            }
            PlaylistEdit::Follow { artist_uri } => {
                let id = artist_id(&artist_uri)?;
                self.send(
                    Method::Put,
                    &format!("/me/following?type=artist&ids={id}"),
                    None,
                )
                .await?;
                Ok(EditOutcome::Done)
            }
            PlaylistEdit::Unfollow { artist_uri } => {
                let id = artist_id(&artist_uri)?;
                self.send(
                    Method::Delete,
                    &format!("/me/following?type=artist&ids={id}"),
                    None,
                )
                .await?;
                Ok(EditOutcome::Done)
            }
            PlaylistEdit::Delete { playlist_uri } => {
                let id = playlist_id(&playlist_uri)?;
                let library = format!("/me/library?uris={}", encode_query(&playlist_uri));
                match self.send(Method::Delete, &library, None).await {
                    // The older unfollow endpoint, deprecated in 2026.
                    Err(FetchError::NotFound) => {
                        self.send(Method::Delete, &format!("/playlists/{id}/followers"), None)
                            .await?;
                    }
                    other => {
                        other?;
                    }
                }
                Ok(EditOutcome::Done)
            }
        }
    }

    async fn is_liked(&self, track_uri: &str) -> Result<bool, FetchError> {
        let path = format!("/me/library/contains?uris={}", encode_query(track_uri));
        let found: Vec<bool> = decode(self.get(&path).await?)?;
        Ok(found.first().copied().unwrap_or(false))
    }

    async fn account(&self) -> Result<Account, FetchError> {
        let me: MeObj = decode(self.get("/me").await?)?;
        let name = me
            .display_name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| me.id.clone());
        Ok(Account { id: me.id, name })
    }

    async fn tracks(&self, collection_uri: &str) -> Result<Vec<Track>, FetchError> {
        if collection_uri == LIKED_URI {
            return self.liked_tracks().await;
        }
        let parts: Vec<&str> = collection_uri.split(':').collect();
        match parts.as_slice() {
            ["spotify", "playlist", id] => self.playlist_tracks(id).await,
            ["spotify", "album", id] => self.album_tracks(id).await,
            _ => Err(FetchError::Other(format!(
                "unsupported collection {collection_uri}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    use serde_json::json;

    use super::*;

    const BASE: &str = "https://api.test/v1";

    #[derive(Default)]
    struct FakeHttp {
        responses: Mutex<HashMap<String, VecDeque<Result<Value, HttpError>>>>,
        calls: Mutex<Vec<(String, String)>>,
        sent: Mutex<Vec<(Method, String, Option<Value>)>>,
    }

    impl FakeHttp {
        fn on(self, path: &str, response: Result<Value, HttpError>) -> Self {
            let url = if path.starts_with("http") {
                path.to_string()
            } else {
                format!("{BASE}{path}")
            };
            self.responses
                .lock()
                .unwrap()
                .entry(url)
                .or_default()
                .push_back(response);
            self
        }

        fn on_send(self, method: Method, path: &str, response: Result<Value, HttpError>) -> Self {
            self.responses
                .lock()
                .unwrap()
                .entry(format!("{method:?} {BASE}{path}"))
                .or_default()
                .push_back(response);
            self
        }

        fn sent(&self) -> Vec<(Method, String, Option<Value>)> {
            self.sent.lock().unwrap().clone()
        }
    }

    impl Http for FakeHttp {
        async fn send_json(
            &self,
            method: Method,
            url: &str,
            token: &str,
            body: Option<Value>,
        ) -> Result<Value, HttpError> {
            self.sent
                .lock()
                .unwrap()
                .push((method, url.to_string(), body));
            self.calls
                .lock()
                .unwrap()
                .push((url.to_string(), token.to_string()));
            self.responses
                .lock()
                .unwrap()
                .get_mut(&format!("{method:?} {url}"))
                .and_then(|queue| queue.pop_front())
                .unwrap_or(Err(HttpError::Status(404)))
        }

        async fn get_json(&self, url: &str, token: &str) -> Result<Value, HttpError> {
            self.calls
                .lock()
                .unwrap()
                .push((url.to_string(), token.to_string()));
            self.responses
                .lock()
                .unwrap()
                .get_mut(url)
                .and_then(|queue| queue.pop_front())
                .unwrap_or(Err(HttpError::Status(404)))
        }
    }

    #[derive(Default)]
    struct CountingTokens(AtomicU32, AtomicU32);

    impl TokenSource for CountingTokens {
        fn invalidate(&self) {
            self.1.fetch_add(1, Ordering::SeqCst);
        }

        async fn token(&self) -> Result<String, FetchError> {
            Ok(format!("t{}", self.0.fetch_add(1, Ordering::SeqCst) + 1))
        }
    }

    fn api(http: FakeHttp) -> WebApi<FakeHttp, CountingTokens> {
        WebApi::with_base(http, CountingTokens::default(), BASE)
    }

    fn api_after_restart(http: FakeHttp) -> WebApi<FakeHttp, CountingTokens> {
        api(http)
    }

    fn img(url: &str, width: Option<u32>) -> ImageObj {
        ImageObj {
            url: url.into(),
            width,
        }
    }

    #[test]
    fn pick_image_prefers_smallest_at_least_250() {
        let images = [
            img("big", Some(640)),
            img("mid", Some(300)),
            img("small", Some(64)),
        ];
        assert_eq!(pick_image(&images).as_deref(), Some("mid"));
        assert_eq!(pick_image(&[img("only", None)]).as_deref(), Some("only"));
        assert_eq!(
            pick_image(&[img("tiny", Some(64))]).as_deref(),
            Some("tiny")
        );
        assert_eq!(pick_image(&[]), None);
    }

    #[tokio::test]
    async fn playlists_follow_pagination_and_tolerate_null_images() {
        let http = FakeHttp::default()
            .on(
                "/me/playlists?limit=50",
                Ok(json!({
                    "items": [{"uri": "spotify:playlist:a", "name": "A", "images": null, "owner": {"display_name": "Mum"}}],
                    "next": "https://api.test/v1/me/playlists?offset=1"
                })),
            )
            .on(
                "https://api.test/v1/me/playlists?offset=1",
                Ok(json!({
                    "items": [null, {"uri": "spotify:playlist:b", "name": "B", "images": [{"url": "u", "width": 300}], "owner": null}],
                    "next": null
                })),
            );
        let result = api(http).section(Section::Playlists).await.unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].subtitle, "Mum");
        assert_eq!(result[0].image_url, None);
        assert_eq!(result[1].image_url.as_deref(), Some("u"));
        assert_eq!(result[1].kind, CollectionKind::Playlist);
    }

    // Review focus 3: odd playlist entries.
    #[tokio::test]
    async fn playlist_tracks_skip_null_local_and_episodes() {
        let http = FakeHttp::default().on(
            "/playlists/p1/items?limit=100",
            Ok(json!({
                "items": [
                    {"track": null},
                    {"track": {"uri": "spotify:local:x", "name": "Local", "is_local": true, "duration_ms": 1, "artists": []}},
                    {"item": {"uri": "spotify:episode:e", "name": "Podcast", "duration_ms": 1}},
                    {"item": {"uri": "spotify:track:t1", "name": "Song", "duration_ms": 61000,
                              "artists": [{"name": "A"}, {"name": "B"}],
                              "album": {"uri": "spotify:album:al", "name": "Al", "images": [{"url": "cover", "width": 300}]}}}
                ],
                "next": null
            })),
        );
        let tracks = api(http).tracks("spotify:playlist:p1").await.unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].artists, "A, B");
        assert_eq!(tracks[0].album, "Al");
        assert_eq!(tracks[0].image_url.as_deref(), Some("cover"));
    }

    #[tokio::test]
    async fn playlist_tracks_fall_back_to_tracks_endpoint() {
        let http = FakeHttp::default().on(
            "/playlists/p1/tracks?limit=100",
            Ok(json!({"items": [{"track": {"uri": "spotify:track:t1", "name": "Song", "duration_ms": 1, "artists": []}}], "next": null})),
        );
        let tracks = api(http).tracks("spotify:playlist:p1").await.unwrap();
        assert_eq!(tracks.len(), 1);
    }

    #[tokio::test]
    async fn albums_and_album_tracks() {
        let http = FakeHttp::default()
            .on(
                "/me/albums?limit=50",
                Ok(json!({"items": [{"album": {"uri": "spotify:album:a1", "name": "Moana",
                    "images": [{"url": "m", "width": 300}], "artists": [{"name": "Various"}]}}], "next": null})),
            )
            .on(
                "/albums/a1",
                Ok(json!({"uri": "spotify:album:a1", "name": "Moana", "images": [{"url": "m", "width": 300}],
                    "artists": [], "tracks": {"items": [{"uri": "spotify:track:t1", "name": "How Far", "duration_ms": 2, "artists": [{"name": "Auli'i"}]}],
                    "next": "https://api.test/v1/albums/a1/tracks?offset=1"}})),
            )
            .on(
                "https://api.test/v1/albums/a1/tracks?offset=1",
                Ok(json!({"items": [{"uri": "spotify:track:t2", "name": "Shiny", "duration_ms": 3, "artists": []}], "next": null})),
            );
        let api = api(http);
        let albums = api.section(Section::Albums).await.unwrap();
        assert_eq!(albums[0].subtitle, "Various");
        let tracks = api.tracks("spotify:album:a1").await.unwrap();
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[1].album, "Moana");
        assert_eq!(tracks[1].image_url.as_deref(), Some("m"));
    }

    #[tokio::test]
    async fn liked_tracks_use_me_tracks() {
        let http = FakeHttp::default().on(
            "/me/tracks?limit=50",
            Ok(json!({"items": [{"track": {"uri": "spotify:track:t1", "name": "S", "duration_ms": 1, "artists": []}}], "next": null})),
        );
        assert_eq!(api(http).tracks(LIKED_URI).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn search_maps_every_group_and_skips_null_entries() {
        let http = FakeHttp::default().on(
            "/search?type=track,artist,album,playlist&limit=10&q=a%20b%26c",
            Ok(json!({
                "tracks": {"items": [{"uri": "spotify:track:t1", "name": "Song", "duration_ms": 1,
                    "artists": [{"name": "Band"}],
                    "album": {"uri": "spotify:album:a1", "name": "Album", "images": []}}], "next": null},
                "artists": {"items": [{"uri": "spotify:artist:r1", "name": "Band", "images": []}], "next": null},
                "albums": {"items": [{"uri": "spotify:album:a1", "name": "Album", "artists": [{"name": "Band"}]}], "next": null},
                "playlists": {"items": [null, {"uri": "spotify:playlist:p1", "name": "Mix",
                    "owner": {"display_name": "Sam"}}, null], "next": null}
            })),
        );
        let found = api(http).search("a b&c").await.unwrap();
        assert_eq!(found.tracks.len(), 1);
        assert_eq!(
            found.tracks[0].album_uri.as_deref(),
            Some("spotify:album:a1")
        );
        assert_eq!(found.artists[0].kind, CollectionKind::Artist);
        assert_eq!(found.albums[0].subtitle, "Band");
        assert_eq!(found.playlists.len(), 1);
        assert_eq!(found.playlists[0].subtitle, "Sam");
    }

    #[tokio::test]
    async fn forbidden_playlist_items_are_reported_as_forbidden() {
        let http =
            FakeHttp::default().on("/playlists/p1/items?limit=100", Err(HttpError::Status(403)));
        assert_eq!(
            api(http).tracks("spotify:playlist:p1").await,
            Err(FetchError::Forbidden)
        );
    }

    #[test]
    fn albums_take_the_year_from_any_release_date_precision() {
        let album = |date: &str| {
            album_collection(&AlbumObj {
                uri: "spotify:album:a".into(),
                name: "A".into(),
                images: None,
                artists: vec![],
                tracks: None,
                release_date: Some(date.into()),
            })
        };
        assert_eq!(album("1986-08-25").year, Some(1986));
        assert_eq!(album("1969").year, Some(1969));
        assert_eq!(album("").year, None);
    }

    #[tokio::test]
    async fn artist_albums_drop_duplicate_names() {
        let http = FakeHttp::default()
            .on(
                "/artists/r1/albums?include_groups=album,single,compilation&limit=10",
                Ok(json!({"items": [
                    {"uri": "spotify:album:a1", "name": "Abbey Road", "artists": []},
                    {"uri": "spotify:album:a2", "name": "Abbey Road", "artists": []}
                ], "next": "https://api.test/v1/artists/r1/albums?offset=10&limit=10"})),
            )
            .on(
                "https://api.test/v1/artists/r1/albums?offset=10&limit=10",
                Ok(json!({"items": [
                    {"uri": "spotify:album:a3", "name": "Help!", "artists": []}
                ], "next": null})),
            );
        let albums = api(http).artist_albums("spotify:artist:r1").await.unwrap();
        let names: Vec<_> = albums.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Abbey Road", "Help!"]);
    }

    fn ok_snapshot() -> Result<Value, HttpError> {
        Ok(json!({"snapshot_id": "s2"}))
    }

    #[tokio::test]
    async fn playlists_record_owner_and_snapshot() {
        let http = FakeHttp::default().on(
            "/me/playlists?limit=50",
            Ok(json!({"items": [{"uri": "spotify:playlist:p1", "name": "Mix",
                "owner": {"id": "me1", "display_name": "Sam"}, "snapshot_id": "s1"}], "next": null})),
        );
        let lists = api(http).section(Section::Playlists).await.unwrap();
        assert_eq!(lists[0].owner_id.as_deref(), Some("me1"));
        assert_eq!(lists[0].snapshot_id.as_deref(), Some("s1"));
    }

    #[tokio::test]
    async fn create_makes_a_private_playlist_then_adds_the_song() {
        let http = FakeHttp::default()
            .on_send(
                Method::Post,
                "/me/playlists",
                Ok(json!({"uri": "spotify:playlist:new", "name": "Road",
                    "owner": {"id": "me1", "display_name": "Sam"}})),
            )
            .on_send(Method::Post, "/playlists/new/items", ok_snapshot());
        let api = api(http);
        let outcome = api
            .apply(PlaylistEdit::Create {
                name: "Road".into(),
                track_uri: Some("spotify:track:t1".into()),
            })
            .await
            .unwrap();
        let EditOutcome::Created(c) = outcome else {
            panic!("expected Created")
        };
        assert_eq!(c.uri, "spotify:playlist:new");
        assert_eq!(c.owner_id.as_deref(), Some("me1"));
        let sent = api.http.sent();
        assert_eq!(sent[0].2, Some(json!({"name": "Road", "public": false})));
        assert_eq!(sent[1].2, Some(json!({"uris": ["spotify:track:t1"]})));
    }

    #[tokio::test]
    async fn remove_and_move_send_snapshots() {
        let http = FakeHttp::default()
            .on_send(Method::Delete, "/playlists/p1/items", ok_snapshot())
            .on_send(Method::Put, "/playlists/p1/items", ok_snapshot())
            .on_send(Method::Put, "/playlists/p1/items", ok_snapshot());
        let api = api(http);
        api.apply(PlaylistEdit::Remove {
            playlist_uri: "spotify:playlist:p1".into(),
            track_uri: "spotify:track:t1".into(),
            snapshot_id: Some("s1".into()),
        })
        .await
        .unwrap();
        for (from, to) in [(1, 4), (4, 1)] {
            api.apply(PlaylistEdit::Move {
                playlist_uri: "spotify:playlist:p1".into(),
                from,
                to,
                snapshot_id: None,
            })
            .await
            .unwrap();
        }
        let sent = api.http.sent();
        assert_eq!(
            sent[0].2,
            Some(json!({"items": [{"uri": "spotify:track:t1"}], "snapshot_id": "s1"}))
        );
        // Moving down inserts after the target; moving up inserts at it.
        assert_eq!(sent[1].2.as_ref().unwrap()["insert_before"], 5);
        assert_eq!(sent[2].2.as_ref().unwrap()["insert_before"], 1);
    }

    #[tokio::test]
    async fn rename_puts_the_name() {
        let http = FakeHttp::default().on_send(Method::Put, "/playlists/p1", Ok(Value::Null));
        let api = api(http);
        api.apply(PlaylistEdit::Rename {
            playlist_uri: "spotify:playlist:p1".into(),
            name: "New".into(),
        })
        .await
        .unwrap();
        assert_eq!(api.http.sent()[0].2, Some(json!({"name": "New"})));
    }

    #[tokio::test]
    async fn delete_uses_the_library_then_falls_back_to_unfollow() {
        let http = FakeHttp::default()
            .on_send(
                Method::Delete,
                "/me/library?uris=spotify%3Aplaylist%3Ap1",
                Err(HttpError::Status(404)),
            )
            .on_send(Method::Delete, "/playlists/p1/followers", Ok(Value::Null));
        let api = api(http);
        api.apply(PlaylistEdit::Delete {
            playlist_uri: "spotify:playlist:p1".into(),
        })
        .await
        .unwrap();
        assert_eq!(api.http.sent().len(), 2);
    }

    #[tokio::test]
    async fn like_unlike_and_check_use_the_library() {
        let uris = "uris=spotify%3Atrack%3At1";
        let http = FakeHttp::default()
            .on_send(Method::Put, &format!("/me/library?{uris}"), Ok(Value::Null))
            .on_send(
                Method::Delete,
                &format!("/me/library?{uris}"),
                Ok(Value::Null),
            )
            .on(&format!("/me/library/contains?{uris}"), Ok(json!([true])));
        let api = api(http);
        let track_uri = "spotify:track:t1".to_string();
        api.apply(PlaylistEdit::Like {
            track_uri: track_uri.clone(),
        })
        .await
        .unwrap();
        api.apply(PlaylistEdit::Unlike {
            track_uri: track_uri.clone(),
        })
        .await
        .unwrap();
        assert!(api.is_liked(&track_uri).await.unwrap());
        let methods: Vec<Method> = api.http.sent().iter().map(|s| s.0).collect();
        assert_eq!(methods, [Method::Put, Method::Delete]);
    }

    #[tokio::test]
    async fn playlists_and_recent_share_one_fetch() {
        let http = FakeHttp::default()
            .on(
                "/me/playlists?limit=50",
                Ok(json!({"items": [], "next": null})),
            )
            .on(
                "/me/player/recently-played?limit=50",
                Ok(json!({"items": [], "next": null})),
            );
        let api = api(http);
        api.section(Section::Playlists).await.unwrap();
        api.section(Section::Recent).await.unwrap();
        let fetches = api
            .http
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(url, _)| url.contains("/me/playlists"))
            .count();
        assert_eq!(fetches, 1);
    }

    #[tokio::test]
    async fn rate_limit_waits_once_then_gives_up() {
        let http = FakeHttp::default()
            .on("/me/albums?limit=50", Err(HttpError::RateLimited(Some(0))))
            .on(
                "/me/albums?limit=50",
                Ok(json!({"items": [], "next": null})),
            );
        assert!(api(http).section(Section::Albums).await.is_ok());

        let http = FakeHttp::default()
            .on("/me/albums?limit=50", Err(HttpError::RateLimited(Some(0))))
            .on("/me/albums?limit=50", Err(HttpError::RateLimited(Some(0))));
        assert!(api(http).section(Section::Albums).await.is_err());

        let http = FakeHttp::default().on(
            "/me/albums?limit=50",
            Err(HttpError::RateLimited(Some(3600))),
        );
        assert!(api(http).section(Section::Albums).await.is_err());
    }

    #[tokio::test]
    async fn followed_artists_follow_pages_and_sort_by_name() {
        let http = FakeHttp::default()
            .on(
                "/me/following?type=artist&limit=50",
                Ok(json!({"artists": {"items": [{"uri": "spotify:artist:b", "name": "beta", "images": []}],
                    "next": "https://api.test/v1/me/following?type=artist&limit=50&after=b"}})),
            )
            .on(
                "https://api.test/v1/me/following?type=artist&limit=50&after=b",
                Ok(json!({"artists": {"items": [{"uri": "spotify:artist:a", "name": "Alpha", "images": []}],
                    "next": null}})),
            );
        let artists = api(http).section(Section::Artists).await.unwrap();
        let names: Vec<_> = artists.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Alpha", "beta"]);
        assert_eq!(artists[0].kind, CollectionKind::Artist);
    }

    #[tokio::test]
    async fn follow_and_unfollow_use_the_following_endpoint() {
        let path = "/me/following?type=artist&ids=r1";
        let http = FakeHttp::default()
            .on_send(Method::Put, path, Ok(Value::Null))
            .on_send(Method::Delete, path, Ok(Value::Null));
        let api = api(http);
        let artist_uri = "spotify:artist:r1".to_string();
        api.apply(PlaylistEdit::Follow {
            artist_uri: artist_uri.clone(),
        })
        .await
        .unwrap();
        api.apply(PlaylistEdit::Unfollow { artist_uri })
            .await
            .unwrap();
        assert_eq!(api.http.sent().len(), 2);
    }

    #[test]
    fn endpoint_families_hide_ids_and_queries() {
        assert_eq!(
            endpoint_family(
                "https://api.spotify.com/v1/playlists/7Fr4Jx8s7WNaKHYDgh048x/items?limit=100"
            ),
            "/playlists/{id}/items"
        );
        assert_eq!(
            endpoint_family("https://api.spotify.com/v1/me/playlists?limit=50"),
            "/me/playlists"
        );
    }

    #[tokio::test]
    async fn a_long_block_stops_further_requests_and_survives_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("blocks.json");
        let http = FakeHttp::default().on(
            "/me/albums?limit=50",
            Err(HttpError::RateLimited(Some(80_000))),
        );
        let api = api(http).with_block_file(file.clone());
        assert_eq!(
            api.section(Section::Albums).await,
            Err(FetchError::RateLimited)
        );
        assert_eq!(
            api.section(Section::Albums).await,
            Err(FetchError::RateLimited)
        );
        assert_eq!(
            api.http.calls.lock().unwrap().len(),
            1,
            "asked Spotify once"
        );

        // A fresh client (a restart) reads the block and doesn't ask either.
        let again = api_after_restart(FakeHttp::default()).with_block_file(file);
        assert_eq!(
            again.section(Section::Albums).await,
            Err(FetchError::RateLimited)
        );
        assert!(again.http.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn refused_write_is_forbidden() {
        let http = FakeHttp::default().on_send(
            Method::Post,
            "/playlists/p1/items",
            Err(HttpError::Status(403)),
        );
        let result = api(http)
            .apply(PlaylistEdit::Add {
                playlist_uri: "spotify:playlist:p1".into(),
                track_uri: "spotify:track:t1".into(),
            })
            .await;
        assert_eq!(result, Err(FetchError::Forbidden));
    }

    #[tokio::test]
    async fn account_uses_display_name_or_falls_back_to_id() {
        let http = FakeHttp::default()
            .on("/me", Ok(json!({"id": "1255644", "display_name": "Sam"})))
            .on("/me", Ok(json!({"id": "1255644", "display_name": null})));
        let api = api(http);
        assert_eq!(api.account().await.unwrap().name, "Sam");
        let fallback = api.account().await.unwrap();
        assert_eq!(
            (fallback.id.as_str(), fallback.name.as_str()),
            ("1255644", "1255644")
        );
    }

    #[tokio::test]
    async fn recent_dedupes_contexts_and_maps_liked() {
        let album = json!({"uri": "spotify:album:a1", "name": "Al", "images": [], "artists": [{"name": "X"}]});
        let t = |ctx: Value| json!({"track": {"uri": "spotify:track:t", "name": "S", "duration_ms": 1, "artists": [], "album": album}, "context": ctx});
        let http = FakeHttp::default()
            .on(
                "/me/playlists?limit=50",
                Ok(json!({"items": [{"uri": "spotify:playlist:p1", "name": "Mine", "images": [], "owner": null}], "next": null})),
            )
            .on(
                "/me/player/recently-played?limit=50",
                Ok(json!({"items": [
                    t(json!({"type": "playlist", "uri": "spotify:playlist:p1"})),
                    t(json!({"type": "playlist", "uri": "spotify:playlist:p1"})),
                    t(json!({"type": "playlist", "uri": "spotify:playlist:unknown"})),
                    t(json!({"type": "collection", "uri": "spotify:user:kid:collection"})),
                    t(json!({"type": "album", "uri": "spotify:album:a1"})),
                    t(Value::Null)
                ], "next": null})),
            );
        let recent = api(http).section(Section::Recent).await.unwrap();
        let uris: Vec<_> = recent.iter().map(|c| c.uri.as_str()).collect();
        assert_eq!(
            uris,
            vec!["spotify:playlist:p1", LIKED_URI, "spotify:album:a1"]
        );
    }

    // Review focus 4: expired access tokens.
    #[tokio::test]
    async fn unauthorized_retries_once_with_fresh_token() {
        let http = FakeHttp::default()
            .on("/me/tracks?limit=50", Err(HttpError::Status(401)))
            .on(
                "/me/tracks?limit=50",
                Ok(json!({"items": [], "next": null})),
            );
        let api = api(http);
        assert_eq!(api.tracks(LIKED_URI).await.unwrap(), vec![]);
        let calls = api.http.calls.lock().unwrap().clone();
        assert_eq!(
            calls.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>(),
            vec!["t1", "t2"]
        );
        assert_eq!(api.tokens.1.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn repeated_unauthorized_is_auth_error_and_network_is_offline() {
        let http = FakeHttp::default()
            .on("/me/tracks?limit=50", Err(HttpError::Status(401)))
            .on("/me/tracks?limit=50", Err(HttpError::Status(401)))
            .on("/me/albums?limit=50", Err(HttpError::Network("dns".into())));
        let api = api(http);
        assert_eq!(api.tracks(LIKED_URI).await, Err(FetchError::Auth));
        assert_eq!(api.section(Section::Albums).await, Err(FetchError::Offline));
    }
}
