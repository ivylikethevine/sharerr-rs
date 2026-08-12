//! `sharerr sync` — run one reconciliation pass and report what changed.

use anyhow::Result;
use sharerr_core::Config;

use crate::sync::Syncer;

pub async fn run(config: &Config, dry_run: bool) -> Result<()> {
    let syncer = Syncer::build(config).await?;

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
