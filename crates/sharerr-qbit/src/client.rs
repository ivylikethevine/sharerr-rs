//! Transport for the qBittorrent WebUI API v2.
//!
//! # The one thing that breaks naive clients
//!
//! **`Referer` is mandatory.** qBittorrent's CSRF check rejects any request whose
//! `Referer`/`Origin` host does not match the WebUI address. A client that omits it
//! authenticates fine and then fails on everything afterwards. It is set in
//! [`QbitClient::dispatch`] and nowhere else, so no call site can forget.
//!
//! # Authentication is an API key, not a session
//!
//! This client speaks only qBittorrent 5.2's WebUI API key
//! (`Options -> Web UI -> API key`), sent as `Authorization: Bearer` on every
//! request. There is no `auth/login` round trip and nothing to renew — a rejected
//! key is reported immediately, with no retry, because there is no session for a
//! retry to recover.
//!
//! # The 401 that is not a wrong key
//!
//! qBittorrent validates the `Host` header's **port** against the port it listens
//! on, and answers `401 Unauthorized` when they differ — before it ever looks at
//! the key. A docker port remap (`-p 18080:8080`) or a reverse proxy on a different
//! port therefore makes a correct key look rejected. [`QbitError::ApiKeyRejected`]
//! says so, because no amount of rotating the key fixes it.

use std::time::Duration;

use reqwest::header::{AUTHORIZATION, HeaderValue, REFERER};
use reqwest::{Method, RequestBuilder, Response, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;
use url::Url;

use crate::error::{QbitError, Result};

const API_PREFIX: &str = "api/v2/";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// The prefix every qBittorrent-issued API key carries. Keys are `qbt_` followed
/// by 28 alphanumeric characters, 32 in total.
pub const API_KEY_PREFIX: &str = "qbt_";

/// The full length of a qBittorrent API key, prefix included.
pub const API_KEY_LEN: usize = 32;

pub(crate) use sharerr_client::clamp_body;

/// Builds the per-request body. A closure rather than a prepared builder because
/// `multipart::Form` cannot be cloned, and a retry needs a fresh one.
pub(crate) type BuildRequest<'a> = &'a (dyn Fn(RequestBuilder) -> RequestBuilder + Send + Sync);

/// A qBittorrent WebUI client, authenticated by API key.
pub struct QbitClient {
    /// Always ends in `/` so `Url::join` appends rather than replaces.
    base: Url,
    /// A key from Options → Web UI → API key, sent as `Authorization: Bearer`.
    /// Requires qBittorrent 5.2 or newer; older builds have no such feature.
    ///
    /// Stored pre-rendered as the header it becomes: building it once means the
    /// per-request path cannot fail, and `HeaderValue::set_sensitive` keeps the key
    /// out of anything that prints the header map.
    bearer: HeaderValue,
    http: reqwest::Client,
    /// Sent on every request. Must match the WebUI host and port.
    referer: HeaderValue,
}

impl std::fmt::Debug for QbitClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QbitClient")
            .field("base", &self.base.as_str())
            .field("api_key", &"<redacted>")
            .finish()
    }
}

impl QbitClient {
    /// A client that authenticates with a qBittorrent API key.
    ///
    /// The key is the one qBittorrent generates under Options → Web UI → API key.
    /// It needs no username: the key identifies the WebUI account by itself, and
    /// there is no login round trip to make.
    pub fn with_api_key(base: &Url, api_key: SecretString) -> Result<Self> {
        let key = api_key.expose_secret();
        if !looks_like_api_key(key) {
            return Err(QbitError::MalformedApiKey);
        }

        // `looks_like_api_key` has just established the value is `qbt_` plus ASCII
        // alphanumerics, so this cannot fail; the fallible form is kept rather than
        // unwrapped so a future loosening of that check cannot turn into a panic.
        let mut bearer = HeaderValue::from_str(&format!("Bearer {key}"))
            .map_err(|_| QbitError::MalformedApiKey)?;
        bearer.set_sensitive(true);

        let http = reqwest::Client::builder()
            .timeout(DEFAULT_TIMEOUT)
            .build()
            .map_err(QbitError::Client)?;
        let base = sharerr_client::normalise_base(base);

        // Origin only — scheme, host, and port, no path. That is what qBittorrent
        // compares against, and a path would just be noise in its logs.
        let referer =
            HeaderValue::from_str(base.origin().ascii_serialization().as_str()).map_err(|_| {
                QbitError::Url {
                    source: url::ParseError::EmptyHost,
                }
            })?;

        Ok(Self {
            base,
            bearer,
            http,
            referer,
        })
    }

    /// The instance this client talks to, for error messages that name it.
    pub fn base_url(&self) -> &Url {
        &self.base
    }

    fn endpoint(&self, path: &str) -> Result<Url> {
        self.base
            .join(API_PREFIX)
            .and_then(|u| u.join(path))
            .map_err(|source| QbitError::Url { source })
    }

    /// Nothing to establish: the key is stateless and `auth/login` rejects it
    /// outright, so this always succeeds and leaves proving the key to the first
    /// real call. [`Self::version`] is that call for every caller in this tree.
    pub async fn login(&self) -> Result<()> {
        Ok(())
    }

