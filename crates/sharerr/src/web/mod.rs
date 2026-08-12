//! The web UI: everything an operator needs to configure sharerr from a browser.
//!
//! The CLI (`sharerr vault set`, hand-edited `sharerr.toml`) still works and is
//! still the right tool for a scripted deployment. This exists so that a plain
//! `docker run` is enough — nothing here should require an `exec` shell.
//!
//! # Assets
//!
//! CSS and htmx are compiled into the binary with `include_str!` rather than
//! served from a directory or fetched from a CDN. The container is expected to run
//! on a network with no egress — `docker/compose.test.yml` enforces exactly that
//! with `internal: true` — so a CDN reference would simply hang, and a
//! `ServeDir` would mean shipping files alongside the binary that the Dockerfile
//! deliberately does not copy.

pub mod auth;
pub mod config_io;
pub mod probe;
pub mod settings;
pub mod templates;

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use sharerr_store::master_key_from_env;

use crate::commands::serve::{RECOVERY_INTERVAL, ServeState};
use crate::web::auth::Sessions;
use crate::web::templates::{StatusPage, render};

/// What the web handlers share: the running server's state plus the session table.
///
/// Sessions live here rather than in [`ServeState`] because they are a property of
/// the UI, not of serving — `sharerr sync` and `sharerr doctor` construct nothing
/// like them.
#[derive(Clone, Debug)]
pub struct WebState {
    pub serve: Arc<ServeState>,
    pub sessions: Arc<Sessions>,
}

/// Every route the browser talks to, ready to merge into the server's router.
///
/// Returns a fully-stated `Router` so it composes with `/health` and `/ready`,
/// which must stay outside the guard: the Dockerfile's HEALTHCHECK curls `/health`
/// with no cookie, and putting it behind login would restart-loop the container an
/// operator is trying to sign into.
pub fn routes(serve: Arc<ServeState>) -> Router {
    let state = WebState {
        serve,
        sessions: Arc::new(Sessions::default()),
    };

    // One `route_layer` over the whole protected group rather than per route, so
    // adding a settings section cannot accidentally ship an unauthenticated
    // endpoint that writes to the vault.
    let protected = Router::new()
        .route("/", get(status_page))
        .route("/settings", get(settings::page))
        .route("/settings/general", post(settings::save_general))
        .route("/settings/sonarr", post(settings::save_sonarr))
        .route("/settings/radarr", post(settings::save_radarr))
        .route("/settings/qbittorrent", post(settings::save_qbittorrent))
        .route("/settings/tracker", post(settings::save_tracker))
        .route("/settings/paths", post(settings::save_paths))
        .route("/settings/sync", post(settings::save_sync))
        .route(
            "/settings/generate/{field}",
            post(settings::generate_secret),
        )
        .route("/settings/test/{service}", post(probe::test))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth,
        ));

    let public = Router::new()
        .route("/setup", get(auth::setup_page).post(auth::setup_submit))
        .route("/login", get(auth::login_page).post(auth::login_submit))
        .route("/logout", post(auth::logout))
        .route("/assets/{file}", get(asset));

    // The cross-origin check goes over *everything*, not just the settings routes:
    // `/login`, `/setup` and `/logout` sit outside the auth guard but are exactly
    // as state-changing, and attaching it per-route is how a future public POST
    // ships unprotected. It skips GET, so page loads and asset fetches are
    // untouched.
    protected
        .merge(public)
        .layer(middleware::from_fn(auth::deny_cross_origin))
        .with_state(state)
}

/// The one page a signed-in operator lands on: what is working, and what is not.
async fn status_page(State(state): State<WebState>) -> Response {
    let config = state.serve.config().await;

    render(&StatusPage {
        signed_in: true,
        blocked: state.serve.blocked_reason().await,
        recovery_secs: RECOVERY_INTERVAL.as_secs(),
        // Checked live rather than cached: the fix is to set the variable and
        // restart, and this banner is how the operator learns that is still needed.
        master_key_present: master_key_from_env().is_ok(),
        tag: config.tag.clone(),
        sonarr_url: config.sonarr.as_ref().map(|s| s.url.to_string()),
        radarr_url: config.radarr.as_ref().map(|r| r.url.to_string()),
        qbit_url: config.qbittorrent.url.to_string(),
        sync_enabled: config.sync.enabled,
        sync_interval_secs: config.sync.interval_secs,
        config_path: state.serve.config_path().display().to_string(),
    })
}

/// Serve one embedded asset.
///
/// An explicit match rather than a lookup table: the set is two files, and a match
/// makes it impossible for a path to escape into the filesystem.
async fn asset(axum::extract::Path(file): axum::extract::Path<String>) -> Response {
    let (body, mime) = match file.as_str() {
        "style.css" => (include_str!("assets/style.css"), "text/css; charset=utf-8"),
        "htmx.min.js" => (
            include_str!("assets/htmx.min.js"),
            "text/javascript; charset=utf-8",
        ),
        _ => return (StatusCode::NOT_FOUND, "no such asset").into_response(),
    };

    (
        [
            (header::CONTENT_TYPE, mime),
            // Safe to cache hard: the assets change only when the binary does, and
            // a stale stylesheet after an upgrade is a genuinely confusing bug to
            // chase. Not `immutable` — the URL carries no version.
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        body,
    )
        .into_response()
}
