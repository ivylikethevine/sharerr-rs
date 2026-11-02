//! One HTTP client for both apps.
//!
//! Sonarr and Radarr expose the same v3 shape — `X-Api-Key` auth, `/api/v3/` base,
//! `/tag` and `/system/status` identical — so the transport lives here once and
//! [`crate::sonarr`] / [`crate::radarr`] only differ in which resources they walk.

use std::collections::HashMap;
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
/// and Readarr on `v1`. Getting it wrong presents as a 404 that looks like a wrong
/// URL rather than a wrong version.
fn api_prefix(kind: MediaSource) -> &'static str {
    match kind {
        MediaSource::Lidarr | MediaSource::Readarr => "api/v1/",
        _ => "api/v3/",
    }
}
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Whether a Whisparr reporting `version` is v3 or later, which is built on
/// Radarr (movies and scenes, tagged per item) rather than v2's Sonarr (sites
/// and episodes, tagged per site). A version that does not parse is taken as
/// v2, the shape every Whisparr had before v3.
pub fn whisparr_walks_like_radarr(version: &str) -> bool {
    version
        .split('.')
        .next()
        .and_then(|major| major.trim().parse::<u32>().ok())
        .is_some_and(|major| major >= 3)
}

use sharerr_client::clamp_body;

/// What [`ArrClient::discover_with_audience`] found.
#[derive(Debug)]
pub struct TaggedFiles {
    pub items: Vec<Discovered>,
    /// The friends each `file_id` carrying only `<tag>-<friend>` tags is for. A
    /// file not listed here is for everyone.
    pub audience: HashMap<i64, Vec<String>>,
}

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
        sharerr_client::debug_redacted(
            f,
            "ArrClient",
            &[
                ("kind", &self.kind as &dyn std::fmt::Debug),
                ("api_base", &self.api_base.as_str() as &dyn std::fmt::Debug),
            ],
            &["api_key"],
        )
    }
}

impl ArrClient {
    pub fn new(kind: MediaSource, base: &Url, api_key: SecretString) -> Result<Self> {
        // Only the *arr apps speak this API. The directory source has no HTTP
        // API at all — refusing here keeps every later call total.
        if !MediaSource::ARRS.contains(&kind) {
            return Err(ArrError::NotAnApp { service: kind });
        }
        let http =
            sharerr_client::http_client_with_timeout(DEFAULT_TIMEOUT).map_err(ArrError::Client)?;
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
        self.find_tag(&tags, label)
    }

    fn find_tag(&self, tags: &[Tag], label: &str) -> Result<i64> {
        tags.iter()
            .find(|t| t.label.eq_ignore_ascii_case(label))
            .map(|t| t.id)
            .ok_or_else(|| ArrError::TagNotFound {
                service: self.kind,
                label: label.to_owned(),
                available: tags.iter().map(|t| t.label.clone()).collect(),
            })
    }

    /// [`Self::discover`] plus every file carrying a `<tag>-<friend>` tag, with
    /// who each of those is for.
    ///
    /// A file also reachable through the plain tag is for everyone, so it gets no
    /// audience: the plain tag is the wider grant, and wins. One walk over all the
    /// tags at once, and any failure fails the source: sharing a partial answer
    /// could leave a friend's item looking like it had no audience.
    pub async fn discover_with_audience(&self, tag_label: &str) -> Result<TaggedFiles> {
        let tags: Vec<Tag> = self.get("tag", &()).await?;
        let plain = self.find_tag(&tags, tag_label)?;

        let prefix = format!("{}-", tag_label.to_ascii_lowercase());
        let friends: Vec<(i64, String)> = tags
            .iter()
            .filter_map(|tag| {
                let label = tag.label.to_ascii_lowercase();
                let friend = label.strip_prefix(&prefix).filter(|f| !f.is_empty())?;
                Some((tag.id, friend.to_owned()))
            })
            .collect();

        let mut tag_ids = vec![plain];
        tag_ids.extend(friends.iter().map(|(id, _)| *id));

        let mut items = Vec::new();
        let mut audience = HashMap::new();
        for (item, entity_tags) in self.walk(&tag_ids).await? {
            if !entity_tags.contains(&plain) {
                let names = friends
                    .iter()
                    .filter(|(id, _)| entity_tags.contains(id))
                    .map(|(_, name)| name.clone())
                    .collect();
                audience.insert(item.file_id, names);
            }
            items.push(item);
        }
        Ok(TaggedFiles { items, audience })
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
        let walked = self.walk(&[tag_id]).await?;
        Ok(walked.into_iter().map(|(item, _)| item).collect())
    }

