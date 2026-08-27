//! Periodically records the tracker's live swarm totals, so the status
//! page's "Swarms" tile can eventually tell "nobody is here right now" from
//! "nobody has been here in a fortnight" — see `migrations/0011_swarm_samples.sql`
//! for why those read identically today.
//!
//! Modelled directly on [`crate::system_stats`]: a sample is taken on a
//! timer and stored, and a handler that wants "how long has it been quiet"
//! reads the store rather than doing its own I/O.

use std::sync::Arc;
use std::time::Duration;

use sharerr_core::endpoint::now_epoch;
use sharerr_store::SwarmSample;

use crate::state::ServeState;

/// How often the background loop samples. An hour, not `system_stats`'
/// five seconds — this feeds a fortnight-scale chart, not a live gauge, and
/// `sharerr_store::swarm::MAX_SAMPLES` is sized against this exact interval.
const POLL_INTERVAL: Duration = Duration::from_secs(3600);

/// Sample the live swarms and record the result, forever. Never returns.
///
/// A store that will not open is skipped rather than treated as an error —
/// same tolerance [`crate::commands::serve::background`] gives a config or
/// credential that is not ready yet — so an instance still bringing up its
/// vault does not spam a warning every hour.
pub async fn poll_loop(state: Arc<ServeState>) {
    loop {
        if let Ok(store) = state.store().await {
            let stats = state.swarms().stats().await;
            if let Err(err) = store
                .record_swarm_sample(SwarmSample {
                    sampled_at: now_epoch(),
                    swarms: stats.swarms as i64,
                    peers: stats.peers as i64,
                    seeders: stats.seeders as i64,
                })
                .await
            {
                tracing::warn!(error = %err, "could not record a swarm history sample");
            }
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
