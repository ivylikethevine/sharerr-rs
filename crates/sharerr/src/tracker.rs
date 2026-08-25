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
//!
//! **Attribution.** The token is not always the one shared instance secret: a
//! magnet built by [`crate::torznab::collect`] carries the requesting friend's
//! own [`Peer::key_hash`](sharerr_store::Peer::key_hash) instead, so a real
//! announce using it can be traced back to them — and revoking that friend
//! (which already zeroes their `key_hash` out of the active peers) reaches
//! the tracker too, not just the feed. The instance-wide shared token still
//! works forever alongside this, unattributed, so nothing seeded before this
//! existed ever breaks. See [`authenticate_token`]. A `.torrent` fetched
//! directly gets the same treatment: [`torrent_file`] rewrites the announce
//! URLs it serves per requester, in memory, rather than caching a variant per
//! peer on disk — see that function's docs.
//!
//! **Rotating the shared token.** Overwriting `tracker.token` outright would
//! instantly lock out everyone still relying on the old value. Instead a
//! rotation (`crate::web::settings::rotate_tracker_token`) keeps the previous
//! value valid too, unattributed, until the operator finalizes it away — see
//! [`authenticate_token`] and [`LegacyTokenStatus`].

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Path, Query, RawQuery, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use sharerr_store::{EndpointKind, Store};
use sharerr_torrent::announce::{
    self, AnnounceError, AnnounceRequest, InfoHash, failure_bencode, scrape_bencode,
};
use utoipa_axum::router::OpenApiRouter;

use crate::state::ServeState;

/// The bencoded content type. Clients do not check it, but a browser hitting the
/// endpoint by hand should not be offered a download prompt.
const BENCODE: &str = "text/plain; charset=utf-8";

/// Everything the tracker handlers need.
#[derive(Debug)]
pub struct TrackerState {
    /// The swarms this router writes are [`ServeState::swarms`]'s — the
    /// status page reads the same ones.
    pub serve: Arc<ServeState>,
}

/// When the previous (rotated-out) shared tracker token was last actually
/// used, so an operator deciding whether to finalize a rotation — see
/// `crate::web::settings::finalize_tracker_token` — can tell "nothing has
/// announced with it in a week" from "no idea, it might still be in use".
///
/// Process-lifetime only, on the same reasoning `crate::gluetun::GluetunStatus`
/// is: losing this on restart is an honest reset back to "unknown", not a
/// correctness problem, so it is not worth a migration to persist.
#[derive(Debug, Default)]
pub struct LegacyTokenStatus {
    last_used_at: tokio::sync::RwLock<Option<i64>>,
}

impl LegacyTokenStatus {
    async fn record_used(&self) {
        *self.last_used_at.write().await = Some(sharerr_core::endpoint::now_epoch());
    }

    /// Cleared when a rotation replaces which token is "previous", or when
    /// that token is finalized away — either way, whatever this used to track
    /// no longer applies.
    pub async fn reset(&self) {
        *self.last_used_at.write().await = None;
    }

    pub async fn snapshot(&self) -> Option<i64> {
        *self.last_used_at.read().await
    }
}

impl TrackerState {
    pub fn new(serve: Arc<ServeState>) -> Self {
        Self { serve }
    }
}

/// Every route the tracker serves.
///
/// Takes the state rather than building it, because the same `Swarms` may be
/// mounted on *two* listeners — the main router and an optional dedicated
/// `tracker.bind` — and two independent swarm maps would stop peers arriving on
/// different listeners from ever being introduced to each other.
///
/// The token variants are separate routes rather than one optional path segment:
/// axum resolves `/announce` and `/announce/{token}` as distinct patterns, and
/// spelling them out keeps the tokenless case from depending on how an optional
/// extractor behaves when the segment is missing.
pub fn routes(state: Arc<TrackerState>) -> axum::Router {
    let (router, _) = api_router().with_state(state).split_for_parts();
    router
}

/// The same routes without state, for [`crate::openapi`].
///
/// Unlike every other surface here these are mounted with `.route` rather than
/// `routes!`, and keep deriving their paths from the constants
/// `sharerr-torrent` writes into announce URLs — that is what stops the two
/// sides of the crate boundary drifting, and it is worth more than the
/// convenience. The document gets them from the handlers' `#[utoipa::path]`
/// attributes instead, listed in `crate::openapi`, and
/// `the_documented_tracker_paths_match_the_constants` holds the literal and
/// the constant together.
pub(crate) fn api_router() -> OpenApiRouter<Arc<TrackerState>> {
    OpenApiRouter::new()
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
}

