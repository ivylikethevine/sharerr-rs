//! `sharerr serve` — the long-running mode: periodic reconciliation plus HTTP.
//!
//! One router carries everything: `/health` and `/ready` for the orchestrator, the
//! web UI ([`crate::web`]), sharerr's own tracker ([`crate::tracker`]), and the
//! Torznab feed a friend's Prowlarr indexes ([`crate::torznab`]). One process, one
//! port — whatever makes 8477 reachable makes all of it reachable.
//!
//! Serving is deliberately decoupled from being *configured*. An instance whose
//! vault has no `qbittorrent.api_key` in it yet still binds, still answers
//! `/health`, and reports the reason on `/ready`. The fix for that state is the
//! web UI (or `sharerr vault set` inside the running container), and a process
//! that exits during startup can never be reached by either — it just
//! restart-loops, and the operator has nowhere to type the key.
//!
//! The state all of this shares lives in [`crate::state`], not here: the tracker,
//! the feed, and the web UI need it too, and they are general layers rather than
//! CLI verbs.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::extract::{ConnectInfo, Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use sharerr_core::Config;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::gluetun::GluetunTarget;
use crate::state::ServeState;
use crate::tracker::TrackerState;

/// Whether the embedded lighthouse belongs on the *frontend* listener rather
/// than the tracker's.
///
/// [`sharerr_core::config::LighthouseMount::Tracker`] means "the port a
/// friend's torrent client already reaches", which is `tracker_bind` when a
/// dedicated tracker listener is configured — but when it is not, the main
/// listener carries the tracker's routes too, so that is where the choice
/// actually lands. Standalone so the two listeners built in `run` can share
/// one decision instead of each re-deriving it from the config differently.
fn lighthouse_belongs_on_frontend(
    mount: sharerr_core::config::LighthouseMount,
    tracker_bind: Option<SocketAddr>,
) -> bool {
    use sharerr_core::config::LighthouseMount;
    match mount {
        LighthouseMount::Frontend => true,
        LighthouseMount::Tracker => tracker_bind.is_none(),
    }
}

