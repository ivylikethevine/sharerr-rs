//! One HTTP client for both apps.
//!
//! Sonarr and Radarr expose the same v3 shape — `X-Api-Key` auth, `/api/v3/` base,
//! `/tag` and `/system/status` identical — so the transport lives here once and
//! [`crate::sonarr`] / [`crate::radarr`] only differ in which resources they walk.

use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;
use sharerr_core::MediaSource;
use url::Url;

use crate::error::{ArrError, Result};
use crate::models::{SystemStatus, Tag};
use crate::{Discovered, radarr, sonarr};

const API_PREFIX: &str = "api/v3/";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Enough of an error body to identify the problem, bounded so a stray HTML error
/// page from a misconfigured reverse proxy does not flood the logs.
const MAX_ERROR_BODY: usize = 400;

pub struct ArrClient {
    kind: MediaSource,
    /// Always ends in `/`, so `Url::join` appends rather than replacing the last
    /// segment. This is what makes reverse-proxy subpaths (`http://host/sonarr/`) work.
    base: Url,
    api_key: SecretString,
    http: reqwest::Client,
}

/// The API key must never reach a log line, and `Config` is logged wholesale at
/// debug level, so this is a real hazard rather than a theoretical one.
impl std::fmt::Debug for ArrClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArrClient")
            .field("kind", &self.kind)
            .field("base", &self.base.as_str())
            .field("api_key", &"<redacted>")
            .finish()
    }
}

impl ArrClient {
    pub fn new(kind: MediaSource, base: &Url, api_key: SecretString) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(DEFAULT_TIMEOUT)
            .build()
            .map_err(ArrError::Client)?;
        Self::with_http(kind, base, api_key, http)
    }

    /// Same as [`Self::new`] but with a caller-supplied client, so tests can point
    /// several clients at one wiremock server without rebuilding TLS state.
    pub fn with_http(
        kind: MediaSource,
        base: &Url,
        api_key: SecretString,
        http: reqwest::Client,
    ) -> Result<Self> {
        let mut base = base.clone();
        if !base.path().ends_with('/') {
            let with_slash = format!("{}/", base.path());
            base.set_path(&with_slash);
        }
        Ok(Self {
            kind,
            base,
            api_key,
            http,
        })
    }

    pub fn kind(&self) -> MediaSource {
        self.kind
    }

    pub fn base_url(&self) -> &Url {
        &self.base
    }

    fn endpoint(&self, path: &str) -> Result<Url> {
        self.base
            .join(API_PREFIX)
            .and_then(|u| u.join(path))
            .map_err(|source| ArrError::Url {
                service: self.kind,
                source,
            })
    }

    /// The only place the API key is read. Everything else goes through here, so
    /// there is exactly one line to audit.
    async fn get<T: DeserializeOwned>(&self, path: &str, query: &[(&str, String)]) -> Result<T> {
        let url = self.endpoint(path)?;

        let response = self
            .http
            .get(url.clone())
            .header("X-Api-Key", self.api_key.expose_secret())
            .query(query)
            .send()
            .await
            .map_err(|source| ArrError::Unreachable {
                service: self.kind,
                url: url.to_string(),
                source,
            })?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(ArrError::Unauthorized {
                service: self.kind,
                status: status.as_u16(),
            });
        }

        if !status.is_success() {
            let body = clamp_body(&response.text().await.unwrap_or_default());
            return Err(ArrError::Status {
                service: self.kind,
                status: status.as_u16(),
                path: path.to_owned(),
                body,
            });
        }

        // Decode from bytes rather than `response.json()` so a malformed payload
        // reports which endpoint produced it.
        let bytes = response
            .bytes()
            .await
            .map_err(|source| ArrError::Unreachable {
                service: self.kind,
                url: url.to_string(),
                source,
            })?;

        serde_json::from_slice(&bytes).map_err(|source| ArrError::Decode {
            service: self.kind,
            path: path.to_owned(),
            source,
        })
    }

    pub(crate) async fn get_list<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<Vec<T>> {
        self.get(path, query).await
    }

    /// Cheap liveness + auth probe for `doctor`.
    pub async fn system_status(&self) -> Result<SystemStatus> {
        self.get("system/status", &[]).await
    }

    /// Resolve a tag label to its numeric id.
    ///
    /// Matching is case-insensitive: Sonarr lowercases labels on save, so an
    /// operator who typed `Sharerr` in the UI and `sharerr` in the config would
    /// otherwise get a silent no-op — the exact failure mode this project treats as
    /// the most likely source of "sharerr does nothing" reports.
    pub async fn tag_id(&self, label: &str) -> Result<i64> {
        let tags: Vec<Tag> = self.get("tag", &[]).await?;

        tags.iter()
            .find(|t| t.label.eq_ignore_ascii_case(label))
            .map(|t| t.id)
            .ok_or_else(|| ArrError::TagNotFound {
                service: self.kind,
                label: label.to_owned(),
                available: tags.iter().map(|t| t.label.clone()).collect(),
            })
    }

    /// Every file belonging to content carrying `tag_label`.
    ///
    /// Returns an empty vec when the tag exists but nothing carries it; a missing
    /// tag is [`ArrError::TagNotFound`], because those two states need different
    /// advice and look identical from the outside.
    pub async fn discover(&self, tag_label: &str) -> Result<Vec<Discovered>> {
        let tag_id = self.tag_id(tag_label).await?;
        match self.kind {
            MediaSource::Sonarr => sonarr::discover(self, tag_id).await,
            MediaSource::Radarr => radarr::discover(self, tag_id).await,
        }
    }
}

/// Clamp an error body for inclusion in a message.
///
/// Bounded by **characters, not bytes**: `String::truncate` panics outright if the
/// byte index lands inside a multi-byte character, and error bodies are exactly
/// where non-ASCII shows up — a localized error page from a reverse proxy, or a
/// title quoted back in the message.
fn clamp_body(body: &str) -> String {
    body.chars().take(MAX_ERROR_BODY).collect()
}