    // ------------------------------------------------------------ transport

    fn dispatch(&self, method: Method, url: Url, build: BuildRequest<'_>) -> RequestBuilder {
        // The one place `Referer` is attached. Do not set it anywhere else.
        // Attached per request rather than as a default header on the
        // `reqwest::Client`, so the secret lives in one place and `Debug` on the
        // client cannot leak it through the header map.
        let request = self
            .http
            .request(method, url)
            .header(REFERER, self.referer.clone())
            .header(AUTHORIZATION, self.bearer.clone());

        build(request)
    }

    pub(crate) async fn send(
        &self,
        method: Method,
        path: &str,
        build: BuildRequest<'_>,
    ) -> Result<Response> {
        let url = self.endpoint(path)?;

        let response = self
            .dispatch(method, url.clone(), build)
            .send()
            .await
            .map_err(|source| QbitError::Unreachable {
                url: url.to_string(),
                detail: sharerr_client::error_chain(&source),
            })?;

        // A rejected key never becomes accepted by asking again, and there is no
        // session to renew — retrying would only walk towards qBittorrent's ban
        // counter for nothing.
        if is_auth_rejection(response.status()) {
            return Err(QbitError::ApiKeyRejected);
        }

        Ok(response)
    }

    /// Send and require a 2xx, discarding the body.
    pub(crate) async fn send_ok(
        &self,
        method: Method,
        path: &str,
        build: BuildRequest<'_>,
    ) -> Result<String> {
        let response = self.send(method, path, build).await?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();

        if !status.is_success() {
            return Err(QbitError::Status {
                status: status.as_u16(),
                path: path.to_owned(),
                body: clamp_body(&body),
            });
        }
        Ok(body)
    }

    /// Send and decode a JSON body.
    pub(crate) async fn send_json<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        build: BuildRequest<'_>,
    ) -> Result<T> {
        let body = self.send_ok(method, path, build).await?;
        serde_json::from_str(&body).map_err(|source| QbitError::Decode {
            path: path.to_owned(),
            source,
        })
    }

    /// `GET /api/v2/app/version` — cheapest possible liveness probe.
    ///
    /// Also the proof that the key works, since key auth has no login step.
    pub async fn version(&self) -> Result<String> {
        let body = self.send_ok(Method::GET, "app/version", &|rb| rb).await?;
        Ok(body.trim().to_owned())
    }
}

/// Whether a status means "the key was not accepted".
///
/// Both, and not one: qBittorrent answers a rejected bearer token with either
/// 401 or 403 depending on the reason, and treating only one as rejection would
/// silently ignore the other.
fn is_auth_rejection(status: StatusCode) -> bool {
    status == StatusCode::FORBIDDEN || status == StatusCode::UNAUTHORIZED
}

/// Whether a string has the shape of a qBittorrent API key.
///
/// Checked at construction rather than on the first request, so a pasted password
/// or a truncated key is named as such instead of arriving as a puzzling 403 from
/// somewhere deep in a sync.
pub fn looks_like_api_key(candidate: &str) -> bool {
    candidate.len() == API_KEY_LEN
        && candidate.starts_with(API_KEY_PREFIX)
        && candidate[API_KEY_PREFIX.len()..]
            .chars()
            .all(|c| c.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_generated_key_is_recognised() {
        // The shape qBittorrent documents: `qbt_` plus 28 alphanumerics.
        assert!(looks_like_api_key("qbt_jCGn3V76XutJwQpsXgIm6A9NLB86"));
    }

    #[test]
    fn a_password_pasted_into_the_key_box_is_not_a_key() {
        assert!(!looks_like_api_key("correct-horse-battery-staple"));
        assert!(!looks_like_api_key("qbt_short"));
        // Right length and prefix, wrong alphabet.
        assert!(!looks_like_api_key("qbt_jCGn3V76XutJwQpsXgIm6A9NLB8-"));
    }

    #[test]
    fn both_statuses_that_mean_the_key_was_rejected_are_treated_alike() {
        assert!(is_auth_rejection(StatusCode::UNAUTHORIZED));
        assert!(is_auth_rejection(StatusCode::FORBIDDEN));
        assert!(!is_auth_rejection(StatusCode::OK));
        assert!(!is_auth_rejection(StatusCode::NOT_FOUND));
    }

    #[test]
    fn debug_never_prints_the_key() {
        let base = Url::parse("http://localhost:8080").unwrap();

        let key = QbitClient::with_api_key(
            &base,
            SecretString::from("qbt_jCGn3V76XutJwQpsXgIm6A9NLB86"),
        )
        .expect("builds");
        let rendered = format!("{key:?}");
        assert!(!rendered.contains("jCGn3V76"), "{rendered}");
    }

    #[test]
    fn a_malformed_key_is_rejected_at_construction() {
        let base = Url::parse("http://localhost:8080").unwrap();
        let err = QbitClient::with_api_key(&base, SecretString::from("not-a-key")).unwrap_err();
        assert!(matches!(err, QbitError::MalformedApiKey), "{err:?}");
    }
}