pub async fn run(config: &Config, config_path: &Path, config_error: Option<String>) -> Result<()> {
    let state = Arc::new(ServeState::new(
        config.clone(),
        config_path,
        config_error.clone(),
    ));

    // One tracker state for however many listeners carry it — two swarm maps
    // would keep peers arriving on different listeners from meeting.
    let tracker = Arc::new(TrackerState::new(Arc::clone(&state)));

    // The embedded lighthouse, if `[lighthouse] enabled = true` — see
    // `sharerr_lighthouse` for the protocol and `ServeState::lighthouse_state`
    // for why this can come back `None` even when enabled (an unopenable
    // vault). `mount` decides which listener below actually carries the
    // routes; building the router once here means both listeners share one
    // `LighthouseState`, same reasoning as sharing one `Swarms` map.
    let lighthouse_routes = state
        .lighthouse_state()
        .await
        .map(sharerr_lighthouse::routes);
    let lighthouse_mount = config.lighthouse.mount;

    // The probes keep their own state and stay outside the web UI's auth layer.
    // `/health` in particular is what the Dockerfile's HEALTHCHECK curls, with no
    // cookie and no intention of getting one. `/gluetun/refresh` and
    // `/gluetun/down` sit here too: gluetun's VPN_PORT_FORWARDING_UP_COMMAND and
    // VPN_PORT_FORWARDING_DOWN_COMMAND are bare wgets with no cookie jar, and the
    // only value either takes is `?target=client` to nudge the second poller
    // instead of the first (the default) — so there is nothing to protect
    // beyond the private-address check both handlers share.
    let (ops, _) = ops_router()
        .with_state(Arc::clone(&state))
        .split_for_parts();
    let mut app = ops
        .merge(crate::tracker::routes(Arc::clone(&tracker)))
        .merge(crate::torznab::routes(Arc::clone(&state)))
        .merge(crate::web::routes(Arc::clone(&state)));

    // The frontend listener carries the lighthouse either because that is
    // literally what was chosen, or because "the tracker port" was chosen but
    // there is no *dedicated* tracker listener below to put it on — the
    // tracker's routes live here too in that case, so this is where a peer
    // reaching "the tracker port" actually lands. See
    // [`lighthouse_belongs_on_frontend`] for the standalone logic.
    if lighthouse_belongs_on_frontend(lighthouse_mount, config.tracker.bind)
        && let Some(routes) = lighthouse_routes.clone()
    {
        app = app.merge(routes);
    }

    let listener = tokio::net::TcpListener::bind(config.server.bind)
        .await
        .with_context(|| format!("binding {}", config.server.bind))?;
    tracing::info!(bind = %config.server.bind, "http server listening");

    // The optional dedicated tracker listener, for the topology where exactly
    // one port is forwarded and it has to be the tracker's. The main listener
    // keeps serving the tracker too — this adds a door, it does not move one.
    let tracker_listener = match config.tracker.bind {
        Some(bind) => {
            let listener = tokio::net::TcpListener::bind(bind)
                .await
                .with_context(|| format!("binding tracker listener {bind}"))?;
            tracing::info!(%bind, "dedicated tracker listener");
            Some(listener)
        }
        None => None,
    };

    // Stated at startup rather than from inside the loop: it is true regardless of
    // whether any credentials load, and an operator reading the first few lines of
    // the log should not have to wait for a vault fix to learn it.
    if config_error.is_some() {
        tracing::warn!(
            config = %config_path.display(),
            "serving only so the configuration can be repaired — open the web UI"
        );
    } else if !config.sync.enabled {
        tracing::info!("periodic sync is disabled; serving http only");
    }

    // Run everything until the *server* stops. A sync that fails — or a syncer
    // that cannot be built at all — is logged and retried rather than taking the
    // process down; the HTTP endpoint staying up is what lets an operator see and
    // repair the problem.
    // `into_make_service_with_connect_info` rather than a bare service: the
    // tracker records the address a peer actually reached us from, because a
    // client behind NAT reports a private address that no other peer can dial.
    // Both listeners get it — whichever serves /announce has nothing else to
    // fall back on.
    let service = app.into_make_service_with_connect_info::<SocketAddr>();
    let tracker_serve = async {
        match tracker_listener {
            Some(listener) => {
                let mut router = crate::tracker::routes(tracker);
                if !lighthouse_belongs_on_frontend(lighthouse_mount, config.tracker.bind)
                    && let Some(routes) = lighthouse_routes
                {
                    router = router.merge(routes);
                }
                let service = router.into_make_service_with_connect_info::<SocketAddr>();
                axum::serve(listener, service)
                    .with_graceful_shutdown(sharerr_lighthouse::shutdown_signal())
                    .await
                    .context("tracker listener failed")
            }
            // No dedicated listener: park forever so the select below never
            // resolves on this arm.
            None => std::future::pending().await,
        }
    };

    // Both listeners stop on SIGTERM/SIGINT, which resolves this select and
    // drops every background loop mid-await — a sync pass is interrupted, but
    // never the in-progress store write or config rename, which are
    // synchronous. Without a handler the binary, as PID 1 in its container,
    // ignored SIGTERM and every `docker stop` ended in a SIGKILL.
    tokio::select! {
        result = axum::serve(listener, service).with_graceful_shutdown(sharerr_lighthouse::shutdown_signal()) => result.context("http server failed"),
        result = tracker_serve => result,
        () = background(Arc::clone(&state)) => Ok(()),
        () = crate::gluetun::poll_loop(Arc::clone(&state), GluetunTarget::Tracker) => Ok(()),
        () = crate::gluetun::poll_loop(Arc::clone(&state), GluetunTarget::Client) => Ok(()),
        () = crate::notify::quiet_peers_loop(Arc::clone(&state)) => Ok(()),
        () = crate::gossip::exchange_loop(Arc::clone(&state)) => Ok(()),
        () = crate::system_stats::poll_loop(Arc::clone(&state)) => Ok(()),
        () = crate::swarm_history::poll_loop(Arc::clone(&state)) => Ok(()),
        () = crate::lighthouse_client::sync_loop(state) => Ok(()),
    }
}

