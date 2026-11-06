//! What each friend shares with us, as their catalog last listed it.
//!
//! Pulled first-hand from each friend during the gossip exchange and replaced
//! whole, so a row is never older than the last successful pull. Read before a
//! build, to join a friend's swarm for a file we hold too rather than start a
//! second swarm for the same bytes. An item that does join one records where
//! it came from, in [`ItemOrigin`].

use std::collections::HashMap;

use sharerr_core::{CatalogEntry, ExternalIds, MediaSource, MediaSpec};
use sqlx::Row;

use crate::db::{Result, Store, StoreError};

/// Where a torrent sharerr did not build came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemOrigin {
    /// The friend whose swarm the item joined, or `None` for a torrent the
    /// operator already had in the client.
    pub peer_id: Option<i64>,
    /// The tracker tiers that torrent came with, kept in the client's list
    /// after our own whenever it is rewritten. Empty when they were not known
    /// at adoption.
    pub upstream: Vec<String>,
}

impl Store {
    /// Record, or with `None` clear, where an item's torrent came from. An
    /// origin with no upstream tiers keeps the ones already stored for the same
    /// item and source, since an adoption that read sharerr's own cache has
    /// none to give.
    pub async fn set_item_origin(
        &self,
        source: MediaSource,
        file_id: i64,
        origin: Option<&ItemOrigin>,
    ) -> Result<()> {
        let Some(origin) = origin else {
            sqlx::query("DELETE FROM item_origin WHERE source = ?1 AND file_id = ?2")
                .bind(source.as_str())
                .bind(file_id)
                .execute(self.pool())
                .await?;
            return Ok(());
        };
        sqlx::query(
            "INSERT INTO item_origin (source, file_id, peer_id, upstream_json) \
             VALUES (?1, ?2, ?3, ?4) ON CONFLICT (source, file_id) DO UPDATE SET \
             peer_id = excluded.peer_id, upstream_json = CASE \
             WHEN excluded.upstream_json = '[]' AND item_origin.peer_id IS excluded.peer_id \
             THEN item_origin.upstream_json ELSE excluded.upstream_json END",
        )
        .bind(source.as_str())
        .bind(file_id)
        .bind(origin.peer_id)
        .bind(encode(&origin.upstream)?)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// The [`ItemOrigin`] recorded for one item, if one is.
    pub async fn item_origin(
        &self,
        source: MediaSource,
        file_id: i64,
    ) -> Result<Option<ItemOrigin>> {
        let row = sqlx::query(
            "SELECT peer_id, upstream_json FROM item_origin WHERE source = ?1 AND file_id = ?2",
        )
        .bind(source.as_str())
        .bind(file_id)
        .fetch_optional(self.pool())
        .await?;
        row.map(|row| {
            Ok(ItemOrigin {
                peer_id: row.try_get("peer_id")?,
                upstream: serde_json::from_str(row.try_get::<&str, _>("upstream_json")?)
                    .unwrap_or_default(),
            })
        })
        .transpose()
    }

    /// Every recorded [`ItemOrigin`], by item. A row this version cannot decode
    /// reads as having no upstream tiers.
    pub async fn item_origins(&self) -> Result<HashMap<(MediaSource, i64), ItemOrigin>> {
        let rows = sqlx::query("SELECT source, file_id, peer_id, upstream_json FROM item_origin")
            .fetch_all(self.pool())
            .await?;
        let mut origins = HashMap::with_capacity(rows.len());
        for row in &rows {
            let Some(source) = MediaSource::parse(row.try_get::<&str, _>("source")?) else {
                continue;
            };
            let upstream =
                serde_json::from_str(row.try_get::<&str, _>("upstream_json")?).unwrap_or_default();
            origins.insert(
                (source, row.try_get("file_id")?),
                ItemOrigin {
                    peer_id: row.try_get("peer_id")?,
                    upstream,
                },
            );
        }
        Ok(origins)
    }

    /// Replace everything recorded for `peer_id`'s catalog with `entries`, in
    /// one transaction, so a reader never sees half of one pull.
    pub async fn replace_peer_catalog(&self, peer_id: i64, entries: &[CatalogEntry]) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("DELETE FROM peer_catalog WHERE peer_id = ?1")
            .bind(peer_id)
            .execute(&mut *tx)
            .await?;
        for entry in entries {
            sqlx::query(
                "INSERT OR REPLACE INTO peer_catalog \
                 (peer_id, info_hash, spec_json, ids_json, size, private, download_url) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )
            .bind(peer_id)
            .bind(entry.info_hash.to_ascii_lowercase())
            .bind(encode(&entry.spec)?)
            .bind(encode(&entry.ids)?)
            .bind(sharerr_core::saturating_i64(entry.size))
            .bind(i64::from(entry.private))
            .bind(&entry.download_url)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Every entry an active friend lists that [`CatalogEntry::matches`] this
    /// file, with the friend's peer id, oldest friendship first so the choice
    /// between two friends sharing one file is stable.
    ///
    /// A row this version cannot decode is skipped rather than failing the
    /// lookup: the caller falls back to building its own torrent either way.
    pub async fn catalog_matches(
        &self,
        spec: &MediaSpec,
        ids: &ExternalIds,
        size: u64,
    ) -> Result<Vec<(i64, CatalogEntry)>> {
        let rows = sqlx::query(
            "SELECT c.peer_id, c.info_hash, c.spec_json, c.ids_json, c.size, c.private, \
             c.download_url FROM peer_catalog c JOIN peers p ON p.id = c.peer_id \
             WHERE c.size = ?1 AND p.revoked_at IS NULL ORDER BY p.id, c.info_hash",
        )
        .bind(sharerr_core::saturating_i64(size))
        .fetch_all(self.pool())
        .await?;

        let mut matched = Vec::new();
        for row in &rows {
            let (Ok(spec_json), Ok(ids_json)) = (
                row.try_get::<&str, _>("spec_json"),
                row.try_get::<&str, _>("ids_json"),
            ) else {
                continue;
            };
            let (Ok(entry_spec), Ok(entry_ids)) = (
                serde_json::from_str::<MediaSpec>(spec_json),
                serde_json::from_str::<ExternalIds>(ids_json),
            ) else {
                continue;
            };
            let entry = CatalogEntry {
                info_hash: row.try_get("info_hash")?,
                spec: entry_spec,
                ids: entry_ids,
                size: u64::try_from(row.try_get::<i64, _>("size")?).unwrap_or_default(),
                private: row.try_get::<i64, _>("private")? != 0,
                download_url: row.try_get("download_url")?,
            };
            if entry.matches(spec, ids, size) {
                matched.push((row.try_get("peer_id")?, entry));
            }
        }
        Ok(matched)
    }

    /// How many entries each friend's last catalog pull listed, by peer id.
    pub async fn peer_catalog_sizes(&self) -> Result<std::collections::HashMap<i64, i64>> {
        let rows = sqlx::query("SELECT peer_id, COUNT(*) AS n FROM peer_catalog GROUP BY peer_id")
            .fetch_all(self.pool())
            .await?;
        rows.iter()
            .map(|row| Ok((row.try_get("peer_id")?, row.try_get("n")?)))
            .collect()
    }
}

fn encode(value: &impl serde::Serialize) -> Result<String> {
    serde_json::to_string(value).map_err(|e| StoreError::Malformed {
        id: 0,
        detail: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use secrecy::SecretString;

    use super::*;
    use crate::PeerScope;

    fn episode(n: u32) -> MediaSpec {
        MediaSpec::Episode {
            series_title: "Harbour Lights".to_owned(),
            season: 2,
            episode: n,
        }
    }

    fn ids() -> ExternalIds {
        ExternalIds {
            tvdb: Some(77),
            ..ExternalIds::default()
        }
    }

    fn entry(hash: char, n: u32, size: u64) -> CatalogEntry {
        CatalogEntry {
            info_hash: hash.to_string().repeat(40),
            spec: episode(n),
            ids: ids(),
            size,
            private: false,
            download_url: format!("http://friend.example/torrents/{hash}.torrent"),
        }
    }

    async fn store_with_friend(label: &str) -> (Store, i64) {
        let store = Store::open_in_memory().await.unwrap();
        let peer = store
            .create_peer(
                label,
                &SecretString::from(format!("{label}-key")),
                PeerScope::All,
            )
            .await
            .unwrap();
        (store, peer.id)
    }

    #[tokio::test]
    async fn a_matching_entry_is_found_by_size_ids_and_slot() {
        let (store, sam) = store_with_friend("Sam").await;
        store
            .replace_peer_catalog(sam, &[entry('a', 4, 100), entry('b', 5, 100)])
            .await
            .unwrap();

        let found = store
            .catalog_matches(&episode(4), &ids(), 100)
            .await
            .unwrap();
        assert_eq!(found, vec![(sam, entry('a', 4, 100))]);
        assert!(
            store
                .catalog_matches(&episode(4), &ids(), 101)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store.peer_catalog_sizes().await.unwrap().get(&sam),
            Some(&2)
        );
    }

    #[tokio::test]
    async fn a_pull_replaces_the_last_one_whole() {
        let (store, sam) = store_with_friend("Sam").await;
        store
            .replace_peer_catalog(sam, &[entry('a', 4, 100)])
            .await
            .unwrap();
        store
            .replace_peer_catalog(sam, &[entry('c', 6, 100)])
            .await
            .unwrap();
        assert!(
            store
                .catalog_matches(&episode(4), &ids(), 100)
                .await
                .unwrap()
                .is_empty(),
            "an entry the latest pull dropped is gone"
        );
        store.replace_peer_catalog(sam, &[]).await.unwrap();
        assert!(store.peer_catalog_sizes().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_revoked_or_deleted_friends_catalog_is_not_offered() {
        let (store, sam) = store_with_friend("Sam").await;
        store
            .replace_peer_catalog(sam, &[entry('a', 4, 100)])
            .await
            .unwrap();
        store.revoke_peer(sam).await.unwrap();
        assert!(
            store
                .catalog_matches(&episode(4), &ids(), 100)
                .await
                .unwrap()
                .is_empty()
        );

        assert!(store.delete_peer(sam).await.unwrap());
        assert!(
            store.peer_catalog_sizes().await.unwrap().is_empty(),
            "deleting the friend cascades to their catalog"
        );
    }

    #[tokio::test]
    async fn an_origin_keeps_its_tiers_until_a_new_friend_or_a_rebuild_replaces_them() {
        let (store, sam) = store_with_friend("Sam").await;
        let key = (MediaSource::Sonarr, 1001);
        let origin = |peer_id: i64, upstream: &[&str]| ItemOrigin {
            peer_id: Some(peer_id),
            upstream: upstream.iter().map(|s| (*s).to_owned()).collect(),
        };
        store
            .set_item_origin(key.0, key.1, Some(&origin(sam, &["http://sam/announce"])))
            .await
            .unwrap();
        store
            .set_item_origin(key.0, key.1, Some(&origin(sam, &[])))
            .await
            .unwrap();
        assert_eq!(
            store.item_origins().await.unwrap().get(&key),
            Some(&origin(sam, &["http://sam/announce"])),
            "an adoption with no tiers to give keeps the stored ones"
        );

        let alex = store
            .create_peer("Alex", &SecretString::from("alex-key"), PeerScope::All)
            .await
            .unwrap()
            .id;
        store
            .set_item_origin(key.0, key.1, Some(&origin(alex, &[])))
            .await
            .unwrap();
        assert_eq!(
            store.item_origins().await.unwrap().get(&key),
            Some(&origin(alex, &[])),
            "another friend's swarm does not inherit the first one's tracker"
        );

        assert_eq!(
            store.item_origin(key.0, key.1).await.unwrap(),
            Some(origin(alex, &[]))
        );
        // The operator's own torrent: no friend, and the same keep-the-tiers
        // rule between two adoptions of it.
        let operators = |upstream: &[&str]| ItemOrigin {
            peer_id: None,
            upstream: upstream.iter().map(|s| (*s).to_owned()).collect(),
        };
        for (written, expected) in [
            (
                operators(&["http://theirs/announce"]),
                operators(&["http://theirs/announce"]),
            ),
            (operators(&[]), operators(&["http://theirs/announce"])),
        ] {
            store
                .set_item_origin(key.0, key.1, Some(&written))
                .await
                .unwrap();
            assert_eq!(
                store.item_origin(key.0, key.1).await.unwrap(),
                Some(expected)
            );
        }

        store.reset_for_rebuild(key.0, key.1).await.unwrap();
        assert!(store.item_origins().await.unwrap().is_empty());
        assert_eq!(store.item_origin(key.0, key.1).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_row_this_version_cannot_decode_is_skipped() {
        let (store, sam) = store_with_friend("Sam").await;
        store
            .replace_peer_catalog(sam, &[entry('a', 4, 100), entry('b', 4, 100)])
            .await
            .unwrap();
        sqlx::query("UPDATE peer_catalog SET spec_json = '{\"kind\":\"podcast\"}' WHERE info_hash LIKE 'a%'")
            .execute(store.pool())
            .await
            .unwrap();
        let found = store
            .catalog_matches(&episode(4), &ids(), 100)
            .await
            .unwrap();
        assert_eq!(found, vec![(sam, entry('b', 4, 100))]);
    }
}
