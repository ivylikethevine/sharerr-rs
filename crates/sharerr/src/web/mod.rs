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
pub mod composition;
pub mod config_io;
pub mod debug;
pub mod diagnostics;
pub mod docs;
pub mod items;
pub mod peers;
pub mod probe;
pub mod settings;
pub mod templates;
pub mod topology;
pub mod wizard;

use std::sync::Arc;
use std::time::Duration;

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
    /// Per-source-address limit on `/login` and `/setup` POSTs — a UI
    /// property, same reasoning as [`Self::sessions`]. See
    /// [`crate::web::auth::Throttle`].
    pub throttle: Arc<crate::web::auth::Throttle>,
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
    ///
    /// The error is boxed only to keep this `Result` small — `Response` alone
    /// is well over clippy's `result_large_err` threshold, and every caller
    /// already destructures with `match` rather than `?`, so unboxing at the
    /// point of use is one extra `*`.
    pub(crate) async fn store_or_503(&self) -> Result<sharerr_store::Store, Box<Response>> {
        self.serve
            .store()
            .await
            .map_err(|reason| Box::new((StatusCode::SERVICE_UNAVAILABLE, reason).into_response()))
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
        throttle: Arc::new(crate::web::auth::Throttle::default()),
    };

    // One `route_layer` over the whole protected group rather than per route, so
    // adding a settings section cannot accidentally ship an unauthenticated
    // endpoint that writes to the vault.
    let protected = Router::new()
        .route("/", get(status_page))
        // Diagnostics merged into the page above; kept as a redirect so an old
        // bookmark or a link in an issue still lands somewhere useful.
        .route("/diagnostics", get(|| async { Redirect::to("/") }))
        .route("/status/tiles", get(stat_tiles))
        .route("/items", get(items::page))
        .route("/items/{source}/{file_id}", get(items::detail))
        .route("/items/{source}/{file_id}/retry", post(items::retry))
        .route("/items/{source}/{file_id}/rebuild", post(items::rebuild))
        .route("/items/{source}/{file_id}/unshare", post(items::unshare))
        .route("/topology", get(topology::page))
        .route("/debug", get(debug::page))
        .route("/peers", get(peers::page).post(peers::add))
        .route("/peers/export", get(peers::export))
        .route("/peers/{id}/scope", post(peers::set_scope))
        .route("/peers/{id}/gossip", post(peers::set_gossip))
        .route("/peers/{id}/revoke", post(peers::revoke))
        .route("/peers/{id}/delete", post(peers::delete))
        .route("/peers/{id}/feed", get(peers::feed_preview))
        .route("/wizard", get(wizard::welcome))
        .route("/wizard/services", get(wizard::services))
        .route("/wizard/paths", get(wizard::paths))
        .route("/wizard/tracker", get(wizard::tracker))
        .route("/wizard/done", get(wizard::done))
        .route("/settings", get(settings::page))
        .route("/settings/general", post(settings::save_general))
        .route("/settings/arr/{source}", post(settings::save_arr))
        .route("/settings/qbittorrent", post(settings::save_qbittorrent))
        .route("/settings/transmission", post(settings::save_transmission))
        .route("/settings/rtorrent", post(settings::save_rtorrent))
        .route(
            "/settings/torrent-backend",
            post(settings::save_torrent_backend),
        )
        .route("/settings/seeding", post(settings::save_seeding))
        .route("/settings/tracker", post(settings::save_tracker))
        .route(
            "/settings/tracker/finalize",
            post(settings::finalize_tracker),
        )
        .route("/settings/lighthouse", post(settings::save_lighthouse))
        .route("/settings/gluetun", post(settings::save_gluetun))
        .route(
            "/settings/gluetun/client",
            post(settings::save_gluetun_client),
        )
        .route("/settings/libraries", post(settings::save_libraries))
        .route("/settings/paths", post(settings::save_paths))
        .route("/settings/sync", post(settings::save_sync))
        .route("/settings/checks", post(settings::save_checks))
        .route(
            "/settings/notifications",
            post(settings::save_notifications),
        )
        .route("/settings/metrics", post(settings::save_metrics))
        .route("/settings/config/export", get(settings::export_config))
        .route("/settings/config/import", post(settings::import_config))
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

    // Throttled as its own group, `route_layer`'d before `/logout` and
    // `/assets` join — those are not an unauthenticated guessing surface, and
    // widening the guard to the whole public router would rate-limit an
    // asset fetch alongside a login attempt for no reason.
    let public = Router::new()
        .route("/setup", get(auth::setup_page).post(auth::setup_submit))
        .route("/login", get(auth::login_page).post(auth::login_submit))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::throttle_unauthenticated_posts,
        ))
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
/// and why. The glance and Diagnostics' deeper checks live together here
/// because both answer "is this instance healthy", and splitting them would
/// make a person chasing a problem hunt for a second page.
async fn status_page(State(state): State<WebState>) -> Response {
    let config = state.serve.config().await;
    // Concurrently, because they share no data and `gather` is the expensive
    // half — it probes every *arr over HTTP and stats every discovered file,
    // while `glance` is two store queries. Run in series the cheap one sat
    // behind the slow one on the page an operator lands on.
    let sync_every = config.sync.enabled.then_some(config.sync.interval_secs);
    let (diag, glance) = tokio::join!(diagnostics::gather(&state), glance(&state, sync_every));

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
        client_name: config.torrent_backend.display_name(),
        client_url: config.torrent_client().url.to_string(),
        sync_enabled: config.sync.enabled,
        sync_interval_secs: config.sync.interval_secs,
        config_path: state.serve.config_path().display().to_string(),
        diag,
    })
}