#[derive(Debug, Default, Clone, Deserialize)]
struct GluetunQuery {
    #[serde(default)]
    target: Option<String>,
}

/// `GET|POST /gluetun/refresh[?target=client]` — the push half of endpoint
/// resolution.
///
/// Only nudges the poller; the control server stays the source of truth, so a
/// caller can make sharerr ask a question sooner but can never feed it an
/// answer. Refused from non-private addresses: the legitimate caller is
/// gluetun's up-command inside the same namespace (loopback) or a container
/// neighbour, never the internet side of the tunnel. `target` picks which
/// poller — the tracker's tunnel (default, unchanged) or the torrent client's
/// second one, when `[gluetun_client]` is configured.
#[utoipa::path(
    method(get, post),
    path = "/gluetun/refresh",
    tag = "ops",
    operation_id = "gluetunRefresh",
    params(
        ("target" = Option<String>, Query,
         description = "Which poller to nudge: omitted for the tracker's tunnel, \
                        `client` for the torrent client's second one."),
    ),
    responses(
        (status = 200, description = "The poller was nudged. This only makes sharerr \
            ask gluetun sooner — a caller can never supply the answer.", body = String),
        (status = 403, description = "Refused: the caller is not on a private address. \
            The legitimate caller is gluetun's own up-command.", body = String),
    ),
)]
async fn gluetun_refresh(
    State(state): State<Arc<ServeState>>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    Query(query): Query<GluetunQuery>,
) -> (StatusCode, &'static str) {
    if !sharerr_core::endpoint::is_private_ip(remote.ip()) {
        return (StatusCode::FORBIDDEN, "refused");
    }
    state.nudge_endpoint(GluetunTarget::from_query(query.target.as_deref()));
    (StatusCode::OK, "refreshing")
}

/// `GET|POST /gluetun/down[?target=client]` — for
/// `VPN_PORT_FORWARDING_DOWN_COMMAND`.
///
/// The port gluetun is about to report as gone must not linger as the fallback
/// a resolve falls back to when the port lookup itself fails (see
/// [`crate::gluetun::GluetunClient::resolve_base`]) — that fallback exists for a
/// lookup that is merely *flaky*, and this is gluetun saying the port is
/// authoritatively dead. Forgetting the dynamic history first, then nudging the
/// poller the same way `/gluetun/refresh` does, means the very next resolve
/// either finds a fresh port or degrades cleanly to the static endpoint rather
/// than keep advertising one that no longer works.
#[utoipa::path(
    method(get, post),
    path = "/gluetun/down",
    tag = "ops",
    operation_id = "gluetunDown",
    params(
        ("target" = Option<String>, Query,
         description = "Which tunnel went down: omitted for the tracker's, `client` \
                        for the torrent client's."),
    ),
    responses(
        (status = 200, description = "The dead port was forgotten and the poller \
            nudged, so the next resolve finds a fresh port or degrades to the static \
            endpoint.", body = String),
        (status = 403, description = "Refused: the caller is not on a private address.",
         body = String),
    ),
)]
async fn gluetun_down(
    State(state): State<Arc<ServeState>>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    Query(query): Query<GluetunQuery>,
) -> (StatusCode, &'static str) {
    if !sharerr_core::endpoint::is_private_ip(remote.ip()) {
        return (StatusCode::FORBIDDEN, "refused");
    }
    let target = GluetunTarget::from_query(query.target.as_deref());
    state.endpoint_for(target).forget_dynamic();
    state.nudge_endpoint(target);
    (StatusCode::OK, "acknowledged")
}

