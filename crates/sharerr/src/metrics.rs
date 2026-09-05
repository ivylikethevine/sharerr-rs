//! `/metrics` (OpenMetrics, for Prometheus) and the dashboard-widget JSON
//! endpoint (for Homepage, Homarr, or Glance).
//!
//! [`MetricsSnapshot`] is the one place that gathers raw numbers from the
//! store, the live swarms, and the gluetun/lighthouse/system pollers; both
//! endpoints render from it rather than each querying independently, so they
//! cannot disagree about what "now" means. This is deliberately a separate
//! aggregator from [`crate::web::mod::glance`], not a shared one: `glance` is
//! polled by an open browser tab every 30 seconds and is documented as
//! costing "two store queries and an in-memory swarm read" — building it from
//! this module's snapshot would add a full item scan and two poller reads to
//! that budget for numbers the status page never shows. A scrape target is
//! pulled on whatever interval the operator's Prometheus is configured with,
//! not this project's, so it pays its own cost independently.
//!
//! Both endpoints are off by default and both require the bearer token in
//! [`sharerr_core::config::secret_keys::METRICS_TOKEN`] — see [`MetricsAuth`]
//! for the fail-closed contract. Disabled, missing, or wrong all answer the
//! same `404`, matching the tracker's and the lighthouse's
//! don't-confirm-existence posture: a bare port scan cannot tell "off" from
//! "wrong token" from "this is not sharerr".
//!
//! No per-item labels, ever — see [`items_by_state`]. A large library would
//! otherwise turn this endpoint into a metric-per-file cardinality liability.

use std::fmt::Write as _;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use sharerr_core::ShareState;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::gluetun::{GluetunSnapshot, GluetunTarget};
use crate::lighthouse_client::LighthouseSnapshot;
use crate::state::ServeState;
use crate::system_stats::SystemSnapshot;

/// The routes this module owns, merged into [`crate::commands::serve::ops_router`]
/// — the same listener `/health` and `/ready` answer on, since both of these
/// carry their own credential rather than needing the web UI's session guard.
pub(crate) fn routes() -> OpenApiRouter<Arc<ServeState>> {
    OpenApiRouter::new()
        .routes(routes!(metrics_endpoint))
        .routes(routes!(dashboard_endpoint))
}

/// Raw numbers, gathered once and shared by both endpoints below — see the
/// module doc for why this is not also what `glance` renders from.
#[derive(Debug, Clone)]
pub(crate) struct MetricsSnapshot {
    /// Every lifecycle state, always present even at zero — see
    /// [`items_by_state`].
    items_by_state: Vec<(ShareState, i64)>,
    seeding_count: i64,
    seeding_bytes: i64,
    /// The most recent *finished* run, same filter [`crate::web::glance`]
    /// applies: an in-flight run has no outcome yet.
    last_run: Option<sharerr_store::RunRecord>,
    swarm: sharerr_torrent::announce::SwarmStats,
    peers_total: usize,
    peers_recent: usize,
    gluetun_tracker: GluetunSnapshot,
    gluetun_client: GluetunSnapshot,
    lighthouse: LighthouseSnapshot,
    system: Option<SystemSnapshot>,
}

/// Every [`ShareState`] paired with its count, in declaration order — a store
/// error or an empty library both read as all-zero rather than an absent
/// series, so a dashboard need not special-case "no data yet".
fn items_by_state(items: &[sharerr_core::SharedItem]) -> Vec<(ShareState, i64)> {
    ShareState::ALL
        .iter()
        .map(|&state| {
            let count = items.iter().filter(|item| item.state == state).count() as i64;
            (state, count)
        })
        .collect()
}

