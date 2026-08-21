//! Sign-in for the web UI: first-run claim, login, logout, and the guard.
//!
//! # Why the UI has a login at all
//!
//! `/health` and `/ready` are two strings worth nothing to an attacker, but the
//! settings pages are a different proposition: they accept API keys and a
//! qBittorrent password, and `server.bind` defaults to `0.0.0.0`. An
//! unauthenticated form that writes to the credential vault would be a worse
//! hole than the vault closes.
//!
//! # What it does and does not defend
//!
//! The session cookie is `HttpOnly` and `SameSite=Strict`, which shuts out both
//! script access and cross-site form submission. It is deliberately **not**
//! `Secure`: sharerr is normally reached over plain HTTP on a LAN, and a `Secure`
//! cookie there is silently dropped by the browser, which presents as "login does
//! nothing". Anyone able to read traffic on that LAN can read the session cookie.
//! That is the same ceiling the vault's own threat model states — put sharerr
//! behind a TLS-terminating proxy if the network is not trusted.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use axum::extract::{Form, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};
use secrecy::SecretString;
use serde::Deserialize;
use sharerr_store::StoreError;
use tokio::sync::RwLock;

use crate::web::WebState;
use crate::web::templates::{LoginPage, SetupPage, render};

const COOKIE_NAME: &str = "sharerr_session";

/// How long a session lives without being used.
///
/// Long, on purpose: this is a self-hosted tool an operator visits occasionally,
/// and an aggressive timeout would mean re-typing a password to read a status
/// page. Idle-based rather than absolute, so an active session is never cut off
/// mid-edit.
const SESSION_TTL: Duration = Duration::from_secs(14 * 24 * 60 * 60);

/// Rejected before hashing, with a message the form can show.
///
/// Enforced here rather than in the store because it is a policy about humans, and
/// the store's floor (non-empty) is a different, lower guarantee.
pub const MIN_PASSWORD_LEN: usize = 8;

/// Live sessions, keyed by the opaque token in the cookie.
///
/// In memory rather than in SQLite. Sessions do not survive a restart, which for a
/// single-instance service costs one extra sign-in after an upgrade and saves a
/// table, a migration, and a cleanup job. It also means a restart is a reliable way
/// to revoke everything.
#[derive(Debug, Default)]
pub struct Sessions {
    inner: RwLock<HashMap<String, Session>>,
}

#[derive(Debug, Clone)]
struct Session {
    username: String,
    /// Extended on every authenticated request, so the TTL is idle time.
    last_seen: Instant,
}

impl Sessions {
    /// Mint a session and return its token.
    pub async fn create(&self, username: &str) -> Result<String, String> {
        // 256 bits: a session token is a bearer credential with a fortnight's life.
        let token = crate::secrets::random_hex(32)?;
        let mut sessions = self.inner.write().await;

        // Expired entries are only ever removed here. There is no reaper task: a
        // handful of stale rows in a HashMap costs nothing, and an instance that is
        // never signed into is exactly the one that should not be waking a timer.
        sessions.retain(|_, session| session.last_seen.elapsed() < SESSION_TTL);

        sessions.insert(
            token.clone(),
            Session {
                username: username.to_owned(),
                last_seen: Instant::now(),
            },
        );
        Ok(token)
    }

    /// The username behind a token, refreshing its idle timer.
    async fn touch(&self, token: &str) -> Option<String> {
        let mut sessions = self.inner.write().await;
        let session = sessions.get_mut(token)?;

        if session.last_seen.elapsed() >= SESSION_TTL {
            sessions.remove(token);
            return None;
        }

        session.last_seen = Instant::now();
        Some(session.username.clone())
    }

    async fn remove(&self, token: &str) {
        self.inner.write().await.remove(token);
    }

    /// Drop every session except `keep`.
    ///
    /// Used after a password change. The point of changing a password is usually
    /// that someone else might know the old one, and leaving their session alive
    /// would make the change cosmetic — sessions are bearer tokens and do not
    /// re-check the password. The current session is spared so the operator is not
    /// signed out of the page they just used.
    async fn revoke_all_except(&self, keep: &str) {
        self.inner.write().await.retain(|token, _| token == keep);
    }
}

/// Check a proposed password against the rules the forms state.
///
/// Shared by setup and by the change-password form so the two cannot drift — the
/// rule is also rendered into the page from [`MIN_PASSWORD_LEN`].
fn password_rejection(password: &str, confirm: &str) -> Option<String> {
    if password != confirm {
        return Some("Those passwords do not match.".to_owned());
    }
    if password.chars().count() < MIN_PASSWORD_LEN {
        return Some(format!(
            "Password must be at least {MIN_PASSWORD_LEN} characters."
        ));
    }
    None
}

