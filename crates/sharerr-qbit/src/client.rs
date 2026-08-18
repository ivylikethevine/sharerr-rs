//! Transport and session handling for the qBittorrent WebUI API v2.
//!
//! # The two things that break naive clients
//!
//! 1. **`Referer` is mandatory.** qBittorrent's CSRF check rejects any request
//!    whose `Referer`/`Origin` host does not match the WebUI address. A client that
//!    omits it authenticates fine and then fails on everything afterwards. It is set
//!    in [`QbitClient::dispatch`] and nowhere else, so no call site can forget.
//! 2. **The login verdict moved.** Up to qBittorrent 5.1, `auth/login` answered
//!    `Ok.` or `Fails.` in the body with HTTP 200 either way, so only the body
//!    carried the verdict. From 5.2 success is **`204 No Content` with an empty
//!    body** and a rejection is **`401 Unauthorized`**. A client that insists on
//!    the literal `Ok.` calls a perfectly good 5.2 login a wrong password — which
//!    is exactly the bug this module was rewritten to fix. Both shapes are accepted
//!    here, because the two versions will coexist in the wild for years.
//!
//! # Which status means "sign in again"
//!
//! The same 5.2 change moved session expiry from `403` to `401`, so both are
//! treated as "the session went away": exactly one re-login and one retry. A
//! second failure is reported rather than retried — at that point the cause is
//! configuration, and looping would only turn it into a ban.
//!
//! # The 401 that is not a wrong password
//!
//! qBittorrent validates the `Host` header's **port** against the port it listens
//! on, and answers `401 Unauthorized` when they differ — before it ever looks at
//! the credentials. A docker port remap (`-p 18080:8080`) or a reverse proxy on a
//! different port therefore makes a correct password look rejected. [`QbitError::Unauthorized`]
//! says so, because no amount of retyping the password fixes it.

use std::time::Duration;

use reqwest::header::{AUTHORIZATION, HeaderValue, REFERER};
use reqwest::{Method, RequestBuilder, Response, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;
use tokio::sync::Mutex;
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

/// How this client proves who it is.
///
/// The two modes are genuinely different protocols, not two ways of spelling the
/// same one: a password buys a session cookie that expires and has to be renewed,
/// while an API key is stateless and is simply attached to every request. Keeping
/// them apart in the type is what stops the session machinery from running — and
/// pointlessly calling `auth/login`, which an API key is explicitly forbidden from
/// touching — when there is no session to manage.
enum Auth {
    Password {
        username: String,
        password: SecretString,
    },
    /// A key from Options → Web UI → API key, sent as `Authorization: Bearer`.
    /// Requires qBittorrent 5.2 or newer; older builds have no such feature.
    ///
    /// Stored pre-rendered as the header it becomes: building it once means the
    /// per-request path cannot fail, and `HeaderValue::set_sensitive` keeps the key
    /// out of anything that prints the header map.
    ApiKey { bearer: HeaderValue },
}

/// A qBittorrent WebUI client.
///
/// Under password auth it holds the session cookie, so [`Self::login`] is called
/// once and every later call reuses it. Under API-key auth there is no session at
/// all and every call carries the key.
pub struct QbitClient {
    /// Always ends in `/` so `Url::join` appends rather than replaces.
    base: Url,
    auth: Auth,
    http: reqwest::Client,
    /// Sent on every request. Must match the WebUI host and port.
    referer: HeaderValue,
    /// `true` once `auth/login` has succeeded. Guarded so that concurrent callers
    /// on a cold client perform one login between them, not one each. Always
    /// `true` under API-key auth, which has nothing to establish.
    session: Mutex<bool>,
}

impl std::fmt::Debug for QbitClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut out = f.debug_struct("QbitClient");
        out.field("base", &self.base.as_str());
        match &self.auth {
            Auth::Password { username, .. } => {
                out.field("username", username)
                    .field("password", &"<redacted>");
            }
            Auth::ApiKey { .. } => {
                out.field("api_key", &"<redacted>");
            }
        }
        out.finish()
    }
}

impl QbitClient {
    /// A client that signs in with a username and password.
    pub fn new(base: &Url, username: &str, password: SecretString) -> Result<Self> {
        Self::build(
            base,
            Auth::Password {
                username: username.to_owned(),
                password,
            },
        )
    }

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

