//! Two-step sign-in: a TOTP code after the password, for an account that
//! turned it on under Settings → Account.
//!
//! The password step is unchanged (`auth::login_submit`); when it passes for
//! an account with a secret, it hands over to [`begin`], which parks the
//! sign-in under a short-lived cookie until `/login/totp` gets a valid code.
//! Codes come from [`crate::totp`]; the store refuses a step already used, so
//! a code is good once. Enrolling is stateless: the fresh secret travels in
//! the form and is stored only once a code from it, and the password, check
//! out. Signing in through a forward-auth proxy never reaches any of this.

use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};
use secrecy::SecretString;
use serde::Deserialize;
use sharerr_core::endpoint::now_epoch;
use sharerr_store::{Store, StoreError};

use crate::web::WebState;
use crate::web::auth::{
    COOKIE_NAME, PENDING_COOKIE, arrived_over_https, current_user, internal, sign_in, too_busy,
};
use crate::web::templates::{LoginPage, TotpLoginPage, TotpPage, render};

/// The pending-sign-in cookie: scoped to `/login`, gone when the step is.
fn pending_cookie(token: String, secure: bool) -> Cookie<'static> {
    Cookie::build((PENDING_COOKIE, token))
        .path("/login")
        .http_only(true)
        .same_site(SameSite::Strict)
        .secure(secure)
        .max_age(time::Duration::minutes(5))
        .build()
}

/// The password was right and the account has a second factor: park the
/// sign-in and ask for the code.
pub(crate) async fn begin(
    state: &WebState,
    jar: CookieJar,
    headers: &HeaderMap,
    username: &str,
) -> Response {
    match state.sessions.begin_second_factor(username).await {
        Ok(token) => (
            jar.add(pending_cookie(token, arrived_over_https(headers))),
            Redirect::to("/login/totp"),
        )
            .into_response(),
        Err(reason) => internal(&reason),
    }
}

async fn pending(state: &WebState, jar: &CookieJar) -> Option<(String, String)> {
    let token = jar.get(PENDING_COOKIE)?.value().to_owned();
    let username = state.sessions.pending_user(&token).await?;
    Some((token, username))
}