/// The announce query string, for the OpenAPI document only.
///
/// Nothing deserializes into this type. The real handler takes `RawQuery` and
/// hands the bytes to [`AnnounceRequest::parse`], because `info_hash` and
/// `peer_id` are **20 raw bytes** percent-encoded, and every string-based
/// extractor replaces the invalid UTF-8 with U+FFFD — destroying the identity
/// the whole protocol is keyed on. So this exists to describe that query to a
/// reader, and `announce_query_matches_the_parser` keeps it honest against the
/// parser's own field names.
#[derive(utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[allow(
    dead_code,
    reason = "documentation shape; the handler parses raw bytes"
)]
pub struct AnnounceParams {
    /// The torrent's info hash: 20 raw bytes, percent-encoded. **Not hex.**
    #[param(required = true)]
    info_hash: String,
    /// The client's own id: 20 raw bytes, percent-encoded.
    #[param(required = true)]
    peer_id: String,
    /// The port the client accepts connections on.
    #[param(required = true, example = 6881)]
    port: u16,
    /// Bytes still to download. `0` means a seeder.
    #[param(required = true)]
    left: u64,
    /// `started`, `stopped`, `completed`, or absent for a periodic re-announce.
    event: Option<String>,
    /// `1` for the compact peer list. Anything else returns the dictionary form.
    compact: Option<u8>,
    /// How many peers to return.
    numwant: Option<usize>,
    /// The client's own address. Honoured **only when it is a private address** —
    /// otherwise the connection's source address wins, so a peer cannot announce
    /// somebody else into the swarm.
    ip: Option<String>,
}

/// The scrape query string. Documentation-only, for the same reason as
/// [`AnnounceParams`].
#[derive(utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[allow(
    dead_code,
    reason = "documentation shape; the handler parses raw bytes"
)]
pub struct ScrapeParams {
    /// One or more info hashes, 20 raw bytes each, percent-encoded. Repeat the
    /// parameter for several. Omitted entirely means every torrent this instance
    /// shares with the caller.
    info_hash: Option<String>,
}

/// `GET /announce`.
#[utoipa::path(
    get,
    path = "/announce",
    tag = "tracker",
    operation_id = "announce",
    params(AnnounceParams),
    responses((status = 200, content_type = "text/plain", body = String, description =
             "A bencoded dictionary. **Always 200** — the BitTorrent tracker protocol \
              reports refusal as a bencoded `failure reason` key, not as an HTTP \
              status, so a client that only checks the status code will read an \
              error as a swarm.")),
)]
pub async fn announce(
    State(state): State<Arc<TrackerState>>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    RawQuery(query): RawQuery,
) -> Response {
    respond(&state, None, remote, query).await
}

/// `GET /announce/{token}`.
#[utoipa::path(
    get,
    path = "/announce/{token}",
    tag = "tracker",
    operation_id = "announceWithToken",
    params(
        ("token" = String, Path, description =
         "The announce token baked into this torrent's announce URL. Identifies which \
          peer is announcing, so a swarm can be attributed and one friend's access \
          revoked without disturbing anyone else's."),
        AnnounceParams,
    ),
    responses((status = 200, content_type = "text/plain", body = String, description =
             "A bencoded dictionary. **Always 200** — the BitTorrent tracker protocol \
              reports refusal as a bencoded `failure reason` key, not as an HTTP \
              status, so a client that only checks the status code will read an \
              error as a swarm.")),
)]
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
    // Parsed first: a malformed query needs neither lookup below.
    let request = AnnounceRequest::parse(query)?;

    // Fails closed: an announce this instance cannot check the token for is
    // no more admissible than one for a hash it cannot check either — see
    // `is_served`'s own identical reasoning. The two checks are independent,
    // so they run together.
    let (auth, served) = tokio::join!(
        authenticate(state, token.as_deref()),
        is_served(state, &request.info_hash)
    );
    let (store, auth) = auth?;
    if !served {
        return Err(AnnounceError::UnknownTorrent);
    }

    let addr = request.resolve_addr(remote.ip());
    let response = state.serve.swarms().announce(&request, addr).await;

    if let Some(peer_id) = auth.attributed_to {
        crate::torznab::record_sighting(
            &store,
            peer_id,
            EndpointKind::Client,
            Some(&addr.to_string()),
        )
        .await;
    }

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
#[utoipa::path(
    get,
    path = "/scrape",
    tag = "tracker",
    operation_id = "scrape",
    params(ScrapeParams),
    responses((status = 200, content_type = "text/plain", body = String, description =
             "A bencoded dictionary. **Always 200** — the BitTorrent tracker protocol \
              reports refusal as a bencoded `failure reason` key, not as an HTTP \
              status, so a client that only checks the status code will read an \
              error as a swarm.")),
)]
pub async fn scrape(State(state): State<Arc<TrackerState>>, RawQuery(query): RawQuery) -> Response {
    handle_scrape(&state, None, query).await
}

