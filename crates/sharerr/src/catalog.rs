//! Each friend's catalog: what their feed would list for us, as data rather
//! than a Torznab document.
//!
//! When two friends hold the same file, a torrent each built is two swarms for
//! the same bytes. The catalog is what lets the second one join the first's
//! swarm instead: [`crate::sync::seed::Seeder`] looks for a matching entry
//! before building. Served over the same per-peer-key authentication as the
//! feed and filtered the same way, so it shows nothing the feed would not.
//!
//! Pulled first-hand during the gossip exchange and never relayed, so unlike a
//! gossip record it carries no signature: the friend's own answer over the key
//! they issued us is the proof.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use sharerr_client::error_chain;
use sharerr_core::{CatalogEntry, ExternalIds, MediaSpec};
use sharerr_store::Store;

use crate::state::ServeState;
use crate::torznab::{Caller, SearchQuery, encode_component};

/// Cap on entries taken from one friend's pull.
const MAX_ENTRIES: usize = 50_000;

/// The catalog as it travels.
#[derive(Debug, Default, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Catalog {
    /// The caller's own token, for the `token` parameter of a `.torrent`
    /// download, as the feed's links carry it. Absent when this instance has
    /// no tracker token, so there is nothing to attribute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// Kept as raw JSON on the way in, so one entry of a kind this version does
    /// not know is skipped rather than failing the whole pull.
    #[schema(value_type = Vec<CatalogItem>)]
    pub entries: Vec<serde_json::Value>,
}

/// One torrent this instance seeds and lets the caller see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CatalogItem {
    /// The v1 info hash, lowercase hex. The `.torrent` is at
    /// `/torrents/{info_hash}.torrent`.
    pub info_hash: String,
    /// What the file is: `kind` (`episode`, `movie`, `track` or `book`) and
    /// that kind's fields, the same shape the management API's items carry.
    #[schema(value_type = Object)]
    pub spec: MediaSpec,
    /// External ids: `tvdb`, `tmdb`, `tvmaze`, `imdb`, `musicbrainz`,
    /// `goodreads`, `hardcover`, `isbn`.
    #[schema(value_type = Object)]
    pub ids: ExternalIds,
    /// Total content size in bytes.
    pub size: u64,
    /// Whether the torrent sets BEP 27's private flag.
    pub private: bool,
}

/// `GET /api/catalog` — what this instance seeds that the caller may see.
#[utoipa::path(
    get,
    path = "/api/catalog",
    tag = "gossip",
    operation_id = "catalog",
    security(("peerApiKey" = [])),
    responses(
        (status = 200, description =
         "Every torrent the caller's feed would list, as data: the same scope, \
          audience and limits.", body = Catalog),
        (status = 401, content_type = "application/xml",
         description = crate::torznab::NO_ACTIVE_PEER, body = String),
        (status = 503, description = "The database is not open yet.", body = String),
    ),
)]
pub async fn catalog(State(state): State<Arc<ServeState>>, caller: Caller) -> Response {
    let query = SearchQuery {
        t: "search".to_owned(),
        ..SearchQuery::default()
    };
    let matched = match crate::torznab::collect(
        &state,
        &query,
        caller.scope(),
        caller.limits(),
        caller.key_hash(),
    )
    .await
    {
        Ok(matched) => matched,
        Err((status, reason)) => return (status, reason).into_response(),
    };

    let entries = matched
        .items
        .iter()
        .filter_map(|item| {
            let item = CatalogItem {
                info_hash: item.info_hash.clone()?.to_ascii_lowercase(),
                spec: item.spec.clone(),
                ids: item.ids.clone(),
                size: item.size,
                private: item.private,
            };
            serde_json::to_value(item).ok()
        })
        .collect();
    axum::Json(Catalog {
        token: matched.download_token().map(str::to_owned),
        entries,
    })
    .into_response()
}

/// Pull one friend's catalog from `base` with the key they issued us, and
/// replace what is stored for them with it.
///
/// Download links are built here from `base`, the address this instance
/// already talks to, rather than taken from the friend's answer, so a catalog
/// can only ever point the sync loop at the friend it came from. A friend
/// whose sharerr predates the catalog answers 404, which reads as an empty
/// one.
pub(crate) async fn pull(
    http: &reqwest::Client,
    store: &Store,
    peer_id: i64,
    base: &str,
    key: &str,
) -> Result<usize, String> {
    let base = base.trim_end_matches('/');
    let response = http
        .get(format!("{base}/api/catalog"))
        .query(&[("apikey", key)])
        .send()
        .await
        // Without the URL: its query carries the key the friend issued us.
        .map_err(|e| format!("catalog: {}", error_chain(&e.without_url())))?;
    let catalog = match response.status() {
        StatusCode::NOT_FOUND => Catalog::default(),
        status if status.is_success() => response
            .json::<Catalog>()
            .await
            .map_err(|e| format!("catalog body: {}", e.without_url()))?,
        status => return Err(format!("catalog answered {status}")),
    };

    let entries = entries_from(base, catalog);
    store
        .replace_peer_catalog(peer_id, &entries)
        .await
        .map_err(|e| format!("storing the catalog: {e}"))?;
    Ok(entries.len())
}

