//! sharerr's own BitTorrent tracker, mounted on the `serve` router.
//!
//! The protocol lives in [`sharerr_torrent::announce`]; this is the HTTP skin over
//! it. Three things are decided here rather than there, because all three need
//! something only the running server has.
//!
//! **Admission.** A hash is served only if the store says this instance is sharing
//! it. Without that check anyone who found the port could register a swarm and use
//! a stranger's home connection as tracker infrastructure.
//!
//! **The peer's address.** Taken from the socket, not from what the client claims,
//! because a client behind NAT reports a private address that no other peer can
//! reach. This is why `serve` binds with `into_make_service_with_connect_info`.
//!
//! **The token.** When `tracker.token` is in the vault, announce URLs carry it in
//! the path and a request without it is refused — so possessing the `.torrent` is
//! what grants the right to announce.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Path, RawQuery, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use sharerr_torrent::announce::{
    self, AnnounceError, AnnounceRequest, InfoHash, Swarms, failure_bencode, scrape_bencode,
};

use crate::commands::serve::ServeState;

/// The bencoded content type. Clients do not check it, but a browser hitting the
/// endpoint by hand should not be offered a download prompt.
const BENCODE: &str = "text/plain; charset=utf-8";

/// Everything the tracker handlers need.
#[derive(Debug)]
pub struct TrackerState {
    pub serve: Arc<ServeState>,
    pub swarms: Swarms,
}

impl TrackerState {
    pub fn new(serve: Arc<ServeState>) -> Self {
        Self {
            serve,
            swarms: Swarms::default(),
        }
    }

    /// The token an announce must carry, if one is configured.
    ///
    /// Read from the running syncer rather than the vault directly: the syncer
    /// already holds it, and going to the vault here would mean an Argon2
    /// derivation on every announce.
    async fn required_token(&self) -> Option<String> {
        self.serve.tracker_token().await
    }
}

/// Every route the tracker serves.
///
/// The token variants are separate routes rather than one optional path segment:
/// axum resolves `/announce` and `/announce/{token}` as distinct patterns, and
/// spelling them out keeps the tokenless case from depending on how an optional
/// extractor behaves when the segment is missing.
pub fn routes(serve: Arc<ServeState>) -> axum::Router {
    let state = Arc::new(TrackerState::new(serve));

    axum::Router::new()
        .route("/announce", axum::routing::get(announce))
        .route("/announce/{token}", axum::routing::get(announce_with_token))
        .route("/scrape", axum::routing::get(scrape))
        .route("/scrape/{token}", axum::routing::get(scrape_with_token))
        .route("/torrents/{name}", axum::routing::get(torrent_file))
        .with_state(state)
}

/// `GET /announce`.
pub async fn announce(
    State(state): State<Arc<TrackerState>>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    RawQuery(query): RawQuery,
) -> Response {
    respond(&state, None, remote, query).await
}

/// `GET /announce/{token}`.
pub async fn announce_with_token(
    State(state): State<Arc<TrackerState>>,
    Path(token): Path<String>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    RawQuery(query): RawQuery,
) -> Response {
    respond(&state, Some(token), remote, query).await
}

async fn respond(
    state: &TrackerState,
    token: Option<String>,
    remote: SocketAddr,
    query: Option<String>,
) -> Response {
    // `RawQuery` rather than `Query<T>`: info_hash is 20 raw bytes, and every
    // string-based extractor replaces the invalid UTF-8 with U+FFFD and destroys
    // the identity the whole protocol is keyed on.
    let query = query.unwrap_or_default();

    match handle_announce(state, token, remote, query.as_bytes()).await {
        Ok(body) => bencode(body),
        Err(err) => {
            tracing::debug!(error = %err, %remote, "refused an announce");
            bencode(failure_bencode(&err.to_string()))
        }
    }
}

async fn handle_announce(
    state: &TrackerState,
    token: Option<String>,
    remote: SocketAddr,
    query: &[u8],
) -> Result<Vec<u8>, AnnounceError> {
    check_token(state.required_token().await.as_deref(), token.as_deref())?;

    let request = AnnounceRequest::parse(query)?;
    if !is_served(state, &request.info_hash).await {
        return Err(AnnounceError::UnknownTorrent);
    }

    let addr = request.resolve_addr(remote.ip());
    let response = state.swarms.announce(&request, addr).await;

    tracing::debug!(
        info_hash = %hex(&request.info_hash),
        peer = %addr,
        event = ?request.event,
        peers = response.peers.len(),
        "announce"
    );

    Ok(response.to_bencode(request.compact))
}

/// `GET /scrape` — swarm counts, for clients and for Prowlarr's seeder column.
pub async fn scrape(State(state): State<Arc<TrackerState>>, RawQuery(query): RawQuery) -> Response {
    handle_scrape(&state, None, query).await
}

/// `GET /scrape/{token}`.
pub async fn scrape_with_token(
    State(state): State<Arc<TrackerState>>,
    Path(token): Path<String>,
    RawQuery(query): RawQuery,
) -> Response {
    handle_scrape(&state, Some(token), query).await
}