/// `GET /scrape/{token}`.
#[utoipa::path(
    get,
    path = "/scrape/{token}",
    tag = "tracker",
    operation_id = "scrapeWithToken",
    params(
        ("token" = String, Path, description = "As on `/announce/{token}`."),
        ScrapeParams,
    ),
    responses((status = 200, content_type = "text/plain", body = String, description =
             "A bencoded dictionary. **Always 200** — the BitTorrent tracker protocol \
              reports refusal as a bencoded `failure reason` key, not as an HTTP \
              status, so a client that only checks the status code will read an \
              error as a swarm.")),
)]
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
    // Scrape is swarm counts, not an announce — nothing here to attribute,
    // so the resolved peer id (if any) is simply discarded.
    if let Err(err) = authenticate(state, token.as_deref()).await {
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

    let (complete, incomplete) = state.serve.swarms().scrape(&info_hash).await;
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

/// The result of checking an announce/scrape token: whether the request is
/// allowed at all, which peer (if any) it should be attributed to, and
/// whether it was allowed specifically via the rotated-out previous token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct TokenAuth {
    attributed_to: Option<i64>,
    via_previous: bool,
}

/// Which peer, if any, an announce/scrape token identifies, and whether the
/// request is allowed at all.
///
/// Four outcomes, in the order checked:
///
/// 1. No token configured on this instance at all → open, always allowed.
///    The announce URLs sharerr generates then carry no token segment
///    either, so an unauthenticated request is the expected shape — today's
///    default, unchanged.
/// 2. Matches the instance's own shared legacy token (`tracker.token` in the
///    vault) → allowed but unattributed. This is what keeps everything
///    seeded before per-peer attribution existed working, and what any
///    friend not yet re-synced to a magnet built after it existed keeps
///    using.
/// 3. Matches the *previous* shared legacy token, if a rotation is in
///    progress → allowed, unattributed, `via_previous: true`. See
///    `crate::web::settings::rotate_tracker_token`: this is the whole point
///    of holding the old value a little longer instead of overwriting it in
///    place.
/// 4. Matches an active peer's own `key_hash` → allowed and attributed. A
///    revoked peer's hash matches nothing — `peer_by_key_hash` already
///    excludes them — which is the entire "cut this friend off reaches the
///    tracker too" payoff: no separate revocation step exists or is needed
///    here.
///
/// Anything else is `Err(AnnounceError::BadToken)`.
///
/// The one admission path for announce and scrape alike: store, both vault
/// tokens, [`authenticate_token`], and the previous-token usage bookkeeping.
///
/// Fails closed: an announce this instance cannot check the token for — a
/// store that will not open, or a vault that will not open — is no more
/// admissible than one for a hash it cannot check either (see `is_served`).
/// The vault half matters: `tracker_token()`'s `None` cannot tell "no token
/// configured" from "could not read the vault", and admitting on the latter
/// silently turned enforcement off after a transient error.
async fn authenticate(
    state: &TrackerState,
    supplied: Option<&str>,
) -> Result<(Store, TokenAuth), AnnounceError> {
    let store = state
        .serve
        .store()
        .await
        .map_err(|_| AnnounceError::BadToken)?;
    let (current, previous) = state.serve.tracker_tokens().await.map_err(|err| {
        tracing::warn!(error = %err, "refusing announces: the vault could not be opened");
        AnnounceError::BadToken
    })?;
    let auth =
        authenticate_token(&store, current.as_deref(), previous.as_deref(), supplied).await?;
    if auth.via_previous {
        state.serve.legacy_token_status().record_used().await;
    }
    Ok((store, auth))
}