/// The status page's stat tiles on their own, for htmx to poll.
///
/// Deliberately *not* `status_page` minus the rendering: this calls `glance`
/// alone and never `diagnostics::gather`, so a browser left open on the status
/// page does not put a request on every configured *arr app every thirty
/// seconds. That distinction is the whole reason the fragment exists.
///
/// Always answers `200`, including when the store will not open — htmx does not
/// swap a non-2xx response, so an error status would leave the last good numbers
/// on screen looking current. The `None` branch of the partial says "unavailable"
/// instead, which is the truth.
async fn stat_tiles(State(state): State<WebState>) -> Response {
    let config = state.serve.config().await;
    let sync_every = config.sync.enabled.then_some(config.sync.interval_secs);

    render(&crate::web::templates::StatTiles {
        glance: glance(&state, sync_every).await,
    })
}

/// A `.toml` document offered as a download — the response shape every
/// backup/restore-style export on this instance shares (the full config, a
/// friends restore block): same content type, same `attachment` disposition,
/// differing only in `filename` and the already-serialized `text`.
pub(crate) fn toml_download(filename: &str, text: String) -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/toml".to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        text,
    )
        .into_response()
}

/// A `WebState` over a bare `ServeState`, for handler-level tests across the
/// web modules — one definition rather than a copy per test module.
#[cfg(test)]
pub(crate) fn web_state(serve: Arc<ServeState>) -> WebState {
    WebState {
        serve,
        sessions: Arc::new(Sessions::default()),
        throttle: Arc::new(crate::web::auth::Throttle::default()),
    }
}