/// The entries of `catalog` this version can use, with download links on
/// `base`.
fn entries_from(base: &str, catalog: Catalog) -> Vec<CatalogEntry> {
    let token = catalog
        .token
        .as_deref()
        .map(|token| format!("?token={}", encode_component(token)))
        .unwrap_or_default();
    catalog
        .entries
        .into_iter()
        .take(MAX_ENTRIES)
        .filter_map(|raw| serde_json::from_value::<CatalogItem>(raw).ok())
        .filter(|item| item.info_hash.len() == 40 && hex::decode(&item.info_hash).is_ok())
        .map(|item| {
            let info_hash = item.info_hash.to_ascii_lowercase();
            CatalogEntry {
                download_url: format!(
                    "{base}{}{token}",
                    crate::tracker::torrent_download_path(&info_hash)
                ),
                info_hash,
                spec: item.spec,
                ids: item.ids,
                size: item.size,
                private: item.private,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use secrecy::SecretString;
    use sharerr_store::PeerScope;

    fn item(hash: &str) -> CatalogItem {
        CatalogItem {
            info_hash: hash.to_owned(),
            spec: MediaSpec::Movie {
                title: "Copper Vale".to_owned(),
                year: Some(2001),
            },
            ids: ExternalIds {
                tmdb: Some(5),
                ..ExternalIds::default()
            },
            size: 100,
            private: false,
        }
    }

    #[test]
    fn links_are_built_on_the_friends_known_address_with_their_token() {
        let catalog = Catalog {
            token: Some("t&k".to_owned()),
            entries: vec![
                serde_json::to_value(item(&"AB".repeat(20))).unwrap(),
                serde_json::json!({"info_hash": "cd".repeat(20), "spec": {"kind": "podcast"}}),
                serde_json::to_value(item("not-a-hash")).unwrap(),
            ],
        };
        let entries = entries_from("http://sam.example:8477", catalog);
        assert_eq!(
            entries.len(),
            1,
            "an unknown kind and a bad hash are skipped"
        );
        assert_eq!(entries[0].info_hash, "ab".repeat(20));
        assert_eq!(
            entries[0].download_url,
            format!(
                "http://sam.example:8477/torrents/{}.torrent?token=t%26k",
                "ab".repeat(20)
            )
        );

        let untokened = entries_from(
            "http://sam.example:8477",
            Catalog {
                token: None,
                entries: vec![serde_json::to_value(item(&"ab".repeat(20))).unwrap()],
            },
        );
        assert!(untokened[0].download_url.ends_with(".torrent"));
    }

    async fn store_with_sam() -> (Store, i64) {
        let store = Store::open_in_memory().await.unwrap();
        let sam = store
            .create_peer("Sam", &SecretString::from("sam-key"), PeerScope::All)
            .await
            .unwrap();
        (store, sam.id)
    }

    #[tokio::test]
    async fn a_pull_stores_the_friends_catalog_and_a_404_empties_it() {
        let server = wiremock::MockServer::start().await;
        let (store, sam) = store_with_sam().await;
        let body = serde_json::json!({
            "entries": [serde_json::to_value(item(&"ab".repeat(20))).unwrap()]
        });
        wiremock::Mock::given(wiremock::matchers::path("/api/catalog"))
            .and(wiremock::matchers::query_param("apikey", "their-key"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(body))
            .up_to_n_times(1)
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let n = pull(&http, &store, sam, &server.uri(), "their-key")
            .await
            .unwrap();
        assert_eq!(n, 1);
        let found = store
            .catalog_matches(&item("").spec, &item("").ids, 100)
            .await
            .unwrap();
        assert_eq!(found.len(), 1);
        assert!(found[0].1.download_url.starts_with(&server.uri()));

        // Nothing else is mounted, so the next pull is a 404: an older friend.
        assert_eq!(
            pull(&http, &store, sam, &server.uri(), "their-key")
                .await
                .unwrap(),
            0
        );
        assert!(store.peer_catalog_sizes().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_refused_pull_leaves_the_last_catalog_in_place() {
        let server = wiremock::MockServer::start().await;
        let (store, sam) = store_with_sam().await;
        store
            .replace_peer_catalog(
                sam,
                &entries_from(
                    "http://sam",
                    Catalog {
                        token: None,
                        entries: vec![serde_json::to_value(item(&"ab".repeat(20))).unwrap()],
                    },
                ),
            )
            .await
            .unwrap();
        wiremock::Mock::given(wiremock::matchers::path("/api/catalog"))
            .respond_with(wiremock::ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let err = pull(&reqwest::Client::new(), &store, sam, &server.uri(), "k")
            .await
            .unwrap_err();
        assert!(err.contains("401"), "{err}");
        assert_eq!(
            store.peer_catalog_sizes().await.unwrap().get(&sam),
            Some(&1)
        );
    }
}
