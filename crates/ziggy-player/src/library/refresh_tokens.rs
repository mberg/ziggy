//! Web API access tokens from the parent's own Spotify developer app (Contingency A).
//!
//! `ziggy-setup auth-web` saves `web-auth.json` with the app's client id and a refresh token.
//! Spotify may rotate the refresh token on every refresh, so the new one is written back.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::FetchError;
use super::web_api::TokenSource;

const TOKEN_URL: &str = "https://accounts.spotify.com/api/token";

/// Must match the redirect URI registered on the developer app and used by `ziggy-setup`.
pub const REDIRECT_URI: &str = "http://127.0.0.1:8898/login";
pub const FILE_NAME: &str = "web-auth.json";

#[derive(Clone, Serialize, Deserialize)]
pub struct WebAuth {
    pub client_id: String,
    pub refresh_token: String,
}

/// Spotify's answer to a refresh. It may hand back a new refresh token.
#[derive(Deserialize)]
struct Refreshed {
    access_token: String,
    expires_in: u64,
    #[serde(default)]
    refresh_token: Option<String>,
}

/// Swaps the refresh token for an access token. The developer app is a public client (PKCE), so
/// no secret is sent.
async fn refresh(client_id: &str, refresh_token: &str) -> Result<Refreshed, FetchError> {
    let response = reqwest::Client::new()
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
        ])
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| {
            tracing::warn!("web token refresh failed: {e}");
            FetchError::Offline
        })?;
    let status = response.status();
    if status == reqwest::StatusCode::BAD_REQUEST || status == reqwest::StatusCode::UNAUTHORIZED {
        // invalid_grant: the sign-in was revoked or expired; `ziggy signin` fixes it.
        tracing::warn!(
            "Spotify refused the saved sign-in: {}",
            response.text().await.unwrap_or_default()
        );
        return Err(FetchError::Auth);
    }
    if !status.is_success() {
        tracing::warn!("web token refresh failed: HTTP {status}");
        return Err(FetchError::Offline);
    }
    response
        .json::<Refreshed>()
        .await
        .map_err(|e| FetchError::Other(e.to_string()))
}

struct Cached {
    token: String,
    expires: Instant,
}

pub struct RefreshTokens {
    path: PathBuf,
    auth: Mutex<WebAuth>,
    cached: Mutex<Option<Cached>>,
}

impl RefreshTokens {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let auth: WebAuth = serde_json::from_slice(&std::fs::read(path)?)?;
        Ok(Self {
            path: path.to_path_buf(),
            auth: Mutex::new(auth),
            cached: Mutex::new(None),
        })
    }

    /// No sign-in saved: every request reports that sign-in is needed.
    pub fn missing(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            auth: Mutex::new(WebAuth {
                client_id: String::new(),
                refresh_token: String::new(),
            }),
            cached: Mutex::new(None),
        }
    }

    fn cached_token(&self) -> Option<String> {
        let cached = self.cached.lock().unwrap();
        cached
            .as_ref()
            .filter(|c| Instant::now() + Duration::from_secs(60) < c.expires)
            .map(|c| c.token.clone())
    }
}

impl TokenSource for RefreshTokens {
    async fn token(&self) -> Result<String, FetchError> {
        if let Some(token) = self.cached_token() {
            return Ok(token);
        }
        let WebAuth {
            client_id,
            refresh_token,
        } = self.auth.lock().unwrap().clone();
        if client_id.is_empty() {
            return Err(FetchError::Auth);
        }
        let fresh = refresh(&client_id, &refresh_token).await?;

        if let Some(rotated) = fresh.refresh_token.filter(|t| !t.is_empty()) {
            let mut auth = self.auth.lock().unwrap();
            if auth.refresh_token != rotated {
                auth.refresh_token = rotated;
                if let Err(e) = serde_json::to_vec_pretty(&*auth)
                    .map_err(anyhow::Error::from)
                    .and_then(|json| std::fs::write(&self.path, json).map_err(Into::into))
                {
                    tracing::warn!("could not save rotated refresh token: {e}");
                }
            }
        }
        *self.cached.lock().unwrap() = Some(Cached {
            token: fresh.access_token.clone(),
            expires: Instant::now() + Duration::from_secs(fresh.expires_in),
        });
        Ok(fresh.access_token)
    }

    fn invalidate(&self) {
        *self.cached.lock().unwrap() = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(dir: &Path) -> RefreshTokens {
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, r#"{"client_id":"abc","refresh_token":"r1"}"#).unwrap();
        RefreshTokens::load(&path).unwrap()
    }

    #[test]
    fn loads_the_saved_file() {
        let dir = tempfile::tempdir().unwrap();
        let t = tokens(dir.path());
        let auth = t.auth.lock().unwrap();
        assert_eq!(
            (auth.client_id.as_str(), auth.refresh_token.as_str()),
            ("abc", "r1")
        );
    }

    #[test]
    fn missing_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(RefreshTokens::load(&dir.path().join(FILE_NAME)).is_err());
    }

    #[test]
    fn cached_token_is_reused_until_near_expiry_and_cleared_by_invalidate() {
        let dir = tempfile::tempdir().unwrap();
        let t = tokens(dir.path());
        *t.cached.lock().unwrap() = Some(Cached {
            token: "tok".into(),
            expires: Instant::now() + Duration::from_secs(3600),
        });
        assert_eq!(t.cached_token().as_deref(), Some("tok"));
        *t.cached.lock().unwrap() = Some(Cached {
            token: "tok".into(),
            expires: Instant::now() + Duration::from_secs(30),
        });
        assert_eq!(t.cached_token(), None);
        t.invalidate();
        assert!(t.cached.lock().unwrap().is_none());
    }
}
