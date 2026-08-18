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
//! somewhere with no egress — a home server behind a VPN container is the normal
//! case — so a CDN reference would simply hang, and a `ServeDir` would mean
//! shipping files alongside the binary that the Dockerfile deliberately does not
//! copy.

pub mod auth;
pub mod config_io;
pub mod diagnostics;
pub mod items;
pub mod peers;
pub mod probe;
pub mod settings;
pub mod templates;

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::middleware;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use sharerr_core::endpoint::now_epoch;
use sharerr_store::master_key_from_env;

use crate::state::{RECOVERY_INTERVAL, ServeState};
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

impl WebState {
    /// Fetch one stored secret.
    ///
    /// Returns `Ok(None)` for "not stored yet", which is a different message from
    /// "the vault will not open" — the first is a field to fill in, the second is
    /// a missing environment variable.
    pub(crate) async fn secret(
        &self,
        key: &'static str,
    ) -> Result<Option<secrecy::SecretString>, String> {
        self.serve
            .open_vault()
            .await?
            .get(key)
            .map_err(|err| err.to_string())
    }

    /// The store, or the one 503 every handler answers with when the database
    /// cannot open. Written here once so handlers cannot drift into inventing
    /// their own failure semantics for the same condition.
    pub(crate) async fn store_or_503(&self) -> Result<sharerr_store::Store, Response> {
        self.serve
            .store()
            .await
            .map_err(|reason| (StatusCode::SERVICE_UNAVAILABLE, reason).into_response())
    }
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
        // Diagnostics merged into the page above; kept as a redirect so an old
        // bookmark or a link in an issue still lands somewhere useful.
        .route("/diagnostics", get(|| async { Redirect::to("/") }))
        .route("/items", get(items::page))
        .route("/peers", get(peers::page).post(peers::add))
        .route("/peers/{id}/scope", post(peers::set_scope))
        .route("/peers/{id}/gossip", post(peers::set_gossip))
        .route("/peers/{id}/revoke", post(peers::revoke))
        .route("/peers/{id}/delete", post(peers::delete))
        .route("/peers/{id}/feed", get(peers::feed_preview))
        .route("/settings", get(settings::page))
        .route("/settings/general", post(settings::save_general))
        .route("/settings/arr/{source}", post(settings::save_arr))
        .route("/settings/qbittorrent", post(settings::save_qbittorrent))
        .route("/settings/tracker", post(settings::save_tracker))
        .route("/settings/gluetun", post(settings::save_gluetun))
        .route(
            "/settings/gluetun/client",
            post(settings::save_gluetun_client),
        )
        .route("/settings/libraries", post(settings::save_libraries))
        .route("/settings/paths", post(settings::save_paths))
        .route("/settings/sync", post(settings::save_sync))
        .route(
            "/settings/notifications",
            post(settings::save_notifications),
        )
        .route(
            "/settings/generate/{field}",
            post(settings::generate_secret),
        )
        .route("/settings/test/{service}", post(probe::test))
        .route("/settings/account/password", post(auth::change_password))
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

/// The one page a signed-in operator lands on: what is working, what is not,
/// and why. Used to be two pages — this one's glance, and Diagnostics' deeper
/// checks — merged because both answered "is this instance healthy" and a
/// person chasing a problem had to know a second page existed at all.
async fn status_page(State(state): State<WebState>) -> Response {
    let config = state.serve.config().await;
    // Concurrently, because they share no data and `gather` is the expensive
    // half — it probes every *arr over HTTP and stats every discovered file,
    // while `glance` is two store queries. Run in series the cheap one sat
    // behind the slow one on the page an operator lands on.
    let (diag, glance) = tokio::join!(diagnostics::gather(&state), glance(&state));

    render(&StatusPage {
        signed_in: true,
        glance,
        blocked: state.serve.blocked_reason().await,
        config_error: state.serve.config_error().await,
        recovery_secs: RECOVERY_INTERVAL.as_secs(),
        // Checked live rather than cached: the fix is to set the variable and
        // restart, and this banner is how the operator learns that is still needed.
        master_key_present: master_key_from_env().is_ok(),
        tag: config.tag.clone(),
        client_name: config.torrent_backend.as_str(),
        client_url: config.torrent_client().url.to_string(),
        sync_enabled: config.sync.enabled,
        sync_interval_secs: config.sync.interval_secs,
        config_path: state.serve.config_path().display().to_string(),

        services: diag.services,
        scanned: diag.scanned,
        rules: diag.rules,
        checked: diag.checked,
        unmapped: diag.unmapped,
        missing: diag.missing,
        more_missing: diag.more_missing,
        invalid: diag.invalid,
        sample: diag.sample,
        readable: diag.readable,
        healthy: diag.healthy,
        gluetun: diag.gluetun,
        swarm_peers: diag.swarm_peers,
        swarm_seeders: diag.swarm_seeders,
        runs: diag.runs,
    })
}

/// The one-glance numbers, gathered from the store and the live swarms.
///
/// `None` when the database is unavailable — the page's existing banners
/// already name that problem, and a strip of zeros next to them would claim
/// "nothing shared" when the truth is "cannot tell".
async fn glance(state: &WebState) -> Option<crate::web::templates::Glance> {
    let store = state.serve.store().await.ok()?;

    let items_shared = store.count_seeding().await.unwrap_or(0);

    // The most recent *finished* run. An in-flight run has no outcome yet, and
    // "last sync: just now" while it still churns would overpromise.
    let last_run = store
        .recent_runs(1)
        .await
        .unwrap_or_default()
        .into_iter()
        .next()
        .filter(|run| run.finished_at.is_some());
    let (last_sync, last_sync_note, last_sync_failed) = match &last_run {
        Some(run) => {
            let when = run.finished_at.map(peers::ago);
            // The glance leads with the timestamp, so the discovered count is
            // left off here; Diagnostics keeps it.
            let (note, failed) = run.summary.describe(false);
            (when, note, failed)
        }
        None => (None, String::new(), false),
    };

    let now = now_epoch();
    let peers = store.list_peers().await.unwrap_or_default();
    let active: Vec<_> = peers.iter().filter(|p| !p.is_revoked()).collect();
    let friends_recent = active
        .iter()
        .filter(|p| p.last_seen_at.is_some_and(|at| now - at < 3600))
        .count();

    let swarm = state.serve.swarms().stats().await;

    Some(crate::web::templates::Glance {
        items_shared,
        last_sync,
        last_sync_note,
        last_sync_failed,
        friends_recent,
        friends_total: active.len(),
        swarm_peers: swarm.peers,
        swarm_seeders: swarm.seeders,
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::state::fixtures::unconfigured;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    /// Drive the *assembled* router, middleware and all.
    ///
    /// Everything under `web/` was previously tested one helper at a time, which
    /// left the two properties that actually matter unasserted: that the auth guard
    /// is wired to every protected route, and that the cross-origin layer really
    /// does cover the public POSTs. Both are facts about how `routes()` composes
    /// its layers, and neither is observable from a unit test of the handler.
    fn router() -> (tempfile::TempDir, Router) {
        let (dir, serve) = unconfigured();
        (dir, routes(serve))
    }

    async fn send(router: Router, request: Request<Body>) -> axum::response::Response {
        router.oneshot(request).await.unwrap()
    }

    fn get(path: &str) -> Request<Body> {
        Request::builder().uri(path).body(Body::empty()).unwrap()
    }

    fn post(path: &str) -> axum::http::request::Builder {
        Request::builder().method("POST").uri(path)
    }

    fn location(response: &axum::response::Response) -> &str {
        response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
    }

    /// The one-glance numbers, checked against a store with known contents.
    #[tokio::test]
    async fn the_glance_counts_items_friends_and_runs() {
        use sharerr_core::model::ShareState;

        let (_dir, serve) = unconfigured();
        let store = serve.store().await.unwrap();

        // One seeding item, one that is not there yet — only the first counts.
        let mut item = sharerr_core::SharedItem {
            id: None,
            source: sharerr_core::MediaSource::Sonarr,
            source_id: 7,
            file_id: 1,
            spec: sharerr_core::MediaSpec::Episode {
                series_title: "Lanternwick Hollow".to_owned(),
                season: 1,
                episode: 1,
            },
            release_title: "Lanternwick.Hollow.S01E01".to_owned(),
            arr_path: std::path::PathBuf::from("/tv/x.mkv"),
            size: 1024,
            ids: sharerr_core::ExternalIds::default(),
            info_hash: None,
            announce_token_fp: None,
            state: ShareState::Pending,
            last_error: None,
            created_at: None,
        };
        store.upsert(&item).await.unwrap();
        store
            .set_info_hash(item.source, item.file_id, &"aa".repeat(20))
            .await
            .unwrap();
        store
            .set_state(item.source, item.file_id, ShareState::Seeding, None)
            .await
            .unwrap();
        item.file_id = 2;
        store.upsert(&item).await.unwrap();

        // One friend seen just now, one never — "1 of 2".
        let sam = store
            .create_peer(
                "Sam",
                &secrecy::SecretString::from("sam-key"),
                sharerr_store::PeerScope::All,
            )
            .await
            .unwrap();
        store.touch_peer(sam.id).await.unwrap();
        store
            .create_peer(
                "Alex",
                &secrecy::SecretString::from("alex-key"),
                sharerr_store::PeerScope::All,
            )
            .await
            .unwrap();

        // A finished run with something to report.
        let run = store.begin_run().await.unwrap();
        store
            .finish_run(
                run,
                &sharerr_store::RunSummary {
                    discovered: 2,
                    added: 1,
                    unshared: 0,
                    failed: 0,
                    error: None,
                },
            )
            .await
            .unwrap();

        let state = WebState {
            serve,
            sessions: Arc::new(Sessions::default()),
        };
        let glance = glance(&state).await.expect("the store is available");

        assert_eq!(glance.items_shared, 1);
        assert_eq!((glance.friends_recent, glance.friends_total), (1, 2));
        assert_eq!(glance.last_sync.as_deref(), Some("just now"));
        assert_eq!(glance.last_sync_note, "1 added");
        assert!(!glance.last_sync_failed);
        assert_eq!(glance.swarm_peers, 0, "nobody has announced");
    }

    /// Every protected route must refuse an anonymous caller.
    ///
    /// Enumerated rather than spot-checked: the guard is applied with a single
    /// `route_layer` over the group, and the failure mode being guarded against is
    /// somebody adding a route *outside* that group. A spot check of one route
    /// would not notice.
    #[tokio::test]
    async fn every_protected_route_refuses_an_anonymous_visitor() {
        let protected_gets = [
            "/",
            "/settings",
            "/diagnostics",
            "/items",
            "/peers",
            "/peers/1/feed",
        ];
        let protected_posts = [
            "/settings/general",
            "/settings/arr/sonarr",
            "/settings/arr/lidarr",
            "/settings/qbittorrent",
            "/settings/tracker",
            "/settings/gluetun",
            "/settings/gluetun/client",
            "/settings/libraries",
            "/settings/paths",
            "/settings/sync",
            "/settings/notifications",
            "/settings/generate/tracker",
            "/settings/test/sonarr",
            "/settings/account/password",
            "/peers",
            "/peers/1/scope",
            "/peers/1/revoke",
            "/peers/1/delete",
        ];

        for path in protected_gets {
            let (_dir, app) = router();
            let response = send(app, get(path)).await;
            assert_eq!(
                response.status(),
                StatusCode::SEE_OTHER,
                "GET {path} must redirect an anonymous visitor, got {:?}",
                response.status()
            );
            // No account exists on this instance, so the destination is /setup.
            assert_eq!(location(&response), "/setup", "GET {path}");
        }

        for path in protected_posts {
            let (_dir, app) = router();
            let response = send(app, post(path).body(Body::empty()).unwrap()).await;
            assert_ne!(
                response.status(),
                StatusCode::OK,
                "POST {path} must not succeed for an anonymous visitor"
            );
            assert_eq!(
                response.status(),
                StatusCode::SEE_OTHER,
                "POST {path} must redirect rather than run, got {:?}",
                response.status()
            );
        }
    }

    /// The public routes must stay public, or a fresh instance is unusable.
    #[tokio::test]
    async fn the_setup_and_login_pages_are_reachable_without_a_session() {
        for path in ["/setup", "/login"] {
            let (_dir, app) = router();
            let response = send(app, get(path)).await;
            assert!(
                response.status().is_success() || response.status().is_redirection(),
                "{path} returned {:?}",
                response.status()
            );
        }
    }

    /// `/assets/*` sits outside the guard so the login page can style itself —
    /// a stylesheet behind a login is a login page with no stylesheet.
    #[tokio::test]
    async fn assets_are_served_without_a_session() {
        let (_dir, app) = router();
        let response = send(app, get("/assets/style.css")).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn an_unknown_asset_is_not_found_rather_than_a_redirect() {
        let (_dir, app) = router();
        let response = send(app, get("/assets/../secrets")).await;
        assert_ne!(response.status(), StatusCode::OK);
    }

    /// The CSRF defence, asserted over the real router rather than over the
    /// helper. `deny_cross_origin` is applied as a `.layer` on the merged router,
    /// and the whole point of putting it there was that it covers `/login` and
    /// `/setup` too — which sit *outside* the auth guard and are the most
    /// attackable POSTs on the instance.
    #[tokio::test]
    async fn a_cross_origin_post_is_refused_on_public_routes_too() {
        for path in ["/login", "/setup", "/logout"] {
            let (_dir, app) = router();
            let request = post(path)
                .header("origin", "https://evil.example")
                .header("host", "box.lan:8477")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("username=a&password=b"))
                .unwrap();

            let response = send(app, request).await;
            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "cross-origin POST to {path} must be refused"
            );
        }
    }

    /// A same-origin POST must get past the layer — a CSRF check that rejects
    /// everything would pass the test above while breaking the application.
    #[tokio::test]
    async fn a_same_origin_post_is_not_refused_by_the_csrf_layer() {
        let (_dir, app) = router();
        let request = post("/login")
            .header("origin", "http://box.lan:8477")
            .header("host", "box.lan:8477")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from("username=a&password=b"))
            .unwrap();

        let response = send(app, request).await;
        assert_ne!(
            response.status(),
            StatusCode::FORBIDDEN,
            "a same-origin sign-in attempt must reach the handler"
        );
    }

    /// GET is exempt from the origin check, or every ordinary page load from a
    /// browser that sends `Origin` would be refused.
    #[tokio::test]
    async fn a_cross_origin_get_is_allowed() {
        let (_dir, app) = router();
        let request = Request::builder()
            .uri("/login")
            .header("origin", "https://evil.example")
            .header("host", "box.lan:8477")
            .body(Body::empty())
            .unwrap();

        let response = send(app, request).await;
        assert_ne!(response.status(), StatusCode::FORBIDDEN);
    }
}
