//! The health checks folded into the combined status/diagnostics page: does
//! the library actually resolve?
//!
//! `doctor` answers this from a shell. The settings page's "Test connection"
//! buttons deliberately do not — they answer a one-line question about one
//! service, while path resolution needs a full discovery walk plus a look at
//! the filesystem. Without this page, the check most likely to explain
//! "sharerr does nothing" was the one an operator who never opens a terminal
//! could not run.
//!
//! The checking is shared with `doctor` via [`crate::checks`]; this module
//! only gathers — the status page renders it together with the glance, since
//! both answer "is this instance healthy" at different levels of detail and
//! a second page to click through to answered no one's question.

use secrecy::SecretString;
use sharerr_core::endpoint::AdvertisedEndpoint;
use sharerr_core::{Config, MediaSource};

use super::WebState;
use super::peers::ago;
use super::templates::{
    DiagnosticsData, EndpointStatus, LighthouseRow, LighthouseView, RunRow, SampleRow, ServiceLine,
};
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
    let api_key = secret_reader(&state.serve).await;

    // Shared with `web::topology::gather` — see `checks::snapshot`'s docs for
    // why the arr probes, library scan, and path check live there instead of
    // being duplicated per page. The run history and the two poller rows do
    // not depend on it, so they are gathered alongside rather than after.
    let (snapshot, runs, gluetun) = tokio::join!(
        checks::snapshot(&config, &api_key),
        recent_run_rows(state),
        futures::future::join_all(
            [GluetunTarget::Tracker, GluetunTarget::Client]
                .into_iter()
                .map(|target| async move {
                    let label = match target {
                        GluetunTarget::Tracker => "Tracker/feed",
                        GluetunTarget::Client => "Torrent client",
                    };
                    endpoint_status(
                        label,
                        target.config(&config),
                        &state.serve.endpoint_for(target),
                        state.serve.gluetun_status(target),
                    )
                    .await
                }),
        ),
    );
    let checks::Snapshot {
        sources,
        libraries,
        paths,
    } = snapshot;

    let mut services: Vec<ServiceLine> = sources
        .iter()
        .map(|(kind, outcome)| describe(*kind, &config, outcome))
        .collect();
    match &libraries {
        checks::LibraryScan::Scanned(scanned) => {
            services.extend(
                scanned
                    .iter()
                    .map(|(library, outcome)| describe_library(library, outcome)),
            );
        }
        checks::LibraryScan::Panicked(err) => {
            // A panicked scan must not make a configured [[library]] install
            // look identical to one with no libraries at all.
            services.push(ServiceLine {
                name: "library".to_owned(),
                message: format!("the scan did not complete: {err}"),
                ok: false,
                url: String::new(),
            });
        }
    }

    // `paths.checked` is `discovered.len()` from `checks::snapshot` — nonzero
    // exactly when either phase found something.
    let scanned = paths.checked > 0;
    let more_missing = paths.missing.len().saturating_sub(MAX_LISTED);

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
        runs,
        lighthouse: lighthouse_view(state, &config).await,
    }
}

/// The vault opened once, wrapped as the `secret` reader `checks` takes.
///
/// Opening the vault derives the key with Argon2 — ~16ms of solid CPU, more
/// on ARM — so a page or badge that needs several secrets opens it once and
/// reads through this rather than going through `WebState::secret` per key.
/// A vault that would not open answers every read with the same reason.
pub(super) async fn secret_reader(
    state: &crate::state::ServeState,
) -> impl Fn(&'static str) -> Result<Option<SecretString>, String> {
    let vault = state.open_vault().await;
    move |key: &'static str| match &vault {
        Ok(vault) => vault.get(key).map_err(|err| err.to_string()),
        Err(reason) => Err(reason.clone()),
    }
}

