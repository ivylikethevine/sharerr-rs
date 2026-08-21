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
        anyhow::bail!(
            "{} item(s) could not be shared and {} *arr app(s) could not be scanned \
             — see the log above",
            report.failed,
            report.sources_failed
        );
    }

    Ok(())
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