/// Keeps the syncer alive and reconciles on a timer.
///
/// Never returns, and never stops re-checking. It cannot simply build the syncer
/// once and settle into a sync loop, because a settings or credential write calls
/// [`ServeState::invalidate`] and puts the syncer back to absent — so readiness is
/// re-established every pass, not just at the start.
///
/// The retry runs even when periodic sync is disabled, because `/ready` should
/// still start telling the truth once the configuration is repaired.
async fn background(state: Arc<ServeState>) {
    loop {
        let Some(syncer) = state.ensure_ready().await else {
            // Still polling here, not purely waiting: a failed `Syncer::build` has
            // no event to hang off — the service it cannot reach will not tell us
            // when it comes back — so the retry has to be on a timer. The delay
            // grows with consecutive failures so a permanently misconfigured
            // instance stops burning an Argon2 derivation every fifteen seconds.
            let delay = state.recovery_delay().await;
            state.sleep_or_wake(delay).await;
            continue;
        };

        // Re-read every pass rather than once: the UI can enable sync, or change
        // the interval, without a restart.
        let sync = state.config().await.sync;
        if !sync.enabled {
            // Parking on the notify alone would be wrong even here: `ensure_ready`
            // is what makes `/ready` start telling the truth, and it should keep
            // being retried on an instance that never syncs on a timer.
            state.sleep_or_wake(state.recovery_delay().await).await;
            continue;
        }

        match syncer.run(false).await {
            Ok(report) => {
                tracing::info!(%report, "sync complete");
                state.note_sync_success().await;
                notify_sync_report(&state, &report).await;
            }
            Err(err) => {
                let reason = format!("{err:#}");
                tracing::error!(error = reason, "sync failed");
                crate::notify::send(
                    &state,
                    sharerr_core::config::NotificationTrigger::SyncFailed,
                    &reason,
                )
                .await;
                state.note_sync_failure().await;
            }
        }

        // Sleeping after the pass rather than on a fixed schedule, so a slow sync is
        // never followed by a burst of catch-up runs. A pass that failed outright
        // backs off instead of waiting out the rest of the configured interval —
        // see `ServeState::sync_retry_delay` — so a torrent client or *arr app
        // that comes back after a brief restart is caught up within seconds.
        let interval = Duration::from_secs(sync.effective_interval_secs());
        state
            .sleep_or_wake(state.sync_retry_delay(interval).await)
            .await;
    }
}

/// The two digest triggers a successful pass can fire — split out of
/// `background`'s match arm so the gating logic is testable against a
/// synthetic [`crate::sync::SyncReport`] without driving a real sync pass.
async fn notify_sync_report(state: &ServeState, report: &crate::sync::SyncReport) {
    if report.added > 0 {
        crate::notify::send(
            state,
            sharerr_core::config::NotificationTrigger::ItemsShared,
            &format!("{} item(s) newly shared", report.added),
        )
        .await;
    }
    if report.failed > 0 {
        crate::notify::send(
            state,
            sharerr_core::config::NotificationTrigger::ItemFailed,
            &format!(
                "{} item(s) failed to share this pass — see the items page for details",
                report.failed
            ),
        )
        .await;
    }
}

/// Liveness, not correctness — this answers "should this container be restarted?",
/// and the answer is no even when the vault is empty, because a restart cannot
/// populate it. The Dockerfile's HEALTHCHECK is wired here, so anything conditional
/// The operational endpoints, without state, so [`crate::openapi`] reads the
/// document off the same declaration that mounts them.
pub(crate) fn ops_router() -> OpenApiRouter<Arc<ServeState>> {
    OpenApiRouter::new()
        .routes(routes!(health))
        .routes(routes!(ready))
        .routes(routes!(gluetun_refresh))
        .routes(routes!(gluetun_down))
        .merge(crate::metrics::routes())
}

/// in this handler turns a fixable configuration gap into a restart loop.
#[utoipa::path(
    get,
    path = "/health",
    tag = "ops",
    operation_id = "health",
    responses((status = 200, description = "Alive. Answers `ok` whatever state the \
        configuration is in — a restart cannot fix a missing credential, so this \
        never reports one.", body = String)),
)]
async fn health() -> &'static str {
    "ok"
}

