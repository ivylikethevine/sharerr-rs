//! Transport and session handling for the qBittorrent WebUI API v2.
//!
//! # The two things that break naive clients
//!
//! 1. **`Referer` is mandatory.** qBittorrent's CSRF check rejects any request
//!    whose `Referer`/`Origin` host does not match the WebUI address. A client that
//!    omits it authenticates fine and then 403s on everything afterwards. It is set
//!    in [`QbitClient::dispatch`] and nowhere else, so no call site can forget.
//! 2. **A failed login is an HTTP 200.** `auth/login` answers `Ok.` or `Fails.` in
//!    the body with a success status either way. Checking only the status code
//!    yields a client that appears logged in and fails later, far from the cause.
//!
//! Sessions expire, so any 403 on a normal call triggers exactly one re-login and
//! one retry. A second 403 is reported rather than retried — at that point the
//! cause is configuration, and looping would only turn it into a ban.

use std::time::Duration;

use reqwest::header::{HeaderValue, REFERER};
use reqwest::{Method, RequestBuilder, Response, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;
use tokio::sync::Mutex;
use url::Url;

use crate::error::{QbitError, Result};

const API_PREFIX: &str = "api/v2/";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_ERROR_BODY: usize = 400;

/// Builds the per-request body. A closure rather than a prepared builder because
/// `multipart::Form` cannot be cloned, and a retry needs a fresh one.
pub(crate) type BuildRequest<'a> = &'a (dyn Fn(RequestBuilder) -> RequestBuilder + Send + Sync);

/// A signed-in qBittorrent WebUI client.
///
/// Holds the session cookie, so [`Self::login`] is called once and every later
/// call reuses it.
pub struct QbitClient {
    /// Always ends in `/` so `Url::join` appends rather than replaces.
    base: Url,
    username: String,
    password: SecretString,
    http: reqwest::Client,
    /// Sent on every request. Must match the WebUI host and port.
    referer: HeaderValue,
    /// `true` once `auth/login` has succeeded. Guarded so that concurrent callers
    /// on a cold client perform one login between them, not one each.
    session: Mutex<bool>,
}

impl std::fmt::Debug for QbitClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QbitClient")
            .field("base", &self.base.as_str())
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

impl QbitClient {
    pub fn new(base: &Url, username: &str, password: SecretString) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(DEFAULT_TIMEOUT)
            // The SID cookie qBittorrent hands back at login rides on this.
            .cookie_store(true)
            .build()
            .map_err(QbitError::Client)?;
        Self::with_http(base, username, password, http)
    }

    /// Same as [`Self::new`] with a caller-supplied client. The client **must**
    /// have a cookie store enabled or every call after login will 403.
    pub fn with_http(
        base: &Url,
        username: &str,
        password: SecretString,
        http: reqwest::Client,
    ) -> Result<Self> {
        let mut base = base.clone();
        if !base.path().ends_with('/') {
            let with_slash = format!("{}/", base.path());
            base.set_path(&with_slash);
        }

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
            username: username.to_owned(),
            password,
            http,
            referer,
            session: Mutex::new(false),
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

    // ------------------------------------------------------------ auth

    /// Authenticate, replacing any existing session.
    ///
    /// Public because `doctor` wants to probe credentials without side effects.
    pub async fn login(&self) -> Result<()> {
        let mut session = self.session.lock().await;
        self.login_inner().await?;
        *session = true;
        Ok(())
    }

    async fn login_inner(&self) -> Result<()> {
        let url = self.endpoint("auth/login")?;
        let response = self
            .http
            .post(url.clone())
            .header(REFERER, self.referer.clone())
            .form(&[
                ("username", self.username.as_str()),
                ("password", self.password.expose_secret()),
            ])
            .send()
            .await
            .map_err(|source| QbitError::Unreachable {
                url: url.to_string(),
                source,
            })?;

        // qBittorrent bans an IP after repeated failures and answers 403 here.
        if response.status() == StatusCode::FORBIDDEN {
            return Err(QbitError::LoginBanned);
        }

        let status = response.status();
        if !status.is_success() {
            let body = clamp_body(&response.text().await.unwrap_or_default());
            return Err(QbitError::Status {
                status: status.as_u16(),
                path: "auth/login".to_owned(),
                body,
            });
        }

        // The body, not the status, carries the verdict.
        let body = response.text().await.unwrap_or_default();
        if body.trim() != "Ok." {
            return Err(QbitError::LoginRejected);
        }

        tracing::debug!(user = %self.username, "authenticated to qBittorrent");
        Ok(())
    }

    async fn ensure_session(&self) -> Result<()> {
        let mut session = self.session.lock().await;
        if !*session {
            self.login_inner().await?;
            *session = true;
        }
        Ok(())
    }

    // ------------------------------------------------------------ transport

    fn dispatch(&self, method: Method, url: Url, build: BuildRequest<'_>) -> RequestBuilder {
        // The one place `Referer` is attached. Do not set it anywhere else.
        build(
            self.http
                .request(method, url)
                .header(REFERER, self.referer.clone()),
        )
    }

    pub(crate) async fn send(
        &self,
        method: Method,
        path: &str,
        build: BuildRequest<'_>,
    ) -> Result<Response> {
        self.ensure_session().await?;
        let url = self.endpoint(path)?;

        let unreachable = |source: reqwest::Error| QbitError::Unreachable {
            url: url.to_string(),
            source,
        };

        let response = self
            .dispatch(method.clone(), url.clone(), build)
            .send()
            .await
            .map_err(unreachable)?;

        if response.status() != StatusCode::FORBIDDEN {
            return Ok(response);
        }

        // The session expired. Re-authenticate and retry once — never more, or a
        // misconfigured Referer turns into a login-failure ban.
        tracing::debug!(path, "qBittorrent session expired; re-authenticating");
        self.login().await?;

        let retried = self
            .dispatch(method, url.clone(), build)
            .send()
            .await
            .map_err(unreachable)?;

        if retried.status() == StatusCode::FORBIDDEN {
            return Err(QbitError::Forbidden {
                path: path.to_owned(),
            });
        }
        Ok(retried)
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
    pub async fn version(&self) -> Result<String> {
        let body = self.send_ok(Method::GET, "app/version", &|rb| rb).await?;
        Ok(body.trim().to_owned())
    }
}

/// Clamp an error body for inclusion in a message.
///
/// Bounded by **characters, not bytes**: `String::truncate` panics outright if the
/// byte index lands inside a multi-byte character, and error bodies are exactly
/// where non-ASCII shows up — a localized error page from a reverse proxy, or a
/// title quoted back in the message.
pub(crate) fn clamp_body(body: &str) -> String {
    body.chars().take(MAX_ERROR_BODY).collect()
}
