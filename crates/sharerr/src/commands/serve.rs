//! `sharerr serve` — the long-running mode: periodic reconciliation plus HTTP.
//!
//! One router carries everything: `/health` and `/ready` for the orchestrator, the
//! web UI ([`crate::web`]), sharerr's own tracker ([`crate::tracker`]), and the
//! Torznab feed a friend's Prowlarr indexes ([`crate::torznab`]). One process, one
//! port — whatever makes 8477 reachable makes all of it reachable.
//!
//! Serving is deliberately decoupled from being *configured*. An instance whose
//! vault has no `qbittorrent.password` in it yet still binds, still answers
//! `/health`, and reports the reason on `/ready`. The fix for that state is the
//! web UI (or `sharerr vault set` inside the running container), and a process
//! that exits during startup can never be reached by either — it just
//! restart-loops, and the operator has nowhere to type the password.
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
use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;
use axum::routing::get;
use sharerr_core::Config;

use crate::state::ServeState;
use crate::tracker::TrackerState;

pub async fn run(config: &Config, config_path: &Path, config_error: Option<String>) -> Result<()> {
    let state = Arc::new(ServeState::new(
        config.clone(),
        config_path,
        config_error.clone(),
    ));

    // One tracker state for however many listeners carry it — two swarm maps
    // would keep peers arriving on different listeners from meeting.
    let tracker = Arc::new(TrackerState::new(Arc::clone(&state)));

    // The probes keep their own state and stay outside the web UI's auth layer.
    // `/health` in particular is what the Dockerfile's HEALTHCHECK curls, with no
    // cookie and no intention of getting one. `/gluetun/refresh` sits here too:
    // gluetun's VPN_PORT_FORWARDING_UP_COMMAND is a bare wget with no cookie jar,
    // and the endpoint only nudges the poller to re-ask the control server — it
    // takes no value from the caller, so there is nothing to protect beyond the
    // private-address check in the handler.
    let app = Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route(
            "/gluetun/refresh",
            get(gluetun_refresh).post(gluetun_refresh),
        )
        .with_state(Arc::clone(&state))
        .merge(crate::tracker::routes(Arc::clone(&tracker)))
        .merge(crate::torznab::routes(Arc::clone(&state)))
        .merge(crate::web::routes(Arc::clone(&state)));

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
                let service = crate::tracker::routes(tracker)
                    .into_make_service_with_connect_info::<SocketAddr>();
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
        () = crate::gluetun::poll_loop(Arc::clone(&state)) => Ok(()),
        () = crate::gossip::exchange_loop(state) => Ok(()),
    }
}

/// `GET|POST /gluetun/refresh` — the push half of endpoint resolution.
///
/// Only nudges the poller; the control server stays the source of truth, so a
/// caller can make sharerr ask a question sooner but can never feed it an
/// answer. Refused from non-private addresses: the legitimate caller is
/// gluetun's up-command inside the same namespace (loopback) or a container
/// neighbour, never the internet side of the tunnel.
async fn gluetun_refresh(
    State(state): State<Arc<ServeState>>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
) -> (StatusCode, &'static str) {
    if !is_private(remote.ip()) {
        return (StatusCode::FORBIDDEN, "refused");
    }
    state.nudge_endpoint();
    (StatusCode::OK, "refreshing")
}

fn is_private(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        std::net::IpAddr::V6(v6) => v6.is_loopback() || (v6.segments()[0] & 0xfe00) == 0xfc00,
    }
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
            Err(err) => tracing::error!(error = format!("{err:#}"), "sync failed"),
        }

        // Sleeping after the pass rather than on a fixed schedule, so a slow sync is
        // never followed by a burst of catch-up runs.
        state
            .sleep_or_wake(Duration::from_secs(sync.interval_secs.max(60)))
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
    /// endpoint takes no input, but an open one would let strangers drive the
    /// poll timer.
    #[tokio::test]
    async fn the_gluetun_refresh_nudge_is_private_only() {
        let (_dir, state) = unconfigured();

        let private = ConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], 40000)));
        let (status, _) = gluetun_refresh(State(Arc::clone(&state)), private).await;
        assert_eq!(status, StatusCode::OK);

        let public = ConnectInfo(std::net::SocketAddr::from(([203, 0, 113, 9], 40000)));
        let (status, _) = gluetun_refresh(State(state), public).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
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