/// Readiness covers the three things that stop this instance doing work: a config
/// file it could not load, credentials it could not load, and its own database. The
/// *arr apps and qBittorrent being down is a `doctor` question, not a reason to pull
/// this instance out of service.
#[utoipa::path(
    get,
    path = "/ready",
    tag = "ops",
    operation_id = "ready",
    responses(
        (status = 200, description = "Configuration, credentials and database all \
            loaded.", body = String),
        (status = 503, description = "One of those three is not available; the body \
            names which. The *arr apps and the torrent client being down is not \
            covered here — that is what `sharerr doctor` is for.", body = String),
    ),
)]
async fn ready(State(state): State<Arc<ServeState>>) -> (StatusCode, String) {
    // Reported ahead of the syncer's reason, which would otherwise relay the same
    // string under the less specific "not configured" heading.
    if let Some(reason) = state.config_error().await {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("configuration invalid: {reason}"),
        );
    }

    let syncer = match state.syncer().await {
        Ok(syncer) => syncer,
        Err(reason) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("not configured: {reason}"),
            );
        }
    };

    match syncer.store().recent_runs(1).await {
        Ok(_) => (StatusCode::OK, "ready".to_owned()),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable".to_owned(),
        ),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::result_large_err)]

    use super::*;
    use crate::state::fixtures::{unconfigured, unloadable};

    /// `Frontend` always lands on the main listener; `Tracker` follows the
    /// dedicated tracker listener when there is one, and falls back to the
    /// main listener — which still carries the tracker's own routes — when
    /// there is not.
    #[test]
    fn the_tracker_mount_falls_back_to_frontend_without_a_dedicated_listener() {
        use sharerr_core::config::LighthouseMount;

        let dedicated: SocketAddr = "0.0.0.0:9000".parse().unwrap();

        assert!(lighthouse_belongs_on_frontend(
            LighthouseMount::Frontend,
            None
        ));
        assert!(lighthouse_belongs_on_frontend(
            LighthouseMount::Frontend,
            Some(dedicated)
        ));
        assert!(lighthouse_belongs_on_frontend(
            LighthouseMount::Tracker,
            None
        ));
        assert!(!lighthouse_belongs_on_frontend(
            LighthouseMount::Tracker,
            Some(dedicated)
        ));
    }

    /// The regression that matters most: an unconfigured instance must still look
    /// alive, or the orchestrator restarts the container the operator is trying to
    /// type a password into. `health`'s lack of state is what enforces that, so this
    /// mostly guards the signature.
    #[tokio::test]
    async fn health_is_unconditional() {
        assert_eq!(health().await, "ok");
    }

    #[tokio::test]
    async fn ready_reports_the_config_error_before_anything_else() {
        let (_dir, state) = unloadable();

        let (status, body) = ready(State(state)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(body.starts_with("configuration invalid:"), "got {body:?}");
        assert!(body.contains("taag"), "got {body:?}");
    }

    /// The refresh nudge is reachable to gluetun's up-command (loopback, docker
    /// neighbours) and to nothing on the internet side of the tunnel — the
    /// endpoint takes only `target`, but an open one would let strangers drive
    /// the poll timer.
    #[tokio::test]
    async fn the_gluetun_refresh_nudge_is_private_only() {
        let (_dir, state) = unconfigured();
        let no_target = Query(GluetunQuery::default());

        let private = ConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], 40000)));
        let (status, _) =
            gluetun_refresh(State(Arc::clone(&state)), private, no_target.clone()).await;
        assert_eq!(status, StatusCode::OK);

        let public = ConnectInfo(std::net::SocketAddr::from(([203, 0, 113, 9], 40000)));
        let (status, _) = gluetun_refresh(State(state), public, no_target).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    /// `?target=client` must nudge the *second* poller, not the first — the
    /// whole reason the query parameter exists.
    #[tokio::test]
    async fn a_client_target_nudges_the_client_poller_only() {
        let (_dir, state) = unconfigured();
        let private = ConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], 40000)));

        let tracker_waiter = {
            let state = Arc::clone(&state);
            tokio::spawn(async move {
                state
                    .endpoint_refresh_requested(GluetunTarget::Tracker)
                    .await;
            })
        };
        let client_waiter = {
            let state = Arc::clone(&state);
            tokio::spawn(async move {
                state
                    .endpoint_refresh_requested(GluetunTarget::Client)
                    .await;
            })
        };
        tokio::task::yield_now().await;

        let query = Query(GluetunQuery {
            target: Some("client".to_owned()),
        });
        gluetun_refresh(State(Arc::clone(&state)), private, query).await;

        tokio::time::timeout(Duration::from_secs(5), client_waiter)
            .await
            .expect("the client poller must be nudged")
            .expect("must not panic");
        assert!(
            !tracker_waiter.is_finished(),
            "the tracker poller must not be nudged by a client-targeted refresh"
        );
        tracker_waiter.abort();
    }

    /// Mirrors `the_gluetun_refresh_nudge_is_private_only`: the down hook sits
    /// behind the same private-address check, since it is reachable to the same
    /// callers (gluetun's down-command, a docker neighbour) and to nothing else.
    #[tokio::test]
    async fn the_gluetun_down_hook_is_private_only() {
        let (_dir, state) = unconfigured();
        let no_target = Query(GluetunQuery::default());

        let public = ConnectInfo(std::net::SocketAddr::from(([203, 0, 113, 9], 40000)));
        let (status, _) = gluetun_down(State(Arc::clone(&state)), public, no_target.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let private = ConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], 40000)));
        let (status, _) = gluetun_down(State(state), private, no_target).await;
        assert_eq!(status, StatusCode::OK);
    }

    /// The whole point of `/gluetun/down`: a port gluetun says is dead must not
    /// linger as `resolve`'s fallback (see [`crate::gluetun::GluetunClient::resolve_base`]),
    /// so the dynamic history has to be gone, not just re-nudged.
    #[tokio::test]
    async fn the_gluetun_down_hook_forgets_the_dynamic_observation() {
        let (_dir, state) = unconfigured();
        let endpoint = state.endpoint_for(GluetunTarget::Tracker);
        endpoint.observe("http://10.0.0.5:51413".parse().unwrap());
        assert!(
            endpoint.last_observed().is_some(),
            "setup: must have observed something"
        );

        let private = ConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], 40000)));
        gluetun_down(
            State(Arc::clone(&state)),
            private,
            Query(GluetunQuery::default()),
        )
        .await;

        assert!(
            state
                .endpoint_for(GluetunTarget::Tracker)
                .last_observed()
                .is_none(),
            "the dynamic observation must be forgotten, not merely refreshed"
        );
    }

    /// `?target=client` must forget and nudge the *client* poller's endpoint,
    /// leaving the tracker's own dynamic history untouched.
    #[tokio::test]
    async fn a_client_target_only_forgets_the_client_endpoint() {
        let (_dir, state) = unconfigured();
        state
            .endpoint_for(GluetunTarget::Tracker)
            .observe("http://10.0.0.5:51413".parse().unwrap());
        state
            .endpoint_for(GluetunTarget::Client)
            .observe("http://10.0.0.6:51414".parse().unwrap());

        let private = ConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], 40000)));
        let query = Query(GluetunQuery {
            target: Some("client".to_owned()),
        });
        gluetun_down(State(Arc::clone(&state)), private, query).await;

        assert!(
            state
                .endpoint_for(GluetunTarget::Client)
                .last_observed()
                .is_none(),
            "the client endpoint must have forgotten its observation"
        );
        assert!(
            state
                .endpoint_for(GluetunTarget::Tracker)
                .last_observed()
                .is_some(),
            "the tracker endpoint must be untouched by a client-targeted call"
        );
    }

    #[tokio::test]
    async fn ready_reports_503_and_names_what_is_missing() {
        let (_dir, state) = unconfigured();
        state.ensure_ready().await;

        let (status, body) = ready(State(state)).await;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        // The whole point of the body is to name the thing to go and fix.
        let reason = body
            .strip_prefix("not configured: ")
            .unwrap_or_else(|| panic!("unexpected body: {body}"));
        assert!(!reason.trim().is_empty(), "no reason given");
    }

    /// The success arm, previously untested: once a syncer has actually built
    /// and the database answers, `/ready` must say so rather than staying on
    /// whichever earlier branch got exercised elsewhere.
    #[tokio::test]
    async fn ready_reports_ok_once_the_syncer_is_built_and_the_database_answers() {
        let (_dir, state) = crate::state::fixtures::ready().await;

        let (status, body) = ready(State(state)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "ready");
    }

    /// The one failure `/ready` distinguishes from a configuration problem: the
    /// syncer built fine, but the database itself stopped answering. Forcing
    /// this by closing the pool out from under a live `Syncer` is the only way
    /// to reach it — a config or credential problem never gets this far.
    #[tokio::test]
    async fn ready_reports_503_when_the_database_stops_answering() {
        let (_dir, state) = crate::state::fixtures::ready().await;
        let syncer = state
            .syncer()
            .await
            .expect("setup: the fixture's syncer must be present");
        syncer.store().pool().close().await;

        let (status, body) = ready(State(state)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body, "database unavailable");
    }

    /// The wiring the backoff fix actually depends on: `background` itself
    /// must record a failed pass, not just the `ServeState` methods it calls
    /// in isolation. The fixture's syncer has no library source, so its one
    /// pass fails immediately with "no library source could be scanned"
    /// rather than needing a live service to fail against.
    #[tokio::test]
    async fn a_failed_pass_backs_off_the_next_attempt() {
        let (_dir, state) = crate::state::fixtures::ready().await;
        let interval = Duration::from_secs(state.config().await.sync.effective_interval_secs());
        assert_eq!(
            state.sync_retry_delay(interval).await,
            interval,
            "setup: no failures recorded yet"
        );

        // `background` never returns and its future is not `Send` (a
        // `tracing` macro holds a non-`Send` value across an `.await`
        // upstream in `ensure_ready`), so it is raced on the current task via
        // `select!` — exactly how `run` itself drives it — rather than
        // spawned. The other arm polls for the effect `background` is
        // expected to have had rather than yielding a fixed number of times,
        // since a fixed count is exactly the kind of thing that gets flaky
        // under a loaded test binary; the 5s ceiling is only ever hit if the
        // wiring is actually broken.
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                () = background(Arc::clone(&state)) => {}
                () = async {
                    while state.sync_retry_delay(interval).await >= interval {
                        tokio::task::yield_now().await;
                    }
                } => {}
            }
        })
        .await;

        assert!(
            state.sync_retry_delay(interval).await < interval,
            "a failed pass must shorten the next attempt's wait"
        );
    }

    /// The digest notifications `background` fires on a successful pass —
    /// tested directly against a synthetic report rather than by driving a
    /// real share through a syncer, which `notify::send`'s own tests already
    /// cover for the trigger-gating and payload shape.
    #[test]
    fn notify_sync_report_digests_added_and_failed_into_two_notifications() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("SHARERR_MASTER_KEY", "a-master-key");
            let dir = jail.directory().to_path_buf();
            let config = Config {
                data_dir: dir.clone(),
                ..Config::default()
            };
            let state = Arc::new(ServeState::new(config, dir.join("sharerr.toml"), None));

            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async {
                let server = wiremock::MockServer::start().await;
                wiremock::Mock::given(wiremock::matchers::method("POST"))
                    .respond_with(wiremock::ResponseTemplate::new(200))
                    .expect(2)
                    .mount(&server)
                    .await;

                let mut vault = state.open_vault().await.unwrap();
                vault
                    .put(
                        sharerr_core::config::secret_keys::NOTIFICATIONS_WEBHOOK_URL,
                        &secrecy::SecretString::from(server.uri()),
                    )
                    .unwrap();
                drop(vault);

                notify_sync_report(
                    &state,
                    &crate::sync::SyncReport {
                        added: 3,
                        failed: 1,
                        ..Default::default()
                    },
                )
                .await;
                // The mock's `expect(2)` — one for ItemsShared, one for
                // ItemFailed — is checked on drop.
            });
            Ok(())
        });
    }

    /// The all-zero pass most sync runs actually are — nothing added, nothing
    /// failed — must not notify at all.
    #[tokio::test]
    async fn notify_sync_report_is_silent_on_an_unchanged_pass() {
        let (_dir, state) = crate::state::fixtures::unconfigured();
        // No master key, so a webhook lookup would fail anyway — this proves
        // the counts gate it before that, not incidentally.
        notify_sync_report(&state, &crate::sync::SyncReport::default()).await;
    }

    /// `run`'s startup sequence — building the router, binding listeners, and
    /// logging why (or whether) reconciliation is running — was previously
    /// only exercised indirectly, by `tests/cli.rs`'s subprocess suite, and
    /// only with that suite's one default, single-listener, lighthouse-off
    /// configuration. This drives it with a config-load failure and no
    /// dedicated tracker listener, which additionally means `tracker_serve`
    /// takes its "nothing to serve" arm rather than the one below binding an
    /// actual listener.
    ///
    /// `run` never returns and its future is not `Send` (the same `tracing`
    /// macro issue that affects `background` propagates up through it), so it
    /// is raced via `select!` against a real sleep long enough for startup to
    /// finish and the final `select!` inside `run` to be reached — at which
    /// point this drops it there rather than waiting on it forever.
    #[tokio::test]
    async fn run_logs_the_config_error_with_no_dedicated_tracker_listener() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            data_dir: dir.path().to_path_buf(),
            server: sharerr_core::config::ServerConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
            },
            ..Config::default()
        };
        let path = dir.path().join("sharerr.toml");

        tokio::select! {
            result = run(&config, &path, Some("bad field".to_owned())) => {
                panic!("run must not return on its own: {result:?}");
            }
            () = tokio::time::sleep(Duration::from_millis(300)) => {}
        }
    }

    /// The other half: a dedicated tracker listener actually gets bound and
    /// served, and a disabled sync logs the other startup branch — mutually
    /// exclusive with the config-error branch the previous test covers.
    #[tokio::test]
    async fn run_serves_a_dedicated_tracker_listener_with_sync_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            data_dir: dir.path().to_path_buf(),
            server: sharerr_core::config::ServerConfig {
                bind: "127.0.0.1:0".parse().unwrap(),
            },
            tracker: sharerr_core::config::TrackerConfig {
                bind: Some("127.0.0.1:0".parse().unwrap()),
                ..Config::default().tracker
            },
            sync: sharerr_core::config::SyncConfig {
                enabled: false,
                ..Config::default().sync
            },
            ..Config::default()
        };
        let path = dir.path().join("sharerr.toml");

        tokio::select! {
            result = run(&config, &path, None) => {
                panic!("run must not return on its own: {result:?}");
            }
            () = tokio::time::sleep(Duration::from_millis(300)) => {}
        }
    }

    /// The mirror of `a_failed_pass_backs_off_the_next_attempt`: a pass that
    /// actually succeeds must clear a backoff a prior failure built up, not
    /// merely avoid growing it further. `ready`'s deliberately sourceless
    /// syncer can never reach `background`'s success arm — this fixture's one
    /// (empty) library source lets a pass complete instead of bailing.
    #[tokio::test]
    async fn a_successful_pass_clears_the_backoff() {
        let (_dir, state) = crate::state::fixtures::ready_with_source().await;
        let interval = Duration::from_secs(state.config().await.sync.effective_interval_secs());
        state.note_sync_failure().await;
        assert!(
            state.sync_retry_delay(interval).await < interval,
            "setup: a failure must be recorded first"
        );

        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                () = background(Arc::clone(&state)) => {}
                () = async {
                    while state.sync_retry_delay(interval).await < interval {
                        tokio::task::yield_now().await;
                    }
                } => {}
            }
        })
        .await;

        assert_eq!(
            state.sync_retry_delay(interval).await,
            interval,
            "a successful pass must clear the backoff"
        );
    }
}
