//! The health checks folded into the combined status/diagnostics page: does
//! the library actually resolve?
//!
//! `doctor` has always answered this from a shell. The settings page's "Test
//! connection" buttons deliberately do not — they answer a one-line question about
//! one service, and path resolution needs a full discovery walk plus a look at the
//! filesystem. The result was that the check most likely to explain "sharerr does
//! nothing" was the one an operator who never opens a terminal could not run.
//!
//! The checking is shared with `doctor` via [`crate::checks`]; this module only
//! gathers. [`gather`] used to render its own page — until that page and Status
//! merged, on the grounds that both answered "is this instance healthy" at two
//! levels of detail a person had to know to click through between.

use secrecy::SecretString;
use sharerr_arr::Discovered;
use sharerr_core::config::secret_keys;
use sharerr_core::endpoint::AdvertisedEndpoint;
use sharerr_core::{Config, MediaSource};

use super::WebState;
use super::peers::ago;
use super::templates::{DiagnosticsData, EndpointStatus, RunRow, SampleRow, ServiceLine};
use crate::checks::{self, ArrOutcome, DirOutcome};
use crate::gluetun::GluetunTarget;

/// How many problem paths to name before summarising the rest.
///
/// A library with a broken mapping has *every* file broken, and a page listing ten
/// thousand of them buries the advice that would fix it.
const MAX_LISTED: usize = 20;

/// How many past runs to show. The status page's glance already answers "is
/// the *last* sync healthy"; this answers "is that typical", which needs a
/// few rather than one — enough to see a pattern, not so many the page reads
/// like a log dump.
const RECENT_RUNS: i64 = 10;

/// Run every health check and return the results, unrendered — [`super::status_page`]
/// folds this into the combined page alongside the glance and the banners.
pub(super) async fn gather(state: &WebState) -> DiagnosticsData {
    let config = state.serve.config().await;

    // One vault open for the whole page. Opening it derives the key with Argon2 —
    // ~16ms of solid CPU, more on ARM — so paying that once per configured
    // service turned this into the most expensive page in the UI.
    let vault = state.serve.open_vault().await;
    let api_key = |key: &'static str| -> Result<Option<SecretString>, String> {
        match &vault {
            Ok(vault) => vault.get(key).map_err(|err| err.to_string()),
            Err(reason) => Err(reason.clone()),
        }
    };

    // The services are independent, so the page waits for the slowest of them
    // rather than the sum of all five.
    let outcomes = futures::future::join_all(
        config
            .configured_sources()
            .into_iter()
            // `configured_sources` yields only *arr apps, each of which has a key.
            .filter_map(|kind| secret_keys::api_key_for(kind).map(|key| (kind, key)))
            .map(|(kind, key)| {
                let api_key = api_key(key);
                let config = &config;
                async move {
                    let url = config.service(kind).map(|s| &s.url);
                    (
                        kind,
                        checks::check_arr(kind, url, api_key, &config.tag).await,
                    )
                }
            }),
    )
    .await;

    let mut services = Vec::new();
    let mut discovered: Vec<Discovered> = Vec::new();
    for (kind, outcome) in outcomes {
        services.push(describe(kind, &config, &outcome));
        discovered.extend(outcome.into_items());
    }

    // The directory scans are filesystem-bound; off the async loop for the same
    // reason as `check_paths` below.
    let libraries = config.library.clone();
    let library_lines = match tokio::task::spawn_blocking(move || {
        libraries
            .iter()
            .map(|library| {
                let outcome = checks::check_library(library);
                (describe_library(library, &outcome), outcome.into_items())
            })
            .collect::<Vec<_>>()
    })
    .await
    {
        Ok(lines) => lines,
        // A panicked scan must not make a configured [[library]] install look
        // identical to one with no libraries at all.
        Err(err) => vec![(
            ServiceLine {
                name: "library".to_owned(),
                message: format!("the scan did not complete: {err}"),
                ok: false,
            },
            Vec::new(),
        )],
    };
    for (line, items) in library_lines {
        services.push(line);
        discovered.extend(items);
    }

    // `check_paths` stats every discovered file. On a container pinned to one
    // CPU there is exactly one worker thread, so running it inline would stall
    // /health and every other request for the duration of the walk.
    let scanned = !discovered.is_empty();
    let paths = {
        let config = config.clone();
        tokio::task::spawn_blocking(move || checks::check_paths(&config, &discovered))
            .await
            // A panicked walk renders as an empty report rather than a 500; the
            // service lines above still carry the useful half of the page.
            .unwrap_or_default()
    };
    let more_missing = paths.missing.len().saturating_sub(MAX_LISTED);

    let swarm = state.serve.swarms().stats().await;
    let runs = recent_run_rows(state).await;
    let gluetun = vec![
        endpoint_status(
            "Tracker/feed",
            &config.gluetun,
            &state.serve.endpoint(),
            state.serve.gluetun_status(GluetunTarget::Tracker),
        )
        .await,
        endpoint_status(
            "Torrent client",
            &config.gluetun_client,
            &state.serve.client_endpoint(),
            state.serve.gluetun_status(GluetunTarget::Client),
        )
        .await,
    ];

    DiagnosticsData {
        services,
        scanned,
        rules: paths.rules,
        checked: paths.checked,
        unmapped: paths.unmapped,
        missing: paths
            .missing
            .iter()
            .take(MAX_LISTED)
            .map(|path| path.display().to_string())
            .collect(),
        more_missing,
        invalid: paths.invalid.iter().take(MAX_LISTED).cloned().collect(),
        readable: paths.readable(),
        healthy: !paths.is_failure(),
        sample: paths.sample.as_ref().map(|sample| SampleRow {
            arr: sample.arr.display().to_string(),
            sharerr: sample.sharerr.display().to_string(),
            qbit: sample.qbit.display().to_string(),
        }),
        gluetun,
        swarm_peers: swarm.peers,
        swarm_seeders: swarm.seeders,
        runs,
    }
}

