//! How much history the store keeps: the three tables that grow on their own
//! (swarm samples, a friend's observed addresses, sync runs) are each pruned
//! on write to a limit, and this is where the limits live.
//!
//! Held behind the `Store` handle rather than passed to every `record_*`
//! call: those have half a dozen callers between the tracker, gossip, the
//! feed and the lighthouse client, and a limit is a property of the
//! database, not of one write. Every clone of a `Store` shares one cell, so
//! `ServeState::replace_config` applying a saved `[retention]` reaches the
//! handles the background loops already hold.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

/// The limits, as a plain value. `0` means unbounded where a table was
/// never pruned before (`sync_runs`); the other two always had a cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retention {
    /// Swarm samples kept in total. See [`crate::swarm::MAX_SAMPLES`].
    pub swarm_samples: i64,
    /// Addresses kept per `(friend, kind)`. See `endpoints::MAX_HISTORY`.
    pub endpoint_history: i64,
    /// Sync runs kept, newest first; `0` keeps every run.
    pub sync_runs: i64,
}

impl Default for Retention {
    fn default() -> Self {
        Self::from(&sharerr_core::config::RetentionConfig::default())
    }
}

impl From<&sharerr_core::config::RetentionConfig> for Retention {
    fn from(config: &sharerr_core::config::RetentionConfig) -> Self {
        let clamp = |n: u64| i64::try_from(n).unwrap_or(i64::MAX);
        Self {
            swarm_samples: clamp(config.swarm_samples),
            endpoint_history: clamp(config.endpoint_history),
            sync_runs: clamp(config.sync_runs),
        }
    }
}

/// The shared, lock-free cell behind every clone of one `Store`.
#[derive(Debug)]
pub(crate) struct RetentionCell {
    swarm_samples: AtomicI64,
    endpoint_history: AtomicI64,
    sync_runs: AtomicI64,
}

impl RetentionCell {
    pub(crate) fn new(retention: Retention) -> Arc<Self> {
        let cell = Self {
            swarm_samples: AtomicI64::new(0),
            endpoint_history: AtomicI64::new(0),
            sync_runs: AtomicI64::new(0),
        };
        cell.set(retention);
        Arc::new(cell)
    }

    pub(crate) fn set(&self, retention: Retention) {
        self.swarm_samples
            .store(retention.swarm_samples, Ordering::Relaxed);
        self.endpoint_history
            .store(retention.endpoint_history, Ordering::Relaxed);
        self.sync_runs.store(retention.sync_runs, Ordering::Relaxed);
    }

    pub(crate) fn get(&self) -> Retention {
        Retention {
            swarm_samples: self.swarm_samples.load(Ordering::Relaxed),
            endpoint_history: self.endpoint_history.load(Ordering::Relaxed),
            sync_runs: self.sync_runs.load(Ordering::Relaxed),
        }
    }
}

impl crate::Store {
    /// Replace the limits every later write prunes to. Nothing is pruned
    /// here: a lowered limit takes effect at the next write to each table.
    pub fn set_retention(&self, retention: Retention) {
        self.retention.set(retention);
    }

    /// The limits in force.
    pub fn retention(&self) -> Retention {
        self.retention.get()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn the_defaults_are_the_constants_the_tables_always_pruned_to() {
        let retention = Retention::default();
        assert_eq!(retention.swarm_samples, 336);
        assert_eq!(retention.endpoint_history, 5);
        assert_eq!(retention.sync_runs, 0, "runs were never pruned before");
    }

    #[test]
    fn a_config_section_converts_with_saturation() {
        let config = sharerr_core::config::RetentionConfig {
            swarm_samples: 10,
            endpoint_history: u64::MAX,
            sync_runs: 7,
        };
        let retention = Retention::from(&config);
        assert_eq!(retention.swarm_samples, 10);
        assert_eq!(retention.endpoint_history, i64::MAX);
        assert_eq!(retention.sync_runs, 7);
    }

    #[tokio::test]
    async fn every_clone_of_a_store_sees_a_replaced_retention() {
        let store = crate::Store::open_in_memory().await.unwrap();
        let clone = store.clone();
        store.set_retention(Retention {
            swarm_samples: 1,
            endpoint_history: 2,
            sync_runs: 3,
        });
        assert_eq!(clone.retention().swarm_samples, 1);
        assert_eq!(clone.retention().endpoint_history, 2);
        assert_eq!(clone.retention().sync_runs, 3);
    }
}
