//! `sharerr sync` — run one reconciliation pass and report what changed.

use std::sync::Arc;

use anyhow::Result;
use sharerr_core::Config;
use sharerr_core::endpoint::AdvertisedEndpoint;

use crate::sync::Syncer;

pub async fn run(config: &Config, dry_run: bool) -> Result<()> {
    let endpoint = Arc::new(AdvertisedEndpoint::new(
        sharerr_core::endpoint::advertised_base(&config.tracker, config.server.bind.port())?,
    ));

    // One-shot resolve from the same source of truth `serve`'s poller uses, so a
    // manual sync inside a gluetun namespace builds torrents that announce to
    // the live forwarded port rather than yesterday's.
    if let Some(control) = &config.gluetun.control_url {
        let api_key = gluetun_api_key(config).await;
        match crate::gluetun::GluetunClient::new(control, api_key) {
            // No prior observation exists in a one-shot run, so there is no
            // fallback port to offer — a failed port lookup here is fatal.
            Ok(client) => match client.resolve_base(None).await {
                Ok(base) => {
                    endpoint.observe(base);
                }
                Err(err) => tracing::warn!(
                    %err,
                    "continuing with the statically configured endpoint"
                ),
            },
            Err(err) => tracing::warn!(%err, "could not build a gluetun client"),
        }
    }

    let syncer = Syncer::build(config, endpoint).await?;

    if dry_run {
        println!("dry run — nothing will be created, changed, or removed\n");
    }

    let report = syncer.run(dry_run).await?;
    println!("{report}");

    if report.has_problems() {
        // Non-zero exit so a scripted invocation notices. The per-item reasons are
        // already on the log and in `shared_items.last_error`. An *arr app that
        // could not be scanned counts too: nothing was lost, but the pass did not
        // cover what it was asked to.
        anyhow::bail!(problems_message(report.failed, report.sources_failed));
    }

    Ok(())
}

/// The error text for a run that left problems behind, split out from [`run`]
/// so the wording can be checked without driving a real sync.
fn problems_message(failed: usize, sources_failed: usize) -> String {
    format!(
        "{failed} item(s) could not be shared and {sources_failed} *arr app(s) could not be scanned \
         — see the log above"
    )
}

/// Best-effort lookup, mirroring [`crate::gluetun::poll_loop`]'s: a vault that
/// will not open (no master key set for this run) means an unkeyed request,
/// and any resulting `401` explains itself.
async fn gluetun_api_key(config: &Config) -> Option<secrecy::SecretString> {
    crate::secrets::open_vault_async(config)
        .await
        .ok()?
        .get(sharerr_core::config::secret_keys::GLUETUN_API_KEY)
        .ok()
        .flatten()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn problems_message_reports_both_counts() {
        let text = problems_message(3, 1);
        assert!(text.contains("3 item(s) could not be shared"));
        assert!(text.contains("1 *arr app(s) could not be scanned"));
    }

    #[test]
    fn problems_message_zero_counts_still_render() {
        let text = problems_message(0, 0);
        assert!(text.starts_with("0 item(s) could not be shared and 0 *arr app(s)"));
    }

    /// `sync` is a one-shot CLI run, and an operator invoking it by hand
    /// usually has no `SHARERR_MASTER_KEY` set. That must degrade to an
    /// unkeyed gluetun request -- whose 401 explains itself -- rather than
    /// failing the whole pass before it reaches the *arr apps.
    #[tokio::test]
    async fn a_missing_vault_yields_no_gluetun_key_rather_than_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            data_dir: dir.path().to_path_buf(),
            ..Config::default()
        };

        assert!(gluetun_api_key(&config).await.is_none());
    }

    /// The bail path's exit code is what a cron wrapper keys off, so the
    /// message has to name both failure kinds even when only one occurred --
    /// an *arr app that could not be scanned is a partial pass, not a clean one.
    #[test]
    fn problems_message_names_both_kinds_when_only_one_occurred() {
        let only_items = problems_message(2, 0);
        assert!(only_items.contains("2 item(s)"));
        assert!(only_items.contains("0 *arr app(s)"));

        let only_sources = problems_message(0, 3);
        assert!(only_sources.contains("0 item(s)"));
        assert!(only_sources.contains("3 *arr app(s)"));
    }
}
