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

use std::sync::{Arc, PoisonError, RwLock};

/// The limits, as a plain value. `0` means unbounded where a table was
/// never pruned before (`sync_runs`); the other two always had a cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retention {
    /// Swarm samples kept in total: by default a fortnight of hourly samples.
    pub swarm_samples: i64,
    /// Addresses kept per `(friend, kind)`: enough to span a few reconnects; more
    /// would be a location log.
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
        Self {
            swarm_samples: sharerr_core::saturating_i64(config.swarm_samples),
            endpoint_history: sharerr_core::saturating_i64(config.endpoint_history),
            sync_runs: sharerr_core::saturating_i64(config.sync_runs),
        }
    }
}

/// The shared cell behind every clone of one `Store`.
pub(crate) type RetentionCell = Arc<RwLock<Retention>>;

impl crate::Store {
    /// Replace the limits every later write prunes to. Nothing is pruned
    /// here: a lowered limit takes effect at the next write to each table.
    pub fn set_retention(&self, retention: Retention) {
        *self
            .retention
            .write()
            .unwrap_or_else(PoisonError::into_inner) = retention;
    }

    /// The limits in force.
    pub fn retention(&self) -> Retention {
        *self
            .retention
            .read()
            .unwrap_or_else(PoisonError::into_inner)
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
