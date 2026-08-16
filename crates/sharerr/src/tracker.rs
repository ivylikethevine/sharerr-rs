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

use crate::state::ServeState;

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
}

/// Every route the tracker serves.
///
/// The token variants are separate routes rather than one optional path segment:
/// axum resolves `/announce` and `/announce/{token}` as distinct patterns, and
/// spelling them out keeps the tokenless case from depending on how an optional
/// extractor behaves when the segment is missing.
pub fn routes(serve: Arc<ServeState>) -> axum::Router {
    let state = Arc::new(TrackerState::new(serve));

    // Mounted from the same constants `sharerr-torrent` writes into announce
    // URLs, so the two sides of the crate boundary cannot drift.
    axum::Router::new()
        .route(sharerr_torrent::ANNOUNCE_PATH, axum::routing::get(announce))
        .route(
            &format!("{}/{{token}}", sharerr_torrent::ANNOUNCE_PATH),
            axum::routing::get(announce_with_token),
        )
        .route(sharerr_torrent::SCRAPE_PATH, axum::routing::get(scrape))
        .route(
            &format!("{}/{{token}}", sharerr_torrent::SCRAPE_PATH),
            axum::routing::get(scrape_with_token),
        )
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
    check_token(
        state.serve.tracker_token().await.as_deref(),
        token.as_deref(),
    )?;

    let request = AnnounceRequest::parse(query)?;
    if !is_served(state, &request.info_hash).await {
        return Err(AnnounceError::UnknownTorrent);
    }

    let addr = request.resolve_addr(remote.ip());
    let response = state.swarms.announce(&request, addr).await;

    tracing::debug!(
        info_hash = %hex::encode(request.info_hash),
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
    if let Err(err) = check_token(
        state.serve.tracker_token().await.as_deref(),
        token.as_deref(),
    ) {
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

    match store.is_shared(&hex::encode(info_hash)).await {
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

fn bencode(body: Vec<u8>) -> Response {
    // Always 200, even for a refusal. Many clients treat a non-2xx as a transport
    // failure and retry forever without ever showing the operator the reason.
    ([(header::CONTENT_TYPE, BENCODE)], body).into_response()
}

/// The URL path one torrent is served under — the shape [`torrent_file`] parses
/// back apart. Kept beside the route so the feed's links cannot drift from it.
pub(crate) fn torrent_download_path(info_hash: &str) -> String {
    format!("/torrents/{info_hash}.torrent")
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
    let path = sharerr_torrent::torrent_file_path(&config.torrent_dir(), &hex::encode(info_hash));

    match tokio::fs::read(&path).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "application/x-bittorrent".to_owned()),
                (
                    header::CONTENT_DISPOSITION,
                    format!(
                        "attachment; filename=\"{}.torrent\"",
                        hex::encode(info_hash)
                    ),
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

    // ------------------------------------------------- router-level coverage

    use crate::state::fixtures::unconfigured;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    async fn get(state: &Arc<crate::state::ServeState>, uri: &str) -> (StatusCode, String) {
        let mut request = Request::builder().uri(uri).body(Body::empty()).unwrap();
        // The announce handler records the address a peer actually reached us from,
        // which `serve` supplies via `into_make_service_with_connect_info`. Driving
        // the router directly skips that, and the extractor then fails with a 500
        // that looks like a handler bug — so the test provides it the way the real
        // server does.
        request
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                [203, 0, 113, 7],
                51413,
            ))));

        let response = routes(Arc::clone(state)).oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    /// The admission rule, asserted over the assembled router rather than the
    /// helper. This is what stops sharerr becoming an open tracker that strangers
    /// can register swarms on, and it is a property of what `routes()` wires up.
    #[tokio::test]
    async fn the_tracker_refuses_an_info_hash_it_is_not_sharing() {
        let (_dir, state) = unconfigured();

        // 20 bytes, percent-encoded the way a real client sends them.
        let hash = "%00".repeat(20);
        let (status, body) = get(
            &state,
            &format!("/announce?info_hash={hash}&peer_id={hash}&port=6881"),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "trackers report refusals in-band");
        assert!(
            body.contains("failure reason"),
            "an unknown hash must be refused, not introduced to peers: {body}"
        );
    }

    /// Every route the tracker claims to serve must actually be wired up. A handler
    /// that exists but was never routed is exactly the class of bug router-level
    /// tests exist to catch.
    #[tokio::test]
    async fn every_tracker_route_is_reachable() {
        let (_dir, state) = unconfigured();
        let hash = "%00".repeat(20);

        for uri in [
            format!("/announce?info_hash={hash}&peer_id={hash}&port=6881"),
            format!("/announce/sometoken?info_hash={hash}&peer_id={hash}&port=6881"),
            "/scrape".to_owned(),
            "/scrape/sometoken".to_owned(),
        ] {
            let (status, _) = get(&state, &uri).await;
            assert_ne!(status, StatusCode::NOT_FOUND, "{uri} is not routed");
        }
    }

    /// A `.torrent` sharerr did not make must not be served, whoever asks — the
    /// same admission rule as the announce endpoint, one layer along.
    #[tokio::test]
    async fn an_unknown_torrent_file_is_not_served() {
        let (_dir, state) = unconfigured();

        let (status, _) = get(&state, "/torrents/deadbeef.torrent").await;
        assert_ne!(
            status,
            StatusCode::OK,
            "serving an arbitrary name would be a file-read primitive"
        );
    }

    /// A path that tries to climb out of the torrent directory must not be honoured.
    #[tokio::test]
    async fn a_traversing_torrent_name_is_refused() {
        let (_dir, state) = unconfigured();

        for name in ["..%2f..%2fetc%2fpasswd", "....//etc/passwd"] {
            let (status, _) = get(&state, &format!("/torrents/{name}")).await;
            assert_ne!(status, StatusCode::OK, "{name} was served");
        }
    }
}
