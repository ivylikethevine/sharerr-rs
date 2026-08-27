//! A periodic record of the tracker's live swarm totals.
//!
//! `sharerr_torrent::announce::Swarms` answers "right now" from memory and is
//! deliberately never persisted, for the reason its own doc comment gives.
//! This table answers a different question that the in-memory map cannot
//! answer at all: has anyone been in the swarm *recently*, as opposed to
//! this exact instant. A missed sample — a restart, a slow tick — is just a
//! gap in the chart, never a wrong number; nothing here needs to be
//! authoritative the way `Swarms` needs to be for admission.

use sqlx::Row;

use crate::db::{Store, StoreError};

type Result<T> = std::result::Result<T, StoreError>;

/// How many samples are kept, total. Unlike `peer_endpoints`'s `MAX_HISTORY`
/// there is no per-key partition here, so this bounds the whole table.
///
/// Sized against `crate::swarm_history`'s one-hour sample interval so the
/// window is genuinely the fortnight the feature exists to show: 14 days *
/// 24 samples/day = 336.
pub const MAX_SAMPLES: i64 = 336;

/// One sample of the tracker's live swarm totals, with when it was taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwarmSample {
    pub sampled_at: i64,
    pub swarms: i64,
    pub peers: i64,
    pub seeders: i64,
}

impl Store {
    /// Record one sample, then prune to the newest [`MAX_SAMPLES`].
    ///
    /// Pruning on every insert rather than gating it on "did this change
    /// anything" — [`Self::record_peer_endpoint`] gates because an upsert
    /// there can be a no-op; every call here is a plain insert, so it always
    /// grows the table by exactly one row.
    pub async fn record_swarm_sample(&self, sample: SwarmSample) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "INSERT INTO swarm_samples (sampled_at, swarms, peers, seeders) \
             VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(sample.sampled_at)
        .bind(sample.swarms)
        .bind(sample.peers)
        .bind(sample.seeders)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "DELETE FROM swarm_samples WHERE id NOT IN \
             (SELECT id FROM swarm_samples ORDER BY sampled_at DESC, id DESC LIMIT ?1)",
        )
        .bind(MAX_SAMPLES)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }

    /// The most recent samples, oldest first — the order a chart draws
    /// left-to-right, newest on the right, same convention
    /// `web::diagnostics::run_chart` uses for the run-history strip.
    pub async fn recent_swarm_samples(&self, limit: i64) -> Result<Vec<SwarmSample>> {
        let rows = sqlx::query(
            "SELECT sampled_at, swarms, peers, seeders FROM \
             (SELECT * FROM swarm_samples ORDER BY sampled_at DESC, id DESC LIMIT ?1) \
             ORDER BY sampled_at ASC",
        )
        .bind(limit)
        .fetch_all(self.pool())
        .await?;

        rows.iter()
            .map(|row| {
                Ok(SwarmSample {
                    sampled_at: row.try_get("sampled_at")?,
                    swarms: row.try_get("swarms")?,
                    peers: row.try_get("peers")?,
                    seeders: row.try_get("seeders")?,
                })
            })
            .collect()
    }

    /// When a sample last recorded at least one peer, or `None` if none ever
    /// has. The cheap read the status tile needs to tell "quiet just now"
    /// from "quiet for a fortnight" without pulling a whole chart's worth of
    /// rows for a number the tile does not otherwise use. Resolution is
    /// whatever the sampler's own interval is — coarse by design, since this
    /// answers "roughly how long", not "since when exactly".
    pub async fn last_active_swarm_sample_at(&self) -> Result<Option<i64>> {
        let row = sqlx::query("SELECT MAX(sampled_at) AS at FROM swarm_samples WHERE peers > 0")
            .fetch_one(self.pool())
            .await?;
        Ok(row.try_get("at")?)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn sample(sampled_at: i64, peers: i64) -> SwarmSample {
        SwarmSample {
            sampled_at,
            swarms: 1,
            peers,
            seeders: 0,
        }
    }

    #[tokio::test]
    async fn recording_a_sample_makes_it_the_most_recent() {
        let store = Store::open_in_memory().await.unwrap();
        store.record_swarm_sample(sample(100, 3)).await.unwrap();
        let recent = store.recent_swarm_samples(10).await.unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].peers, 3);
    }

    #[tokio::test]
    async fn samples_come_back_oldest_first() {
        let store = Store::open_in_memory().await.unwrap();
        for at in [300, 100, 200] {
            store.record_swarm_sample(sample(at, 0)).await.unwrap();
        }
        let recent = store.recent_swarm_samples(10).await.unwrap();
        let times: Vec<i64> = recent.iter().map(|s| s.sampled_at).collect();
        assert_eq!(times, vec![100, 200, 300]);
    }

    #[tokio::test]
    async fn the_history_is_bounded_to_max_samples() {
        let store = Store::open_in_memory().await.unwrap();
        for at in 0..(MAX_SAMPLES + 20) {
            store.record_swarm_sample(sample(at, 0)).await.unwrap();
        }
        let recent = store.recent_swarm_samples(MAX_SAMPLES + 100).await.unwrap();
        assert_eq!(
            recent.len() as i64,
            MAX_SAMPLES,
            "old samples must be pruned"
        );
        assert_eq!(
            recent.last().unwrap().sampled_at,
            MAX_SAMPLES + 19,
            "the newest must survive"
        );
    }

    #[tokio::test]
    async fn last_active_sample_is_none_when_nobody_has_ever_been_seen() {
        let store = Store::open_in_memory().await.unwrap();
        store.record_swarm_sample(sample(100, 0)).await.unwrap();
        assert_eq!(store.last_active_swarm_sample_at().await.unwrap(), None);
    }

    #[tokio::test]
    async fn last_active_sample_ignores_quiet_samples_that_came_after() {
        let store = Store::open_in_memory().await.unwrap();
        store.record_swarm_sample(sample(100, 2)).await.unwrap();
        store.record_swarm_sample(sample(200, 0)).await.unwrap();
        assert_eq!(
            store.last_active_swarm_sample_at().await.unwrap(),
            Some(100)
        );
    }
}