/// The signed-in user, if any.
pub async fn current_user(state: &WebState, jar: &CookieJar) -> Option<String> {
    let token = jar.get(COOKIE_NAME)?.value().to_owned();
    state.sessions.touch(&token).await
}

/// Gate for every page that is not `/login`, `/setup`, `/assets/*`, `/health`, or
/// `/ready`.
///
/// An unauthenticated visitor is sent to `/setup` when the instance has no account
/// yet and to `/login` when it does — the two states need different pages, and
/// guessing wrong strands a first-time user on a form they cannot satisfy.
pub async fn require_auth(
    State(state): State<WebState>,
    jar: CookieJar,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    if current_user(&state, &jar).await.is_some() {
        return next.run(request).await;
    }

    match instance_is_claimed(&state).await {
        Ok(true) => Redirect::to("/login").into_response(),
        Ok(false) => Redirect::to("/setup").into_response(),
        // The database is the thing that knows whether an account exists. If it
        // cannot be opened, sending the visitor to a login form they can never
        // satisfy would hide the actual fault behind a password prompt.
        Err(reason) => (StatusCode::SERVICE_UNAVAILABLE, reason).into_response(),
    }
}

/// Refuse a state-changing request that came from another site.
///
/// Applied once over the whole router rather than per handler, with no exemption
/// for `/login` or `/setup`: a rejection there just returns the refusal verbatim
/// like anywhere else, so carving them out would buy nothing while leaving the
/// two most attackable POSTs relying on someone remembering an inline call.
pub async fn deny_cross_origin(request: axum::extract::Request, next: Next) -> Response {
    // GETs are not state-changing and browsers do not send `Origin` for ordinary
    // navigation, so checking them would reject every normal page load.
    if request.method() != axum::http::Method::GET
        && let Some(refusal) = cross_origin_refusal(request.headers())
    {
        return refusal;
    }

    next.run(request).await
}

async fn instance_is_claimed(state: &WebState) -> Result<bool, String> {
    let store = state.serve.store().await?;
    store
        .user_count()
        .await
        .map(|count| count > 0)
        .map_err(|err| format!("reading accounts: {err}"))
}

#[derive(Debug, Deserialize)]
pub struct SetupForm {
    username: String,
    password: String,
    confirm: String,
}

#[derive(Debug, Deserialize)]
pub struct LoginForm {
    username: String,
    password: String,
}

pub async fn setup_page(State(state): State<WebState>) -> Response {
    match instance_is_claimed(&state).await {
        Ok(true) => Redirect::to("/login").into_response(),
        Ok(false) => render(&SetupPage::blank()),
        Err(reason) => (StatusCode::SERVICE_UNAVAILABLE, reason).into_response(),
    }
}

pub async fn setup_submit(
    State(state): State<WebState>,
    jar: CookieJar,
    Form(form): Form<SetupForm>,
) -> Response {
    let reject = |message: &str| render(&SetupPage::rejected(&form.username, message));

    if let Some(message) = password_rejection(&form.password, &form.confirm) {
        return reject(&message);
    }

    let store = match state.store_or_503().await {
        Ok(store) => store,
        Err(response) => return response,
    };

    // Re-checked here and not only in `setup_page`: two people racing the form on an
    // unclaimed instance would both have been shown it.
    match store.user_count().await {
        Ok(0) => {}
        Ok(_) => return Redirect::to("/login").into_response(),
        Err(err) => return internal(&format!("reading accounts: {err}")),
    }

    let password = SecretString::from(form.password.clone());
    match store.create_user(&form.username, &password).await {
        Ok(_) => {}
        // Both of these are the user's mistake and safe to show verbatim.
        Err(StoreError::InvalidUser(message)) => return reject(message),
        Err(StoreError::UserExists { .. }) => return Redirect::to("/login").into_response(),
        Err(err) => return internal(&format!("creating the account: {err}")),
    }

    tracing::info!(username = %form.username, "operator account created");
    sign_in(&state, jar, &form.username).await
}

pub async fn login_page(State(state): State<WebState>, jar: CookieJar) -> Response {
    // Already signed in? Skip the form rather than making them re-enter a password
    // to reach a page they can already see.
    if current_user(&state, &jar).await.is_some() {
        return Redirect::to("/").into_response();
    }

    match instance_is_claimed(&state).await {
        Ok(true) => render(&LoginPage::blank()),
        Ok(false) => Redirect::to("/setup").into_response(),
        Err(reason) => (StatusCode::SERVICE_UNAVAILABLE, reason).into_response(),
    }
}

