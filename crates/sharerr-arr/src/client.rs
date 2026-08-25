//! One HTTP client for both apps.
//!
//! Sonarr and Radarr expose the same v3 shape — `X-Api-Key` auth, `/api/v3/` base,
//! `/tag` and `/system/status` identical — so the transport lives here once and
//! [`crate::sonarr`] / [`crate::radarr`] only differ in which resources they walk.

use std::time::Duration;

use reqwest::{Method, RequestBuilder, Response};
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sharerr_core::MediaSource;
use url::Url;

use crate::error::{ArrError, Result};
use crate::models::{SystemStatus, Tag};
use crate::{Discovered, lidarr, radarr, readarr, sonarr};

/// The API prefix is per-source: Sonarr, Radarr and Whisparr are on `v3`, Lidarr
/// and Readarr on `v1`. Matched on the source directly rather than through
/// `MediaSource::api_version`, so a new source cannot fall into the wrong
/// prefix by way of a string that no longer matches.
fn api_prefix(kind: MediaSource) -> &'static str {
    match kind {
        MediaSource::Lidarr | MediaSource::Readarr => "api/v1/",
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
    /// The API root — the normalised base plus [`api_prefix`], joined once at
    /// construction rather than on every call. Always ends in `/`, so
    /// `Url::join` appends rather than replacing the last segment. This is what
    /// makes reverse-proxy subpaths (`http://host/sonarr/`) work.
    api_base: Url,
    api_key: SecretString,
    http: reqwest::Client,
}

/// The API key must never reach a log line, and `Config` is logged wholesale at
/// debug level, so this is a real hazard rather than a theoretical one.
impl std::fmt::Debug for ArrClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArrClient")
            .field("kind", &self.kind)
            .field("api_base", &self.api_base.as_str())
            .field("api_key", &"<redacted>")
            // `finish_non_exhaustive` rather than `finish`: the omission is
            // deliberate, and rendering `..` says so to whoever reads the log
            // instead of implying this is the whole struct.
            .finish_non_exhaustive()
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
        let api_base = sharerr_client::normalise_base(base)
            .join(api_prefix(kind))
            .map_err(|source| ArrError::Url {
                service: kind,
                source,
            })?;
        Ok(Self {
            kind,
            api_base,
            api_key,
            http,
        })
    }

    /// Whether this client talks to Sonarr or Radarr.
    pub fn kind(&self) -> MediaSource {
        self.kind
    }

    fn endpoint(&self, path: &str) -> Result<Url> {
        self.api_base.join(path).map_err(|source| ArrError::Url {
            service: self.kind,
            source,
        })
    }

    /// The only place the API key is read. Every request starts here, so there
    /// is exactly one line to audit.
    fn request(&self, method: Method, path: &str) -> Result<(Url, RequestBuilder)> {
        let url = self.endpoint(path)?;
        let builder = self
            .http
            .request(method, url.clone())
            .header("X-Api-Key", self.api_key.expose_secret());
        Ok((url, builder))
    }

    fn unreachable(&self, url: &Url, source: &reqwest::Error) -> ArrError {
        ArrError::Unreachable {
            service: self.kind,
            url: url.to_string(),
            detail: sharerr_client::error_chain(source),
        }
    }

    /// Send a prepared request and require a 2xx, handing back the response
    /// for the caller to read the body.
    async fn send(&self, url: &Url, path: &str, builder: RequestBuilder) -> Result<Response> {
        let response = builder
            .send()
            .await
            .map_err(|source| self.unreachable(url, &source))?;
        self.check_status(response, path).await
    }

    /// Turn a non-2xx response into the matching [`ArrError`], `path` named for the
    /// message; otherwise hand the response back for the caller to read the body.
    async fn check_status(
        &self,
        response: reqwest::Response,
        path: &str,
    ) -> Result<reqwest::Response> {
        let status = response.status();
        if sharerr_client::is_auth_rejection(status) {
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
        Ok(response)
    }

    /// `GET` a resource and decode its JSON body.
    pub(crate) async fn get<T, Q>(&self, path: &str, query: &Q) -> Result<T>
    where
        T: DeserializeOwned,
        Q: Serialize + ?Sized,
    {
        let (url, builder) = self.request(Method::GET, path)?;
        let response = self.send(&url, path, builder.query(query)).await?;

        // Decode from bytes rather than `response.json()` so a malformed payload
        // reports which endpoint produced it.
        let bytes = response
            .bytes()
            .await
            .map_err(|source| self.unreachable(&url, &source))?;

        serde_json::from_slice(&bytes).map_err(|source| ArrError::Decode {
            service: self.kind,
            path: path.to_owned(),
            source,
        })
    }

    /// Cheap liveness + auth probe for `doctor`.
    pub async fn system_status(&self) -> Result<SystemStatus> {
        self.get("system/status", &()).await
    }

    /// Resolve a tag label to its numeric id.
    ///
    /// Matching is case-insensitive: Sonarr lowercases labels on save, so an
    /// operator who typed `Sharerr` in the UI and `sharerr` in the config would
    /// otherwise get a silent no-op — the exact failure mode this project treats as
    /// the most likely source of "sharerr does nothing" reports.
    pub async fn tag_id(&self, label: &str) -> Result<i64> {
        let tags: Vec<Tag> = self.get("tag", &()).await?;

        tags.iter()
            .find(|t| t.label.eq_ignore_ascii_case(label))
            .map(|t| t.id)
            .ok_or_else(|| ArrError::TagNotFound {
                service: self.kind,
                label: label.to_owned(),
                available: tags.iter().map(|t| t.label.clone()).collect(),
            })
    }

    /// Create a tag. Used only by `sharerr doctor --fix` — nothing in an ordinary
    /// sync creates content in the *arr app it reads from.
    ///
    /// Sonarr/Radarr lowercase a label on save and treat the comparison as
    /// case-insensitive (see [`Self::tag_id`]), so creating one that already
    /// exists under different casing is harmless — the app dedupes it.
    pub async fn create_tag(&self, label: &str) -> Result<()> {
        #[derive(Serialize)]
        struct NewTag<'a> {
            label: &'a str,
        }

        let (url, builder) = self.request(Method::POST, "tag")?;
        self.send(&url, "tag", builder.json(&NewTag { label }))
            .await?;
        Ok(())
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