/// Takes `store`, `required`, and `previous` as plain parameters rather than
/// reaching into `TrackerState` itself: the peer-hash branch is exactly the
/// part worth testing directly against an in-memory `Store`, with no vault or
/// master key needed to exercise it.
async fn authenticate_token(
    store: &Store,
    required: Option<&str>,
    previous: Option<&str>,
    supplied: Option<&str>,
) -> Result<TokenAuth, AnnounceError> {
    let Some(required) = required else {
        return Ok(TokenAuth::default());
    };
    let Some(supplied) = supplied else {
        return Err(AnnounceError::BadToken);
    };

    if crate::secrets::constant_time_eq(required, supplied) {
        return Ok(TokenAuth::default());
    }

    if let Some(previous) = previous
        && crate::secrets::constant_time_eq(previous, supplied)
    {
        return Ok(TokenAuth {
            via_previous: true,
            ..TokenAuth::default()
        });
    }

    match store.peer_by_key_hash(supplied).await {
        Ok(Some(peer)) => Ok(TokenAuth {
            attributed_to: Some(peer.id),
            ..TokenAuth::default()
        }),
        Ok(None) => Err(AnnounceError::BadToken),
        Err(err) => {
            tracing::warn!(error = %err, "could not check a peer's announce token");
            Err(AnnounceError::BadToken)
        }
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

/// The optional query string on a `.torrent` download.
#[derive(Debug, Deserialize)]
pub struct TorrentFileQuery {
    /// A peer's own `key_hash`, the same value [`crate::torznab::Matched::download_url`]
    /// embeds — see [`torrent_file`].
    token: Option<String>,
}

/// `GET /torrents/{info_hash}.torrent` — the file itself.
///
/// Serves out of `data_dir/torrents`, which is where the factory wrote it. The
/// Torznab feed links here, so this is what a friend's Sonarr actually fetches.
///
/// The cached file on disk always carries the shared instance token (or none),
/// because it is written once by the sync loop and reused for every requester.
/// When the request carries a `token` that still resolves to an active peer,
/// the announce URLs are rewritten in memory — never on disk — to that peer's
/// own token before the response goes out, the same attribution the feed's
/// magnet links already carry. Roadmap Stage 2; see `docs/ROADMAP.md`.
#[utoipa::path(
    get,
    path = "/torrents/{name}",
    tag = "tracker",
    operation_id = "torrentFile",
    params(
        ("name" = String, Path, description =
         "`{info_hash}.torrent` — 40 lowercase hex characters and the suffix. Parsed \
          as a hash rather than used as a path component, so no traversal is possible."),
        ("token" = Option<String>, Query, description =
         "The requesting peer's own token, as the feed's download links carry. When it \
          resolves to an active peer the announce URLs are rewritten to that peer's \
          token in the response body — never on disk."),
    ),
    responses(
        (status = 200, content_type = "application/x-bittorrent",
         description = "The `.torrent` file.", body = Vec<u8>),
        (status = 404, description =
         "Not a torrent name, not currently shared, or shared but its cached file is \
          missing from disk. Deliberately one status for all three: a caller without \
          a valid token learns nothing about what this instance holds.", body = String),
    ),
)]
pub async fn torrent_file(
    State(state): State<Arc<TrackerState>>,
    Path(name): Path<String>,
    Query(query): Query<TorrentFileQuery>,
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

    let path = sharerr_torrent::torrent_file_path(
        &state.serve.torrent_dir().await,
        &hex::encode(info_hash),
    );

    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(err) => {
            // The row says seeding but the file is gone — a wiped /data with an
            // intact database, usually. Worth an error line: the friend's Sonarr
            // sees only a failed download.
            tracing::error!(path = %path.display(), error = %err, "torrent file missing");
            return (StatusCode::NOT_FOUND, "torrent file missing").into_response();
        }
    };

    let bytes = match query.token.as_deref() {
        Some(supplied) => attributed_bytes(&state, bytes, supplied).await,
        None => bytes,
    };

    (
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
        .into_response()
}

/// Rewrite `bytes`' announce URLs to carry `supplied` as the token, when it
/// resolves to a peer whose access has not been revoked. Any other outcome —
/// no such peer, a revoked one, a database that will not answer, a rewrite
/// that fails to parse — falls back to serving `bytes` unchanged: this
/// parameter only ever narrows attribution, so a caller for whom it cannot be
/// honoured still gets the same file an unauthenticated download always got.
///
/// Takes `bytes` by value so every fallback arm hands the same buffer back
/// rather than copying it.
async fn attributed_bytes(state: &TrackerState, bytes: Vec<u8>, supplied: &str) -> Vec<u8> {
    let store = match state.serve.store().await {
        Ok(store) => store,
        Err(_) => return bytes,
    };

    let peer = match store.peer_by_key_hash(supplied).await {
        Ok(Some(peer)) => peer,
        Ok(None) => return bytes,
        Err(err) => {
            tracing::warn!(error = %err, "could not check a peer's download token");
            return bytes;
        }
    };

    let announce =
        match sharerr_torrent::announce_set_for(&state.serve.endpoint(), Some(&peer.key_hash)) {
            Ok(announce) => announce,
            Err(err) => {
                tracing::warn!(error = %err, "could not build a per-peer announce set");
                return bytes;
            }
        };

    match sharerr_torrent::rewrite_announce(&bytes, &announce) {
        Ok(rewritten) => rewritten,
        Err(err) => {
            tracing::warn!(error = %err, "could not attribute a .torrent download");
            bytes
        }
    }
}