pub async fn login_submit(
    State(state): State<WebState>,
    jar: CookieJar,
    Form(form): Form<LoginForm>,
) -> Response {
    let store = match state.store_or_503().await {
        Ok(store) => store,
        Err(response) => return response,
    };

    let password = SecretString::from(form.password.clone());
    match store.verify_password(&form.username, &password).await {
        Ok(true) => sign_in(&state, jar, &form.username).await,
        Ok(false) => {
            // Deliberately one message for both a wrong password and an unknown
            // username. `Store::verify_password` already equalises the timing;
            // saying "no such user" here would undo that.
            tracing::warn!(username = %form.username, "rejected sign-in");
            (
                StatusCode::UNAUTHORIZED,
                render(&LoginPage::rejected(
                    &form.username,
                    "That username and password do not match.",
                )),
            )
                .into_response()
        }
        Err(err) => internal(&format!("checking the password: {err}")),
    }
}

pub async fn logout(State(state): State<WebState>, jar: CookieJar) -> Response {
    if let Some(cookie) = jar.get(COOKIE_NAME) {
        state.sessions.remove(cookie.value()).await;
    }

    // Removing the cookie as well as the server-side session: leaving a stale token
    // in the browser means every later request carries a credential that no longer
    // works, which reads as a bug on the next sign-in.
    let jar = jar.remove(Cookie::from(COOKIE_NAME));
    (jar, Redirect::to("/login")).into_response()
}

#[derive(Debug, Deserialize)]
pub struct ChangePasswordForm {
    current_password: String,
    new_password: String,
    confirm_password: String,
}

/// Change the signed-in operator's own password.
///
/// This closes the one hole in the account model: [`sharerr_store::Store::set_password`]
/// existed and was tested, but nothing called it, so an operator who forgot their
/// password had no route back in short of deleting a row from the `users` table by
/// hand.
///
/// The current password is required even though the session already proves who
/// this is. A session cookie is a long-lived bearer token — a borrowed laptop is
/// enough to hold one — and re-asking is what stops it being escalated into
/// permanent ownership of the account.
pub async fn change_password(
    State(state): State<WebState>,
    jar: CookieJar,
    Form(form): Form<ChangePasswordForm>,
) -> Response {
    let Some(username) = current_user(&state, &jar).await else {
        // The guard should have caught this; treat it as a session that expired
        // between rendering the form and submitting it.
        return Redirect::to("/login").into_response();
    };

    // Rejections render back onto the settings page rather than redirecting, so the
    // reason sits next to the form that caused it — the same choice every other
    // settings rejection makes.
    use crate::web::settings::reject as settings_error;

    if let Some(message) = password_rejection(&form.new_password, &form.confirm_password) {
        return settings_error(&state, &message).await;
    }

    let store = match state.store_or_503().await {
        Ok(store) => store,
        Err(response) => return response,
    };

    // Verified before anything is written, and reported in the same words as a
    // failed sign-in.
    let current = SecretString::from(form.current_password.clone());
    match store.verify_password(&username, &current).await {
        Ok(true) => {}
        Ok(false) => {
            tracing::warn!(%username, "rejected a password change: wrong current password");
            return settings_error(&state, "That is not your current password.").await;
        }
        Err(err) => return internal(&format!("checking the password: {err}")),
    }

    let new = SecretString::from(form.new_password.clone());
    match store.set_password(&username, &new).await {
        Ok(true) => {}
        // The account vanished between the two queries, which means someone is
        // editing the database underneath us.
        Ok(false) => return settings_error(&state, "That account no longer exists.").await,
        Err(StoreError::InvalidUser(message)) => return settings_error(&state, message).await,
        Err(err) => return internal(&format!("changing the password: {err}")),
    }

    // Everything else holding a session was authorised by the *old* password.
    if let Some(cookie) = jar.get(COOKIE_NAME) {
        state.sessions.revoke_all_except(cookie.value()).await;
    }

    tracing::info!(%username, "password changed");
    Redirect::to("/settings?saved=account").into_response()
}

async fn sign_in(state: &WebState, jar: CookieJar, username: &str) -> Response {
    match state.sessions.create(username).await {
        Ok(token) => (jar.add(session_cookie(token)), Redirect::to("/")).into_response(),
        Err(reason) => internal(&reason),
    }
}

fn session_cookie(token: String) -> Cookie<'static> {
    Cookie::build((COOKIE_NAME, token))
        .path("/")
        .http_only(true)
        // Strict, not Lax: this is what makes a cross-site POST unable to carry the
        // session, which is the whole of the CSRF defence for the settings forms.
        // Nothing links into sharerr from elsewhere, so the usual cost of Strict —
        // arriving from an external link and appearing signed out — does not apply.
        .same_site(SameSite::Strict)
        // No `.secure(true)`: see the module header. A Secure cookie over plain
        // HTTP is silently discarded, which would break sign-in on the LAN
        // deployments this is built for.
        .build()
}

