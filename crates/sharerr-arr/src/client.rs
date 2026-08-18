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
use crate::{Discovered, lidarr, radarr, readarr, sonarr};

/// The API prefix is per-source: Sonarr, Radarr and Whisparr are on `v3`, Lidarr
/// and Readarr on `v1`. See [`MediaSource::api_version`].
fn api_prefix(kind: MediaSource) -> &'static str {
    // Static rather than formatted: this runs on every single HTTP call.
    match kind.api_version() {
        "v1" => "api/v1/",
        _ => "api/v3/",
    }
}
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

use sharerr_client::clamp_body;

/// A Sonarr or Radarr v3 client.
///
/// One transport for both, because the two APIs are near-identical; [`Self::kind`]
/// is what decides the resource walk.
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
        // Only the *arr apps speak this API. The directory source has no HTTP
        // API at all — refusing here keeps every later call total.
        if !MediaSource::ARRS.contains(&kind) {
            return Err(ArrError::NotAnApp { service: kind });
        }
        let http = reqwest::Client::builder()
            .timeout(DEFAULT_TIMEOUT)
            .build()
            .map_err(ArrError::Client)?;
        Ok(Self {
            kind,
            base: sharerr_client::normalise_base(base),
            api_key,
            http,
        })
    }

    /// Whether this client talks to Sonarr or Radarr.
    pub fn kind(&self) -> MediaSource {
        self.kind
    }

    fn endpoint(&self, path: &str) -> Result<Url> {
        self.base
            .join(api_prefix(self.kind))
            .and_then(|u| u.join(path))
            .map_err(|source| ArrError::Url {
                service: self.kind,
                source,
            })
    }

    /// The only place the API key is read. Everything else goes through here, so
    /// there is exactly one line to audit.
    pub(crate) async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T> {
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
                detail: sharerr_client::error_chain(&source),
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
                detail: sharerr_client::error_chain(&source),
            })?;

        serde_json::from_slice(&bytes).map_err(|source| ArrError::Decode {
            service: self.kind,
            path: path.to_owned(),
            source,
        })
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
        self.discover_with_tag_id(tag_id).await
    }

    /// [`Self::discover`] for a caller that already resolved the tag id — the
    /// health checks do, and re-resolving here cost every probe a duplicate
    /// `/tag` round trip.
    pub async fn discover_with_tag_id(&self, tag_id: i64) -> Result<Vec<Discovered>> {
        match self.kind {
            // Whisparr is Sonarr's codebase with a different catalogue, so it walks
            // series and episode files identically — the same code, not a copy.
            MediaSource::Sonarr | MediaSource::Whisparr => sonarr::discover(self, tag_id).await,
            MediaSource::Radarr => radarr::discover(self, tag_id).await,
            MediaSource::Lidarr => lidarr::discover(self, tag_id).await,
            MediaSource::Readarr => readarr::discover(self, tag_id).await,
            // Unreachable: `Self::new` refuses to build a client for anything
            // that does not speak the *arr API.
            MediaSource::Directory => Err(ArrError::NotAnApp { service: self.kind }),
        }
    }
}