/// What the lighthouse poller is doing, or `None` when none is configured.
///
/// Reads the running poller's own record rather than probing anything: a
/// lighthouse is contacted on a 15-minute timer, and dialling one from a page
/// load would report on a request the poller never made.
async fn lighthouse_view(
    state: &WebState,
    config: &sharerr_core::config::Config,
) -> Option<LighthouseView> {
    let configured = config.lighthouse.urls.len();
    if configured == 0 {
        return None;
    }

    let snapshot = state.serve.lighthouse_status().snapshot().await;
    let rows: Vec<LighthouseRow> = snapshot
        .lighthouses
        .iter()
        .map(|report| LighthouseRow {
            url: report.url.clone(),
            last_success: report.last_success_at.map(ago),
            last_error: report.last_error.clone(),
        })
        .collect();

    // Healthy means every configured lighthouse has accepted a report and none
    // is currently failing. A URL the poller has not reached yet has no row at
    // all, so a short row list is itself a failure to report — which is why
    // this compares against `configured` rather than against `rows.len()`.
    let accepting = rows
        .iter()
        .filter(|row| row.last_success.is_some() && row.last_error.is_none())
        .count();

    Some(LighthouseView {
        configured,
        last_pass: snapshot.last_pass_at.map(ago),
        healthy: accepting == configured,
        rows,
        last_recovery: snapshot.last_recovery_at.map(ago),
        last_recovery_peer: snapshot.last_recovery_peer.clone(),
        lookups_attempted: snapshot.lookups_attempted,
    })
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
                    when_absolute: super::peers::absolute(run.started_at),
                    // Nothing to measure to yet, and "0s" would read as a run
                    // that finished instantly rather than one still going.
                    took: String::new(),
                    summary: "still running".to_owned(),
                    failed: false,
                };
            };
            let (summary, failed) = run.summary.describe(true);
            RunRow {
                when: ago(finished_at),
                when_absolute: super::peers::absolute(finished_at),
                took: super::peers::took(run.started_at, finished_at),
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
        last_observed: super::settings::gluetun_last_observed(endpoint),
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
        ArrOutcome::Ready {
            version,
            items,
            app_name,
            ..
        } => (
            true,
            // The app's own name, so a Sonarr URL that actually answers as
            // Radarr is visible here rather than only in `doctor`.
            format!(
                "{} file(s) tagged {:?} ({app_name} v{version})",
                items.len(),
                config.tag
            ),
        ),
        ArrOutcome::TagUnused { version } => (
            true,
            format!(
                "connected, but nothing carries the {:?} tag yet (v{version})",
                config.tag
            ),
        ),
        ArrOutcome::TagMissing { version } => (
            false,
            format!(
                "no tag named {:?} exists there — create it first (v{version})",
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
        name: super::settings::title_case(kind.as_str()),
        message,
        ok,
        // Already in hand from the config this function was passed. A line
        // reading "could not reach it: connection refused" is far more
        // actionable next to the address that was actually dialled.
        url: config
            .service(kind)
            .map(|service| service.url.to_string())
            .unwrap_or_default(),
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
        // The path is the identity here, and it is already in `name`.
        url: String::new(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::Arc;

    use sharerr_core::config::{GluetunConfig, LibraryConfig, LibraryKind, ServiceConfig};
    use sharerr_store::db::RunSummary;
    use url::Url;

    use super::*;
    use crate::web::auth::Sessions;

    /// Same helper `web/settings.rs` and `web/probe.rs` build: a `WebState`
    /// wired to a `ServeState` fixture rather than a real signed-in router,
    /// which nothing else in this module's test needs.
    fn web_state(serve: Arc<crate::state::ServeState>) -> WebState {
        WebState {
            serve,
            sessions: Arc::new(Sessions::default()),
        }
    }

    // ---------------------------------------------------------------- gather

    /// A fresh instance — no sources, no vault, nothing on disk — must still
    /// render a page rather than panicking. This is the state a container
    /// boots into on its very first start.
    #[tokio::test]
    async fn gather_on_an_unconfigured_instance_degrades_gracefully() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);

        let data = gather(&state).await;

        assert!(data.services.is_empty(), "{:?}", data.services);
        assert!(!data.scanned);
        assert_eq!(data.rules, 0);
        assert_eq!(data.checked, 0);
        assert!(data.missing.is_empty());
        assert_eq!(data.more_missing, 0);
        assert!(data.invalid.is_empty());
        assert!(data.sample.is_none());
        // No missing/invalid paths at all is a healthy report, even though
        // nothing was actually checked.
        assert!(data.healthy);
        assert!(data.runs.is_empty());
        assert_eq!(data.gluetun.len(), 2);
        // `enabled` defaults to true for both pollers — it is independent of
        // whether a `control_url` is set, see `GluetunConfig::default`.
        assert!(data.gluetun[0].enabled);
        assert!(!data.gluetun[0].configured);
        assert!(data.gluetun[1].enabled);
    }

    /// A configured Sonarr URL with no master key set in this process finds
    /// the vault unreadable rather than open — see CLAUDE.md's "no tier-1
    /// fixture opens a real vault" rule, and `web/probe.rs`'s tests, which
    /// document the same deterministic outcome. This still exercises a real
    /// branch: `configured_sources` picking Sonarr up, the vault lookup
    /// failing, and that rendering as an unhealthy service line rather than
    /// a panic or a silently dropped source.
    #[tokio::test]
    async fn gather_reports_a_configured_arr_source_as_credential_unreadable() {
        let (dir, serve) = crate::state::fixtures::unconfigured();
        let config = Config {
            data_dir: dir.path().to_path_buf(),
            sonarr: Some(ServiceConfig {
                url: Url::parse("http://sonarr.example:8989").unwrap(),
            }),
            ..Config::default()
        };
        serve.replace_config(config).await;
        let state = web_state(serve);

        let data = gather(&state).await;

        assert_eq!(data.services.len(), 1);
        assert!(!data.services[0].ok, "{:?}", data.services[0]);
        assert_eq!(data.services[0].name, "Sonarr");
        // Nothing was discovered, so path checking has nothing to say either.
        assert!(!data.scanned);
        assert!(data.healthy);
    }

    /// A `[[library]]` pointed at a real directory is the one path through
    /// `gather` that can be genuinely healthy without a vault: the file was
    /// scanned from disk moments before `check_paths` re-stats it, so it is
    /// always there.
    #[tokio::test]
    async fn gather_scans_a_real_library_and_reports_it_healthy() {
        let (dir, serve) = crate::state::fixtures::unconfigured();
        let media = tempfile::tempdir().unwrap();
        let library = sharerr_testkit::library::tv_library(media.path()).unwrap();

        let config = Config {
            data_dir: dir.path().to_path_buf(),
            library: vec![LibraryConfig {
                path: library.root.join("tv"),
                kind: LibraryKind::Tv,
            }],
            ..Config::default()
        };
        serve.replace_config(config).await;
        let state = web_state(serve);

        let data = gather(&state).await;

        assert_eq!(data.services.len(), 1);
        assert!(data.services[0].ok, "{:?}", data.services[0]);
        assert!(data.services[0].name.contains("library"));
        assert!(data.scanned);
        assert_eq!(data.checked, library.files.len());
        assert_eq!(data.readable, library.files.len());
        assert!(data.missing.is_empty());
        assert!(data.invalid.is_empty());
        assert!(data.healthy);
        let sample = data.sample.expect("a scanned library yields a sample");
        assert_eq!(sample.arr, sample.sharerr);
    }

    /// A `[[library]]` pointed at a directory that does not exist must be
    /// named as broken, not silently skipped — this is `doctor`'s "does
    /// nothing" failure mode, reachable from the web UI too.
    #[tokio::test]
    async fn gather_reports_a_missing_library_directory() {
        let (dir, serve) = crate::state::fixtures::unconfigured();
        let config = Config {
            data_dir: dir.path().to_path_buf(),
            library: vec![LibraryConfig {
                path: dir.path().join("does-not-exist"),
                kind: LibraryKind::Movie,
            }],
            ..Config::default()
        };
        serve.replace_config(config).await;
        let state = web_state(serve);

        let data = gather(&state).await;

        assert_eq!(data.services.len(), 1);
        assert!(!data.services[0].ok, "{:?}", data.services[0]);
        assert!(data.services[0].message.contains("does not exist"));
        // Nothing was discovered, so the scan flag stays false even though a
        // library is configured.
        assert!(!data.scanned);
    }

    /// The run history renders "still running" for an unfinished run and the
    /// stored error for a failed one, newest first, capped at `RECENT_RUNS`
    /// even when more rows exist.
    #[tokio::test]
    async fn gather_renders_run_history_and_caps_it_at_recent_runs() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let store = serve
            .store()
            .await
            .expect("store opens with an empty vault");

        // More rows than RECENT_RUNS reports, so the cap actually bites.
        let total = RECENT_RUNS as usize + 3;
        for i in 0..total {
            let id = store.begin_run().await.unwrap();
            if i == total - 1 {
                // The most recent run: still in flight.
                continue;
            }
            let summary = if i == total - 2 {
                RunSummary {
                    error: Some("could not reach sonarr".to_owned()),
                    ..RunSummary::default()
                }
            } else {
                RunSummary {
                    discovered: 4,
                    added: 1,
                    ..RunSummary::default()
                }
            };
            store.finish_run(id, &summary).await.unwrap();
        }

        let state = web_state(serve);
        let data = gather(&state).await;

        assert_eq!(data.runs.len(), RECENT_RUNS as usize);
        // Newest first: the last-inserted (unfinished) run leads.
        assert_eq!(data.runs[0].summary, "still running");
        assert!(!data.runs[0].failed);
        // The one before it recorded an error.
        assert!(data.runs[1].failed);
        assert_eq!(data.runs[1].summary, "could not reach sonarr");
        assert!(!data.runs[2].failed);
    }

    /// An observed endpoint must show up as both `current` and
    /// `last_observed`, for whichever poller it belongs to — the two entries
    /// must not be swapped or conflated.
    #[tokio::test]
    async fn gather_reflects_endpoint_observations_per_poller() {
        let (dir, serve) = crate::state::fixtures::unconfigured();
        let config = Config {
            data_dir: dir.path().to_path_buf(),
            gluetun: GluetunConfig {
                enabled: true,
                control_url: Some(Url::parse("http://127.0.0.1:8000").unwrap()),
                ..GluetunConfig::default()
            },
            ..Config::default()
        };
        serve.replace_config(config).await;
        serve
            .endpoint()
            .observe(Url::parse("http://tracker.example:9000").unwrap());
        serve
            .client_endpoint()
            .observe(Url::parse("http://client.example:9001").unwrap());
        let state = web_state(serve);

        let data = gather(&state).await;

        assert_eq!(data.gluetun.len(), 2);
        let tracker = &data.gluetun[0];
        assert!(tracker.enabled);
        assert!(tracker.configured);
        assert_eq!(
            tracker.current.as_deref(),
            Some("http://tracker.example:9000/")
        );
        assert!(tracker.last_observed.is_some());

        let client = &data.gluetun[1];
        // No `[gluetun_client]` configured, but it's still enabled by default
        // (independent of `control_url`), and the observation still shows —
        // the two pollers are independent.
        assert!(client.enabled);
        assert!(!client.configured);
        assert_eq!(
            client.current.as_deref(),
            Some("http://client.example:9001/")
        );
    }

    // -------------------------------------------------------- describe (arr)

    #[test]
    fn describe_covers_every_arr_outcome() {
        let config = Config::default();

        let cases: Vec<(ArrOutcome, bool)> = vec![
            (ArrOutcome::NotConfigured, false),
            (ArrOutcome::NoCredential, false),
            (
                ArrOutcome::CredentialUnreadable("no master key".to_owned()),
                false,
            ),
            (ArrOutcome::BadUrl("not a url".to_owned()), false),
            (
                ArrOutcome::Unreachable("connection refused".to_owned()),
                false,
            ),
            (ArrOutcome::AuthRejected, false),
            (ArrOutcome::Failed("500".to_owned()), false),
            (
                ArrOutcome::TagMissing {
                    version: "4.0.0".to_owned(),
                },
                false,
            ),
            (
                ArrOutcome::TagUnused {
                    version: "4.0.0".to_owned(),
                },
                true,
            ),
            (
                ArrOutcome::Ready {
                    version: "4.0.0".to_owned(),
                    app_name: "Sonarr".to_owned(),
                    items: Vec::new(),
                },
                true,
            ),
        ];

        for (outcome, expect_ok) in cases {
            let line = describe(MediaSource::Sonarr, &config, &outcome);
            assert_eq!(line.ok, expect_ok, "{outcome:?} -> {line:?}");
            assert_eq!(line.name, "Sonarr");
            assert!(!line.message.is_empty());
        }
    }

    /// The distinction the whole module exists to preserve, reasserted at
    /// the rendering layer: a missing tag and an unused one must not read
    /// the same to an operator.
    #[test]
    fn describe_distinguishes_tag_missing_from_tag_unused() {
        let config = Config::default();

        let missing = describe(
            MediaSource::Radarr,
            &config,
            &ArrOutcome::TagMissing {
                version: "1.0".to_owned(),
            },
        );
        let unused = describe(
            MediaSource::Radarr,
            &config,
            &ArrOutcome::TagUnused {
                version: "1.0".to_owned(),
            },
        );

        assert_ne!(missing.message, unused.message);
        assert!(missing.message.contains("no tag named"));
        assert!(unused.message.contains("nothing carries"));
        // Both outcomes carry the version they were told, and both used to
        // discard it — leaving the page unable to say which *arr it reached.
        assert!(missing.message.contains("1.0"), "{}", missing.message);
        assert!(unused.message.contains("1.0"), "{}", unused.message);
    }

    /// "could not reach it" is only actionable next to the address that was
    /// actually dialled — the cause is usually a typo visible in the URL.
    #[test]
    fn describe_carries_the_address_it_was_talking_to() {
        let config = Config {
            radarr: Some(ServiceConfig {
                url: Url::parse("http://radarr.example:7878").unwrap(),
            }),
            ..Config::default()
        };

        let line = describe(
            MediaSource::Radarr,
            &config,
            &ArrOutcome::Unreachable("connection refused".to_owned()),
        );

        assert!(line.url.contains("radarr.example:7878"), "{}", line.url);
    }

    /// A service with no section at all has no address to name, and must not
    /// invent one — the line still has to render.
    #[test]
    fn an_unconfigured_service_reports_no_address() {
        let line = describe(
            MediaSource::Radarr,
            &Config::default(),
            &ArrOutcome::NotConfigured,
        );

        assert!(line.url.is_empty());
        assert!(!line.message.is_empty());
    }

    /// A library line is identified by its path, which is already the name —
    /// a URL there would be a second, emptier identity.
    #[test]
    fn a_library_line_has_no_address() {
        let library = LibraryConfig {
            path: std::path::PathBuf::from("/media/tv"),
            kind: LibraryKind::Tv,
        };
        let line = describe_library(&library, &DirOutcome::Empty);

        assert!(line.url.is_empty());
    }

    #[test]
    fn describe_names_a_healthy_service_with_its_file_count() {
        let mut config = Config::default();
        config.tag = "sharerr".to_owned();
        let outcome = ArrOutcome::Ready {
            version: "4.0.15".to_owned(),
            app_name: "Sonarr".to_owned(),
            items: vec![sample_discovered(), sample_discovered()],
        };

        let line = describe(MediaSource::Sonarr, &config, &outcome);

        assert!(line.ok);
        assert!(line.message.contains('2'), "{}", line.message);
        assert!(line.message.contains("v4.0.15"), "{}", line.message);
    }

    fn sample_discovered() -> sharerr_core::Discovered {
        sharerr_core::Discovered {
            source: MediaSource::Sonarr,
            source_id: 1,
            file_id: 2,
            spec: sharerr_core::MediaSpec::Movie {
                title: "Gilded Ferry".to_owned(),
                year: Some(2019),
            },
            arr_path: std::path::PathBuf::from("/tv/Gilded Ferry/ep.mkv"),
            size: 2,
            ids: sharerr_core::ExternalIds::default(),
            scene_name: None,
        }
    }

    // ------------------------------------------------------- describe_library

    #[test]
    fn describe_library_covers_every_dir_outcome() {
        let library = LibraryConfig {
            path: std::path::PathBuf::from("/media/tv"),
            kind: LibraryKind::Tv,
        };

        let cases: Vec<(DirOutcome, bool)> = vec![
            (DirOutcome::Missing, false),
            (DirOutcome::NotADirectory, false),
            (
                DirOutcome::Unreadable("permission denied".to_owned()),
                false,
            ),
            (DirOutcome::Empty, true),
            (
                DirOutcome::Ready {
                    skipped: 0,
                    items: vec![sample_discovered()],
                },
                true,
            ),
            (
                DirOutcome::Ready {
                    skipped: 2,
                    items: vec![sample_discovered()],
                },
                true,
            ),
        ];

        for (outcome, expect_ok) in cases {
            let line = describe_library(&library, &outcome);
            assert_eq!(line.ok, expect_ok, "{outcome:?} -> {line:?}");
            assert!(line.name.contains("/media/tv"));
        }
    }

    /// Skipped files are named as such rather than silently vanishing from
    /// the count — an operator staring at "3 file(s)" when 5 exist on disk
    /// would otherwise have no way to learn why.
    #[test]
    fn describe_library_mentions_skipped_files_only_when_there_are_any() {
        let library = LibraryConfig {
            path: std::path::PathBuf::from("/media/tv"),
            kind: LibraryKind::Tv,
        };

        let clean = describe_library(
            &library,
            &DirOutcome::Ready {
                skipped: 0,
                items: vec![sample_discovered()],
            },
        );
        assert!(!clean.message.contains("skipped"));

        let dirty = describe_library(
            &library,
            &DirOutcome::Ready {
                skipped: 2,
                items: vec![sample_discovered()],
            },
        );
        assert!(dirty.message.contains("2 skipped"));
    }

    // --------------------------------------------------------- endpoint_status

    #[tokio::test]
    async fn endpoint_status_reports_nothing_configured_and_never_observed() {
        let gluetun_config = GluetunConfig::default();
        let endpoint = AdvertisedEndpoint::new(None);
        let status = Arc::new(crate::gluetun::GluetunStatus::default());

        let row = endpoint_status("Tracker/feed", &gluetun_config, &endpoint, status).await;

        assert_eq!(row.label, "Tracker/feed");
        // `enabled` defaults to true — it's independent of `control_url`.
        assert!(row.enabled);
        assert!(!row.configured);
        assert!(row.current.is_none());
        assert!(row.last_observed.is_none());
        assert!(row.last_poll.is_none());
        assert!(row.last_success.is_none());
        assert!(row.last_error.is_none());
    }

    #[tokio::test]
    async fn endpoint_status_prefers_the_dynamic_observation_over_the_static_base() {
        let gluetun_config = GluetunConfig {
            enabled: true,
            control_url: Some(Url::parse("http://127.0.0.1:8000").unwrap()),
            ..GluetunConfig::default()
        };
        let endpoint = AdvertisedEndpoint::new(Some(Url::parse("http://static.example").unwrap()));
        endpoint.observe(Url::parse("http://dynamic.example").unwrap());
        let status = Arc::new(crate::gluetun::GluetunStatus::default());

        let row = endpoint_status("Torrent client", &gluetun_config, &endpoint, status).await;

        assert!(row.enabled);
        assert!(row.configured);
        assert_eq!(row.current.as_deref(), Some("http://dynamic.example/"));
        let observed = row.last_observed.expect("an observation was recorded");
        assert!(
            observed.starts_with("http://dynamic.example/"),
            "{observed}"
        );
    }

    // --------------------------------------------------------- recent_run_rows

    /// A store that cannot open — an unwritable `data_dir` — must degrade to
    /// an empty run history rather than fail the whole page.
    #[tokio::test]
    async fn recent_run_rows_is_empty_when_the_store_cannot_open() {
        let (dir, serve) = crate::state::fixtures::unconfigured();
        // A file where the store would need to create a directory: `create_dir_all`
        // fails deterministically, regardless of which user runs the test.
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"not a directory").unwrap();
        let config = Config {
            data_dir: blocker.join("data"),
            ..Config::default()
        };
        serve.replace_config(config).await;
        let state = web_state(serve);

        let runs = recent_run_rows(&state).await;

        assert!(runs.is_empty());
    }
}