#[cfg(test)]
mod tests {
    // `result_large_err` for the same reason `state.rs`'s tests allow it: the
    // Jail closures below return `Result<_, figment::Error>`, and that error is
    // figment's to size, not ours.
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::result_large_err)]

    use super::*;

    use secrecy::SecretString;
    use sharerr_store::{ObservedVia, PeerScope};

    #[tokio::test]
    async fn legacy_token_status_reflects_record_used_and_reset() {
        let status = LegacyTokenStatus::default();
        assert!(status.snapshot().await.is_none());

        status.record_used().await;
        assert!(status.snapshot().await.is_some());

        status.reset().await;
        assert!(
            status.snapshot().await.is_none(),
            "a rotation or a finalize must clear the previous reading"
        );
    }

    #[tokio::test]
    async fn an_unconfigured_token_accepts_any_announce_and_attributes_nothing() {
        let store = Store::open_in_memory().await.unwrap();
        assert_eq!(
            authenticate_token(&store, None, None, None).await,
            Ok(TokenAuth::default())
        );
        assert_eq!(
            authenticate_token(&store, None, None, Some("anything")).await,
            Ok(TokenAuth::default()),
            "a stray path segment is not a reason to refuse"
        );
    }

    #[tokio::test]
    async fn the_shared_legacy_token_still_works_and_stays_unattributed() {
        let store = Store::open_in_memory().await.unwrap();
        assert_eq!(
            authenticate_token(&store, Some("s3cret"), None, Some("s3cret")).await,
            Ok(TokenAuth::default())
        );

        for wrong in [
            None,
            Some(""),
            Some("s3cre"),
            Some("s3crets"),
            Some("S3CRET"),
        ] {
            assert_eq!(
                authenticate_token(&store, Some("s3cret"), None, wrong).await,
                Err(AnnounceError::BadToken),
                "{wrong:?} should not be accepted"
            );
        }
    }

    /// The whole reason a rotation holds two tokens at once: whatever still
    /// carries the old one keeps working, unattributed, and is flagged as
    /// having used the previous token specifically so the operator can see
    /// it before finalizing.
    #[tokio::test]
    async fn a_rotation_in_progress_still_accepts_the_previous_token() {
        let store = Store::open_in_memory().await.unwrap();
        assert_eq!(
            authenticate_token(
                &store,
                Some("new-token"),
                Some("old-token"),
                Some("old-token")
            )
            .await,
            Ok(TokenAuth {
                attributed_to: None,
                via_previous: true,
            })
        );
        assert_eq!(
            authenticate_token(
                &store,
                Some("new-token"),
                Some("old-token"),
                Some("new-token")
            )
            .await,
            Ok(TokenAuth::default()),
            "the current token must not be reported as the previous one"
        );
        assert_eq!(
            authenticate_token(
                &store,
                Some("new-token"),
                Some("old-token"),
                Some("neither-of-these")
            )
            .await,
            Err(AnnounceError::BadToken)
        );
    }

    /// The whole point: an announce carrying a friend's own `key_hash`
    /// resolves to them, and revoking them (already possible today, for the
    /// feed) now silently reaches the tracker too, with no new revocation
    /// step of its own.
    #[tokio::test]
    async fn a_peers_own_key_hash_authenticates_and_attributes_them_until_revoked() {
        let store = Store::open_in_memory().await.unwrap();
        let sam = store
            .create_peer("Sam", &SecretString::from("sam-key"), PeerScope::All)
            .await
            .unwrap();

        assert_eq!(
            authenticate_token(&store, Some("s3cret"), None, Some(&sam.key_hash)).await,
            Ok(TokenAuth {
                attributed_to: Some(sam.id),
                via_previous: false,
            })
        );

        store.revoke_peer(sam.id).await.unwrap();
        assert_eq!(
            authenticate_token(&store, Some("s3cret"), None, Some(&sam.key_hash)).await,
            Err(AnnounceError::BadToken),
            "a revoked friend's own token must stop working"
        );
    }

    /// The other half of attribution: once `authenticate_token` names a
    /// peer, a real client address must land in peer endpoint memory as a
    /// first-hand [`ObservedVia::Direct`] sighting of their
    /// [`EndpointKind::Client`] — the actual gap this whole feature closes.
    #[tokio::test]
    async fn a_successful_attribution_records_the_clients_address() {
        let store = Store::open_in_memory().await.unwrap();
        let sam = store
            .create_peer("Sam", &SecretString::from("sam-key"), PeerScope::All)
            .await
            .unwrap();

        crate::torznab::record_sighting(
            &store,
            sam.id,
            EndpointKind::Client,
            Some("203.0.113.9:51413"),
        )
        .await;

        let endpoints = store.peer_endpoints(sam.id).await.unwrap();
        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0].kind, EndpointKind::Client);
        assert_eq!(endpoints[0].via, ObservedVia::Direct);
        assert_eq!(endpoints[0].addr, "203.0.113.9:51413");
    }

    // ------------------------------------------------- router-level coverage

    use crate::state::fixtures::unconfigured;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    /// Drive `body` against a state whose vault genuinely opens.
    ///
    /// [`authenticate`] fails closed on a vault it cannot read, so the
    /// `unconfigured()` fixture — which has no master key — refuses *every*
    /// announce and scrape with `invalid announce token` before the handler
    /// under test is reached. Any test asserting what happens after admission
    /// needs a real `SHARERR_MASTER_KEY`, and the vault it opens is empty, so
    /// no token is configured and admission is the "none required" path.
    ///
    /// `Jail` scopes the variable to this closure and serializes against every
    /// other Jail-based test in the binary; a bare `std::env::set_var` would do
    /// neither and race the parallel runner. `Jail` is not async, hence the
    /// plain `#[test]` callers and the runtime built here.
    fn with_open_vault<F, Fut>(body: F)
    where
        F: FnOnce(Arc<crate::state::ServeState>) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        figment::Jail::expect_with(|jail| {
            jail.set_env("SHARERR_MASTER_KEY", "tracker-tests-master-key");
            let config = sharerr_core::Config {
                data_dir: jail.directory().to_path_buf(),
                ..Default::default()
            };
            let path = jail.directory().join("sharerr.toml");
            let state = Arc::new(crate::state::ServeState::new(config, path, None));
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(body(state));
            Ok(())
        });
    }

    async fn get(state: &Arc<crate::state::ServeState>, uri: &str) -> (StatusCode, String) {
        let state = Arc::new(TrackerState::new(Arc::clone(state)));
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

        let response = routes(state).oneshot(request).await.unwrap();
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

    /// End to end, through the real router, for a torrent this instance
    /// actually shares: the announce is admitted and answered rather than
    /// refused. No prior test in this module exercises the success path —
    /// every existing one is a rejection.
    ///
    /// Needs [`with_open_vault`], not `unconfigured()`: admission fails closed
    /// on a vault it cannot read, so without a master key this never gets past
    /// [`authenticate`] to the behaviour it means to check. The vault it opens
    /// is empty, so no token is configured and this is the "none required"
    /// path — the peer-hash branch is proven by `authenticate_token`'s own unit
    /// tests and by `a_successful_attribution_records_the_clients_address`.
    #[test]
    fn an_announce_for_a_shared_torrent_is_admitted_and_answered() {
        with_open_vault(|state| async move {
            use sharerr_core::model::{ExternalIds, MediaSource, MediaSpec, ShareState, SharedItem};

            let store = state.store().await.unwrap();

            let hash_hex = "00".repeat(20);
            store
                .upsert(&SharedItem {
                    id: None,
                    source: MediaSource::Sonarr,
                    source_id: 1,
                    file_id: 1,
                    spec: MediaSpec::Episode {
                        series_title: "Lanternwick Hollow".to_owned(),
                        season: 1,
                        episode: 1,
                    },
                    release_title: "Lanternwick.Hollow.S01E01.WEB-DL.x264-SHARERR".to_owned(),
                    arr_path: std::path::PathBuf::from("/tv/s01e01.mkv"),
                    size: 1,
                    ids: ExternalIds::default(),
                    info_hash: None,
                    announce_token_fp: None,
                    created_by_sharerr: true,
                    state: ShareState::Pending,
                    last_error: None,
                    created_at: None,
                })
                .await
                .unwrap();
            store
                .set_seeding(MediaSource::Sonarr, 1, &hash_hex, None, true)
                .await
                .unwrap();

            let hash_query = "%00".repeat(20);
            let (status, body) = get(
                &state,
                &format!("/announce?info_hash={hash_query}&peer_id={hash_query}&port=6881"),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert!(!body.contains("failure reason"), "body was: {body}");
        });
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

    /// The other half of [`with_open_vault`]'s reason for existing: a vault that
    /// cannot be opened refuses announces rather than admitting them.
    ///
    /// [`authenticate`] deliberately fails closed here, because
    /// `tracker_token()`'s `None` cannot distinguish "no token configured" from
    /// "could not read the vault", and admitting on the latter would silently
    /// switch enforcement off after a transient error — a master-key file briefly
    /// unreadable during a mount or a rotation.
    ///
    /// The master key is set to the empty string rather than left alone: empty
    /// counts as unset (see `sharerr_store::vault::master_key_from`), so this
    /// pins "no master key" deterministically instead of inheriting whatever a
    /// concurrently-running Jail test happens to have set. Five tests in this
    /// module used to depend on that accident in the opposite direction — they
    /// asserted post-admission behaviour and passed only while some other test
    /// held a master key in the environment.
    #[test]
    fn an_announce_is_refused_when_the_vault_cannot_be_opened() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("SHARERR_MASTER_KEY", "");
            let config = sharerr_core::Config {
                data_dir: jail.directory().to_path_buf(),
                ..Default::default()
            };
            let path = jail.directory().join("sharerr.toml");
            let state = Arc::new(crate::state::ServeState::new(config, path, None));

            tokio::runtime::Runtime::new().unwrap().block_on(async {
                let hash = "%00".repeat(20);
                let (status, body) = get(
                    &state,
                    &format!("/announce?info_hash={hash}&peer_id={hash}&port=6881"),
                )
                .await;
                assert_eq!(status, StatusCode::OK, "trackers report refusals in-band");
                assert!(
                    body.contains("invalid announce token"),
                    "an unreadable vault must refuse, not admit: {body}"
                );
            });
            Ok(())
        });
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

    // ------------------------------------------------- per-peer download attribution

    /// Like `unconfigured`, but with an advertised host set — needed for
    /// anything that builds an [`sharerr_torrent::AnnounceSet`], which fails
    /// closed with no endpoint configured at all.
    fn with_advertised_host() -> (tempfile::TempDir, Arc<crate::state::ServeState>) {
        let dir = tempfile::tempdir().unwrap();
        let mut config = sharerr_core::config::Config {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        config.tracker.advertised_host = Some("seed.example".to_owned());
        let path = dir.path().join("sharerr.toml");
        (
            dir,
            Arc::new(crate::state::ServeState::new(config, path, None)),
        )
    }

    fn built_torrent(dir: &std::path::Path, announce_url: &str) -> sharerr_torrent::BuiltTorrent {
        let media = dir.join("movie.mkv");
        sharerr_testkit::media::write_media_file(&media, 4096, 1).unwrap();
        let announce = sharerr_torrent::AnnounceSet::single(url::Url::parse(announce_url).unwrap());
        sharerr_torrent::LavaTorrentFactory
            .create(&sharerr_torrent::TorrentRequest {
                path: &media,
                announce: &announce,
            })
            .unwrap()
    }

    /// The whole point of Stage 2: a `.torrent` download carrying a peer's own
    /// token gets an announce rewritten to that token, in memory — never the
    /// shared instance token the cached file on disk actually holds.
    #[tokio::test]
    async fn a_downloaded_torrent_is_attributed_to_the_peer_whose_token_it_carries() {
        let (dir, state) = with_advertised_host();
        let store = state.store().await.unwrap();
        let sam = store
            .create_peer("Sam", &SecretString::from("sam-key"), PeerScope::All)
            .await
            .unwrap();

        let built = built_torrent(
            dir.path(),
            "http://seed.example:8477/announce/shared-secret",
        );
        let tracker_state = TrackerState::new(Arc::clone(&state));

        let rewritten = attributed_bytes(&tracker_state, built.data.clone(), &sam.key_hash).await;

        let announce = sharerr_torrent::read_announce(&rewritten).unwrap().unwrap();
        assert!(
            announce.contains(&sam.key_hash),
            "expected Sam's own token in {announce}"
        );
        assert!(
            !announce.contains("shared-secret"),
            "the shared instance token must not leak into an attributed download: {announce}"
        );
    }

    /// A token that does not resolve to any currently active peer — unknown,
    /// or belonging to someone since revoked — must not break the download; it
    /// falls back to serving the file exactly as cached, same as no token at
    /// all. This is what keeps every download link from before this feature
    /// existed working unchanged.
    #[tokio::test]
    async fn an_unresolvable_token_falls_back_to_the_cached_file_unchanged() {
        let (dir, state) = with_advertised_host();
        let store = state.store().await.unwrap();
        let alex = store
            .create_peer("Alex", &SecretString::from("alex-key"), PeerScope::All)
            .await
            .unwrap();
        store.revoke_peer(alex.id).await.unwrap();

        let built = built_torrent(dir.path(), "http://seed.example:8477/announce");
        let tracker_state = TrackerState::new(Arc::clone(&state));

        for token in ["not-a-real-token", &alex.key_hash] {
            let result = attributed_bytes(&tracker_state, built.data.clone(), token).await;
            assert_eq!(
                result, built.data,
                "token {token:?} should not change the served bytes"
            );
        }
    }

    /// Bytes that do not parse as a `.torrent` at all — the cached file
    /// somehow corrupted — must still fall back to being served unchanged
    /// rather than the rewrite attempt propagating a failure to the caller.
    #[tokio::test]
    async fn attribution_falls_back_when_the_cached_bytes_do_not_parse_as_a_torrent() {
        let (_dir, state) = with_advertised_host();
        let store = state.store().await.unwrap();
        let sam = store
            .create_peer("Sam", &SecretString::from("sam-key"), PeerScope::All)
            .await
            .unwrap();
        let tracker_state = TrackerState::new(Arc::clone(&state));

        let garbage = b"not a bencoded torrent".to_vec();
        let result = attributed_bytes(&tracker_state, garbage.clone(), &sam.key_hash).await;
        assert_eq!(result, garbage);
    }

    // ------------------------------------------------------------- scrape

    /// A scrape naming no `info_hash` at all is refused with a specific
    /// reason rather than treated as "tell me about everything" — this
    /// tracker never enumerates its whole library.
    #[test]
    fn scrape_with_no_info_hash_is_refused() {
        with_open_vault(|state| async move {
            let (status, body) = get(&state, "/scrape").await;
            assert_eq!(status, StatusCode::OK, "trackers report refusals in-band");
            assert!(
                body.contains("this tracker only scrapes specific torrents"),
                "{body}"
            );
        });
    }

    /// An `info_hash` of the wrong length cannot be a real info hash — refused
    /// by shape before any store lookup happens.
    #[test]
    fn scrape_with_a_short_info_hash_is_refused() {
        with_open_vault(|state| async move {
            let (status, body) = get(&state, "/scrape?info_hash=%00").await;
            assert_eq!(status, StatusCode::OK);
            assert!(
                body.contains("info_hash must be exactly 20 bytes"),
                "{body}"
            );
        });
    }

    /// A well-formed hash for a torrent this instance does not share gets an
    /// empty scrape answer, not a failure — a client scraping something we
    /// withdrew should learn "no peers", not "something went wrong".
    #[test]
    fn scrape_for_an_unshared_torrent_is_not_a_failure() {
        with_open_vault(|state| async move {
            let hash = "%00".repeat(20);

            let (status, body) = get(&state, &format!("/scrape?info_hash={hash}")).await;
            assert_eq!(status, StatusCode::OK);
            assert!(!body.contains("failure reason"), "{body}");
        });
    }

    /// The success path: a torrent this instance actually shares gets real
    /// scrape counts back rather than the empty-files fallback above.
    #[test]
    fn scrape_for_a_shared_torrent_reports_it() {
        with_open_vault(|state| async move {
            use sharerr_core::model::{ExternalIds, MediaSource, MediaSpec, ShareState, SharedItem};

            let store = state.store().await.unwrap();

            let hash_hex = "00".repeat(20);
            store
                .upsert(&SharedItem {
                    id: None,
                    source: MediaSource::Sonarr,
                    source_id: 1,
                    file_id: 1,
                    spec: MediaSpec::Episode {
                        series_title: "Lanternwick Hollow".to_owned(),
                        season: 1,
                        episode: 1,
                    },
                    release_title: "Lanternwick.Hollow.S01E01.WEB-DL.x264-SHARERR".to_owned(),
                    arr_path: std::path::PathBuf::from("/tv/s01e01.mkv"),
                    size: 1,
                    ids: ExternalIds::default(),
                    info_hash: None,
                    announce_token_fp: None,
                    created_by_sharerr: true,
                    state: ShareState::Pending,
                    last_error: None,
                    created_at: None,
                })
                .await
                .unwrap();
            store
                .set_seeding(MediaSource::Sonarr, 1, &hash_hex, None, true)
                .await
                .unwrap();

            let hash_query = "%00".repeat(20);
            let (status, body) = get(&state, &format!("/scrape?info_hash={hash_query}")).await;
            assert_eq!(status, StatusCode::OK);
            assert!(!body.contains("failure reason"), "{body}");
        });
    }

    // -------------------------------------------------------- torrent_file

    /// The success path end to end: a cached `.torrent` for a shared torrent
    /// is served with the right headers and its bytes untouched when no
    /// per-peer token is supplied.
    #[tokio::test]
    async fn torrent_file_serves_the_cached_bytes_for_a_shared_torrent() {
        use sharerr_core::model::{ExternalIds, MediaSource, MediaSpec, ShareState, SharedItem};

        let (dir, state) = unconfigured();
        let store = state.store().await.unwrap();

        let built = built_torrent(dir.path(), "http://seed.example:8477/announce");
        let config = state.config().await;
        let torrent_dir = config.torrent_dir();
        tokio::fs::create_dir_all(&torrent_dir).await.unwrap();
        tokio::fs::write(
            sharerr_torrent::torrent_file_path(&torrent_dir, &built.info_hash),
            &built.data,
        )
        .await
        .unwrap();

        store
            .upsert(&SharedItem {
                id: None,
                source: MediaSource::Sonarr,
                source_id: 1,
                file_id: 1,
                spec: MediaSpec::Episode {
                    series_title: "Lanternwick Hollow".to_owned(),
                    season: 1,
                    episode: 1,
                },
                release_title: "Lanternwick.Hollow.S01E01.WEB-DL.x264-SHARERR".to_owned(),
                arr_path: std::path::PathBuf::from("/tv/s01e01.mkv"),
                size: 1,
                ids: ExternalIds::default(),
                info_hash: None,
                announce_token_fp: None,
                created_by_sharerr: true,
                state: ShareState::Pending,
                last_error: None,
                created_at: None,
            })
            .await
            .unwrap();
        store
            .set_seeding(MediaSource::Sonarr, 1, &built.info_hash, None, true)
            .await
            .unwrap();

        let uri = format!("/torrents/{}.torrent", built.info_hash);
        let state_arc = Arc::new(TrackerState::new(Arc::clone(&state)));
        let request = Request::builder().uri(&uri).body(Body::empty()).unwrap();
        let response = routes(state_arc).oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/x-bittorrent")
        );
        let disposition = response
            .headers()
            .get(header::CONTENT_DISPOSITION)
            .and_then(|v| v.to_str().ok())
            .unwrap()
            .to_owned();
        assert!(disposition.contains(&built.info_hash), "{disposition}");

        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        assert_eq!(bytes.as_ref(), built.data.as_slice());
    }
}