        Self::build(base, Auth::ApiKey { bearer })
    }

    fn build(base: &Url, auth: Auth) -> Result<Self> {
        // The SID cookie qBittorrent hands back at login rides on the cookie
        // store; without one every call after login would be rejected. (5.2 renamed
        // it to `QBT_SID_<port>`, which is why nothing here names the cookie.)
        let http = reqwest::Client::builder()
            .timeout(DEFAULT_TIMEOUT)
            .cookie_store(true)
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

        // An API-key client has no session to establish, so it starts "established"
        // and `ensure_session` never fires.
        let established = matches!(auth, Auth::ApiKey { .. });

        Ok(Self {
            base,
            auth,
            http,
            referer,
            session: Mutex::new(established),
        })
    }

    /// The instance this client talks to, for error messages that name it.
    pub fn base_url(&self) -> &Url {
        &self.base
    }

    /// Whether this client authenticates with an API key rather than a password.
    pub fn uses_api_key(&self) -> bool {
        matches!(self.auth, Auth::ApiKey { .. })
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
    ///
    /// Under API-key auth there is nothing to do — the key is stateless and
    /// `auth/login` rejects it outright — so this succeeds and leaves proving the
    /// key to the first real call. [`Self::version`] is that call for every caller
    /// in this tree.
    pub async fn login(&self) -> Result<()> {
        if self.uses_api_key() {
            return Ok(());
        }
        let mut session = self.session.lock().await;
        self.login_inner().await?;
        *session = true;
        Ok(())
    }

    async fn login_inner(&self) -> Result<()> {
        let Auth::Password { username, password } = &self.auth else {
            return Ok(());
        };

        let url = self.endpoint("auth/login")?;
        let response = self
            .http
            .post(url.clone())
            .header(REFERER, self.referer.clone())
            .form(&[
                ("username", username.as_str()),
                ("password", password.expose_secret()),
            ])
            .send()
            .await
            .map_err(|source| QbitError::Unreachable {
                url: url.to_string(),
                source,
            })?;

        let status = response.status();

        // qBittorrent bans an IP after repeated failures and answers 403 here.
        if status == StatusCode::FORBIDDEN {
            return Err(QbitError::LoginBanned);
        }

        // 5.2 and newer reject credentials with 401 — but so does a Host-header
        // port mismatch, before the password is even read. One variant, both
        // fixes, because the response cannot tell them apart.
        if status == StatusCode::UNAUTHORIZED {
            return Err(QbitError::Unauthorized);
        }

        if !status.is_success() {
            let body = clamp_body(&response.text().await.unwrap_or_default());
            return Err(QbitError::Status {
                status: status.as_u16(),
                path: "auth/login".to_owned(),
                body,
            });
        }

        // 5.2 and newer: `204 No Content`, empty body. There is nothing to check
        // beyond the status, and demanding `Ok.` here is what made a good login
        // look like a wrong password.
        if status == StatusCode::NO_CONTENT {
            tracing::debug!(user = %username, "authenticated to qBittorrent");
            return Ok(());
        }

        // Up to 5.1: 200 either way, and the body carries the verdict.
        let body = response.text().await.unwrap_or_default();
        let body = body.trim();
        if body.is_empty() || body == "Ok." {
            tracing::debug!(user = %username, "authenticated to qBittorrent");
            return Ok(());
        }

        Err(QbitError::LoginRejected)
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
        let request = self
            .http
            .request(method, url)
            .header(REFERER, self.referer.clone());

        let request = match &self.auth {
            // Attached per request rather than as a default header on the
            // `reqwest::Client`, so the secret lives in one place and `Debug` on
            // the client cannot leak it through the header map.
            Auth::ApiKey { bearer } => request.header(AUTHORIZATION, bearer.clone()),
            Auth::Password { .. } => request,
        };

        build(request)
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

        if !is_auth_rejection(response.status()) {
            return Ok(response);
        }

        // A rejected API key never becomes accepted by asking again, and there is
        // no login to redo — retrying would only walk towards the ban counter.
        if self.uses_api_key() {
            return Err(QbitError::ApiKeyRejected);
        }

        // The session expired. Re-authenticate and retry once — never more, or a
        // misconfigured Referer turns into a login-failure ban. Both statuses are
        // checked because 5.2 moved expiry from 403 to 401.
        tracing::debug!(path, "qBittorrent session expired; re-authenticating");
        self.login().await?;

        let retried = self
            .dispatch(method, url.clone(), build)
            .send()
            .await
            .map_err(unreachable)?;

        if is_auth_rejection(retried.status()) {
            return Err(QbitError::Forbidden {
                status: retried.status().as_u16(),
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
    ///
    /// Also the proof that an API key works, since key auth has no login step.
    pub async fn version(&self) -> Result<String> {
        let body = self.send_ok(Method::GET, "app/version", &|rb| rb).await?;
        Ok(body.trim().to_owned())
    }
}

/// Whether a status means "you are not signed in".
///
/// Both, and not one: up to 5.1 an expired session answered 403, and from 5.2 it
/// answers 401. Handling only the version in front of you is how this breaks again
/// on the next upgrade.
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
    fn both_statuses_that_mean_signed_out_are_treated_alike() {
        assert!(is_auth_rejection(StatusCode::UNAUTHORIZED));
        assert!(is_auth_rejection(StatusCode::FORBIDDEN));
        assert!(!is_auth_rejection(StatusCode::OK));
        assert!(!is_auth_rejection(StatusCode::NOT_FOUND));
    }

    /// The secret must not reach the log, whichever mode the client is in.
    #[test]
    fn debug_never_prints_a_secret() {
        let base = Url::parse("http://localhost:8080").unwrap();

        let password =
            QbitClient::new(&base, "admin", SecretString::from("hunter2")).expect("builds");
        let rendered = format!("{password:?}");
        assert!(!rendered.contains("hunter2"), "{rendered}");

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