/// The refusal for a cross-site form post, or `None` if the request is fine.
///
/// `Option<Response>` rather than `Result<(), Response>`: an axum `Response` is a
/// large type, and a `Result` whose error variant is that big is a lint the
/// workspace would otherwise carry for no benefit. `None` reads as "nothing to
/// say" at every call site anyway.
///
/// `SameSite=Strict` is the real defence; this is the belt to its braces, and it
/// catches the case of a browser that does not enforce SameSite. A request with no
/// `Origin` is allowed through — `curl` sends none, and refusing it would break
/// scripted use of these same endpoints for no security gain, since an attacker's
/// page cannot suppress the header.
fn cross_origin_refusal(headers: &HeaderMap) -> Option<Response> {
    let origin = headers.get("origin").and_then(|v| v.to_str().ok())?;

    let Some(host) = headers.get("host").and_then(|v| v.to_str().ok()) else {
        return Some(internal("request has an Origin but no Host"));
    };

    // Compare authorities, not whole URLs: Origin carries a scheme, Host does not.
    let origin_host = origin
        .split_once("://")
        .map_or(origin, |(_, rest)| rest)
        .trim_end_matches('/');

    if origin_host == host {
        return None;
    }

    tracing::warn!(origin, host, "rejected a cross-origin form submission");
    Some(
        (
            StatusCode::FORBIDDEN,
            "This form was submitted from another site.",
        )
            .into_response(),
    )
}

pub fn internal(message: &str) -> Response {
    tracing::error!(message, "web request failed");
    (StatusCode::INTERNAL_SERVER_ERROR, message.to_owned()).into_response()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use axum::http::HeaderValue;

    #[tokio::test]
    async fn a_session_round_trips_and_can_be_revoked() {
        let sessions = Sessions::default();
        let token = sessions.create("ivy").await.unwrap();

        assert_eq!(sessions.touch(&token).await.as_deref(), Some("ivy"));
        sessions.remove(&token).await;
        assert!(sessions.touch(&token).await.is_none());
    }

    #[tokio::test]
    async fn an_unknown_token_is_not_a_session() {
        let sessions = Sessions::default();
        sessions.create("ivy").await.unwrap();
        assert!(sessions.touch("not-a-real-token").await.is_none());
    }

    #[tokio::test]
    async fn tokens_are_unguessable_and_never_repeat() {
        let sessions = Sessions::default();
        let a = sessions.create("ivy").await.unwrap();
        let b = sessions.create("ivy").await.unwrap();

        assert_ne!(a, b, "two sessions must not share a token");
        assert_eq!(a.len(), 64, "256 bits, hex encoded");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[tokio::test]
    async fn an_idle_session_expires() {
        let sessions = Sessions::default();
        let token = sessions.create("ivy").await.unwrap();

        // Reach in and age the entry rather than sleeping for a fortnight.
        if let Some(session) = sessions.inner.write().await.get_mut(&token) {
            session.last_seen = Instant::now() - SESSION_TTL - Duration::from_secs(1);
        }

        assert!(sessions.touch(&token).await.is_none());
        assert!(
            !sessions.inner.read().await.contains_key(&token),
            "an expired session should be dropped, not merely refused"
        );
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            let name = axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap();
            map.insert(name, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    #[test]
    fn a_same_origin_post_is_allowed() {
        assert!(
            cross_origin_refusal(&headers(&[
                ("origin", "http://box.lan:8477"),
                ("host", "box.lan:8477"),
            ]))
            .is_none()
        );
    }

    #[test]
    fn a_cross_origin_post_is_refused() {
        let refusal = cross_origin_refusal(&headers(&[
            ("origin", "https://evil.example"),
            ("host", "box.lan:8477"),
        ]))
        .expect("a foreign origin must not be accepted");
        assert_eq!(refusal.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn a_request_without_an_origin_is_allowed() {
        // curl and the scripted use of these endpoints send no Origin, and an
        // attacker's page cannot suppress it — so absence is not evidence of abuse.
        assert!(cross_origin_refusal(&headers(&[("host", "box.lan:8477")])).is_none());
    }

    #[test]
    fn a_port_mismatch_counts_as_cross_origin() {
        assert!(
            cross_origin_refusal(&headers(&[
                ("origin", "http://box.lan:9999"),
                ("host", "box.lan:8477"),
            ]))
            .is_some()
        );
    }
}