/// The full UTF-8 body of a response, for tests that assert on rendered
/// content — one definition rather than a copy per test module.
///
/// `unwrap`: test-only, and not nested in a `mod tests { #![allow(..)] }`
/// block the way its callers are, since it needs to be reachable from all of
/// them — a bad body here is this helper's own bug, not something a caller
/// should have to handle.
#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub(crate) async fn body_of(response: Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// The `Location` header of a redirect, or `""` — shared by the tests that
/// assert where a save or a sign-in lands.
#[cfg(test)]
pub(crate) fn location(response: &Response) -> &str {
    response
        .headers()
        .get(header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
}

/// The one-glance numbers, gathered from the store and the live swarms.
///
/// `None` when the database is unavailable — the page's existing banners
/// already name that problem, and a strip of zeros next to them would claim
/// "nothing shared" when the truth is "cannot tell".
async fn glance(
    state: &WebState,
    sync_every: Option<u64>,
) -> Option<crate::web::templates::Glance> {
    let store = state.serve.store().await.ok()?;

    // One aggregate for both the count and the size — `PeerScope::All`
    // admits every source, so this is the whole seeding set.
    let seeding = store
        .seeding_summary(sharerr_store::PeerScope::All)
        .await
        .unwrap_or_default();
    let items_shared = seeding.count;
    let shared_size = if seeding.size > 0 {
        items::human_size(seeding.size.unsigned_abs())
    } else {
        String::new()
    };

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
    // The loop sleeps `interval` after each pass finishes, so the deadline is
    // the last finish plus the interval — an estimate, and worded as one. A
    // pass that failed outright backs off to a shorter wait (see
    // `ServeState::sync_retry_delay`), so this has to ask the same question
    // the loop itself just did rather than assume the full interval.
    let next_sync = match (
        sync_every,
        last_run.as_ref().and_then(|run| run.finished_at),
    ) {
        (Some(every), Some(finished)) => {
            let every = state
                .serve
                .sync_retry_delay(Duration::from_secs(every))
                .await
                .as_secs() as i64;
            let due_in = finished + every - now;
            if due_in <= 0 {
                "due now".to_owned()
            } else if due_in < 90 {
                "in under 2 min".to_owned()
            } else if due_in < 3600 {
                format!("in ~{} min", due_in / 60)
            } else {
                format!("in ~{} h", due_in / 3600)
            }
        }
        _ => String::new(),
    };
    let peers = store.list_peers().await.unwrap_or_default();
    let active: Vec<_> = peers.iter().filter(|p| !p.is_revoked()).collect();
    let friends_recent = active
        .iter()
        .filter(|p| p.last_seen_at.is_some_and(|at| now - at < 3600))
        .count();

    let swarm = state.serve.swarms().stats().await;

    // Only worth asking when the swarm is quiet right now — a peer visibly
    // announcing needs no "since when" to tell it apart from a fortnight of
    // silence. `unwrap_or_default()` reads a store error the same way an
    // empty result does: "unknown" rather than a banner of its own for a
    // number nothing else on this tile depends on.
    let swarm_quiet_since = if swarm.peers == 0 {
        store
            .last_active_swarm_sample_at()
            .await
            .unwrap_or_default()
            .map(peers::ago)
    } else {
        None
    };

    let (cpu_percent, memory_usage, disk_usage) = match state.serve.system_status().snapshot().await
    {
        Some(sample) => {
            let (cpu, memory, disk) = crate::system_stats::format(sample);
            (Some(cpu), Some(memory), disk)
        }
        None => (None, None, None),
    };

    Some(crate::web::templates::Glance {
        items_shared,
        shared_size,
        last_sync,
        last_sync_note,
        last_sync_failed,
        friends_recent,
        friends_total: active.len(),
        swarm_peers: swarm.peers,
        swarm_seeders: swarm.seeders,
        swarm_torrents: swarm.swarms,
        swarm_quiet_since,
        next_sync,
        cpu_percent,
        memory_usage,
        disk_usage,
    })
}

/// Serve one embedded asset.
///
/// An explicit match rather than a lookup table: the set is a handful of files,
/// and a match makes it impossible for a path to escape into the filesystem.
/// `pub(crate)` rather than private: `commands::preview` reuses this handler
/// directly, so the mock pages it serves style themselves with the exact same
/// embedded CSS/JS a real instance does, instead of a second copy that could
/// drift out of sync with it.
pub(crate) async fn asset(axum::extract::Path(file): axum::extract::Path<String>) -> Response {
    let (body, mime) = match file.as_str() {
        "style.css" => (include_str!("assets/style.css"), "text/css; charset=utf-8"),
        "htmx.min.js" => (
            include_str!("assets/htmx.min.js"),
            "text/javascript; charset=utf-8",
        ),
        "favicon.svg" => (include_str!("assets/favicon.svg"), "image/svg+xml"),
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
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::result_large_err)]

    use super::*;
    use crate::state::fixtures::unconfigured;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    /// Drive the *assembled* router, middleware and all.
    ///
    /// Testing handlers one at a time leaves two properties unasserted: that the
    /// auth guard is wired to every protected route, and that the cross-origin
    /// layer really does cover the public POSTs. Both are facts about how
    /// `routes()` composes its layers, and neither is observable from a unit
    /// test of the handler.
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

    /// `throttle_unauthenticated_posts` reads the connection's own address,
    /// which `into_make_service_with_connect_info` supplies in the real
    /// server. Driving the router directly with `.oneshot` skips that, and
    /// the extractor then fails with a 500 that looks like a handler bug —
    /// see `tracker.rs`'s `get` test helper for the same trap. Every test
    /// that POSTs to `/login` or `/setup` and expects to reach the handler
    /// needs this.
    fn insert_connect_info(request: &mut Request<Body>) {
        request
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                [203, 0, 113, 50],
                12345,
            ))));
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
            media: None,
            info_hash: None,
            announce_token_fp: None,
            created_by_sharerr: true,
            private: true,
            state: ShareState::Pending,
            last_error: None,
            created_at: None,
            achieved_ratio: None,
            ratio_limit_reported: None,
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

        let state = web_state(serve);
        let glance = glance(&state, None).await.expect("the store is available");

        assert_eq!(glance.items_shared, 1);
        assert_eq!((glance.friends_recent, glance.friends_total), (1, 2));
        assert_eq!(glance.last_sync.as_deref(), Some("just now"));
        assert_eq!(glance.last_sync_note, "1 added");
        assert!(!glance.last_sync_failed);
        assert_eq!(glance.swarm_peers, 0, "nobody has announced");
        assert_eq!(
            glance.swarm_quiet_since, None,
            "no swarm sample has ever been recorded"
        );
    }

    /// The tile's "quiet since" wording depends on the swarm-history sampler
    /// having recorded a past peer, not the live in-memory swarm this test
    /// leaves empty.
    #[tokio::test]
    async fn swarm_quiet_since_reports_the_last_time_anyone_was_seen() {
        let (_dir, serve) = unconfigured();
        let store = serve.store().await.unwrap();
        store
            .record_swarm_sample(sharerr_store::SwarmSample {
                sampled_at: now_epoch() - 3600,
                swarms: 1,
                peers: 2,
                seeders: 1,
            })
            .await
            .unwrap();

        let state = web_state(serve);
        let glance = glance(&state, None).await.expect("the store is available");

        assert_eq!(
            glance.swarm_peers, 0,
            "the live swarm is empty in this test"
        );
        assert!(
            glance.swarm_quiet_since.is_some(),
            "a past active sample exists"
        );
    }

    /// The status page's "next sync" estimate must reflect the backoff a
    /// failed pass earns (see `ServeState::sync_retry_delay`), not blindly
    /// promise the full configured interval while the background loop is
    /// actually about to retry within seconds.
    #[tokio::test]
    async fn next_sync_reflects_a_failed_passs_backoff() {
        let (_dir, serve) = unconfigured();
        let store = serve.store().await.unwrap();

        let run = store.begin_run().await.unwrap();
        store
            .finish_run(
                run,
                &sharerr_store::RunSummary {
                    discovered: 0,
                    added: 0,
                    unshared: 0,
                    failed: 0,
                    error: Some("qBittorrent unreachable".to_owned()),
                },
            )
            .await
            .unwrap();
        serve.note_sync_failure().await;

        let state = web_state(serve);
        // A 15-minute interval: if this ignored the backoff it would say
        // "in ~15 min", not the ~30s a first failure actually backs off to.
        let glance = glance(&state, Some(900))
            .await
            .expect("the store is available");

        assert!(glance.last_sync_failed);
        assert_eq!(glance.next_sync, "in under 2 min", "{glance:?}");
    }

    /// `secret` opens the vault and forwards its `.get`, rather than only
    /// forwarding `open_vault`'s own error — a real vault is required to
    /// exercise the forwarding line itself, so this uses the `figment::Jail`
    /// pattern from `secrets.rs` (scoped, serialized `SHARERR_MASTER_KEY`)
    /// rather than skipping the branch as untestable.
    #[test]
    fn secret_forwards_a_lookup_against_a_real_vault() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("SHARERR_MASTER_KEY", "a-master-key");
            let dir = jail.directory().to_path_buf();
            let config = sharerr_core::Config {
                data_dir: dir.clone(),
                ..sharerr_core::Config::default()
            };
            let path = dir.join("sharerr.toml");
            let serve = Arc::new(ServeState::new(config, path, None));
            let state = web_state(serve);

            let runtime = tokio::runtime::Runtime::new().unwrap();
            let result = runtime.block_on(state.secret("nonexistent-key"));
            assert!(matches!(result, Ok(None)));
            Ok(())
        });
    }

    /// The status page, hit directly (not through the router — no auth cookie
    /// needed, mirroring `web/settings.rs`'s handler-level test style) so its
    /// own assembly of the diagnostics/glance/banner fields is exercised, not
    /// just the anonymous-redirect path above.
    #[tokio::test]
    async fn the_status_page_renders_for_a_fresh_unconfigured_instance() {
        let (_dir, serve) = unconfigured();
        let state = web_state(serve);

        let response = status_page(State(state)).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    /// The polled fragment is the tiles and nothing else: no `<html>` around it
    /// (htmx swaps it into a div), and none of the page's banners or diagnostics
    /// sections, which would otherwise be duplicated into the page every thirty
    /// seconds.
    #[tokio::test]
    async fn the_stat_tiles_fragment_is_the_tiles_alone() {
        let (_dir, serve) = unconfigured();
        let state = web_state(serve);

        let response = stat_tiles(State(state)).await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "htmx does not swap a non-2xx response, so this must not report failure as a status"
        );

        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("stat-grid"), "{html}");
        assert!(!html.contains("<html"), "a fragment, not a page: {html}");
        assert!(
            !html.contains("hx-get=\"/status/tiles\""),
            "the swap target lives in the page, not in what it swaps in — nesting \
             it here would multiply the pollers on every refresh: {html}"
        );
    }

    #[tokio::test]
    async fn the_topology_page_renders_for_a_fresh_unconfigured_instance() {
        let (_dir, serve) = unconfigured();
        let state = web_state(serve);

        let response = topology::page(State(state)).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn htmx_and_the_favicon_are_served_by_name() {
        for file in ["htmx.min.js", "favicon.svg"] {
            let response = asset(axum::extract::Path(file.to_owned())).await;
            assert_eq!(response.status(), StatusCode::OK, "{file}");
        }
    }

    #[tokio::test]
    async fn an_unknown_asset_name_is_not_found() {
        let response = asset(axum::extract::Path("not-a-real-asset".to_owned())).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
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
            "/status/tiles",
            "/settings",
            "/diagnostics",
            "/items",
            "/peers",
            "/peers/export",
            "/peers/1/feed",
            "/wizard",
            "/wizard/services",
            "/wizard/paths",
            "/wizard/tracker",
            "/wizard/done",
        ];
        let protected_posts = [
            "/settings/general",
            "/settings/arr/sonarr",
            "/settings/arr/lidarr",
            "/settings/qbittorrent",
            "/settings/transmission",
            "/settings/torrent-backend",
            "/settings/seeding",
            "/settings/tracker",
            "/settings/lighthouse",
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

    /// Claiming a fresh instance lands on the wizard, not the status page —
    /// nothing is configured yet, so the guided flow is the useful thing to
    /// see first. Signing in again later still goes straight to `/`.
    #[tokio::test]
    async fn claiming_the_instance_redirects_to_the_wizard() {
        let (_dir, app) = router();
        let mut request = post("/setup")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(
                "username=operator&password=hunter22&confirm=hunter22",
            ))
            .unwrap();
        // `throttle_unauthenticated_posts` needs the connection's address,
        // which `into_make_service_with_connect_info` supplies in the real
        // server — see `tracker.rs`'s `get` helper for the same trap.
        insert_connect_info(&mut request);
        let response = send(app, request).await;

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(location(&response), "/wizard");
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

    /// The login throttle, asserted over the real router: the Nth same-origin
    /// POST from one source address to `/login` is refused with `429` and a
    /// `Retry-After` header, and a GET from the very same address is
    /// unaffected — the throttle only ever counts POSTs.
    #[tokio::test]
    async fn too_many_login_posts_from_one_address_are_refused() {
        let (_dir, app) = router();

        let login_attempt = || {
            let mut request = post("/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("username=a&password=wrong"))
                .unwrap();
            insert_connect_info(&mut request);
            request
        };

        for attempt in 0..5 {
            let response = send(app.clone(), login_attempt()).await;
            assert_ne!(
                response.status(),
                StatusCode::TOO_MANY_REQUESTS,
                "attempt {attempt} should still be within budget"
            );
        }

        let refused = send(app.clone(), login_attempt()).await;
        assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(
            refused
                .headers()
                .contains_key(axum::http::header::RETRY_AFTER),
            "a 429 must tell the client when to try again"
        );

        // A page load from the same address is a GET, never counted, and
        // must still work while the address is throttled for POSTs — the
        // fresh, unclaimed instance this test runs against redirects `/login`
        // to `/setup` rather than rendering it, so "not throttled" is
        // "anything but 429", not a bare 200.
        let mut get_request = get("/login");
        insert_connect_info(&mut get_request);
        let page = send(app, get_request).await;
        assert_ne!(page.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    /// A same-origin POST must get past the layer — a CSRF check that rejects
    /// everything would pass the test above while breaking the application.
    #[tokio::test]
    async fn a_same_origin_post_is_not_refused_by_the_csrf_layer() {
        let (_dir, app) = router();
        let mut request = post("/login")
            .header("origin", "http://box.lan:8477")
            .header("host", "box.lan:8477")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from("username=a&password=b"))
            .unwrap();
        insert_connect_info(&mut request);

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