/// Gather everything both endpoints render from. Never fails outright: a
/// store that will not open reads as an all-zero library rather than taking
/// the endpoint down, the same tolerance [`crate::web::glance`] gives a
/// briefly-unavailable database.
pub(crate) async fn gather(state: &ServeState) -> MetricsSnapshot {
    let (items_by_state_vec, seeding, last_run, peers_total, peers_recent) =
        match state.store().await {
            Ok(store) => {
                let (counts, seeding_summary, runs, peers) = tokio::join!(
                    store.counts_by_state(),
                    store.seeding_summary(sharerr_store::PeerScope::All),
                    store.recent_runs(1),
                    store.list_peers(),
                );
                let last_run = runs
                    .unwrap_or_default()
                    .into_iter()
                    .next()
                    .filter(|run| run.finished_at.is_some());
                let peers = peers.unwrap_or_default();
                let active: Vec<_> = peers.iter().filter(|p| !p.is_revoked()).collect();
                let now = sharerr_core::endpoint::now_epoch();
                let recent = active
                    .iter()
                    .filter(|p| p.last_seen_at.is_some_and(|at| now - at < 3600))
                    .count();
                (
                    counts.unwrap_or_else(|_| items_by_state(&[])),
                    seeding_summary.unwrap_or_default(),
                    last_run,
                    active.len(),
                    recent,
                )
            }
            Err(_) => (
                items_by_state(&[]),
                sharerr_store::SeedingSummary::default(),
                None,
                0,
                0,
            ),
        };

    MetricsSnapshot {
        items_by_state: items_by_state_vec,
        seeding_count: seeding.count,
        seeding_bytes: seeding.size,
        last_run,
        swarm: state.swarms().stats().await,
        peers_total,
        peers_recent,
        gluetun_tracker: state
            .gluetun_status(GluetunTarget::Tracker)
            .snapshot()
            .await,
        gluetun_client: state.gluetun_status(GluetunTarget::Client).snapshot().await,
        lighthouse: state.lighthouse_status().snapshot().await,
        system: state.system_status().snapshot().await,
    }
}

/// One OpenMetrics gauge line, with zero or more `label="value"` pairs.
/// `labels` is a fixed small slice — no per-item labels ever reach this, per
/// the module doc — so building the string here rather than via a formatting
/// crate keeps the one hand-rolled shape sharerr already uses for Torznab
/// XML and bencode.
fn sample(out: &mut String, name: &str, labels: &[(&str, &str)], value: impl std::fmt::Display) {
    let _ = write!(out, "{name}");
    if !labels.is_empty() {
        let _ = write!(out, "{{");
        for (index, (key, val)) in labels.iter().enumerate() {
            if index > 0 {
                let _ = write!(out, ",");
            }
            let _ = write!(out, "{key}=\"{val}\"");
        }
        let _ = write!(out, "}}");
    }
    let _ = writeln!(out, " {value}");
}

/// One metric family: `# HELP`, `# TYPE`, then its samples — Prometheus's own
/// exposition order, which OpenMetrics does not mandate but every real
/// scraper expects.
fn family(out: &mut String, name: &str, help: &str, samples: impl FnOnce(&mut String)) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} gauge");
    samples(out);
}

