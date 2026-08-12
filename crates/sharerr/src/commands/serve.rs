//! `sharerr serve` — the long-running mode: periodic reconciliation plus HTTP.
//!
//! Milestone 1's HTTP surface is only `/health` and `/ready`, which is what a
//! container orchestrator needs. The tracker, the Torznab endpoint, and the web UI
//! arrive later and mount onto this same router.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use sharerr_core::Config;

use crate::sync::Syncer;

pub async fn run(config: &Config) -> Result<()> {
    let syncer = Arc::new(Syncer::build(config).await?);

    let app = Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .with_state(Arc::clone(&syncer));

    let listener = tokio::net::TcpListener::bind(config.server.bind)
        .await
        .with_context(|| format!("binding {}", config.server.bind))?;
    tracing::info!(bind = %config.server.bind, "http server listening");

    let server = axum::serve(listener, app);

    if !config.sync.enabled {
        tracing::info!("periodic sync is disabled; serving http only");
        return server.await.context("http server failed");
    }

    let interval = Duration::from_secs(config.sync.interval_secs.max(60));
    tracing::info!(interval_secs = interval.as_secs(), "periodic sync enabled");

    // Run both until either stops. A sync failure is logged and retried on the next
    // tick rather than taking the process down — the HTTP endpoint staying up is
    // what lets an operator see that something is wrong.
    tokio::select! {
        result = server => result.context("http server failed"),
        () = sync_loop(syncer, interval) => Ok(()),
    }
}

async fn sync_loop(syncer: Arc<Syncer>, period: Duration) {
    let mut ticker = tokio::time::interval(period);
    // A slow sync must not cause a burst of catch-up runs afterwards.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        ticker.tick().await;
        match syncer.run(false).await {
            Ok(report) => tracing::info!(%report, "sync complete"),
            Err(err) => tracing::error!(error = format!("{err:#}"), "sync failed"),
        }
    }
}

async fn health() -> &'static str {
    "ok"
}

/// Readiness is about the database, which is the one dependency sharerr owns. The
/// *arr apps and qBittorrent being down is a `doctor` question, not a reason to
/// pull this instance out of service.
async fn ready(State(syncer): State<Arc<Syncer>>) -> (StatusCode, &'static str) {
    match syncer.store().recent_runs(1).await {
        Ok(_) => (StatusCode::OK, "ready"),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "database unavailable"),
    }
}
