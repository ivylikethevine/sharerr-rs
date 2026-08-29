//! Where each friend has recently been seen.
//!
//! A peer row is a credential and carries no address; this module is the memory
//! beside it. Each observation is `(kind, addr, when, how)`, with the API, the
//! torrent client, and the tracker recorded **separately** — a friend on a
//! dual-VPN setup has them behind different exits while both belong to one
//! sharerr. The history is short and newest-first: a reconnect that briefly
//! returns an old exit is remembered rather than trusted.

use sharerr_core::endpoint::{MAX_FUTURE_SKEW_SECS, now_epoch};
use sqlx::Row;

use crate::db::{Store, StoreError};

type Result<T> = std::result::Result<T, StoreError>;

/// How many addresses are kept per `(peer, kind)`.
///
/// Enough to span a few of the friend's reconnects; more would be a location
/// log, which is precisely what this table must not become.
const MAX_HISTORY: usize = 5;

/// Which of a friend's addresses an observation describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointKind {
    /// The source address of an authenticated feed or gossip request.
    Api,
    /// Their torrent client, as their own gossip reports it.
    Client,
    /// Their sharerr's announce/feed endpoint, as their own gossip reports it.
    Tracker,
}

sharerr_core::str_enum!(EndpointKind {
    Api => "api",
    Client => "client",
    Tracker => "tracker",
});

/// How an observation arrived.
///
/// Trust order, most to least: [`Self::Direct`] (we saw the connection
/// ourselves) > [`Self::Gossip`] (a mutual friend relayed the subject's own
/// signed record) > [`Self::Lighthouse`] (a semi-anonymous rendezvous
/// service answered a lookup) > [`Self::Restored`] (an operator typed it
/// into a `[[peers]]` bootstrap block, with no way to say how stale it
/// already was) — the lighthouse is the fallback for when gossip already had
/// no path back to the peer, and a restored address is the fallback for when
/// nothing has been observed at all since the friend was re-added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservedVia {
    /// We saw the connection ourselves.
    Direct,
    /// A record the subject signed, relayed by a mutual friend.
    Gossip,
    /// A record the subject signed, retrieved from a lighthouse.
    Lighthouse,
    /// Seeded from a `[[peers]]` bootstrap block — see
    /// `sharerr_core::config::PeerImport`. Least trusted: sharerr never saw
    /// this address itself, and has no idea how long ago it was current.
    Restored,
}

sharerr_core::str_enum!(
    ObservedVia {
        Direct => "direct",
        Gossip => "gossip",
        Lighthouse => "lighthouse",
        Restored => "restored",
    },
    lenient = Restored,
    "Anything unrecognised reads as restored — the *least* trusted rank, \
     which is the safe direction for a value a newer version may have \
     written."
);

/// One sighting of one of a friend's addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerEndpoint {
    pub kind: EndpointKind,
    /// `host:port`, or a URL for [`EndpointKind::Tracker`]. Stored as text —
    /// the store does not interpret it, only remembers it.
    pub addr: String,
    /// Unix seconds of the most recent observation.
    pub observed_at: i64,
    pub via: ObservedVia,
}

/// Decode one `peer_endpoints` row into a [`PeerEndpoint`] — shared by
/// [`Store::peer_endpoints`] and [`Store::peer_endpoints_for`], which read
/// the same four columns and skip a row the same way on a decode failure.
fn row_to_endpoint(row: &sqlx::sqlite::SqliteRow) -> Option<PeerEndpoint> {
    Some(PeerEndpoint {
        // A kind written by a newer version is skipped, not an error.
        kind: EndpointKind::parse(row.try_get::<&str, _>("kind").ok()?)?,
        addr: row.try_get("addr").ok()?,
        observed_at: row.try_get("observed_at").ok()?,
        via: ObservedVia::parse(row.try_get::<&str, _>("via").ok()?),
    })
}