/// Render `snapshot` as OpenMetrics text. Hand-rendered rather than pulling
/// in a metrics crate — this project already hand-writes Torznab XML and
/// bencode, both harder, and a page of gauges is in character.
pub(crate) fn render(snapshot: &MetricsSnapshot) -> String {
    let mut out = String::new();

    family(
        &mut out,
        "sharerr_items",
        "Items by lifecycle state.",
        |out| {
            for (state, count) in &snapshot.items_by_state {
                sample(out, "sharerr_items", &[("state", state.as_str())], count);
            }
        },
    );

    family(
        &mut out,
        "sharerr_seeding_bytes",
        "Total size on disk of items currently seeding.",
        |out| sample(out, "sharerr_seeding_bytes", &[], snapshot.seeding_bytes),
    );

    if let Some(run) = &snapshot.last_run {
        family(
            &mut out,
            "sharerr_sync_last_run_timestamp_seconds",
            "When the last finished sync pass completed.",
            |out| {
                if let Some(finished_at) = run.finished_at {
                    sample(
                        out,
                        "sharerr_sync_last_run_timestamp_seconds",
                        &[],
                        finished_at,
                    );
                }
            },
        );
        if let Some(finished_at) = run.finished_at {
            family(
                &mut out,
                "sharerr_sync_last_run_duration_seconds",
                "How long the last finished sync pass took.",
                |out| {
                    sample(
                        out,
                        "sharerr_sync_last_run_duration_seconds",
                        &[],
                        (finished_at - run.started_at).max(0),
                    );
                },
            );
        }
        family(
            &mut out,
            "sharerr_sync_last_run_ok",
            "1 if the last finished sync pass succeeded outright, 0 if it errored.",
            |out| {
                let ok = i32::from(run.summary.error.is_none());
                sample(out, "sharerr_sync_last_run_ok", &[], ok);
            },
        );
        family(
            &mut out,
            "sharerr_sync_last_run",
            "What the last finished sync pass did, by result.",
            |out| {
                for (result, value) in [
                    ("discovered", run.summary.discovered),
                    ("added", run.summary.added),
                    ("unshared", run.summary.unshared),
                    ("failed", run.summary.failed),
                ] {
                    sample(out, "sharerr_sync_last_run", &[("result", result)], value);
                }
            },
        );
    }

    family(
        &mut out,
        "sharerr_swarm_torrents",
        "Distinct torrents with at least one live peer right now.",
        |out| sample(out, "sharerr_swarm_torrents", &[], snapshot.swarm.swarms),
    );
    family(
        &mut out,
        "sharerr_swarm_peers",
        "Live peers across every swarm right now, seeders included.",
        |out| sample(out, "sharerr_swarm_peers", &[], snapshot.swarm.peers),
    );
    family(
        &mut out,
        "sharerr_swarm_seeders",
        "The subset of sharerr_swarm_peers with the whole thing.",
        |out| sample(out, "sharerr_swarm_seeders", &[], snapshot.swarm.seeders),
    );

    family(
        &mut out,
        "sharerr_peers",
        "Friends by activity, bounded by the friends list.",
        |out| {
            sample(
                out,
                "sharerr_peers",
                &[("state", "total")],
                snapshot.peers_total,
            );
            sample(
                out,
                "sharerr_peers",
                &[("state", "recent")],
                snapshot.peers_recent,
            );
        },
    );

    let gluetun_targets = [
        ("tracker", &snapshot.gluetun_tracker),
        ("client", &snapshot.gluetun_client),
    ];
    if gluetun_targets
        .iter()
        .any(|(_, status)| status.last_success_at.is_some())
    {
        family(
            &mut out,
            "sharerr_gluetun_last_success_timestamp_seconds",
            "When each gluetun poller last successfully resolved an endpoint. \
             Absent for a target that has never succeeded.",
            |out| {
                for (target, status) in gluetun_targets {
                    if let Some(at) = status.last_success_at {
                        sample(
                            out,
                            "sharerr_gluetun_last_success_timestamp_seconds",
                            &[("target", target)],
                            at,
                        );
                    }
                }
            },
        );
    }

    if let Some(last_pass_at) = snapshot.lighthouse.last_pass_at {
        family(
            &mut out,
            "sharerr_lighthouse_last_pass_timestamp_seconds",
            "When the lighthouse report-and-lookup pass last ran to completion.",
            |out| {
                sample(
                    out,
                    "sharerr_lighthouse_last_pass_timestamp_seconds",
                    &[],
                    last_pass_at,
                );
            },
        );
    }
    // One label per *configured* lighthouse — bounded the same way peer
    // labels are, by a list an operator typed in, never by content this
    // instance shares.
    if !snapshot.lighthouse.lighthouses.is_empty() {
        family(
            &mut out,
            "sharerr_lighthouse_last_success_timestamp_seconds",
            "When each configured lighthouse last accepted a report. Absent \
             for one that has never succeeded.",
            |out| {
                for report in &snapshot.lighthouse.lighthouses {
                    if let Some(at) = report.last_success_at {
                        sample(
                            out,
                            "sharerr_lighthouse_last_success_timestamp_seconds",
                            &[("lighthouse", &report.url)],
                            at,
                        );
                    }
                }
            },
        );
    }

    if let Some(system) = &snapshot.system {
        family(
            &mut out,
            "sharerr_system_cpu_percent",
            "CPU utilization across every core, as last sampled.",
            |out| sample(out, "sharerr_system_cpu_percent", &[], system.cpu_percent),
        );
        family(
            &mut out,
            "sharerr_system_memory_bytes",
            "Memory in use versus total, as last sampled.",
            |out| {
                sample(
                    out,
                    "sharerr_system_memory_bytes",
                    &[("kind", "used")],
                    system.memory_used,
                );
                sample(
                    out,
                    "sharerr_system_memory_bytes",
                    &[("kind", "total")],
                    system.memory_total,
                );
            },
        );
        if let (Some(used), Some(total)) = (system.disk_used, system.disk_total) {
            family(
                &mut out,
                "sharerr_system_disk_bytes",
                "Disk usage of the filesystem holding the data directory, as \
                 last sampled.",
                |out| {
                    sample(out, "sharerr_system_disk_bytes", &[("kind", "used")], used);
                    sample(
                        out,
                        "sharerr_system_disk_bytes",
                        &[("kind", "total")],
                        total,
                    );
                },
            );
        }
    }

    out.push_str("# EOF\n");
    out
}