/// The last few sync runs, newest first — "is the last one healthy" is the
/// status page's glance; this is "is that typical", which needs more than
/// one data point. Empty (not an error) when the store is unavailable — the
/// rest of this page still has a useful answer without it.
async fn recent_run_rows(state: &WebState) -> Vec<RunRow> {
    let Ok(store) = state.serve.store().await else {
        return Vec::new();
    };
    let Ok(runs) = store.recent_runs(RECENT_RUNS).await else {
        return Vec::new();
    };

    runs.into_iter()
        .map(|run| {
            let Some(finished_at) = run.finished_at else {
                return RunRow {
                    when: ago(run.started_at),
                    summary: "still running".to_owned(),
                    failed: false,
                };
            };
            let (summary, failed) = match &run.summary.error {
                Some(error) => (error.clone(), true),
                None => {
                    let mut parts = vec![format!("{} discovered", run.summary.discovered)];
                    if run.summary.added > 0 {
                        parts.push(format!("{} added", run.summary.added));
                    }
                    if run.summary.unshared > 0 {
                        parts.push(format!("{} unshared", run.summary.unshared));
                    }
                    if run.summary.failed > 0 {
                        parts.push(format!("{} failed", run.summary.failed));
                    }
                    (parts.join(", "), run.summary.failed > 0)
                }
            };
            RunRow {
                when: ago(finished_at),
                summary,
                failed,
            }
        })
        .collect()
}

/// One gluetun poller's row, pre-rendered for the template.
async fn endpoint_status(
    label: &'static str,
    gluetun_config: &sharerr_core::config::GluetunConfig,
    endpoint: &AdvertisedEndpoint,
    status: std::sync::Arc<crate::gluetun::GluetunStatus>,
) -> EndpointStatus {
    let snapshot = status.snapshot().await;
    EndpointStatus {
        label,
        enabled: gluetun_config.enabled,
        configured: gluetun_config.control_url.is_some(),
        current: endpoint.current().map(|base| base.to_string()),
        last_observed: endpoint
            .last_observed()
            .map(|observed| format!("{} ({})", observed.base, ago(observed.observed_at))),
        last_poll: snapshot.last_poll_at.map(ago),
        last_success: snapshot.last_success_at.map(ago),
        last_error: snapshot.last_error,
    }
}

/// One line per service, saying whether it contributed anything to the scan.
///
/// The wording is this page's own — the same outcome renders differently as a
/// settings badge — but the *conditions* come from `checks`, so this cannot drift
/// away from what `doctor` reports.
fn describe(kind: MediaSource, config: &Config, outcome: &ArrOutcome) -> ServiceLine {
    let (ok, message) = match outcome {
        ArrOutcome::Ready { version, items, .. } => (
            true,
            format!(
                "{} file(s) tagged {:?} (v{version})",
                items.len(),
                config.tag
            ),
        ),
        ArrOutcome::TagUnused { .. } => (
            true,
            format!(
                "connected, but nothing carries the {:?} tag yet",
                config.tag
            ),
        ),
        ArrOutcome::TagMissing { .. } => (
            false,
            format!(
                "no tag named {:?} exists there — create it first",
                config.tag
            ),
        ),
        ArrOutcome::AuthRejected => (false, "the API key was rejected".to_owned()),
        ArrOutcome::Unreachable(reason) => (false, format!("could not reach it: {reason}")),
        ArrOutcome::NoCredential => (false, "no API key stored yet".to_owned()),
        ArrOutcome::CredentialUnreadable(reason) => (false, reason.clone()),
        ArrOutcome::BadUrl(reason) | ArrOutcome::Failed(reason) => (false, reason.clone()),
        ArrOutcome::NotConfigured => (false, "not configured".to_owned()),
    };

    ServiceLine {
        name: kind.as_str().to_owned(),
        message,
        ok,
    }
}

/// One line per `[[library]]` directory, in this page's voice.
fn describe_library(
    library: &sharerr_core::config::LibraryConfig,
    outcome: &DirOutcome,
) -> ServiceLine {
    let (ok, message) = match outcome {
        DirOutcome::Ready { skipped: 0, items } => (
            true,
            format!("{} {} file(s)", items.len(), library.kind.as_str()),
        ),
        DirOutcome::Ready { skipped, items } => (
            true,
            format!(
                "{} {} file(s); {skipped} skipped — their names could not be classified",
                items.len(),
                library.kind.as_str()
            ),
        ),
        DirOutcome::Empty => (true, "empty — nothing to share yet".to_owned()),
        DirOutcome::Missing => (
            false,
            "does not exist as sharerr sees it — check the mount".to_owned(),
        ),
        DirOutcome::NotADirectory => (false, "not a directory".to_owned()),
        DirOutcome::Unreadable(reason) => (false, format!("could not scan: {reason}")),
    };

    ServiceLine {
        name: format!("library {}", library.path.display()),
        message,
        ok,
    }
}