    /// Every file of an entity carrying any of `tag_ids`, with the tags that
    /// entity carries.
    async fn walk(&self, tag_ids: &[i64]) -> Result<Vec<(Discovered, Vec<i64>)>> {
        match self.kind {
            MediaSource::Sonarr => sonarr::discover(self, tag_ids).await,
            // Whisparr v2 is Sonarr's codebase and v3 is Radarr's, both on the
            // same `v3` prefix, so only the reported version tells them apart.
            MediaSource::Whisparr => {
                if whisparr_walks_like_radarr(&self.system_status().await?.version) {
                    radarr::discover(self, tag_ids).await
                } else {
                    sonarr::discover(self, tag_ids).await
                }
            }
            MediaSource::Radarr => radarr::discover(self, tag_ids).await,
            MediaSource::Lidarr => lidarr::discover(self, tag_ids).await,
            MediaSource::Readarr => readarr::discover(self, tag_ids).await,
            // Unreachable: `Self::new` refuses to build a client for anything
            // that does not speak the *arr API.
            MediaSource::Directory => Err(ArrError::NotAnApp { service: self.kind }),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn a_key() -> SecretString {
        SecretString::from("test-key")
    }

    /// The one thing the redaction exists to prevent: the key must never
    /// appear in the client's own `Debug` output, since `Config` (and by
    /// extension anything holding one) is logged wholesale at debug level.
    #[test]
    fn debug_redacts_the_api_key() {
        let client = ArrClient::new(
            MediaSource::Sonarr,
            &Url::parse("http://sonarr.example").unwrap(),
            SecretString::from("super-secret-key"),
        )
        .unwrap();

        let rendered = format!("{client:?}");
        assert!(!rendered.contains("super-secret-key"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(
            rendered.contains("Sonarr") || rendered.contains("sonarr"),
            "{rendered}"
        );
    }

    /// `MediaSource::Directory` has no *arr HTTP API at all — refusing to
    /// build a client for it here is what keeps every later call in this
    /// module total, rather than pushing the check onto every caller.
    #[test]
    fn new_refuses_a_non_arr_media_source() {
        let err = ArrClient::new(
            MediaSource::Directory,
            &Url::parse("http://sonarr.example").unwrap(),
            a_key(),
        )
        .unwrap_err();

        assert!(matches!(err, ArrError::NotAnApp { .. }), "{err:?}");
    }

    /// A base URL that cannot act as a base (no authority/path to join
    /// against) must be reported as the specific `Url` error `new` names,
    /// rather than panicking on the `.join` inside it.
    #[test]
    fn new_reports_a_base_url_that_cannot_be_joined() {
        let unjoinable = Url::parse("data:text/plain,hello").unwrap();

        let err = ArrClient::new(MediaSource::Sonarr, &unjoinable, a_key()).unwrap_err();

        assert!(matches!(err, ArrError::Url { .. }), "{err:?}");
    }

    /// Same underlying `Url::join` failure as `new`'s, reached from the other
    /// call site: a request path that cannot be resolved against an
    /// otherwise-valid `api_base`.
    #[test]
    fn request_reports_an_unjoinable_path() {
        let client = ArrClient::new(
            MediaSource::Sonarr,
            &Url::parse("http://sonarr.example").unwrap(),
            a_key(),
        )
        .unwrap();

        let err = client.request(Method::GET, "http://[bad").unwrap_err();

        assert!(matches!(err, ArrError::Url { .. }), "{err:?}");
    }

    #[test]
    fn whisparr_v3_and_later_walk_like_radarr() {
        for version in ["3.0.0.1", "3.6.6.1913", "4.0.0.0"] {
            assert!(whisparr_walks_like_radarr(version), "{version}");
        }
        for version in ["2.0.0.548", "", "unknown", "v3"] {
            assert!(!whisparr_walks_like_radarr(version), "{version}");
        }
    }

    /// The match in `discover_with_tag_id` is only exhaustive because this arm
    /// exists — `new` already refuses to build a client for `Directory`, so
    /// this is defensive rather than reachable through the public API. Built
    /// via a struct literal (private fields, same module) specifically to
    /// exercise it.
    #[tokio::test]
    async fn discover_with_tag_id_refuses_a_non_arr_kind() {
        let client = ArrClient {
            kind: MediaSource::Directory,
            api_base: Url::parse("http://sonarr.example/api/v3/").unwrap(),
            api_key: a_key(),
            http: reqwest::Client::new(),
        };

        let err = client.discover_with_tag_id(1).await.unwrap_err();

        assert!(matches!(err, ArrError::NotAnApp { .. }), "{err:?}");
    }
}