/// The dashboard-widget payload — Homepage, Homarr, and Glance all read a
/// "custom API" JSON endpoint shaped like this. Raw numbers, not the
/// pre-rendered strings the status page's tiles use: a dashboard widget does
/// its own formatting, and the point of building this from [`MetricsSnapshot`]
/// rather than the status page's `Glance` is that neither has to parse the
/// other's prose.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct DashboardWidget {
    items_shared: i64,
    shared_bytes: i64,
    last_sync_at: Option<i64>,
    last_sync_ok: Option<bool>,
    friends_total: usize,
    friends_recent: usize,
    swarm_torrents: usize,
    swarm_peers: usize,
    swarm_seeders: usize,
}

impl From<&MetricsSnapshot> for DashboardWidget {
    fn from(snapshot: &MetricsSnapshot) -> Self {
        Self {
            items_shared: snapshot.seeding_count,
            shared_bytes: snapshot.seeding_bytes,
            last_sync_at: snapshot.last_run.as_ref().and_then(|run| run.finished_at),
            last_sync_ok: snapshot
                .last_run
                .as_ref()
                .map(|run| run.summary.error.is_none()),
            friends_total: snapshot.peers_total,
            friends_recent: snapshot.peers_recent,
            swarm_torrents: snapshot.swarm.swarms,
            swarm_peers: snapshot.swarm.peers,
            swarm_seeders: snapshot.swarm.seeders,
        }
    }
}

/// Authentication as an extractor, same reasoning as `torznab::Caller`'s own
/// doc: a handler that declares `_auth: MetricsAuth` cannot compile without
/// it, where a hand-invoked check is one forgotten call away from an open
/// endpoint.
///
/// Disabled, no token configured, an unopenable vault, or a wrong token all
/// refuse identically with `404` — never `401` — so nothing about the
/// response tells a caller which of those is true, matching the tracker's
/// and the lighthouse's don't-confirm-existence posture. The vault case is
/// the one worth naming: an unopenable vault is *not* treated as "no token
/// configured", the same fail-closed reasoning `tracker::authenticate`
/// documents, because the two are indistinguishable to `try_metrics_token`
/// and admitting on the latter would silently turn enforcement off after a
/// transient error.
pub(crate) struct MetricsAuth;

impl axum::extract::FromRequestParts<Arc<ServeState>> for MetricsAuth {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &Arc<ServeState>,
    ) -> Result<Self, Self::Rejection> {
        let refused = || StatusCode::NOT_FOUND.into_response();

        if !state.config().await.metrics.enabled {
            return Err(refused());
        }

        let required = match state.try_metrics_token().await {
            Ok(token) => token,
            Err(err) => {
                tracing::warn!(error = %err, "refusing /metrics: the vault could not be opened");
                return Err(refused());
            }
        };
        let Some(required) = required.filter(|token| !token.is_empty()) else {
            return Err(refused());
        };

        let Some(supplied) = bearer_token(&parts.headers) else {
            return Err(refused());
        };

        if crate::secrets::constant_time_eq(&required, &supplied) {
            Ok(Self)
        } else {
            Err(refused())
        }
    }
}

/// The `Authorization: Bearer <token>` header, if present and well-formed.
fn bearer_token(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::to_owned)
}

/// `GET /metrics` — OpenMetrics text for Prometheus. Off by default and
/// behind [`MetricsAuth`]; see the module doc for the full contract.
#[utoipa::path(
    get,
    path = "/metrics",
    tag = "ops",
    operation_id = "metrics",
    responses(
        (status = 200, description = "OpenMetrics text.", body = String,
         content_type = "application/openmetrics-text; version=1.0.0; charset=utf-8"),
        (status = 404, description = "Disabled, or the caller's bearer token was \
            missing or wrong — the two are indistinguishable on purpose.",
         body = String),
    ),
    security(("metricsToken" = [])),
)]
async fn metrics_endpoint(State(state): State<Arc<ServeState>>, _auth: MetricsAuth) -> Response {
    let snapshot = gather(&state).await;
    (
        StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "application/openmetrics-text; version=1.0.0; charset=utf-8",
        )],
        render(&snapshot),
    )
        .into_response()
}

