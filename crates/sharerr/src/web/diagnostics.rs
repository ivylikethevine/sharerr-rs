//! The diagnostics page: does the library actually resolve?
//!
//! `doctor` has always answered this from a shell. The settings page's "Test
//! connection" buttons deliberately do not — they answer a one-line question about
//! one service, and path resolution needs a full discovery walk plus a look at the
//! filesystem. The result was that the check most likely to explain "sharerr does
//! nothing" was the one an operator who never opens a terminal could not run.
//!
//! The checking is shared with `doctor` via [`crate::checks`]; this module only
//! gathers and renders.

use axum::extract::State;
use axum::response::Response;
use secrecy::SecretString;
use sharerr_arr::Discovered;
use sharerr_core::config::secret_keys;
use sharerr_core::{Config, MediaSource};

use super::WebState;
use super::templates::{DiagnosticsPage, SampleRow, ServiceLine, render};
use crate::checks::{self, ArrOutcome};

/// How many problem paths to name before summarising the rest.
///
/// A library with a broken mapping has *every* file broken, and a page listing ten
/// thousand of them buries the advice that would fix it.
const MAX_LISTED: usize = 20;

pub async fn page(State(state): State<WebState>) -> Response {
    let config = state.serve.config().await;

    let mut services = Vec::new();
    let mut discovered: Vec<Discovered> = Vec::new();

    for kind in [MediaSource::Sonarr, MediaSource::Radarr] {
        let (service, key) = match kind {
            MediaSource::Sonarr => (config.sonarr.as_ref(), secret_keys::SONARR_API_KEY),
            MediaSource::Radarr => (config.radarr.as_ref(), secret_keys::RADARR_API_KEY),
        };

        // An unconfigured service is not a fault to report — plenty of instances
        // run only one of the two.
        if service.is_none() {
            continue;
        }

        let outcome = checks::check_arr(
            kind,
            service.map(|s| &s.url),
            secret(&state, key).await,
            &config.tag,
        )
        .await;
        services.push(describe(kind, &config, &outcome));
        discovered.extend(outcome.into_items());
    }

    let paths = checks::check_paths(&config, &discovered);
    let more_missing = paths.missing.len().saturating_sub(MAX_LISTED);

    render(&DiagnosticsPage {
        signed_in: true,
        services,
        scanned: !discovered.is_empty(),
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
    })
}

async fn secret(state: &WebState, key: &'static str) -> Result<Option<SecretString>, String> {
    state
        .serve
        .open_vault()
        .await?
        .get(key)
        .map_err(|err| err.to_string())
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
