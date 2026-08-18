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
use axum::Router;
use axum::extract::{ConnectInfo, Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use serde::Deserialize;
use sharerr_core::Config;

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
    let lighthouse_routes = state.lighthouse_state().await.map(sharerr_lighthouse::routes);
    let lighthouse_mount = config.lighthouse.mount;

    // The probes keep their own state and stay outside the web UI's auth layer.
    // `/health` in particular is what the Dockerfile's HEALTHCHECK curls, with no
    // cookie and no intention of getting one. `/gluetun/refresh` and
    // `/gluetun/down` sit here too: gluetun's VPN_PORT_FORWARDING_UP_COMMAND and
    // VPN_PORT_FORWARDING_DOWN_COMMAND are bare wgets with no cookie jar, and the
    // only value either takes is `?target=client` to nudge the second poller
    // instead of the first (default, and the only choice before it existed) —
    // so there is nothing to protect beyond the private-address check both
    // handlers share.
    let mut app = Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route(
            "/gluetun/refresh",
            get(gluetun_refresh).post(gluetun_refresh),
        )
        .route("/gluetun/down", get(gluetun_down).post(gluetun_down))
        .with_state(Arc::clone(&state))
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
                    .await
                    .context("tracker listener failed")
            }
            // No dedicated listener: park forever so the select below never
            // resolves on this arm.
            None => std::future::pending().await,
        }
    };

    tokio::select! {
        result = axum::serve(listener, service) => result.context("http server failed"),
        result = tracker_serve => result,
        () = background(Arc::clone(&state)) => Ok(()),
        () = crate::gluetun::poll_loop(Arc::clone(&state), GluetunTarget::Tracker) => Ok(()),
        () = crate::gluetun::poll_loop(Arc::clone(&state), GluetunTarget::Client) => Ok(()),
        () = crate::notify::quiet_peers_loop(Arc::clone(&state)) => Ok(()),
        () = crate::gossip::exchange_loop(state) => Ok(()),
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
            Ok(report) => tracing::info!(%report, "sync complete"),
            Err(err) => {
                let reason = format!("{err:#}");
                tracing::error!(error = reason, "sync failed");
                crate::notify::send(&state, "sync failed", &reason).await;
            }
        }

        // Sleeping after the pass rather than on a fixed schedule, so a slow sync is
        // never followed by a burst of catch-up runs.
        state
            .sleep_or_wake(Duration::from_secs(sync.effective_interval_secs()))
            .await;
    }
}

/// Liveness, not correctness — this answers "should this container be restarted?",
/// and the answer is no even when the vault is empty, because a restart cannot
/// populate it. The Dockerfile's HEALTHCHECK is wired here, so anything conditional
/// in this handler turns a fixable configuration gap into a restart loop.
async fn health() -> &'static str {
    "ok"
}

/// Readiness covers the three things that stop this instance doing work: a config
/// file it could not load, credentials it could not load, and its own database. The
/// *arr apps and qBittorrent being down is a `doctor` question, not a reason to pull
/// this instance out of service.
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
    #![allow(clippy::unwrap_used, clippy::expect_used)]

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

        assert!(lighthouse_belongs_on_frontend(LighthouseMount::Frontend, None));
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
                    .await
            })
        };
        let client_waiter = {
            let state = Arc::clone(&state);
            tokio::spawn(async move {
                state
                    .endpoint_refresh_requested(GluetunTarget::Client)
                    .await
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
}