impl Store {
    /// Record a sighting, refreshing the timestamp if the address is already
    /// known and pruning the `(peer, kind)` history to the newest few.
    ///
    /// An observation older than what is already recorded for the same address
    /// is ignored — that is the property that stops a replayed or slow-travelled
    /// gossip record from rewinding what a fresher one established.
    ///
    /// `observed_at` is `None` for a sighting with no real timestamp attached
    /// — [`ObservedVia::Restored`]'s own doc comment is explicit that "no
    /// idea how stale this already was" is the whole point of the variant, so
    /// a caller with nothing real to report cannot honestly claim "now" just
    /// to have something to pass. `None` therefore only ever *fills a gap*:
    /// it inserts a first sighting of an address this method has never seen,
    /// but never touches a row that already exists, since there is no
    /// timestamp here to compare against whatever produced that row. Without
    /// this, a restored peer's fabricated "now" could out-rank — and
    /// silently downgrade the recorded `via` of — a genuine `Direct` sighting
    /// of the same address.
    pub async fn record_peer_endpoint(
        &self,
        peer_id: i64,
        kind: EndpointKind,
        addr: &str,
        observed_at: Option<i64>,
        via: ObservedVia,
    ) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        let written = match observed_at {
            Some(observed_at) => {
                // Clamped, not rejected: an observation is still worth keeping
                // even from a peer whose clock runs fast, but an unclamped
                // future timestamp would win every `excluded.observed_at > …`
                // comparison below forever, past the point the sender's clock
                // is fixed — see `MAX_FUTURE_SKEW_SECS`.
                let observed_at = observed_at.min(now_epoch().saturating_add(MAX_FUTURE_SKEW_SECS));
                sqlx::query(
                    "INSERT INTO peer_endpoints (peer_id, kind, addr, observed_at, via) \
                     VALUES (?1, ?2, ?3, ?4, ?5) \
                     ON CONFLICT (peer_id, kind, addr) DO UPDATE \
                     SET observed_at = excluded.observed_at, via = excluded.via \
                     WHERE excluded.observed_at > peer_endpoints.observed_at",
                )
                .bind(peer_id)
                .bind(kind.as_str())
                .bind(addr)
                .bind(observed_at)
                .bind(via.as_str())
                .execute(&mut *tx)
                .await?
                .rows_affected()
            }
            None => sqlx::query(
                "INSERT INTO peer_endpoints (peer_id, kind, addr, observed_at, via) \
                     VALUES (?1, ?2, ?3, ?4, ?5) \
                     ON CONFLICT (peer_id, kind, addr) DO NOTHING",
            )
            .bind(peer_id)
            .bind(kind.as_str())
            .bind(addr)
            .bind(now_epoch())
            .bind(via.as_str())
            .execute(&mut *tx)
            .await?
            .rows_affected(),
        };

        // Prune beyond the newest MAX_HISTORY. Done here rather than on a timer:
        // inserts are the only thing that grows the table — so an upsert that
        // changed nothing (an older sighting of a known address) has nothing
        // to prune either.
        if written > 0 {
            sqlx::query(
                "DELETE FROM peer_endpoints WHERE peer_id = ?1 AND kind = ?2 AND id NOT IN \
                 (SELECT id FROM peer_endpoints WHERE peer_id = ?1 AND kind = ?2 \
                  ORDER BY observed_at DESC, id DESC LIMIT ?3)",
            )
            .bind(peer_id)
            .bind(kind.as_str())
            .bind(MAX_HISTORY as i64)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(())
    }

    /// Every recorded sighting for one peer, newest first within each kind.
    pub async fn peer_endpoints(&self, peer_id: i64) -> Result<Vec<PeerEndpoint>> {
        let rows = sqlx::query(
            "SELECT kind, addr, observed_at, via FROM peer_endpoints \
             WHERE peer_id = ?1 ORDER BY kind, observed_at DESC, id DESC",
        )
        .bind(peer_id)
        .fetch_all(self.pool())
        .await?;

        Ok(rows.iter().filter_map(row_to_endpoint).collect())
    }

