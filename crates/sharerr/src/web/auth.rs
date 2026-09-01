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
//! script access and cross-site form submission. Whether it is also `Secure` is
//! decided per request, from what a reverse proxy says the browser used: sharerr
//! terminates no TLS itself, so on the plain-HTTP LAN it normally lives on, the
//! flag is left off — a `Secure` cookie there is silently dropped by the browser
//! and presents as "login does nothing" — and behind a TLS-terminating proxy it
//! is set, which is the deployment where it is worth having. Setting it from the
//! request rather than from a config key means an operator who puts a proxy in
//! front gets the protection without knowing there was a knob to turn. See
//! [`arrived_over_https`] for why headers nobody authenticated are good enough
//! evidence for this one decision, and for the line past which they are not.
//!
//! Over plain HTTP, anyone able to read traffic on that LAN can still read the
//! session cookie; `Secure` protects a cookie in flight, not one that was never
//! encrypted in the first place. That is the same ceiling the vault's own threat
//! model states — put sharerr behind a TLS-terminating proxy if the network is
//! not trusted.

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
    headers: HeaderMap,
    Form(form): Form<SetupForm>,
) -> Response {
    let reject = |message: &str| render(&SetupPage::rejected(&form.username, message));

    if let Some(message) = password_rejection(&form.password, &form.confirm) {
        return reject(&message);
    }

    let store = match state.store_or_503().await {
        Ok(store) => store,
        Err(response) => return *response,
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
    // A fresh instance has nothing configured yet — the wizard, not the
    // status page, is the useful first thing to see.
    sign_in(&state, jar, &headers, &form.username, "/wizard").await
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
    headers: HeaderMap,
    Form(form): Form<LoginForm>,
) -> Response {
    let store = match state.store_or_503().await {
        Ok(store) => store,
        Err(response) => return *response,
    };

    let password = SecretString::from(form.password.clone());
    match store.verify_password(&form.username, &password).await {
        Ok(true) => sign_in(&state, jar, &headers, &form.username, "/").await,
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

pub async fn logout(State(state): State<WebState>, jar: CookieJar, headers: HeaderMap) -> Response {
    if let Some(cookie) = jar.get(COOKIE_NAME) {
        state.sessions.remove(cookie.value()).await;
    }

    // Removing the cookie as well as the server-side session: leaving a stale token
    // in the browser means every later request carries a credential that no longer
    // works, which reads as a bug on the next sign-in.
    //
    // Built by the same builder that set it, because a browser only honours a
    // removal whose Path (and Secure, if set) match the original's — a bare
    // `Cookie::from(name)` carries no Path at all and only worked here by luck,
    // since a browser defaults an absent Path to the request URI's directory and
    // `/logout`'s directory happens to be `/`. `CookieJar::remove` blanks the
    // value and back-dates the expiry; every other attribute survives from what
    // is built here, which is why the scheme is re-detected rather than assumed
    // plain: a Secure cookie set over a TLS proxy cannot be overwritten by a
    // removal that arrives without it.
    let jar = jar.remove(session_cookie(String::new(), arrived_over_https(&headers)));
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
        Err(response) => return *response,
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

async fn sign_in(
    state: &WebState,
    jar: CookieJar,
    headers: &HeaderMap,
    username: &str,
    destination: &str,
) -> Response {
    match state.sessions.create(username).await {
        Ok(token) => (
            jar.add(session_cookie(token, arrived_over_https(headers))),
            Redirect::to(destination),
        )
            .into_response(),
        Err(reason) => internal(&reason),
    }
}

fn session_cookie(token: String, secure: bool) -> Cookie<'static> {
    Cookie::build((COOKIE_NAME, token))
        .path("/")
        .http_only(true)
        // Strict, not Lax: this is what makes a cross-site POST unable to carry the
        // session, which is the whole of the CSRF defence for the settings forms.
        // Nothing links into sharerr from elsewhere, so the usual cost of Strict —
        // arriving from an external link and appearing signed out — does not apply.
        .same_site(SameSite::Strict)
        // Set from the request rather than pinned either way: see
        // `arrived_over_https`. Always-on would make a browser silently discard
        // the cookie on the plain-HTTP LAN this is built for, which presents as
        // "login does nothing"; always-off throws the flag away on the TLS proxy
        // the docs recommend, which is the deployment that most needs it.
        .secure(secure)
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

/// Whether the request reached sharerr over HTTPS, so the session cookie can
/// carry `Secure` exactly when a browser will accept it.
///
/// sharerr terminates no TLS of its own — it always serves over a plain TCP
/// listener — so the only evidence available is what a reverse proxy in front
/// says it did. `X-Forwarded-Proto` is checked first because it is what every
/// proxy in the deployment guide emits by default; RFC 7239's `Forwarded` is the
/// fallback for the ones configured to speak the standardised header instead.
/// Whichever is present decides, including when it says `http` — a proxy that
/// states the scheme is better evidence than a second opinion from the other
/// header.
///
/// Both headers can carry a comma-separated chain when a request crosses more
/// than one hop, written left to right. The leftmost entry is the hop nearest
/// the browser, which is the only hop that saw the scheme the browser actually
/// used, so that is the one read here.
///
/// # These headers are spoofable, and that is fine for this one use
///
/// Anyone who can reach the port can send `X-Forwarded-Proto: https`, and there
/// is no list of trusted proxies to check it against — sharerr does not know
/// what is in front of it. Neither lie buys anything. Claiming `https` on a
/// plain-HTTP connection makes the browser discard the `Secure` cookie it is
/// handed, so the spoofer denies their own sign-in. Claiming `http` on a TLS
/// connection produces a cookie without `Secure`, and the flag rides on the
/// response to the spoofer's own request, so it is a downgrade they can inflict
/// only on themselves, never on somebody else's live session.
///
/// That reasoning is specific to a cookie flag, and it does not generalise. This
/// must never become the input to anything a spoofed value would *grant* —
/// an authentication bypass for "internal" traffic, a redirect target, a
/// rate-limit exemption. Each of those turns a free header into a free
/// privilege.
fn arrived_over_https(headers: &HeaderMap) -> bool {
    if let Some(value) = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
    {
        return first_hop(value).eq_ignore_ascii_case("https");
    }

    headers
        .get("forwarded")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| forwarded_proto(first_hop(value)))
        .is_some_and(|proto| proto.eq_ignore_ascii_case("https"))
}

/// The leftmost entry of a comma-separated proxy chain, trimmed.
fn first_hop(value: &str) -> &str {
    value.split(',').next().unwrap_or(value).trim()
}

/// The `proto` parameter of one RFC 7239 forwarded-element, with any quotes
/// stripped.
///
/// An element is a `;`-separated list of `name=value` pairs in any order, whose
/// names are case-insensitive and whose values may be quoted (`proto="https"`).
/// Splitting on `;` without tracking quoting would mis-parse a quoted value
/// containing a semicolon; no proxy writes one, and the only parameter read here
/// is a four-letter scheme.
fn forwarded_proto(element: &str) -> Option<&str> {
    element.split(';').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        name.trim()
            .eq_ignore_ascii_case("proto")
            .then_some(value.trim().trim_matches('"'))
    })
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
    use sharerr_testkit::secrets::fresh_password;

    #[tokio::test]
    async fn a_session_round_trips_and_can_be_revoked() {
        let sessions = Sessions::default();
        let token = sessions.create("operator").await.unwrap();

        assert_eq!(sessions.touch(&token).await.as_deref(), Some("operator"));
        sessions.remove(&token).await;
        assert!(sessions.touch(&token).await.is_none());
    }

    #[tokio::test]
    async fn an_unknown_token_is_not_a_session() {
        let sessions = Sessions::default();
        sessions.create("operator").await.unwrap();
        assert!(sessions.touch("not-a-real-token").await.is_none());
    }

    #[tokio::test]
    async fn tokens_are_unguessable_and_never_repeat() {
        let sessions = Sessions::default();
        let a = sessions.create("operator").await.unwrap();
        let b = sessions.create("operator").await.unwrap();

        assert_ne!(a, b, "two sessions must not share a token");
        assert_eq!(a.len(), 64, "256 bits, hex encoded");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[tokio::test]
    async fn an_idle_session_expires() {
        let sessions = Sessions::default();
        let token = sessions.create("operator").await.unwrap();

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

    #[test]
    fn a_request_with_no_proxy_headers_is_treated_as_plain_http() {
        assert!(!arrived_over_https(&headers(&[("host", "box.lan:8477")])));
    }

    #[test]
    fn an_x_forwarded_proto_of_https_means_the_browser_spoke_tls() {
        assert!(arrived_over_https(&headers(&[
            ("host", "sharerr.example"),
            ("x-forwarded-proto", "https"),
        ])));
    }

    #[test]
    fn an_x_forwarded_proto_of_http_keeps_the_cookie_insecure() {
        assert!(!arrived_over_https(&headers(&[(
            "x-forwarded-proto",
            "http",
        )])));
    }

    #[test]
    fn a_proxy_chain_is_read_from_the_hop_nearest_the_browser() {
        // Two hops: the outer one terminated TLS and spoke plain HTTP inward. The
        // browser still used HTTPS, so the cookie may carry Secure.
        assert!(arrived_over_https(&headers(&[(
            "x-forwarded-proto",
            "https, http",
        )])));
        // The mirror image is not the same claim: TLS on an inner hop only says
        // two proxies encrypted a link the browser never touched.
        assert!(!arrived_over_https(&headers(&[(
            "x-forwarded-proto",
            "http, https",
        )])));
    }

    #[test]
    fn a_forwarded_scheme_is_matched_regardless_of_case_or_padding() {
        assert!(arrived_over_https(&headers(&[(
            "x-forwarded-proto",
            "  HTTPS  ",
        )])));
    }

    #[test]
    fn an_rfc_7239_forwarded_header_is_read_when_there_is_no_x_forwarded_proto() {
        assert!(arrived_over_https(&headers(&[(
            "forwarded",
            "for=192.0.2.60;proto=https;by=203.0.113.43",
        )])));
        // Parameter names are case-insensitive and values may be quoted.
        assert!(arrived_over_https(&headers(&[(
            "forwarded",
            "Proto=\"HTTPS\"",
        )])));
        assert!(!arrived_over_https(&headers(&[(
            "forwarded",
            "for=192.0.2.60;proto=http",
        )])));
        // A `Forwarded` carrying no `proto` says nothing about the scheme, which
        // is not the same as saying it was HTTPS.
        assert!(!arrived_over_https(&headers(&[(
            "forwarded",
            "for=192.0.2.60"
        )])));
    }

    #[test]
    fn x_forwarded_proto_decides_when_a_proxy_sends_both_headers() {
        // Not a vote between them: a proxy that spells out `http` is stating a
        // fact, and a second header must not promote the connection behind its
        // back.
        assert!(!arrived_over_https(&headers(&[
            ("x-forwarded-proto", "http"),
            ("forwarded", "proto=https"),
        ])));
    }

    #[test]
    fn password_rejection_flags_mismatch_before_length() {
        assert_eq!(
            password_rejection(&fresh_password(), &fresh_password()),
            Some("Those passwords do not match.".to_owned())
        );
        assert_eq!(
            // Deliberately short and matching: the one case that must reach
            // the length check rather than the mismatch check, and a
            // generated password (see `fresh_password`) is always long
            // enough to pass it. `password_rejection` is pure string
            // validation — nothing here is ever hashed or sent anywhere —
            // but CodeQL's hard-coded-cryptographic-value query cannot tell
            // that from a value reaching a parameter named `password`.
            password_rejection("short", "short"),
            Some(format!(
                "Password must be at least {MIN_PASSWORD_LEN} characters."
            ))
        );
        let password = fresh_password();
        assert_eq!(password_rejection(&password, &password), None);
    }

    // A `WebState` over `state::fixtures::unconfigured()`, same as web/settings.rs's
    // handler tests — a real sqlite-backed `Store` (no vault involved), so account
    // creation, login, and password change all exercise the genuine store queries.
    use crate::web::{body_of, web_state};

    /// Create `operator` with a generated password on `serve`'s store, and hand the
    /// password back for whichever form field needs it — the shape underneath
    /// most of this module's tests.
    async fn seeded_user(serve: &crate::state::ServeState) -> String {
        let password = fresh_password();
        serve
            .store()
            .await
            .unwrap()
            .create_user("operator", &SecretString::from(password.clone()))
            .await
            .unwrap();
        password
    }

    #[tokio::test]
    async fn setup_page_renders_the_form_on_an_unclaimed_instance() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let response = setup_page(State(web_state(serve))).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn setup_page_redirects_to_login_once_claimed() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        seeded_user(&serve).await;

        let response = setup_page(State(web_state(serve))).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::LOCATION)
                .unwrap(),
            "/login"
        );
    }

    #[tokio::test]
    async fn setup_submit_rejects_mismatched_passwords_without_creating_an_account() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let store = serve.store().await.unwrap();
        let state = web_state(serve);

        let response = setup_submit(
            State(state),
            CookieJar::new(),
            HeaderMap::new(),
            Form(SetupForm {
                username: "operator".to_owned(),
                password: fresh_password(),
                confirm: fresh_password(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert!(body_of(response).await.contains("do not match"));
        assert_eq!(store.user_count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn setup_submit_creates_the_first_account_and_signs_in() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let store = serve.store().await.unwrap();
        let state = web_state(serve);

        let password = fresh_password();
        let response = setup_submit(
            State(state),
            CookieJar::new(),
            HeaderMap::new(),
            Form(SetupForm {
                username: "operator".to_owned(),
                password: password.clone(),
                confirm: password,
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::LOCATION)
                .unwrap(),
            "/wizard",
            "a fresh instance lands on the wizard, not the status page"
        );
        assert!(
            response
                .headers()
                .get_all(axum::http::header::SET_COOKIE)
                .iter()
                .any(|c| c.to_str().unwrap().starts_with(COOKIE_NAME)),
        );
        assert_eq!(store.user_count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn setup_submit_redirects_when_someone_else_won_the_race() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let store = serve.store().await.unwrap();
        store
            .create_user("first", &SecretString::from(fresh_password()))
            .await
            .unwrap();
        let state = web_state(serve);

        let password = fresh_password();
        let response = setup_submit(
            State(state),
            CookieJar::new(),
            HeaderMap::new(),
            Form(SetupForm {
                username: "second".to_owned(),
                password: password.clone(),
                confirm: password,
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::LOCATION)
                .unwrap(),
            "/login"
        );
        assert_eq!(store.user_count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn login_page_redirects_to_setup_on_an_unclaimed_instance() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let response = login_page(State(web_state(serve)), CookieJar::new()).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::LOCATION)
                .unwrap(),
            "/setup"
        );
    }

    #[tokio::test]
    async fn login_page_renders_the_form_once_claimed() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        seeded_user(&serve).await;

        let response = login_page(State(web_state(serve)), CookieJar::new()).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn login_submit_signs_in_on_a_correct_password() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let password = seeded_user(&serve).await;

        let response = login_submit(
            State(web_state(serve)),
            CookieJar::new(),
            HeaderMap::new(),
            Form(LoginForm {
                username: "operator".to_owned(),
                password,
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::LOCATION)
                .unwrap(),
            "/"
        );
    }

    #[tokio::test]
    async fn login_submit_rejects_a_wrong_password_with_one_generic_message() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        seeded_user(&serve).await;

        let response = login_submit(
            State(web_state(serve)),
            CookieJar::new(),
            HeaderMap::new(),
            Form(LoginForm {
                username: "operator".to_owned(),
                password: fresh_password(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(body_of(response).await.contains("do not match"));
    }

    /// The `Set-Cookie` line this response writes for the session, if any.
    ///
    /// Borrowed from the response rather than owned: every assertion below is a
    /// substring check, and the error messages read better with the whole line.
    fn session_set_cookie(response: &Response) -> Option<&str> {
        response
            .headers()
            .get_all(axum::http::header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find(|value| value.starts_with(COOKIE_NAME))
    }

    #[tokio::test]
    async fn a_sign_in_over_plain_http_leaves_the_cookie_without_secure() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let password = seeded_user(&serve).await;

        let response = login_submit(
            State(web_state(serve)),
            CookieJar::new(),
            headers(&[("host", "box.lan:8477")]),
            Form(LoginForm {
                username: "operator".to_owned(),
                password,
            }),
        )
        .await;

        let cookie = session_set_cookie(&response).expect("a sign-in must set the session cookie");
        assert!(
            !cookie.contains("Secure"),
            "a Secure cookie on the plain-HTTP LAN is dropped by the browser, which \
             presents as a login that does nothing: {cookie}"
        );
    }

    #[tokio::test]
    async fn a_sign_in_behind_a_tls_proxy_marks_the_cookie_secure() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let password = seeded_user(&serve).await;

        let response = login_submit(
            State(web_state(serve)),
            CookieJar::new(),
            headers(&[("host", "sharerr.example"), ("x-forwarded-proto", "https")]),
            Form(LoginForm {
                username: "operator".to_owned(),
                password,
            }),
        )
        .await;

        let cookie = session_set_cookie(&response).expect("a sign-in must set the session cookie");
        assert!(cookie.contains("; Secure"), "{cookie}");
        assert!(cookie.contains("HttpOnly"), "{cookie}");
        assert!(cookie.contains("SameSite=Strict"), "{cookie}");
    }

    #[tokio::test]
    async fn a_first_run_setup_behind_a_tls_proxy_marks_the_cookie_secure() {
        // The other route into `sign_in`. A first run through a proxy is how most
        // operators first meet this cookie, and it is the one sign-in that cannot
        // be repeated to correct a bad flag.
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);

        let password = fresh_password();
        let response = setup_submit(
            State(state),
            CookieJar::new(),
            headers(&[("forwarded", "proto=https")]),
            Form(SetupForm {
                username: "operator".to_owned(),
                password: password.clone(),
                confirm: password,
            }),
        )
        .await;

        let cookie = session_set_cookie(&response).expect("setup must sign the operator in");
        assert!(cookie.contains("; Secure"), "{cookie}");
    }

    #[tokio::test]
    async fn logout_expires_the_cookie_on_the_path_it_was_set_on() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);
        let token = state.sessions.create("operator").await.unwrap();

        // Assembled from a request header rather than with `.add`, because
        // `CookieJar::remove` only emits a removal for a cookie the request
        // actually carried — a jar the handler built itself would produce no
        // `Set-Cookie` at all and this test would assert on nothing.
        let sent = format!("{COOKIE_NAME}={token}");
        let jar = CookieJar::from_headers(&headers(&[("cookie", &sent)]));

        let response = logout(
            State(state.clone()),
            jar,
            headers(&[("host", "box.lan:8477")]),
        )
        .await;

        let cookie = session_set_cookie(&response).expect("logout must expire the cookie");
        assert!(
            cookie.contains("Path=/"),
            "a removal is only honoured when its Path matches the original's: {cookie}"
        );
        assert!(cookie.contains("Max-Age=0"), "{cookie}");
        assert!(!cookie.contains("Secure"), "{cookie}");
        assert!(state.sessions.touch(&token).await.is_none());
    }

    #[tokio::test]
    async fn logout_behind_a_tls_proxy_expires_a_secure_cookie() {
        // A removal missing `Secure` cannot overwrite a cookie that has it, so the
        // scheme has to be worked out again here rather than assumed to be plain.
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);
        let token = state.sessions.create("operator").await.unwrap();
        let sent = format!("{COOKIE_NAME}={token}");
        let jar = CookieJar::from_headers(&headers(&[("cookie", &sent)]));

        let response = logout(
            State(state),
            jar,
            headers(&[("x-forwarded-proto", "https")]),
        )
        .await;

        let cookie = session_set_cookie(&response).expect("logout must expire the cookie");
        assert!(cookie.contains("; Secure"), "{cookie}");
        assert!(cookie.contains("Path=/"), "{cookie}");
    }

    #[tokio::test]
    async fn logout_clears_both_the_session_and_the_cookie() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);
        let token = state.sessions.create("operator").await.unwrap();
        let jar = CookieJar::new().add(session_cookie(token.clone(), false));

        let response = logout(State(state.clone()), jar, HeaderMap::new()).await;

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::LOCATION)
                .unwrap(),
            "/login"
        );
        assert!(state.sessions.touch(&token).await.is_none());
    }

    #[tokio::test]
    async fn change_password_without_a_session_bounces_to_login() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let response = change_password(
            State(web_state(serve)),
            CookieJar::new(),
            Form(ChangePasswordForm {
                current_password: fresh_password(),
                new_password: fresh_password(),
                confirm_password: fresh_password(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::LOCATION)
                .unwrap(),
            "/login"
        );
    }

    #[tokio::test]
    async fn change_password_rejects_the_wrong_current_password() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        seeded_user(&serve).await;
        let state = web_state(serve);
        let token = state.sessions.create("operator").await.unwrap();
        let jar = CookieJar::new().add(session_cookie(token, false));

        let new_password = fresh_password();
        let response = change_password(
            State(state),
            jar,
            Form(ChangePasswordForm {
                current_password: fresh_password(),
                new_password: new_password.clone(),
                confirm_password: new_password,
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(
            body_of(response)
                .await
                .contains("not your current password")
        );
    }

    #[tokio::test]
    async fn change_password_succeeds_and_revokes_every_other_session() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let current_password = seeded_user(&serve).await;
        let store = serve.store().await.unwrap();
        let state = web_state(serve);
        let current = state.sessions.create("operator").await.unwrap();
        let other = state.sessions.create("operator").await.unwrap();
        let jar = CookieJar::new().add(session_cookie(current.clone(), false));

        let new_password = fresh_password();
        let response = change_password(
            State(state.clone()),
            jar,
            Form(ChangePasswordForm {
                current_password,
                new_password: new_password.clone(),
                confirm_password: new_password.clone(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::LOCATION)
                .unwrap(),
            "/settings?saved=account"
        );
        assert!(
            store
                .verify_password("operator", &SecretString::from(new_password))
                .await
                .unwrap()
        );
        assert!(state.sessions.touch(&current).await.is_some());
        assert!(state.sessions.touch(&other).await.is_none());
    }

    #[test]
    fn internal_reports_the_message_as_a_500() {
        let response = internal("something broke");
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
