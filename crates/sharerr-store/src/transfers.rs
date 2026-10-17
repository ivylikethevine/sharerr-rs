//! What the tracker's announces say has moved: per friend, and in total.
//!
//! The schema and the reasoning behind its two homes for the same deltas
//! are in `migrations/0013_transfer_totals.sql`. The tracker computes the
//! deltas (`sharerr_torrent::announce::Transfer`); this only adds them up.

use sharerr_core::endpoint::now_epoch;
use sqlx::Row;

use crate::db::{Store, StoreError};

type Result<T> = std::result::Result<T, StoreError>;

/// The instance-wide running totals, across every admitted announce.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TransferTotals {
    /// Bytes clients report having downloaded from swarms this tracker
    /// serves: the one first-hand figure for bytes leaving this instance.
    pub downloaded: i64,
    /// Bytes those same clients report having uploaded to others.
    pub uploaded: i64,
    /// When a non-zero delta last landed, or `None` if none ever has.
    pub updated_at: Option<i64>,
}

/// Saturating rather than wrapping: a client reporting more than `i64::MAX`
/// bytes is lying, and a clamped total is less wrong than a negative one.
fn clamp(bytes: u64) -> i64 {
    i64::try_from(bytes).unwrap_or(i64::MAX)
}

impl Store {
    /// Add one announce's delta to the instance total and, when the announce
    /// was attributed, to that friend's own figures.
    ///
    /// A zero delta writes nothing: the common re-announce of a torrent
    /// nobody is pulling costs the sync loop no write lock, the same concern
    /// `touch_peer`'s throttle answers for `last_seen_at`. Non-zero deltas
    /// are rarer by construction and worth the write each.
    ///
    /// An attributed `peer_id` that no longer exists (deleted between the
    /// token check and here) updates the total and silently misses the
    /// friend, which is the right outcome: the bytes still moved.
    pub async fn record_transfer(
        &self,
        peer_id: Option<i64>,
        downloaded: u64,
        uploaded: u64,
    ) -> Result<()> {
        if downloaded == 0 && uploaded == 0 {
            return Ok(());
        }
        let (downloaded, uploaded) = (clamp(downloaded), clamp(uploaded));

        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "UPDATE transfer_totals SET downloaded = downloaded + ?1, uploaded = uploaded + ?2, \
             updated_at = ?3 WHERE id = 1",
        )
        .bind(downloaded)
        .bind(uploaded)
        .bind(now_epoch())
        .execute(&mut *tx)
        .await?;
        if let Some(peer_id) = peer_id {
            sqlx::query(
                "UPDATE peers SET downloaded = downloaded + ?1, uploaded = uploaded + ?2 \
                 WHERE id = ?3",
            )
            .bind(downloaded)
            .bind(uploaded)
            .bind(peer_id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// The instance-wide totals. Always one row, created by the migration.
    pub async fn transfer_totals(&self) -> Result<TransferTotals> {
        let row = sqlx::query(
            "SELECT downloaded, uploaded, updated_at FROM transfer_totals WHERE id = 1",
        )
        .fetch_one(self.pool())
        .await?;
        Ok(TransferTotals {
            downloaded: row.try_get("downloaded")?,
            uploaded: row.try_get("uploaded")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use secrecy::SecretString;

    use super::*;
    use crate::PeerScope;

    #[tokio::test]
    async fn a_fresh_store_has_one_zero_row() {
        let store = Store::open_in_memory().await.unwrap();
        assert_eq!(
            store.transfer_totals().await.unwrap(),
            TransferTotals::default()
        );
    }

    #[tokio::test]
    async fn an_attributed_delta_lands_on_the_friend_and_the_total() {
        let store = Store::open_in_memory().await.unwrap();
        let sam = store
            .create_peer("Sam", &SecretString::from("sam-key"), PeerScope::All)
            .await
            .unwrap();
        assert_eq!((sam.downloaded, sam.uploaded), (0, 0));

        store
            .record_transfer(Some(sam.id), 4_000, 100)
            .await
            .unwrap();
        store.record_transfer(Some(sam.id), 1_000, 0).await.unwrap();

        let sam = store.peer(sam.id).await.unwrap().unwrap();
        assert_eq!((sam.downloaded, sam.uploaded), (5_000, 100));
        let totals = store.transfer_totals().await.unwrap();
        assert_eq!((totals.downloaded, totals.uploaded), (5_000, 100));
        assert!(totals.updated_at.is_some());
    }

    #[tokio::test]
    async fn an_unattributed_delta_reaches_only_the_total() {
        let store = Store::open_in_memory().await.unwrap();
        let sam = store
            .create_peer("Sam", &SecretString::from("sam-key"), PeerScope::All)
            .await
            .unwrap();

        store.record_transfer(None, 0, 2_048).await.unwrap();

        let totals = store.transfer_totals().await.unwrap();
        assert_eq!((totals.downloaded, totals.uploaded), (0, 2_048));
        let sam = store.peer(sam.id).await.unwrap().unwrap();
        assert_eq!((sam.downloaded, sam.uploaded), (0, 0));
    }

    #[tokio::test]
    async fn a_zero_delta_writes_nothing() {
        let store = Store::open_in_memory().await.unwrap();
        store.record_transfer(None, 0, 0).await.unwrap();
        assert_eq!(
            store.transfer_totals().await.unwrap().updated_at,
            None,
            "an empty delta must not stamp updated_at"
        );
    }

    #[tokio::test]
    async fn the_total_survives_deleting_the_friend_it_came_from() {
        let store = Store::open_in_memory().await.unwrap();
        let sam = store
            .create_peer("Sam", &SecretString::from("sam-key"), PeerScope::All)
            .await
            .unwrap();
        store.record_transfer(Some(sam.id), 10, 0).await.unwrap();
        assert!(store.delete_peer(sam.id).await.unwrap());

        assert_eq!(store.transfer_totals().await.unwrap().downloaded, 10);
        // And a delta for a friend who is already gone still counts.
        store.record_transfer(Some(sam.id), 5, 0).await.unwrap();
        assert_eq!(store.transfer_totals().await.unwrap().downloaded, 15);
    }

    #[test]
    fn an_absurd_delta_saturates_rather_than_wrapping() {
        assert_eq!(clamp(u64::MAX), i64::MAX);
        assert_eq!(clamp(7), 7);
    }
}