/// `GET /dashboard` — the same numbers as JSON, for a Homepage/Homarr/Glance
/// custom-API widget. Off by default and behind [`MetricsAuth`], same as
/// `/metrics`.
#[utoipa::path(
    get,
    path = "/dashboard",
    tag = "ops",
    operation_id = "dashboardWidget",
    responses(
        (status = 200, description = "The dashboard-widget payload.", body = DashboardWidget),
        (status = 404, description = "Disabled, or the caller's bearer token was \
            missing or wrong — the two are indistinguishable on purpose.",
         body = String),
    ),
    security(("metricsToken" = [])),
)]
async fn dashboard_endpoint(State(state): State<Arc<ServeState>>, _auth: MetricsAuth) -> Response {
    let snapshot = gather(&state).await;
    axum::Json(DashboardWidget::from(&snapshot)).into_response()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::result_large_err)]

    use sharerr_store::RunSummary;

    use super::*;

    fn empty_snapshot() -> MetricsSnapshot {
        MetricsSnapshot {
            items_by_state: items_by_state(&[]),
            seeding_count: 0,
            seeding_bytes: 0,
            last_run: None,
            swarm: sharerr_torrent::announce::SwarmStats::default(),
            peers_total: 0,
            peers_recent: 0,
            gluetun_tracker: GluetunSnapshot::default(),
            gluetun_client: GluetunSnapshot::default(),
            lighthouse: LighthouseSnapshot::default(),
            system: None,
        }
    }

    #[test]
    fn a_zero_count_state_still_emits_a_line() {
        let text = render(&empty_snapshot());
        assert!(
            text.contains("sharerr_items{state=\"seeding\"} 0"),
            "{text}"
        );
        assert!(
            text.contains("sharerr_items{state=\"pending\"} 0"),
            "{text}"
        );
        assert!(text.trim_end().ends_with("# EOF"), "{text}");
    }

    #[test]
    fn absent_sections_render_no_lines() {
        // No last run, no gluetun success, no lighthouse, no system sample —
        // none of those metric families should appear at all, rather than as
        // a family header with zero samples under it.
        let text = render(&empty_snapshot());
        for absent in [
            "sharerr_sync_last_run",
            "sharerr_gluetun_last_success",
            "sharerr_lighthouse_last_pass",
            "sharerr_lighthouse_last_success",
            "sharerr_system_",
        ] {
            assert!(!text.contains(absent), "unexpected {absent} in:\n{text}");
        }
    }

    #[test]
    fn a_successful_run_reports_ok_and_its_counts() {
        let mut snapshot = empty_snapshot();
        snapshot.last_run = Some(sharerr_store::RunRecord {
            id: 1,
            started_at: 1_000,
            finished_at: Some(1_004),
            summary: RunSummary {
                discovered: 5,
                added: 2,
                unshared: 1,
                failed: 0,
                error: None,
            },
        });
        let text = render(&snapshot);
        assert!(text.contains("sharerr_sync_last_run_timestamp_seconds 1004"));
        assert!(text.contains("sharerr_sync_last_run_duration_seconds 4"));
        assert!(text.contains("sharerr_sync_last_run_ok 1"));
        assert!(text.contains("sharerr_sync_last_run{result=\"discovered\"} 5"));
        assert!(text.contains("sharerr_sync_last_run{result=\"added\"} 2"));
    }

    #[test]
    fn an_outright_failure_reports_not_ok() {
        let mut snapshot = empty_snapshot();
        snapshot.last_run = Some(sharerr_store::RunRecord {
            id: 2,
            started_at: 1_000,
            finished_at: Some(1_001),
            summary: RunSummary {
                discovered: 0,
                added: 0,
                unshared: 0,
                failed: 0,
                error: Some("qbittorrent unreachable".to_owned()),
            },
        });
        let text = render(&snapshot);
        assert!(text.contains("sharerr_sync_last_run_ok 0"), "{text}");
    }

    #[test]
    fn dashboard_widget_reads_from_the_same_snapshot() {
        let mut snapshot = empty_snapshot();
        snapshot.seeding_count = 12;
        snapshot.seeding_bytes = 4096;
        snapshot.peers_total = 3;
        snapshot.peers_recent = 1;
        let widget = DashboardWidget::from(&snapshot);
        assert_eq!(widget.items_shared, 12);
        assert_eq!(widget.shared_bytes, 4096);
        assert_eq!(widget.friends_total, 3);
        assert_eq!(widget.friends_recent, 1);
        assert_eq!(widget.last_sync_at, None);
    }

    #[test]
    fn bearer_token_strips_the_prefix() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer abc123".parse().unwrap(),
        );
        assert_eq!(bearer_token(&headers).as_deref(), Some("abc123"));
    }

    #[test]
    fn bearer_token_is_absent_without_the_prefix() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(axum::http::header::AUTHORIZATION, "abc123".parse().unwrap());
        assert_eq!(bearer_token(&headers), None);
    }

    #[test]
    fn gluetun_success_is_reported_per_target_that_has_succeeded() {
        // Tracker succeeded, client never has: one family, one sample, and
        // no `target="client"` line pretending a zero is a timestamp.
        let mut snapshot = empty_snapshot();
        snapshot.gluetun_tracker.last_success_at = Some(1_700_000_000);
        let text = render(&snapshot);
        assert!(
            text.contains(
                "sharerr_gluetun_last_success_timestamp_seconds{target=\"tracker\"} 1700000000"
            ),
            "{text}"
        );
        assert!(!text.contains("target=\"client\""), "{text}");
    }

    #[test]
    fn lighthouse_pass_and_per_lighthouse_success_render_only_what_happened() {
        let mut snapshot = empty_snapshot();
        snapshot.lighthouse.last_pass_at = Some(1_700_000_100);
        snapshot.lighthouse.lighthouses = vec![
            crate::lighthouse_client::LighthouseReport {
                url: "https://beacon.example/".to_owned(),
                last_success_at: Some(1_700_000_050),
                last_error: None,
            },
            crate::lighthouse_client::LighthouseReport {
                url: "https://never.example/".to_owned(),
                last_success_at: None,
                last_error: Some("403".to_owned()),
            },
        ];
        let text = render(&snapshot);
        assert!(
            text.contains("sharerr_lighthouse_last_pass_timestamp_seconds 1700000100"),
            "{text}"
        );
        assert!(
            text.contains(
                "sharerr_lighthouse_last_success_timestamp_seconds{lighthouse=\"https://beacon.example/\"} 1700000050"
            ),
            "{text}"
        );
        assert!(
            !text.contains("never.example"),
            "a lighthouse that never succeeded gets no sample: {text}"
        );
    }

    #[test]
    fn system_sections_render_and_disk_is_optional() {
        let mut snapshot = empty_snapshot();
        snapshot.system = Some(SystemSnapshot {
            cpu_percent: 12.5,
            memory_used: 1_024,
            memory_total: 4_096,
            disk_used: Some(10),
            disk_total: Some(100),
        });
        let text = render(&snapshot);
        assert!(text.contains("sharerr_system_cpu_percent 12.5"), "{text}");
        assert!(
            text.contains("sharerr_system_memory_bytes{kind=\"used\"} 1024"),
            "{text}"
        );
        assert!(
            text.contains("sharerr_system_memory_bytes{kind=\"total\"} 4096"),
            "{text}"
        );
        assert!(
            text.contains("sharerr_system_disk_bytes{kind=\"used\"} 10"),
            "{text}"
        );
        assert!(
            text.contains("sharerr_system_disk_bytes{kind=\"total\"} 100"),
            "{text}"
        );

        // A data directory whose filesystem could not be measured drops the
        // disk family entirely rather than reporting zeros.
        snapshot.system = Some(SystemSnapshot {
            cpu_percent: 0.0,
            memory_used: 0,
            memory_total: 0,
            disk_used: None,
            disk_total: None,
        });
        let text = render(&snapshot);
        assert!(text.contains("sharerr_system_cpu_percent"), "{text}");
        assert!(!text.contains("sharerr_system_disk_bytes"), "{text}");
    }

    // ------------------------------------------------- gather

    use crate::state::fixtures::{store_unopenable, unconfigured};
    use secrecy::SecretString;
    use sharerr_core::model::{ExternalIds, MediaSource, MediaSpec, SharedItem};

    fn seeding_item(file_id: i64, size: u64) -> SharedItem {
        SharedItem {
            id: None,
            source: MediaSource::Sonarr,
            source_id: file_id,
            file_id,
            spec: MediaSpec::Episode {
                series_title: "Lanternwick Hollow".to_owned(),
                season: 1,
                episode: file_id as u32,
            },
            release_title: format!("Lanternwick.Hollow.S01E0{file_id}.WEB-DL.x264-SHARERR"),
            arr_path: std::path::PathBuf::from(format!("/tv/s01e0{file_id}.mkv")),
            size,
            ids: ExternalIds::default(),
            media: None,
            info_hash: None,
            announce_token_fp: None,
            created_by_sharerr: true,
            state: ShareState::Pending,
            last_error: None,
            created_at: None,
            achieved_ratio: None,
            ratio_limit_reported: None,
        }
    }

    /// The tolerance the doc promises: a store that will not open is an
    /// all-zero library, not a failed scrape. Every state still has a line.
    #[tokio::test]
    async fn gather_reads_all_zero_when_the_store_will_not_open() {
        let (_dir, state) = store_unopenable();
        let snapshot = gather(&state).await;
        assert_eq!(snapshot.items_by_state.len(), ShareState::ALL.len());
        assert!(snapshot.items_by_state.iter().all(|(_, n)| *n == 0));
        assert_eq!(snapshot.seeding_count, 0);
        assert_eq!(snapshot.seeding_bytes, 0);
        assert!(snapshot.last_run.is_none());
        assert_eq!((snapshot.peers_total, snapshot.peers_recent), (0, 0));
        assert!(snapshot.system.is_none());
    }

    /// Against a real store: items by state, the seeding total, friends
    /// split into total-and-active versus seen-within-the-hour, and the
    /// last *finished* run.
    #[tokio::test]
    async fn gather_counts_items_friends_and_the_last_finished_run() {
        let (_dir, state) = unconfigured();
        let store = state.store().await.unwrap();

        store.upsert(&seeding_item(1, 1_000)).await.unwrap();
        store.upsert(&seeding_item(2, 24)).await.unwrap();
        store
            .set_seeding(MediaSource::Sonarr, 1, &"00".repeat(20), None, true)
            .await
            .unwrap();

        let key = |label: &str| SecretString::from(format!("{label}-key"));
        let seen = store
            .create_peer("alex", &key("alex"), sharerr_store::PeerScope::All)
            .await
            .unwrap();
        assert!(store.touch_peer(seen.id).await.unwrap().updated());
        store
            .create_peer("quiet", &key("quiet"), sharerr_store::PeerScope::All)
            .await
            .unwrap();
        let gone = store
            .create_peer("gone", &key("gone"), sharerr_store::PeerScope::All)
            .await
            .unwrap();
        assert!(store.revoke_peer(gone.id).await.unwrap());

        let run = store.begin_run().await.unwrap();
        store
            .finish_run(
                run,
                &RunSummary {
                    discovered: 2,
                    added: 1,
                    unshared: 0,
                    failed: 0,
                    error: None,
                },
            )
            .await
            .unwrap();

        let snapshot = gather(&state).await;
        let count = |wanted: ShareState| {
            snapshot
                .items_by_state
                .iter()
                .find(|(state, _)| *state == wanted)
                .map(|(_, n)| *n)
                .unwrap()
        };
        assert_eq!(count(ShareState::Seeding), 1);
        assert_eq!(count(ShareState::Pending), 1);
        assert_eq!(snapshot.seeding_count, 1);
        assert_eq!(snapshot.seeding_bytes, 1_000);
        assert_eq!(snapshot.peers_total, 2, "revoked friends do not count");
        assert_eq!(
            snapshot.peers_recent, 1,
            "only the touched friend is recent"
        );
        let last = snapshot
            .last_run
            .as_ref()
            .expect("the finished run is reported");
        assert_eq!(last.id, run);
        assert_eq!(last.summary.added, 1);

        let widget = DashboardWidget::from(&snapshot);
        assert_eq!(widget.items_shared, 1);
        assert_eq!(widget.friends_total, 2);
        assert_eq!(widget.friends_recent, 1);
        assert_eq!(widget.last_sync_ok, Some(true));
        assert!(widget.last_sync_at.is_some());
    }

    /// An in-flight run has no outcome yet, so it is not "the last run" —
    /// the same filter the status page's glance applies.
    #[tokio::test]
    async fn gather_ignores_a_run_that_has_not_finished() {
        let (_dir, state) = unconfigured();
        let store = state.store().await.unwrap();
        store.begin_run().await.unwrap();
        let snapshot = gather(&state).await;
        assert!(snapshot.last_run.is_none());
        assert_eq!(DashboardWidget::from(&snapshot).last_sync_ok, None);
    }

    // ------------------------------------------------- router-level coverage

    use axum::body::Body;
    use axum::http::Request;
    use sharerr_core::config::secret_keys;
    use tower::ServiceExt;

    const TOKEN: &str = "metrics-tests-bearer-token";

    fn metrics_enabled(data_dir: &std::path::Path) -> sharerr_core::Config {
        sharerr_core::Config {
            data_dir: data_dir.to_path_buf(),
            metrics: sharerr_core::config::MetricsConfig { enabled: true },
            ..Default::default()
        }
    }

    async fn get(
        state: &Arc<ServeState>,
        uri: &str,
        bearer: Option<&str>,
    ) -> (StatusCode, axum::http::HeaderMap, String) {
        let (router, _) = routes().with_state(Arc::clone(state)).split_for_parts();
        let mut request = Request::builder().uri(uri);
        if let Some(token) = bearer {
            request = request.header(axum::http::header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let response = router
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        (
            status,
            headers,
            String::from_utf8_lossy(&bytes).into_owned(),
        )
    }

    /// Both endpoints against a state whose vault genuinely opens, with
    /// `metrics.enabled = true` and `store` a hook to seed the vault before
    /// the first request caches what it found. Same `Jail` reasoning as
    /// `tracker.rs`'s `with_open_vault`: `Jail` scopes the master key to this
    /// closure and serialises against every other Jail test in the binary.
    fn with_open_vault<F, Fut>(store: impl FnOnce(&mut sharerr_store::Vault), body: F)
    where
        F: FnOnce(Arc<ServeState>) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        figment::Jail::expect_with(|jail| {
            jail.set_env("SHARERR_MASTER_KEY", "metrics-tests-master-key");
            let config = metrics_enabled(jail.directory());
            let path = jail.directory().join("sharerr.toml");
            let state = Arc::new(ServeState::new(config, path, None));
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                let mut vault = state.open_vault().await.unwrap();
                store(&mut vault);
                drop(vault);
                body(state).await;
            });
            Ok(())
        });
    }

    #[tokio::test]
    async fn both_endpoints_are_a_404_while_metrics_are_disabled() {
        let (_dir, state) = unconfigured();
        for uri in ["/metrics", "/dashboard"] {
            let (status, _, _) = get(&state, uri, Some(TOKEN)).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        }
    }

    /// Enabled, but the vault cannot be opened: refuse, never admit. This is
    /// the fail-closed arm the extractor's doc singles out. Wrapped in `Jail`
    /// with the key cleared, per CLAUDE.md: a bare test asserting the vault
    /// *fails* to open races any Jail test that sets the key on another
    /// thread.
    #[test]
    fn an_unopenable_vault_refuses_rather_than_disabling_enforcement() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            let config = metrics_enabled(jail.directory());
            let path = jail.directory().join("sharerr.toml");
            let state = Arc::new(ServeState::new(config, path, None));
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                let (status, _, _) = get(&state, "/metrics", Some(TOKEN)).await;
                assert_eq!(status, StatusCode::NOT_FOUND);
            });
            Ok(())
        });
    }

    #[test]
    fn no_token_in_the_vault_is_a_404_even_with_a_bearer() {
        with_open_vault(
            |_| {},
            |state| async move {
                let (status, _, _) = get(&state, "/metrics", Some(TOKEN)).await;
                assert_eq!(status, StatusCode::NOT_FOUND);
            },
        );
    }

    #[test]
    fn an_empty_stored_token_counts_as_none() {
        with_open_vault(
            |vault| {
                vault
                    .put(
                        secret_keys::METRICS_TOKEN,
                        &SecretString::from(String::new()),
                    )
                    .unwrap();
            },
            |state| async move {
                let (status, _, _) = get(&state, "/metrics", Some("")).await;
                assert_eq!(status, StatusCode::NOT_FOUND);
            },
        );
    }

    #[test]
    fn a_missing_or_wrong_bearer_is_a_404_and_the_right_one_is_admitted() {
        with_open_vault(
            |vault| {
                vault
                    .put(
                        secret_keys::METRICS_TOKEN,
                        &SecretString::from(TOKEN.to_owned()),
                    )
                    .unwrap();
            },
            |state| async move {
                let (status, _, _) = get(&state, "/metrics", None).await;
                assert_eq!(status, StatusCode::NOT_FOUND, "no header");

                let (status, _, _) = get(&state, "/metrics", Some("not-the-token")).await;
                assert_eq!(status, StatusCode::NOT_FOUND, "wrong token");

                let (status, headers, body) = get(&state, "/metrics", Some(TOKEN)).await;
                assert_eq!(status, StatusCode::OK);
                assert_eq!(
                    headers
                        .get(axum::http::header::CONTENT_TYPE)
                        .and_then(|v| v.to_str().ok()),
                    Some("application/openmetrics-text; version=1.0.0; charset=utf-8")
                );
                assert!(
                    body.contains("sharerr_items{state=\"seeding\"} 0"),
                    "{body}"
                );
                assert!(body.trim_end().ends_with("# EOF"), "{body}");

                let (status, headers, body) = get(&state, "/dashboard", Some(TOKEN)).await;
                assert_eq!(status, StatusCode::OK);
                assert_eq!(
                    headers
                        .get(axum::http::header::CONTENT_TYPE)
                        .and_then(|v| v.to_str().ok()),
                    Some("application/json")
                );
                let widget: serde_json::Value = serde_json::from_str(&body).unwrap();
                assert_eq!(widget["items_shared"], 0);
                assert_eq!(widget["friends_total"], 0);
                assert_eq!(widget["last_sync_ok"], serde_json::Value::Null);
            },
        );
    }
}