pub async fn login_page(State(state): State<WebState>, jar: CookieJar) -> Response {
    match pending(&state, &jar).await {
        Some(_) => render(&TotpLoginPage {
            signed_in: false,
            error: None,
        }),
        None => Redirect::to("/login").into_response(),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct CodeForm {
    code: String,
}

pub async fn login_submit(
    State(state): State<WebState>,
    jar: CookieJar,
    headers: HeaderMap,
    Form(form): Form<CodeForm>,
) -> Response {
    let Some((token, username)) = pending(&state, &jar).await else {
        return Redirect::to("/login").into_response();
    };
    let store = match state.store_or_503().await {
        Ok(store) => store,
        Err(response) => return *response,
    };

    match check_code(&store, &username, &form.code).await {
        Ok(true) => {
            state.sessions.finish_second_factor(&token).await;
            let jar = jar.remove(pending_cookie(String::new(), arrived_over_https(&headers)));
            sign_in(&state, jar, &headers, &username, "/").await
        }
        Ok(false) => {
            tracing::warn!(%username, "rejected a sign-in code");
            if state.sessions.fail_second_factor(&token).await {
                (
                    StatusCode::UNAUTHORIZED,
                    render(&TotpLoginPage {
                        signed_in: false,
                        error: Some("That code is not valid.".to_owned()),
                    }),
                )
                    .into_response()
            } else {
                let jar = jar.remove(pending_cookie(String::new(), arrived_over_https(&headers)));
                (
                    StatusCode::UNAUTHORIZED,
                    jar,
                    render(&LoginPage::rejected(
                        &username,
                        "Too many wrong codes. Sign in again.",
                    )),
                )
                    .into_response()
            }
        }
        Err(err) => internal(&format!("checking the code: {err}")),
    }
}

/// Whether `code` is current for `username`'s stored secret, spending its
/// step if so.
async fn check_code(store: &Store, username: &str, code: &str) -> Result<bool, StoreError> {
    let Some(secret) = store.totp_secret(username).await? else {
        return Ok(false);
    };
    match crate::totp::verify(&secret, code, now_epoch()) {
        Some(step) => store.claim_totp_step(username, step).await,
        None => Ok(false),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct PageQuery {
    saved: Option<String>,
}

pub async fn account_page(
    State(state): State<WebState>,
    jar: CookieJar,
    Query(query): Query<PageQuery>,
) -> Response {
    let Some(username) = current_user(&state, &jar).await else {
        return Redirect::to("/login").into_response();
    };
    let store = match state.store_or_503().await {
        Ok(store) => store,
        Err(response) => return *response,
    };
    match store.totp_secret(&username).await {
        Ok(Some(_)) => render(&enabled_page(username, query.saved, None)),
        Ok(None) => match crate::totp::generate_secret() {
            Ok(secret) => enrol_page(username, secret, query.saved, None),
            Err(reason) => internal(&reason),
        },
        Err(err) => internal(&format!("reading the account: {err}")),
    }
}

fn enabled_page(username: String, saved: Option<String>, error: Option<String>) -> TotpPage {
    TotpPage {
        signed_in: true,
        username,
        enabled: true,
        secret: String::new(),
        qr_svg: String::new(),
        otpauth_uri: String::new(),
        saved,
        error,
    }
}

fn enrol_page(
    username: String,
    secret: String,
    saved: Option<String>,
    error: Option<String>,
) -> Response {
    let otpauth_uri = crate::totp::otpauth_uri(&username, &secret);
    match crate::totp::qr_svg(&otpauth_uri) {
        Ok(qr_svg) => render(&TotpPage {
            signed_in: true,
            username,
            enabled: false,
            secret,
            qr_svg,
            otpauth_uri,
            saved,
            error,
        }),
        Err(reason) => internal(&format!("rendering the QR code: {reason}")),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct EnableForm {
    secret: String,
    code: String,
    current_password: String,
}

/// Turn the factor on, once the password and a code from the new secret
/// both check out. Every other session was signed in without it, so they
/// are signed out.
pub async fn enable(
    State(state): State<WebState>,
    jar: CookieJar,
    Form(form): Form<EnableForm>,
) -> Response {
    let Some(username) = current_user(&state, &jar).await else {
        return Redirect::to("/login").into_response();
    };
    let store = match state.store_or_503().await {
        Ok(store) => store,
        Err(response) => return *response,
    };
    let retry = |message: &str| {
        enrol_page(
            username.clone(),
            form.secret.clone(),
            None,
            Some(message.to_owned()),
        )
    };
    if !crate::totp::is_valid_secret(&form.secret) {
        return Redirect::to("/settings/account/totp").into_response();
    }
    match password_ok(&store, &username, &form.current_password).await {
        Ok(true) => {}
        Ok(false) => return retry("That is not your current password."),
        Err(response) => return *response,
    }
    let Some(step) = crate::totp::verify(&form.secret, &form.code, now_epoch()) else {
        return retry("That code does not match. Check the phone's clock and try the next one.");
    };
    match store.set_totp_secret(&username, Some(&form.secret)).await {
        Ok(true) => {}
        Ok(false) => return internal("that account no longer exists"),
        Err(err) => return internal(&format!("saving the secret: {err}")),
    }
    // The confirming code is spent here, so it cannot also sign someone in.
    if let Err(err) = store.claim_totp_step(&username, step).await {
        return internal(&format!("saving the secret: {err}"));
    }
    if let Some(cookie) = jar.get(COOKIE_NAME) {
        state.sessions.revoke_all_except(cookie.value()).await;
    }
    tracing::info!(%username, "two-step sign-in turned on");
    Redirect::to("/settings/account/totp?saved=on").into_response()
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct DisableForm {
    code: String,
    current_password: String,
}

/// Turn the factor off: the password and a current code, so a borrowed
/// session alone cannot strip it.
pub async fn disable(
    State(state): State<WebState>,
    jar: CookieJar,
    Form(form): Form<DisableForm>,
) -> Response {
    let Some(username) = current_user(&state, &jar).await else {
        return Redirect::to("/login").into_response();
    };
    let store = match state.store_or_503().await {
        Ok(store) => store,
        Err(response) => return *response,
    };
    let refuse = |message: &str| {
        render(&enabled_page(
            username.clone(),
            None,
            Some(message.to_owned()),
        ))
    };
    match password_ok(&store, &username, &form.current_password).await {
        Ok(true) => {}
        Ok(false) => return refuse("That is not your current password."),
        Err(response) => return *response,
    }
    match check_code(&store, &username, &form.code).await {
        Ok(true) => {}
        Ok(false) => return refuse("That code is not valid."),
        Err(err) => return internal(&format!("checking the code: {err}")),
    }
    match store.set_totp_secret(&username, None).await {
        Ok(_) => {
            tracing::info!(%username, "two-step sign-in turned off");
            Redirect::to("/settings/account/totp?saved=off").into_response()
        }
        Err(err) => internal(&format!("clearing the secret: {err}")),
    }
}

/// Boxed like `WebState::store_or_503`'s error, to keep the `Result` small.
async fn password_ok(store: &Store, username: &str, password: &str) -> Result<bool, Box<Response>> {
    let password = SecretString::from(password.to_owned());
    match store.verify_password(username, &password).await {
        Ok(verified) => Ok(verified),
        Err(StoreError::TooBusy) => Err(Box::new(too_busy())),
        Err(err) => Err(Box::new(internal(&format!("checking the password: {err}")))),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::web::web_state;
    use sharerr_testkit::secrets::fresh_password;

    /// An account with two-step sign-in on, and its password and secret.
    async fn enrolled(state: &WebState) -> (String, String) {
        let password = fresh_password();
        let store = state.serve.store().await.unwrap();
        store
            .create_user("operator", &SecretString::from(password.clone()))
            .await
            .unwrap();
        let secret = crate::totp::generate_secret().unwrap();
        store
            .set_totp_secret("operator", Some(&secret))
            .await
            .unwrap();
        (password, secret)
    }

    fn code(secret: &str) -> String {
        crate::totp::code_at(secret, now_epoch())
    }

    async fn pending_jar(state: &WebState) -> CookieJar {
        let token = state
            .sessions
            .begin_second_factor("operator")
            .await
            .unwrap();
        CookieJar::new().add(Cookie::new(PENDING_COOKIE, token))
    }

    async fn signed_in_jar(state: &WebState) -> (CookieJar, String) {
        let token = state.sessions.create("operator").await.unwrap();
        (
            CookieJar::new().add(Cookie::new(COOKIE_NAME, token.clone())),
            token,
        )
    }

    fn location(response: &Response) -> &str {
        response
            .headers()
            .get(axum::http::header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
    }

    async fn body_of(response: Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    fn code_form(code: String) -> Form<CodeForm> {
        Form(CodeForm { code })
    }

    #[tokio::test]
    async fn the_code_step_needs_a_pending_sign_in() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);
        let response = login_page(State(state.clone()), CookieJar::new()).await;
        assert_eq!(location(&response), "/login");

        let jar = pending_jar(&state).await;
        let response = login_page(State(state), jar).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn a_current_code_signs_in_once_and_cannot_be_replayed() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);
        let (_, secret) = enrolled(&state).await;
        let current = code(&secret);

        let jar = pending_jar(&state).await;
        let response = login_submit(
            State(state.clone()),
            jar,
            HeaderMap::new(),
            code_form(current.clone()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(location(&response), "/");

        // The same code, on a fresh password step, is spent.
        let jar = pending_jar(&state).await;
        let response = login_submit(State(state), jar, HeaderMap::new(), code_form(current)).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(body_of(response).await.contains("not valid"));
    }

    #[tokio::test]
    async fn too_many_wrong_codes_send_the_operator_back_to_the_password() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);
        enrolled(&state).await;
        let jar = pending_jar(&state).await;

        for _ in 0..4 {
            let response = login_submit(
                State(state.clone()),
                jar.clone(),
                HeaderMap::new(),
                code_form("000000".to_owned()),
            )
            .await;
            assert!(body_of(response).await.contains("not valid"));
        }
        let response = login_submit(
            State(state.clone()),
            jar.clone(),
            HeaderMap::new(),
            code_form("000000".to_owned()),
        )
        .await;
        assert!(body_of(response).await.contains("Sign in again"));
        assert_eq!(location(&login_page(State(state), jar).await), "/login");
    }

    #[tokio::test]
    async fn enrolling_needs_the_password_and_a_code_and_signs_out_other_sessions() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);
        let password = fresh_password();
        let store = state.serve.store().await.unwrap();
        store
            .create_user("operator", &SecretString::from(password.clone()))
            .await
            .unwrap();
        let (jar, _) = signed_in_jar(&state).await;
        let (_, other) = signed_in_jar(&state).await;

        let page = account_page(
            State(state.clone()),
            jar.clone(),
            Query(PageQuery::default()),
        )
        .await;
        assert!(body_of(page).await.contains("<svg"), "the enrolment QR");

        let secret = crate::totp::generate_secret().unwrap();
        let form = |code: String, current_password: String| {
            Form(EnableForm {
                secret: secret.clone(),
                code,
                current_password,
            })
        };

        let response = enable(
            State(state.clone()),
            jar.clone(),
            form(code(&secret), fresh_password()),
        )
        .await;
        assert!(
            body_of(response)
                .await
                .contains("not your current password")
        );
        let response = enable(
            State(state.clone()),
            jar.clone(),
            form("000000".to_owned(), password.clone()),
        )
        .await;
        assert!(body_of(response).await.contains("does not match"));
        assert_eq!(store.totp_secret("operator").await.unwrap(), None);

        let response = enable(
            State(state.clone()),
            jar.clone(),
            form(code(&secret), password),
        )
        .await;
        assert_eq!(location(&response), "/settings/account/totp?saved=on");
        assert_eq!(store.totp_secret("operator").await.unwrap(), Some(secret));
        let stale = CookieJar::new().add(Cookie::new(COOKIE_NAME, other));
        assert_eq!(
            current_user(&state, &stale).await,
            None,
            "other sessions end"
        );
        assert_eq!(
            current_user(&state, &jar).await.as_deref(),
            Some("operator")
        );
    }

    #[tokio::test]
    async fn a_secret_the_form_did_not_mint_is_refused() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);
        let password = fresh_password();
        state
            .serve
            .store()
            .await
            .unwrap()
            .create_user("operator", &SecretString::from(password.clone()))
            .await
            .unwrap();
        let (jar, _) = signed_in_jar(&state).await;
        let weak = "AAAA".to_owned();
        let response = enable(
            State(state),
            jar,
            Form(EnableForm {
                code: code(&weak),
                secret: weak,
                current_password: password,
            }),
        )
        .await;
        assert_eq!(location(&response), "/settings/account/totp");
    }

    #[tokio::test]
    async fn switching_off_needs_the_password_and_a_current_code() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);
        let (password, secret) = enrolled(&state).await;
        let (jar, _) = signed_in_jar(&state).await;
        let store = state.serve.store().await.unwrap();

        let page = account_page(
            State(state.clone()),
            jar.clone(),
            Query(PageQuery::default()),
        )
        .await;
        assert!(body_of(page).await.contains("Turn off"));

        let form = |code: String, current_password: String| {
            Form(DisableForm {
                code,
                current_password,
            })
        };
        let response = disable(
            State(state.clone()),
            jar.clone(),
            form(code(&secret), fresh_password()),
        )
        .await;
        assert!(
            body_of(response)
                .await
                .contains("not your current password")
        );
        let response = disable(
            State(state.clone()),
            jar.clone(),
            form("000000".to_owned(), password.clone()),
        )
        .await;
        assert!(body_of(response).await.contains("not valid"));
        assert!(store.totp_secret("operator").await.unwrap().is_some());

        let response = disable(State(state), jar, form(code(&secret), password)).await;
        assert_eq!(location(&response), "/settings/account/totp?saved=off");
        assert_eq!(store.totp_secret("operator").await.unwrap(), None);
    }

    #[tokio::test]
    async fn the_account_page_wants_a_session() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let response = account_page(
            State(web_state(serve)),
            CookieJar::new(),
            Query(PageQuery::default()),
        )
        .await;
        assert_eq!(location(&response), "/login");
    }
}