async fn handle_scrape(
    state: &TrackerState,
    token: Option<String>,
    query: Option<String>,
) -> Response {
    if let Err(err) = check_token(state.required_token().await.as_deref(), token.as_deref()) {
        return bencode(failure_bencode(&err.to_string()));
    }

    let params = announce::parse_query(query.unwrap_or_default().as_bytes());

    // A scrape with no info_hash means "tell me about everything you have", which
    // this tracker deliberately does not answer — it would enumerate the whole
    // library to anyone who found the port.
    let Some(raw) = params.get("info_hash") else {
        return bencode(failure_bencode(
            "this tracker only scrapes specific torrents",
        ));
    };
    let Ok(info_hash) = InfoHash::try_from(raw.as_slice()) else {
        return bencode(failure_bencode("info_hash must be exactly 20 bytes"));
    };

    if !is_served(state, &info_hash).await {
        // An empty `files` dict rather than a failure: a client scraping a torrent
        // we have withdrawn should learn there are no peers, not that something
        // went wrong.
        return bencode(scrape_bencode(&[]));
    }

    let (complete, incomplete) = state.swarms.scrape(&info_hash).await;
    bencode(scrape_bencode(&[(info_hash, complete, incomplete)]))
}

/// Whether this instance is sharing the torrent, per the store.
///
/// A database that cannot be read is treated as "not served". Announcing peers to
/// each other on the strength of an unverified hash is the one thing this check
/// exists to prevent, so failing open would defeat it.
async fn is_served(state: &TrackerState, info_hash: &InfoHash) -> bool {
    let Ok(store) = state.serve.store().await else {
        tracing::warn!("refusing announces: the database is unavailable");
        return false;
    };

    match store.is_shared(&hex(info_hash)).await {
        Ok(served) => served,
        Err(err) => {
            tracing::warn!(error = %err, "could not check whether a torrent is shared");
            false
        }
    }
}

/// Compare the token in the URL against the configured one.
fn check_token(required: Option<&str>, supplied: Option<&str>) -> Result<(), AnnounceError> {
    let Some(required) = required else {
        // No token configured: the announce URLs sharerr generates have no token
        // segment either, so an unauthenticated announce is the expected shape.
        return Ok(());
    };

    if crate::secrets::constant_time_eq(required, supplied.unwrap_or_default()) {
        Ok(())
    } else {
        Err(AnnounceError::BadToken)
    }
}

/// Lowercase hex, matching exactly what the store holds in `info_hash`.
fn hex(bytes: &InfoHash) -> String {
    hex::encode(bytes)
}

fn bencode(body: Vec<u8>) -> Response {
    // Always 200, even for a refusal. Many clients treat a non-2xx as a transport
    // failure and retry forever without ever showing the operator the reason.
    ([(header::CONTENT_TYPE, BENCODE)], body).into_response()
}

/// `GET /torrents/{info_hash}.torrent` — the file itself.
///
/// Serves out of `data_dir/torrents`, which is where the factory wrote it. The
/// Torznab feed links here, so this is what a friend's Sonarr actually fetches.
pub async fn torrent_file(
    State(state): State<Arc<TrackerState>>,
    Path(name): Path<String>,
) -> Response {
    let Some(hex_hash) = name.strip_suffix(".torrent") else {
        return (StatusCode::NOT_FOUND, "not a torrent").into_response();
    };

    // Parsed as a hash rather than used as a path component. `..` and `/` cannot
    // survive this, so the filename below is always exactly 40 hex characters.
    let Some(info_hash) = announce::info_hash_from_hex(hex_hash) else {
        return (StatusCode::NOT_FOUND, "not a torrent").into_response();
    };

    if !is_served(&state, &info_hash).await {
        return (StatusCode::NOT_FOUND, "not shared").into_response();
    }

    let config = state.serve.config().await;
    let path = sharerr_torrent::torrent_file_path(&config.torrent_dir(), &hex(&info_hash));

    match tokio::fs::read(&path).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "application/x-bittorrent".to_owned()),
                (
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{}.torrent\"", hex(&info_hash)),
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(err) => {
            // The row says seeding but the file is gone — a wiped /data with an
            // intact database, usually. Worth an error line: the friend's Sonarr
            // sees only a failed download.
            tracing::error!(path = %path.display(), error = %err, "torrent file missing");
            (StatusCode::NOT_FOUND, "torrent file missing").into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn an_unconfigured_token_accepts_any_announce() {
        assert!(check_token(None, None).is_ok());
        assert!(
            check_token(None, Some("anything")).is_ok(),
            "a stray path segment is not a reason to refuse"
        );
    }

    #[test]
    fn a_configured_token_must_match_exactly() {
        assert!(check_token(Some("s3cret"), Some("s3cret")).is_ok());

        for wrong in [
            None,
            Some(""),
            Some("s3cre"),
            Some("s3crets"),
            Some("S3CRET"),
        ] {
            assert_eq!(
                check_token(Some("s3cret"), wrong),
                Err(AnnounceError::BadToken),
                "{wrong:?} should not be accepted"
            );
        }
    }

    #[test]
    fn hashes_render_as_lowercase_hex_matching_what_the_store_holds() {
        assert_eq!(hex(&[0x0a; 20]), "0a".repeat(20));
        assert_eq!(hex(&[0xff; 20]), "ff".repeat(20));
    }
}