    /// [`Self::peer_endpoints`] for every id in `peer_ids`, one round trip
    /// instead of one per friend.
    ///
    /// The Friends page and the topology view both render every friend's
    /// history on one load, and both used to `join_all` a `peer_endpoints`
    /// call per friend — concurrent, but still one connection and one query
    /// plan checked out per row of a page that is people, not rows. A peer
    /// with no recorded sighting is simply absent from the returned map
    /// rather than present with an empty `Vec`; callers already reach for
    /// this with `.unwrap_or_default()` either way.
    pub async fn peer_endpoints_for(
        &self,
        peer_ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, Vec<PeerEndpoint>>> {
        if peer_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let placeholders = vec!["?"; peer_ids.len()].join(", ");
        let sql = format!(
            "SELECT peer_id, kind, addr, observed_at, via FROM peer_endpoints \
             WHERE peer_id IN ({placeholders}) ORDER BY peer_id, kind, observed_at DESC, id DESC"
        );
        let mut query = sqlx::query(&sql);
        for id in peer_ids {
            query = query.bind(id);
        }
        let rows = query.fetch_all(self.pool()).await?;

        let mut by_peer: std::collections::HashMap<i64, Vec<PeerEndpoint>> =
            std::collections::HashMap::new();
        for row in &rows {
            let Ok(peer_id) = row.try_get::<i64, _>("peer_id") else {
                continue;
            };
            if let Some(endpoint) = row_to_endpoint(row) {
                by_peer.entry(peer_id).or_default().push(endpoint);
            }
        }
        Ok(by_peer)
    }

    /// Bind a peer to their gossip identity, trust-on-first-use.
    ///
    /// Returns `false` when the peer already has a *different* pubkey — that is
    /// a conflict the caller must refuse, not overwrite: a peer's identity does
    /// not change, and a second key presented over the same API key is either a
    /// reinstall (the operator re-pairs by clearing it) or an impersonation.
    pub async fn bind_peer_pubkey(&self, peer_id: i64, pubkey: &str) -> Result<bool> {
        let affected = sqlx::query(
            "UPDATE peers SET pubkey = ?2 WHERE id = ?1 AND (pubkey IS NULL OR pubkey = ?2)",
        )
        .bind(peer_id)
        .bind(pubkey)
        .execute(self.pool())
        .await?
        .rows_affected();
        Ok(affected > 0)
    }

    /// Set or clear where this friend's own sharerr can be pulled from. Blank
    /// or absent clears it; anything else must parse as a URL, and the
    /// *normalized* form (`url::Url`'s own `Display`) is what gets stored,
    /// not the caller's raw text. This is the one place that validates and
    /// normalizes a gossip URL — `web::peers::set_gossip` and the
    /// `[[peers]]` bootstrap importer both go through it rather than each
    /// carrying their own copy of the same `url::Url::parse` call.
    pub async fn set_peer_gossip_url(&self, peer_id: i64, url: Option<&str>) -> Result<bool> {
        let url = match url.map(str::trim) {
            None | Some("") => None,
            Some(raw) => Some(
                url::Url::parse(raw)
                    .map_err(|source| StoreError::InvalidGossipUrl {
                        value: raw.to_owned(),
                        source,
                    })?
                    .to_string(),
            ),
        };
        let affected = sqlx::query("UPDATE peers SET gossip_url = ?2 WHERE id = ?1")
            .bind(peer_id)
            .bind(url)
            .execute(self.pool())
            .await?
            .rows_affected();
        Ok(affected > 0)
    }

    /// Keep the peer's latest verified self-record, exactly as signed, so it can
    /// be relayed to mutual friends without touching a byte of it.
    pub async fn set_peer_gossip_record(&self, peer_id: i64, record: &str) -> Result<()> {
        sqlx::query("UPDATE peers SET gossip_record = ?2 WHERE id = ?1")
            .bind(peer_id)
            .bind(record)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// The stored raw self-record for one peer, if any.
    pub async fn peer_gossip_record(&self, peer_id: i64) -> Result<Option<String>> {
        let row = sqlx::query("SELECT gossip_record FROM peers WHERE id = ?1")
            .bind(peer_id)
            .fetch_optional(self.pool())
            .await?;
        Ok(row.and_then(|row| row.try_get("gossip_record").ok()))
    }

    /// [`Self::peer_gossip_record`] for every id in `peer_ids`, one round
    /// trip instead of one per matched peer. The gossip relay endpoint used
    /// to await this sequentially inside a `for` loop over every peer the
    /// caller named — worse than the friends-list pages' at least-concurrent
    /// `join_all`, since it paid each round trip one after another.
    /// A peer with no stored record (or an id absent from `peer_ids`) is
    /// simply absent from the returned map.
    pub async fn peer_gossip_records(
        &self,
        peer_ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, String>> {
        if peer_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let placeholders = vec!["?"; peer_ids.len()].join(", ");
        let sql = format!(
            "SELECT id, gossip_record FROM peers \
             WHERE id IN ({placeholders}) AND gossip_record IS NOT NULL"
        );
        let mut query = sqlx::query(&sql);
        for id in peer_ids {
            query = query.bind(id);
        }
        let rows = query.fetch_all(self.pool()).await?;

        Ok(rows
            .iter()
            .filter_map(|row| {
                let id: i64 = row.try_get("id").ok()?;
                let record: String = row.try_get("gossip_record").ok()?;
                Some((id, record))
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::peers::PeerScope;
    use secrecy::SecretString;

    async fn store_with_peer() -> (Store, i64) {
        let store = Store::open_in_memory().await.unwrap();
        let peer = store
            .create_peer("Sam", &SecretString::from("sam-key"), PeerScope::All)
            .await
            .unwrap();
        (store, peer.id)
    }

    #[tokio::test]
    async fn sightings_are_recorded_newest_first_and_deduplicated() {
        let (store, peer) = store_with_peer().await;

        store
            .record_peer_endpoint(
                peer,
                EndpointKind::Api,
                "203.0.113.5:51413",
                Some(100),
                ObservedVia::Direct,
            )
            .await
            .unwrap();
        store
            .record_peer_endpoint(
                peer,
                EndpointKind::Api,
                "203.0.113.9:51413",
                Some(200),
                ObservedVia::Direct,
            )
            .await
            .unwrap();
        // The same address again, later: refreshed, not duplicated.
        store
            .record_peer_endpoint(
                peer,
                EndpointKind::Api,
                "203.0.113.5:51413",
                Some(300),
                ObservedVia::Direct,
            )
            .await
            .unwrap();

        let endpoints = store.peer_endpoints(peer).await.unwrap();
        assert_eq!(
            endpoints
                .iter()
                .map(|e| (e.addr.as_str(), e.observed_at))
                .collect::<Vec<_>>(),
            vec![("203.0.113.5:51413", 300), ("203.0.113.9:51413", 200)]
        );
    }

    /// The replay defence: an observation cannot rewind a fresher one.
    #[tokio::test]
    async fn an_older_sighting_does_not_overwrite_a_newer_one() {
        let (store, peer) = store_with_peer().await;

        store
            .record_peer_endpoint(
                peer,
                EndpointKind::Api,
                "203.0.113.5:1",
                Some(500),
                ObservedVia::Direct,
            )
            .await
            .unwrap();
        store
            .record_peer_endpoint(
                peer,
                EndpointKind::Api,
                "203.0.113.5:1",
                Some(100),
                ObservedVia::Gossip,
            )
            .await
            .unwrap();

        let endpoints = store.peer_endpoints(peer).await.unwrap();
        assert_eq!(endpoints[0].observed_at, 500);
        assert_eq!(endpoints[0].via, ObservedVia::Direct);
    }

    /// `None` — a restored sighting with no real timestamp — must never
    /// overwrite a row that already exists, since it has nothing to compare
    /// against. Before this, the caller had to fabricate `now_epoch()` to
    /// call this method at all, which could out-rank (and silently
    /// downgrade the `via` of) a genuine, older `Direct` sighting of the
    /// same address.
    #[tokio::test]
    async fn a_restored_sighting_with_no_timestamp_never_overwrites_an_existing_row() {
        let (store, peer) = store_with_peer().await;

        store
            .record_peer_endpoint(
                peer,
                EndpointKind::Api,
                "203.0.113.5:1",
                Some(100),
                ObservedVia::Direct,
            )
            .await
            .unwrap();
        store
            .record_peer_endpoint(
                peer,
                EndpointKind::Api,
                "203.0.113.5:1",
                None,
                ObservedVia::Restored,
            )
            .await
            .unwrap();

        let endpoints = store.peer_endpoints(peer).await.unwrap();
        assert_eq!(endpoints[0].observed_at, 100);
        assert_eq!(
            endpoints[0].via,
            ObservedVia::Direct,
            "a restored sighting with no timestamp must not downgrade a genuine one"
        );
    }

    /// The other half: `None` still inserts a first sighting of an address
    /// this method has never seen — it only refuses to *overwrite*.
    #[tokio::test]
    async fn a_restored_sighting_with_no_timestamp_still_records_a_new_address() {
        let (store, peer) = store_with_peer().await;

        store
            .record_peer_endpoint(
                peer,
                EndpointKind::Api,
                "203.0.113.5:1",
                None,
                ObservedVia::Restored,
            )
            .await
            .unwrap();

        let endpoints = store.peer_endpoints(peer).await.unwrap();
        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0].addr, "203.0.113.5:1");
        assert_eq!(endpoints[0].via, ObservedVia::Restored);
    }

    /// A wildly future `observed_at` — a sender's clock set wrong — is
    /// clamped rather than trusted outright: an unclamped one would win every
    /// later comparison forever, past the point the sender's clock is fixed.
    #[tokio::test]
    async fn a_far_future_observed_at_is_clamped() {
        let (store, peer) = store_with_peer().await;
        let far_future = now_epoch() + MAX_FUTURE_SKEW_SECS + 3600;

        store
            .record_peer_endpoint(
                peer,
                EndpointKind::Api,
                "203.0.113.5:1",
                Some(far_future),
                ObservedVia::Direct,
            )
            .await
            .unwrap();

        let endpoints = store.peer_endpoints(peer).await.unwrap();
        assert!(
            endpoints[0].observed_at < far_future,
            "expected the timestamp to be clamped, got {}",
            endpoints[0].observed_at
        );
        assert!(endpoints[0].observed_at <= now_epoch() + MAX_FUTURE_SKEW_SECS);
    }

    /// The dual-VPN case the schema exists for: the API and the client behind
    /// different exits, neither overwriting the other.
    #[tokio::test]
    async fn kinds_are_recorded_separately() {
        let (store, peer) = store_with_peer().await;

        store
            .record_peer_endpoint(
                peer,
                EndpointKind::Api,
                "203.0.113.5:8477",
                Some(100),
                ObservedVia::Direct,
            )
            .await
            .unwrap();
        store
            .record_peer_endpoint(
                peer,
                EndpointKind::Client,
                "198.51.100.7:6881",
                Some(100),
                ObservedVia::Gossip,
            )
            .await
            .unwrap();

        let endpoints = store.peer_endpoints(peer).await.unwrap();
        assert_eq!(endpoints.len(), 2);
        assert!(endpoints.iter().any(|e| e.kind == EndpointKind::Api));
        assert!(endpoints.iter().any(|e| e.kind == EndpointKind::Client));
    }

    #[tokio::test]
    async fn the_history_is_bounded_per_kind() {
        let (store, peer) = store_with_peer().await;

        for i in 0..20i64 {
            store
                .record_peer_endpoint(
                    peer,
                    EndpointKind::Api,
                    &format!("203.0.113.5:{}", 1000 + i),
                    Some(i),
                    ObservedVia::Direct,
                )
                .await
                .unwrap();
        }

        let endpoints = store.peer_endpoints(peer).await.unwrap();
        assert_eq!(endpoints.len(), MAX_HISTORY);
        assert_eq!(endpoints[0].observed_at, 19, "the newest must survive");
    }

    /// TOFU: the first pubkey binds, the same one re-binds harmlessly, a
    /// different one is refused rather than replacing the identity.
    #[tokio::test]
    async fn a_peers_pubkey_binds_once() {
        let (store, peer) = store_with_peer().await;

        assert!(store.bind_peer_pubkey(peer, "aa11").await.unwrap());
        assert!(store.bind_peer_pubkey(peer, "aa11").await.unwrap());
        assert!(
            !store.bind_peer_pubkey(peer, "bb22").await.unwrap(),
            "a second identity over the same key must be refused"
        );

        let peers = store.list_peers().await.unwrap();
        assert_eq!(peers[0].pubkey.as_deref(), Some("aa11"));
    }

    #[test]
    fn every_observed_via_round_trips_and_unknown_values_default_to_restored() {
        for via in [
            ObservedVia::Direct,
            ObservedVia::Gossip,
            ObservedVia::Lighthouse,
            ObservedVia::Restored,
        ] {
            assert_eq!(ObservedVia::parse(via.as_str()), via);
        }
        assert_eq!(ObservedVia::parse("carrier-pigeon"), ObservedVia::Restored);
    }

    #[tokio::test]
    async fn deleting_a_peer_takes_their_endpoint_history_with_them() {
        let (store, peer) = store_with_peer().await;
        store
            .record_peer_endpoint(
                peer,
                EndpointKind::Api,
                "203.0.113.5:1",
                Some(100),
                ObservedVia::Direct,
            )
            .await
            .unwrap();

        store.delete_peer(peer).await.unwrap();

        let orphans: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM peer_endpoints")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(orphans, 0, "endpoint rows must not outlive their peer");
    }

    /// The bulk form must group strictly by peer — one friend's sightings
    /// must never leak into another's entry, a peer with no sightings must
    /// be simply absent (not present with an empty `Vec`), and the result
    /// must match calling the single-peer form for each id.
    #[tokio::test]
    async fn peer_endpoints_for_groups_strictly_by_peer() {
        let store = Store::open_in_memory().await.unwrap();
        let sam = store
            .create_peer("Sam", &SecretString::from("sam-key"), PeerScope::All)
            .await
            .unwrap();
        let alex = store
            .create_peer("Alex", &SecretString::from("alex-key"), PeerScope::All)
            .await
            .unwrap();
        // Blair is created but never sighted — must not appear in the map.
        let blair = store
            .create_peer("Blair", &SecretString::from("blair-key"), PeerScope::All)
            .await
            .unwrap();

        store
            .record_peer_endpoint(
                sam.id,
                EndpointKind::Api,
                "203.0.113.5:1",
                Some(100),
                ObservedVia::Direct,
            )
            .await
            .unwrap();
        store
            .record_peer_endpoint(
                alex.id,
                EndpointKind::Client,
                "203.0.113.9:2",
                Some(200),
                ObservedVia::Gossip,
            )
            .await
            .unwrap();

        let bulk = store
            .peer_endpoints_for(&[sam.id, alex.id, blair.id])
            .await
            .unwrap();

        assert_eq!(bulk.len(), 2, "blair has no sightings and must be absent");
        assert_eq!(
            bulk[&sam.id],
            store.peer_endpoints(sam.id).await.unwrap(),
            "must match the single-peer form"
        );
        assert_eq!(
            bulk[&alex.id],
            store.peer_endpoints(alex.id).await.unwrap(),
            "must match the single-peer form"
        );
        assert!(!bulk.contains_key(&blair.id));
    }

    #[tokio::test]
    async fn peer_endpoints_for_an_empty_list_is_an_empty_map_not_every_peer() {
        let (store, peer) = store_with_peer().await;
        store
            .record_peer_endpoint(
                peer,
                EndpointKind::Api,
                "203.0.113.5:1",
                Some(100),
                ObservedVia::Direct,
            )
            .await
            .unwrap();

        let bulk = store.peer_endpoints_for(&[]).await.unwrap();
        assert!(bulk.is_empty());
    }

    /// Same grouping guarantee as `peer_endpoints_for`, for gossip records:
    /// strictly by peer, a peer with no stored record simply absent.
    #[tokio::test]
    async fn peer_gossip_records_groups_strictly_by_peer() {
        let store = Store::open_in_memory().await.unwrap();
        let sam = store
            .create_peer("Sam", &SecretString::from("sam-key"), PeerScope::All)
            .await
            .unwrap();
        let alex = store
            .create_peer("Alex", &SecretString::from("alex-key"), PeerScope::All)
            .await
            .unwrap();
        // Blair has no gossip record — must not appear in the map.
        let blair = store
            .create_peer("Blair", &SecretString::from("blair-key"), PeerScope::All)
            .await
            .unwrap();

        store
            .set_peer_gossip_record(sam.id, "sam-record")
            .await
            .unwrap();
        store
            .set_peer_gossip_record(alex.id, "alex-record")
            .await
            .unwrap();

        let bulk = store
            .peer_gossip_records(&[sam.id, alex.id, blair.id])
            .await
            .unwrap();

        assert_eq!(bulk.len(), 2, "blair has no record and must be absent");
        assert_eq!(bulk[&sam.id], "sam-record");
        assert_eq!(bulk[&alex.id], "alex-record");
        assert!(!bulk.contains_key(&blair.id));
    }
}
