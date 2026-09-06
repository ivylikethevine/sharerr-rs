//! Endpoint gossip: friends telling each other where they are.
//!
//! If A, B and C share with each other and A's address changes, B noticing first
//! should be enough for C to learn it — nobody should have to be reachable at
//! their *old* address in order to advertise the new one. Records ride the
//! existing peer-authenticated `/api` surface rather than a second protocol.
//!
//! # Trust model
//!
//! Every record is **signed by the peer it describes** (Ed25519), so a friend
//! can relay it but never rewrite it, and it carries a `signed_at` so an older
//! sighting cannot overwrite a newer one. A peer's public key is bound
//! trust-on-first-use from the first *self*-record they present over the API key
//! we issued them; from then on it is their identity, and a different key over
//! the same credential is refused rather than replacing it.
//!
//! # Who learns what
//!
//! A pull names the public keys the caller already knows
//! (`GET /api/gossip/endpoints?peers=pk1,pk2`), and the answer is the
//! intersection with our own peers — so nobody is told about the existence, let
//! alone the address, of a peer they are not already sharing with. This is
//! stricter than scoping by `PeerScope`: knowing the key *is* the proof of the
//! existing relationship.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use ed25519_dalek::{Signer, SigningKey};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use sharerr_client::error_chain;
use sharerr_core::config::secret_keys;
use sharerr_core::endpoint::{MAX_FUTURE_SKEW_SECS, now_epoch};
// `RecordEndpoint`, `EndpointRecord`, `verify`, and `signable_bytes` are
// `sharerr_lighthouse`'s, re-exported rather than redeclared — the
// lighthouse relays the identical wire format gossip uses, per the design
// brief, and `sharerr` already depends on `sharerr-lighthouse`, so nothing
// stops the two sharing one definition. A record signed for gossip must
// verify unchanged against a lighthouse, and vice versa.
pub use sharerr_lighthouse::{EndpointRecord, RecordEndpoint, signable_bytes, verify};
use sharerr_store::{EndpointKind, ObservedVia, Store};

use crate::state::ServeState;
use crate::torznab::Caller;

/// Cap on records accepted in one POST — a friend relays a friend list, not a
/// crawl of the internet.
const MAX_RECORDS: usize = 64;

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

/// The wire shape of both gossip endpoints' bodies.
#[derive(Debug, Default, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = GossipRecordBatch)]
pub struct RecordBatch {
    pub records: Vec<EndpointRecord>,
}

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// This instance's signing identity.
pub struct Identity {
    signing: SigningKey,
}

impl std::fmt::Debug for Identity {
    /// Hand-written so the private half cannot reach a log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("pubkey", &self.pubkey_hex())
            .finish()
    }
}

impl Identity {
    /// Load the signing key from the vault, minting one on first use.
    pub fn load_or_create(vault: &mut sharerr_store::Vault) -> Result<Self, String> {
        let (seed, minted) = crate::secrets::load_or_create_seed(
            vault,
            secret_keys::IDENTITY_SIGNING_KEY,
            "identity key",
        )?;
        let signing = SigningKey::from_bytes(&seed);
        if minted {
            tracing::info!(pubkey = %hex::encode(signing.verifying_key().to_bytes()), "minted a gossip identity");
        }
        Ok(Self { signing })
    }

    pub fn pubkey_hex(&self) -> String {
        hex::encode(self.signing.verifying_key().to_bytes())
    }

    /// Produce this instance's signed self-record.
    pub fn sign_record(
        &self,
        endpoints: Vec<RecordEndpoint>,
        signed_at: i64,
    ) -> Result<EndpointRecord, String> {
        let pubkey = self.pubkey_hex();
        let bytes = signable_bytes(&pubkey, &endpoints, signed_at)
            .map_err(|err| format!("serialising the record: {err}"))?;
        let signature = hex::encode(self.signing.sign(&bytes).to_bytes());
        Ok(EndpointRecord {
            pubkey,
            endpoints,
            signed_at,
            signature,
        })
    }
}

/// This instance's current self-record: identity from the vault, endpoints from
/// the live advertised bases. `None` when the vault (and so the identity) is
/// unavailable — gossip still relays without it, it just cannot speak for
/// itself.
pub(crate) async fn self_record(state: &ServeState) -> Option<EndpointRecord> {
    let Some(identity) = state.gossip_identity().await else {
        tracing::debug!("no gossip identity available");
        return None;
    };

    let now = now_epoch();
    let mut endpoints = Vec::new();
    if let Some(base) = state.endpoint().current() {
        let addr = sharerr_core::endpoint::base_string(&base);
        // Tracker and Api share one listener today, so they always carry the
        // same address; recorded separately so a friend who only understands
        // one of the two kinds still gets it.
        endpoints.push(RecordEndpoint {
            kind: EndpointKind::Tracker.as_str().to_owned(),
            addr: addr.clone(),
            observed_at: now,
        });
        endpoints.push(RecordEndpoint {
            kind: EndpointKind::Api.as_str().to_owned(),
            addr,
            observed_at: now,
        });
    }
    // Unlike Tracker/Api, Client is genuinely independent. Present only once
    // `[gluetun_client]` (or some other future source) has actually observed
    // the torrent client's own address; absent is honest where nothing knows
    // it, rather than repeating the tracker's address as a guess.
    if let Some(base) = state.client_endpoint().current() {
        endpoints.push(RecordEndpoint {
            kind: EndpointKind::Client.as_str().to_owned(),
            addr: sharerr_core::endpoint::base_string(&base),
            observed_at: now,
        });
    }

    match identity.sign_record(endpoints, now) {
        Ok(record) => Some(record),
        Err(reason) => {
            tracing::warn!(reason, "could not sign a self-record");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Ingest
// ---------------------------------------------------------------------------

/// What one batch of records amounted to, for logging and the POST response.
#[derive(Debug, Default, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[schema(as = GossipIngestSummary)]
pub struct IngestSummary {
    pub accepted: usize,
    /// Signature or shape failures — records nobody should have sent.
    pub invalid: usize,
    /// Valid records about peers we do not share with; ignored by design.
    pub unknown: usize,
    /// Valid records no newer than what is already stored.
    pub stale: usize,
}

/// Take a batch of records presented by an authenticated peer.
///
/// The presenter matters twice: their own self-record is what TOFU-binds their
/// pubkey, and endpoints from a self-record are first-hand (`direct`) where
/// relayed ones are `gossip`.
pub async fn ingest(
    store: &Store,
    presenter_id: i64,
    records: Vec<EndpointRecord>,
) -> IngestSummary {
    let mut summary = IngestSummary::default();

    let peers = match store.list_peers().await {
        Ok(peers) => peers,
        Err(err) => {
            tracing::warn!(error = %err, "could not list peers for gossip ingest");
            return summary;
        }
    };

    let now = now_epoch();
    // Identity is the pubkey, nothing else — indexed once rather than scanned
    // per record.
    let by_pubkey: std::collections::HashMap<&str, &sharerr_store::Peer> = peers
        .iter()
        .filter_map(|p| p.pubkey.as_deref().map(|pubkey| (pubkey, p)))
        .collect();

    for record in records.into_iter().take(MAX_RECORDS) {
        if let Err(reason) = verify(&record) {
            tracing::debug!(reason, "rejected a gossip record");
            summary.invalid += 1;
            continue;
        }
        // A `signed_at` further in the future than clock skew explains is
        // treated the same as a bad signature: shape freshness is decided
        // purely by comparing `signed_at` values below, so an unclamped
        // future one would lock the subject's slot — every genuine update
        // reads as `stale` — until this host's clock catches up to it, which
        // for a wildly wrong sender clock is never.
        if record.signed_at > now.saturating_add(MAX_FUTURE_SKEW_SECS) {
            tracing::debug!("rejected a gossip record signed too far in the future");
            summary.invalid += 1;
            continue;
        }

        // Who is this record about?
        let subject_id = match by_pubkey.get(record.pubkey.as_str()) {
            Some(subject) if !subject.is_revoked() => subject.id,
            Some(_) => {
                summary.unknown += 1;
                continue;
            }
            None => {
                // Unbound pubkey. The one legitimate case is the presenter's own
                // first self-record: their API key authenticated the request, so
                // the key they sign with becomes theirs — trust on first use.
                let presenter = peers.iter().find(|p| p.id == presenter_id);
                let presenter_unbound = presenter.is_some_and(|p| p.pubkey.is_none());
                if presenter_unbound
                    && store
                        .bind_peer_pubkey(presenter_id, &record.pubkey)
                        .await
                        .unwrap_or(false)
                {
                    tracing::info!(
                        peer = presenter_id,
                        pubkey = %record.pubkey,
                        "bound a peer to their gossip identity"
                    );
                    presenter_id
                } else {
                    // A record about somebody we do not know — or a presenter
                    // trying to present a second identity. Ignored either way:
                    // gossip must not teach us about strangers.
                    summary.unknown += 1;
                    continue;
                }
            }
        };

        // Freshness: never let an older record rewind a newer one. Only the
        // timestamp is read out of the stored record — the endpoints and the
        // signature would be deserialised and dropped.
        #[derive(Deserialize)]
        struct SignedAt {
            signed_at: i64,
        }
        let stored_signed_at = store
            .peer_gossip_record(subject_id)
            .await
            .ok()
            .flatten()
            .and_then(|raw| serde_json::from_str::<SignedAt>(&raw).ok())
            .map(|stored| stored.signed_at);
        if stored_signed_at.is_some_and(|stored| stored >= record.signed_at) {
            summary.stale += 1;
            continue;
        }

        let via = if subject_id == presenter_id {
            ObservedVia::Direct
        } else {
            ObservedVia::Gossip
        };
        for endpoint in &record.endpoints {
            let Some(kind) = EndpointKind::parse(&endpoint.kind) else {
                continue;
            };
            if let Err(err) = store
                .record_peer_endpoint(
                    subject_id,
                    kind,
                    &endpoint.addr,
                    Some(endpoint.observed_at),
                    via,
                )
                .await
            {
                tracing::warn!(error = %err, "could not record a gossiped endpoint");
            }
        }

        match serde_json::to_string(&record) {
            Ok(raw) => {
                if let Err(err) = store.set_peer_gossip_record(subject_id, &raw).await {
                    tracing::warn!(error = %err, "could not store a gossip record");
                }
            }
            Err(err) => tracing::warn!(error = %err, "could not serialise a gossip record"),
        }
        summary.accepted += 1;
    }

    summary
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct PullQuery {
    /// Comma-separated hex pubkeys the caller already knows.
    #[serde(default)]
    peers: String,
}

/// `GET /api/gossip/endpoints?peers=pk1,pk2` — the pull side.
#[utoipa::path(
    get,
    path = "/api/gossip/endpoints",
    tag = "gossip",
    operation_id = "gossipPull",
    security(("peerApiKey" = [])),
    params(
        ("peers" = Option<String>, Query, description =
         "Comma-separated hex pubkeys the caller already knows, so the answer can \
          skip them."),
    ),
    responses(
        (status = 200, description =
         "Signed endpoint records: this instance's own first, then any it holds for \
          peers it shares with. Each is signed by the peer it describes, so nothing \
          here has to be trusted on the relayer's word.", body = RecordBatch),
        (status = 401, content_type = "application/xml", description =
         "No `apikey`, or one that matches no active peer. Answered as Torznab's own \
          XML error, the same as the feed — this rides the feed's authentication.",
         body = String),
        (status = 503, description = "The database is not open yet.", body = String),
    ),
)]
pub async fn pull(
    State(state): State<Arc<ServeState>>,
    // Unused beyond authenticating the caller — the extractor is what rejects an
    // unauthenticated request; the pull side has nothing further to check per-peer.
    _caller: Caller,
    Query(query): Query<PullQuery>,
) -> Response {
    let Ok(store) = state.store().await else {
        return (StatusCode::SERVICE_UNAVAILABLE, "not ready").into_response();
    };

    let mut batch = RecordBatch::default();
    if let Some(own) = self_record(&state).await {
        batch.records.push(own);
    }

    let wanted: std::collections::HashSet<&str> = query
        .peers
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if !wanted.is_empty()
        && let Ok(peers) = store.list_peers().await
    {
        // The intersection rule: only relay records for peers the caller
        // proved they already know by naming the pubkey.
        let matched_ids: Vec<i64> = peers
            .iter()
            .filter(|p| !p.is_revoked())
            .filter_map(|peer| {
                let pubkey = peer.pubkey.as_deref()?;
                wanted.contains(pubkey).then_some(peer.id)
            })
            .collect();

        // One round trip for every matched peer's record, rather than
        // awaiting `peer_gossip_record` once per peer inside this loop.
        if let Ok(records) = store.peer_gossip_records(&matched_ids).await {
            for id in &matched_ids {
                if let Some(raw) = records.get(id)
                    && let Ok(record) = serde_json::from_str::<EndpointRecord>(raw)
                {
                    batch.records.push(record);
                }
            }
        }
    }

    axum::Json(batch).into_response()
}

/// `POST /api/gossip/endpoints` — the push side, for a friend whose address
/// changed and who can therefore no longer be pulled from.
#[utoipa::path(
    post,
    path = "/api/gossip/endpoints",
    tag = "gossip",
    operation_id = "gossipPush",
    security(("peerApiKey" = [])),
    request_body = RecordBatch,
    responses(
        (status = 200, description =
         "What the batch amounted to. Records about peers this instance does not \
          share with are counted `unknown` and dropped by design, not rejected — a \
          friend relaying their whole view is normal.", body = IngestSummary),
        (status = 401, content_type = "application/xml",
         description = "No `apikey`, or one that matches no active peer.", body = String),
        (status = 503, description = "The database is not open yet.", body = String),
    ),
)]
pub async fn push(
    State(state): State<Arc<ServeState>>,
    caller: Caller,
    axum::Json(batch): axum::Json<RecordBatch>,
) -> Response {
    let presenter = caller.peer_id();
    let Ok(store) = state.store().await else {
        return (StatusCode::SERVICE_UNAVAILABLE, "not ready").into_response();
    };

    let summary = ingest(&store, presenter, batch.records).await;
    tracing::debug!(?summary, presenter, "gossip push");
    axum::Json(summary).into_response()
}

// ---------------------------------------------------------------------------
// Outbound exchange
// ---------------------------------------------------------------------------

/// Periodically exchange records with every friend whose sharerr we know how to
/// reach. Never returns.
pub async fn exchange_loop(state: Arc<ServeState>) {
    // Built once and reused for the life of the poller — the same pattern
    // gluetun::poll_loop uses, and for the same reason: `reqwest::Client` is
    // an Arc internally, so building a fresh one every exchange discards a
    // live connection pool for nothing. A build failure is kept rather than
    // retried every interval; nothing here can fix a broken TLS backend by
    // trying again in fifteen minutes.
    let http = sharerr_client::http_client_with_timeout(Duration::from_secs(15))
        .map_err(|e| e.to_string());

    loop {
        let outcome = match &http {
            Ok(http) => run_exchange(&state, http).await,
            Err(reason) => Err(reason.clone()),
        };
        if let Err(reason) = outcome {
            tracing::debug!(reason, "gossip exchange skipped");
        }
        // Read fresh each pass, the same as `sync`'s own loop
        // (`commands::serve::background`) — cheap, and it means a changed
        // `gossip.exchange_secs` takes effect on the next tick rather than
        // needing a restart.
        let interval = state.with_config(|c| c.gossip.exchange_secs).await;
        tokio::time::sleep(Duration::from_secs(interval)).await;
    }
}

async fn run_exchange(state: &Arc<ServeState>, http: &reqwest::Client) -> Result<(), String> {
    let store = state.store().await?;
    let peers = store.list_peers().await.map_err(|e| e.to_string())?;

    let outbound: Vec<_> = peers
        .iter()
        .filter(|p| !p.is_revoked() && p.gossip_url.is_some())
        .collect();
    if outbound.is_empty() {
        return Ok(());
    }

    // What we will ask about: every identity we already know. Naming them is
    // what keeps the exchange inside existing relationships.
    let known: Vec<&str> = peers.iter().filter_map(|p| p.pubkey.as_deref()).collect();
    let own = self_record(state).await;

    // Opened fresh every exchange, unlike `http` above: a peer's outbound key
    // can rotate between ticks (a re-friending, a revoke-and-reissue), and a
    // stale in-memory copy would gossip under a key the peer no longer
    // recognises. `http` carries no such per-tick state, which is the whole
    // difference.
    let vault = state.open_vault().await?;

    // Concurrently: the friends are independent hosts behind independent keys,
    // and each exchange can sit on the 15s timeout above. In series, one friend
    // behind a dead tunnel delayed every friend after them — with five friends
    // and two unreachable, a single pass burned 30s of wall time doing nothing.
    let (store, own, known) = (&store, own.as_ref(), &known);
    let exchanges = outbound.into_iter().filter_map(|peer| {
        let Ok(Some(key)) = vault.get(&secret_keys::peer_gossip_key(peer.id)) else {
            tracing::debug!(peer = %peer.label, "no outbound key stored — skipping gossip");
            return None;
        };

        Some(async move {
            // `filter` above; this cannot fire but the type does not know.
            let Some(url) = peer.gossip_url.as_deref() else {
                return;
            };

            if let Err(reason) =
                exchange_with(http, store, peer.id, url, key.expose_secret(), own, known).await
            {
                tracing::debug!(peer = %peer.label, reason, "gossip exchange failed");
            }
        })
    });
    futures::future::join_all(exchanges).await;

    Ok(())
}

/// One push-then-pull against one friend's sharerr.
async fn exchange_with(
    http: &reqwest::Client,
    store: &Store,
    peer_id: i64,
    base: &str,
    key: &str,
    own: Option<&EndpointRecord>,
    known: &[&str],
) -> Result<(), String> {
    let base = base.trim_end_matches('/');
    let endpoint = format!("{base}/api/gossip/endpoints");

    // The key and the peer list ride as real query parameters rather than being
    // formatted into the URL: reqwest escapes them, and a key containing a `&`
    // would otherwise silently truncate the request.
    //
    // `error_chain` rather than `{e}` on the sends — reqwest's own Display stops
    // at "error sending request for url (…)" and drops the cause, which is the
    // "Connection refused" an operator actually needs.
    if let Some(own) = own {
        let batch = RecordBatch {
            records: vec![own.clone()],
        };
        http.post(&endpoint)
            .query(&[("apikey", key)])
            .json(&batch)
            .send()
            .await
            .map_err(|e| format!("push: {}", error_chain(&e)))?;
    }

    let response = http
        .get(&endpoint)
        .query(&[("apikey", key), ("peers", &known.join(","))])
        .send()
        .await
        .map_err(|e| format!("pull: {}", error_chain(&e)))?;
    if !response.status().is_success() {
        return Err(format!("pull answered {}", response.status()));
    }
    let batch: RecordBatch = response
        .json()
        .await
        .map_err(|e| format!("pull body: {e}"))?;

    let summary = ingest(store, peer_id, batch.records).await;
    if summary.accepted > 0 {
        tracing::info!(
            peer = peer_id,
            accepted = summary.accepted,
            "gossip ingested"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::result_large_err)]

    use super::*;
    use crate::test_support::vault_in;
    use secrecy::SecretString;
    use sharerr_store::PeerScope;

    /// Distinct, non-constant key material for a test identity.
    ///
    /// `seed` distinguishes one test identity from another — 1 is Alex, 2 is
    /// Sam — and is deliberately *not* the key itself. The key is that label
    /// mixed into a per-run random base, so no cryptographic key is hard-coded
    /// in the tree while identities stay distinct, and stable within a run.
    ///
    /// Ed25519 accepts any 32 bytes as a seed, so mixing this way is sound.
    fn test_key_bytes(seed: u8) -> [u8; 32] {
        static BASE: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
        let mut bytes = *BASE.get_or_init(|| {
            let mut base = [0u8; 32];
            getrandom::fill(&mut base).expect("the OS RNG is available");
            base
        });
        // XOR into one byte: distinct labels stay distinct.
        bytes[0] ^= seed;
        bytes
    }

    fn identity(seed: u8) -> Identity {
        Identity {
            signing: SigningKey::from_bytes(&test_key_bytes(seed)),
        }
    }

    fn record_for(id: &Identity, addr: &str, signed_at: i64) -> EndpointRecord {
        id.sign_record(
            vec![RecordEndpoint {
                kind: "tracker".to_owned(),
                addr: addr.to_owned(),
                observed_at: signed_at,
            }],
            signed_at,
        )
        .unwrap()
    }

    async fn store_with(labels: &[&str]) -> (Store, Vec<i64>) {
        let store = Store::open_in_memory().await.unwrap();
        let mut ids = Vec::new();
        for label in labels {
            let peer = store
                .create_peer(
                    label,
                    &SecretString::from(format!("{label}-key")),
                    PeerScope::All,
                )
                .await
                .unwrap();
            ids.push(peer.id);
        }
        (store, ids)
    }

    #[test]
    fn load_or_create_mints_an_identity_and_then_reloads_the_same_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = vault_in(&dir);

        let minted = Identity::load_or_create(&mut vault).unwrap();
        let pubkey = minted.pubkey_hex();

        let reloaded = Identity::load_or_create(&mut vault).unwrap();
        assert_eq!(
            reloaded.pubkey_hex(),
            pubkey,
            "a second load must return the same identity, not mint a fresh one"
        );
    }

    #[test]
    fn a_corrupt_stored_key_is_reported_rather_than_silently_reminted() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = vault_in(&dir);
        vault
            .put(
                secret_keys::IDENTITY_SIGNING_KEY,
                &SecretString::from("not-hex"),
            )
            .unwrap();

        assert!(Identity::load_or_create(&mut vault).is_err());
    }

    #[test]
    fn a_signed_record_verifies_and_a_tampered_one_does_not() {
        let id = identity(1);
        let record = record_for(&id, "http://203.0.113.9:41234", 1000);
        assert!(verify(&record).is_ok());

        let mut tampered = record.clone();
        tampered.endpoints[0].addr = "http://attacker.example:1".to_owned();
        assert!(
            verify(&tampered).is_err(),
            "no friend may rewrite somebody else's address"
        );

        let mut rewound = record.clone();
        rewound.signed_at -= 1;
        assert!(verify(&rewound).is_err(), "the timestamp is signed too");
    }

    /// TOFU: the presenter's first self-record binds their pubkey; a different
    /// identity presented later over the same credential is refused.
    #[tokio::test]
    async fn the_first_self_record_binds_the_presenters_identity() {
        let (store, ids) = store_with(&["Sam"]).await;
        let sam = identity(1);

        let summary = ingest(&store, ids[0], vec![record_for(&sam, "http://a:1", 1000)]).await;
        assert_eq!(summary.accepted, 1);

        let peers = store.list_peers().await.unwrap();
        assert_eq!(peers[0].pubkey.as_deref(), Some(sam.pubkey_hex().as_str()));

        // A second identity over the same key is an impersonation, not a rebind.
        let impostor = identity(2);
        let summary = ingest(
            &store,
            ids[0],
            vec![record_for(&impostor, "http://evil:1", 2000)],
        )
        .await;
        assert_eq!(summary.accepted, 0);
        assert_eq!(summary.unknown, 1);
    }

    /// The relay case gossip exists for: B presents A's signed record, and it
    /// lands on A's peer row — marked as gossip, not as a first-hand sighting.
    #[tokio::test]
    async fn a_relayed_record_reaches_the_subjects_row() {
        let (store, ids) = store_with(&["Alex", "Blair"]).await;
        let alex = identity(1);

        // Alex speaks for themselves once, binding their identity.
        ingest(
            &store,
            ids[0],
            vec![record_for(&alex, "http://old:1", 1000)],
        )
        .await;

        // Blair relays Alex's newer record.
        let summary = ingest(
            &store,
            ids[1],
            vec![record_for(&alex, "http://new:2", 2000)],
        )
        .await;
        assert_eq!(summary.accepted, 1);

        let endpoints = store.peer_endpoints(ids[0]).await.unwrap();
        assert_eq!(endpoints[0].addr, "http://new:2");
        assert_eq!(endpoints[0].via, ObservedVia::Gossip);
    }

    /// An older record must not rewind a newer one, however it arrives.
    #[tokio::test]
    async fn an_older_record_is_stale_not_accepted() {
        let (store, ids) = store_with(&["Alex", "Blair"]).await;
        let alex = identity(1);

        ingest(
            &store,
            ids[0],
            vec![record_for(&alex, "http://new:2", 2000)],
        )
        .await;
        let summary = ingest(
            &store,
            ids[1],
            vec![record_for(&alex, "http://old:1", 1000)],
        )
        .await;

        assert_eq!(summary.accepted, 0);
        assert_eq!(summary.stale, 1);
        let endpoints = store.peer_endpoints(ids[0]).await.unwrap();
        assert_eq!(endpoints[0].addr, "http://new:2");
    }

    /// A `signed_at` further in the future than clock skew explains is
    /// rejected outright, not merely deferred: accepting it would let it win
    /// every freshness comparison forever, past the point the sender's clock
    /// is fixed.
    #[tokio::test]
    async fn a_record_signed_too_far_in_the_future_is_rejected() {
        let (store, ids) = store_with(&["Alex"]).await;
        let alex = identity(1);
        let far_future = now_epoch() + MAX_FUTURE_SKEW_SECS + 3600;

        let summary = ingest(
            &store,
            ids[0],
            vec![record_for(&alex, "http://a:1", far_future)],
        )
        .await;

        assert_eq!(summary.accepted, 0);
        assert_eq!(summary.invalid, 1);
        assert!(store.peer_endpoints(ids[0]).await.unwrap().is_empty());
    }

    /// A record about a pubkey no peer row carries is ignored: gossip must not
    /// teach us about strangers.
    #[tokio::test]
    async fn records_about_strangers_are_ignored() {
        let (store, ids) = store_with(&["Sam"]).await;
        let sam = identity(1);
        ingest(&store, ids[0], vec![record_for(&sam, "http://sam:1", 1000)]).await;

        let stranger = identity(9);
        let summary = ingest(
            &store,
            ids[0],
            vec![record_for(&stranger, "http://stranger:1", 2000)],
        )
        .await;

        assert_eq!(summary.accepted, 0);
        assert_eq!(summary.unknown, 1);
        assert_eq!(store.list_peers().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_invalid_signature_is_rejected() {
        let (store, ids) = store_with(&["Sam"]).await;
        let mut record = record_for(&identity(1), "http://a:1", 1000);
        record.signature = "00".repeat(64);

        let summary = ingest(&store, ids[0], vec![record]).await;
        assert_eq!(summary.invalid, 1);
        assert_eq!(summary.accepted, 0);
    }

    // ------------------------------------------------- router-level coverage

    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    /// The assembled `/api` router, over a state holding the named peers.
    async fn served(labels: &[&str]) -> (tempfile::TempDir, Arc<ServeState>) {
        let (dir, state) = crate::state::fixtures::unconfigured();
        let store = state.store().await.unwrap();
        for label in labels {
            store
                .create_peer(
                    label,
                    &SecretString::from(format!("{label}-key")),
                    PeerScope::All,
                )
                .await
                .unwrap();
        }
        (dir, state)
    }

    async fn request(
        state: &Arc<ServeState>,
        method: &str,
        uri: &str,
        body: Option<&RecordBatch>,
    ) -> (StatusCode, String) {
        let mut builder = Request::builder().method(method).uri(uri);
        let body = match body {
            Some(batch) => {
                builder = builder.header("content-type", "application/json");
                Body::from(serde_json::to_vec(batch).unwrap())
            }
            None => Body::empty(),
        };
        let response = crate::torznab::routes(Arc::clone(state))
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    /// The gossip surface is behind the same closed door as the feed.
    #[tokio::test]
    async fn gossip_requires_a_peer_key() {
        let (_dir, state) = served(&["Sam"]).await;

        let (status, _) = request(&state, "GET", "/api/gossip/endpoints", None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let (status, _) = request(&state, "GET", "/api/gossip/endpoints?apikey=wrong", None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    /// The full exchange over HTTP: Alex's sharerr pushes their self-record,
    /// then Blair — naming Alex's pubkey, proving they already know them —
    /// pulls it back out. A pull naming nobody gets nobody.
    #[tokio::test]
    async fn a_pushed_record_is_relayed_only_to_those_who_name_its_owner() {
        let (_dir, state) = served(&["Alex", "Blair"]).await;
        let alex = identity(1);
        let record = record_for(&alex, "http://203.0.113.9:41234", 1000);

        let (status, _) = request(
            &state,
            "POST",
            "/api/gossip/endpoints?apikey=Alex-key",
            Some(&RecordBatch {
                records: vec![record.clone()],
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // Blair names Alex's pubkey and gets the record.
        let (status, body) = request(
            &state,
            "GET",
            &format!(
                "/api/gossip/endpoints?apikey=Blair-key&peers={}",
                alex.pubkey_hex()
            ),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("203.0.113.9"), "{body}");

        // Naming nobody yields nothing — the intersection rule.
        let (_, body) = request(
            &state,
            "GET",
            "/api/gossip/endpoints?apikey=Blair-key",
            None,
        )
        .await;
        assert!(
            !body.contains("203.0.113.9"),
            "a pull must not volunteer records the caller did not prove they know: {body}"
        );
    }

    // ------------------------------------------------------- outbound exchange

    #[tokio::test]
    async fn exchange_with_pushes_the_own_record_and_ingests_the_pull() {
        let server = wiremock::MockServer::start().await;
        let (store, ids) = store_with(&["Alex"]).await;
        let alex = identity(1);
        let own = record_for(&alex, "http://me:1", 1000);
        let friend = record_for(&identity(2), "http://friend:2", 2000);

        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/api/gossip/endpoints"))
            .respond_with(wiremock::ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/gossip/endpoints"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(RecordBatch {
                    records: vec![friend],
                }),
            )
            .expect(1)
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        exchange_with(
            &http,
            &store,
            ids[0],
            &server.uri(),
            "outbound-key",
            Some(&own),
            &[],
        )
        .await
        .unwrap();

        // The pulled record names a peer we don't share with, so it lands as
        // "unknown" rather than a new row — the assertion that matters here is
        // that the pull's body actually reached `ingest`, not that it was
        // accepted.
        assert_eq!(store.list_peers().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn exchange_with_skips_the_push_when_there_is_no_own_record() {
        let server = wiremock::MockServer::start().await;
        let (store, ids) = store_with(&["Alex"]).await;

        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(RecordBatch::default()),
            )
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        exchange_with(&http, &store, ids[0], &server.uri(), "k", None, &[])
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn exchange_with_reports_a_non_success_pull_status_as_an_error() {
        let server = wiremock::MockServer::start().await;
        let (store, ids) = store_with(&["Alex"]).await;

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let http = reqwest::Client::new();
        let err = exchange_with(&http, &store, ids[0], &server.uri(), "k", None, &[])
            .await
            .unwrap_err();
        assert!(err.contains("500"), "{err}");
    }

    #[tokio::test]
    async fn exchange_with_reports_an_unreachable_host_as_an_error_naming_the_cause() {
        let (store, ids) = store_with(&["Alex"]).await;
        let http = reqwest::Client::new();

        // Port 0 never accepts a connection — a stand-in for "the friend's
        // sharerr is offline" without depending on any real network.
        let err = exchange_with(&http, &store, ids[0], "http://127.0.0.1:0", "k", None, &[])
            .await
            .unwrap_err();
        assert!(
            err.starts_with("push: ") || err.starts_with("pull: "),
            "{err}"
        );
    }

    #[tokio::test]
    async fn run_exchange_is_a_no_op_when_no_peer_has_a_gossip_url() {
        let (_dir, state) = served(&["Sam"]).await;
        let http = reqwest::Client::new();
        assert!(run_exchange(&state, &http).await.is_ok());
    }

    /// `self_record` and `run_exchange`'s full push/pull path both depend on a
    /// real identity — which means a vault backed by an actual
    /// `SHARERR_MASTER_KEY`, not the `state::fixtures::unconfigured()` fixture
    /// every other test in this module uses (deliberately vault-less, per this
    /// repo's CLAUDE.md). `secrets.rs` already has a `#[test]` that legitimately
    /// sets that env var via `figment::Jail`, so — same reasoning as
    /// `sync::tests::build_succeeds_with_a_configured_library_and_torrent_client`
    /// — these run inside a `Jail` too, which clears/serializes the env instead
    /// of racing it, and drive their own runtime rather than `#[tokio::test]`'s
    /// (which would already hold one on this thread).
    #[test]
    fn self_record_signs_the_current_tracker_api_and_client_endpoints() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("SHARERR_MASTER_KEY", "a-master-key");
            let config = sharerr_core::Config {
                data_dir: jail.directory().to_path_buf(),
                ..sharerr_core::Config::default()
            };
            let state = ServeState::new(config, jail.directory().join("sharerr.toml"), None);
            state
                .endpoint()
                .set_static(Some(url::Url::parse("http://198.51.100.5:6881/").unwrap()));
            state
                .client_endpoint()
                .set_static(Some(url::Url::parse("http://198.51.100.9:9091/").unwrap()));

            let runtime = tokio::runtime::Runtime::new().unwrap();
            let record = runtime
                .block_on(self_record(&state))
                .expect("an identity should mint on first use and the record should sign");

            let kinds: Vec<&str> = record.endpoints.iter().map(|e| e.kind.as_str()).collect();
            assert!(kinds.contains(&"tracker"), "{kinds:?}");
            assert!(kinds.contains(&"api"), "{kinds:?}");
            assert!(
                kinds.contains(&"client"),
                "the client endpoint is independent of tracker/api and must appear once observed: {kinds:?}"
            );
            assert!(
                verify(&record).is_ok(),
                "self_record must sign, not just assemble"
            );
            Ok(())
        });
    }

    /// The full outbound exchange, end to end: a friend with a stored key and a
    /// `gossip_url` gets pushed our record and pulled from — proving
    /// `run_exchange` actually wires `self_record`, the vault-stored per-peer
    /// key, and `exchange_with` together, not just each piece in isolation.
    #[test]
    fn run_exchange_pushes_and_pulls_a_friend_with_a_stored_key_and_url() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("SHARERR_MASTER_KEY", "a-master-key");
            let config = sharerr_core::Config {
                data_dir: jail.directory().to_path_buf(),
                ..sharerr_core::Config::default()
            };
            let state = Arc::new(ServeState::new(
                config.clone(),
                jail.directory().join("sharerr.toml"),
                None,
            ));

            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async {
                let server = wiremock::MockServer::start().await;
                wiremock::Mock::given(wiremock::matchers::method("POST"))
                    .and(wiremock::matchers::path("/api/gossip/endpoints"))
                    .respond_with(wiremock::ResponseTemplate::new(200))
                    .expect(1)
                    .mount(&server)
                    .await;
                wiremock::Mock::given(wiremock::matchers::method("GET"))
                    .and(wiremock::matchers::path("/api/gossip/endpoints"))
                    .respond_with(
                        wiremock::ResponseTemplate::new(200).set_body_json(RecordBatch::default()),
                    )
                    .expect(1)
                    .mount(&server)
                    .await;

                let store = state.store().await.unwrap();
                let peer = store
                    .create_peer("Friend", &SecretString::from("friend-key"), PeerScope::All)
                    .await
                    .unwrap();
                store
                    .set_peer_gossip_url(peer.id, Some(&server.uri()))
                    .await
                    .unwrap();

                // The outbound key `run_exchange` looks up per peer — stored
                // directly via a `Vault` opened on the same path/master key
                // `state.open_vault()` will resolve, matching this repo's
                // convention of exercising vault-backed logic through the plain
                // `Vault` API rather than routing test setup through the web layer.
                let mut vault = sharerr_store::Vault::open(
                    config.vault_path(),
                    &SecretString::from("a-master-key"),
                )
                .unwrap();
                vault
                    .put(
                        &secret_keys::peer_gossip_key(peer.id),
                        &SecretString::from("outbound-key"),
                    )
                    .unwrap();
                drop(vault);

                let http = reqwest::Client::new();
                run_exchange(&state, &http).await.unwrap();
            });
            Ok(())
        });
    }

    /// The Debug impl exists specifically so the private signing key can never
    /// reach a log line — assert the redaction, not just that it compiles.
    #[test]
    fn identity_debug_does_not_expose_the_private_key() {
        let id = identity(7);
        let debug = format!("{id:?}");
        assert!(debug.contains(&id.pubkey_hex()));
        assert!(
            !debug.contains(&hex::encode(test_key_bytes(7))),
            "the private key bytes must never appear in Debug output: {debug}"
        );
    }

    /// `ingest` treats a record about an already-revoked peer the same as one
    /// about a total stranger: `unknown`, never applied — revocation must not
    /// be reversible by a friend simply relaying a record.
    #[tokio::test]
    async fn ingest_ignores_a_record_about_a_revoked_peer() {
        let (store, ids) = store_with(&["Sam"]).await;
        let sam = identity(1);
        ingest(&store, ids[0], vec![record_for(&sam, "http://sam:1", 1000)]).await;
        store.revoke_peer(ids[0]).await.unwrap();

        let summary = ingest(
            &store,
            ids[0],
            vec![record_for(&sam, "http://sam-new:2", 2000)],
        )
        .await;
        assert_eq!(summary.accepted, 0);
        assert_eq!(summary.unknown, 1);
    }

    /// An endpoint kind this build does not recognise is skipped rather than
    /// stored or rejected wholesale — the record around it is still accepted,
    /// which is what lets a newer sharerr add kinds without breaking older
    /// friends relaying them.
    #[tokio::test]
    async fn an_unknown_endpoint_kind_is_skipped_but_the_record_is_still_accepted() {
        let (store, ids) = store_with(&["Sam"]).await;
        let sam = identity(1);
        let record = sam
            .sign_record(
                vec![
                    RecordEndpoint {
                        kind: "some-future-kind".to_owned(),
                        addr: "http://future:1".to_owned(),
                        observed_at: 1000,
                    },
                    RecordEndpoint {
                        kind: "tracker".to_owned(),
                        addr: "http://sam:1".to_owned(),
                        observed_at: 1000,
                    },
                ],
                1000,
            )
            .unwrap();

        let summary = ingest(&store, ids[0], vec![record]).await;
        assert_eq!(summary.accepted, 1);

        let endpoints = store.peer_endpoints(ids[0]).await.unwrap();
        assert_eq!(endpoints.len(), 1, "only the recognised kind is stored");
        assert_eq!(endpoints[0].addr, "http://sam:1");
    }

    /// `self_record` returns `None` — rather than a record with no
    /// endpoints — when there is no gossip identity to sign with, which is
    /// the state of a fresh, unconfigured instance.
    ///
    /// `state::fixtures::unconfigured()` relies on `SHARERR_MASTER_KEY` being
    /// absent from the process env, same as `commands::vault`'s no-master-key
    /// tests — so this needs the same `Jail`-clears-and-serializes treatment to
    /// stay safe next to this module's own Jail-based tests that legitimately
    /// set the var (`self_record_signs_the_current_tracker_api_and_client_endpoints`,
    /// `run_exchange_pushes_and_pulls_a_friend_with_a_stored_key_and_url`).
    #[test]
    fn self_record_is_none_without_a_gossip_identity() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            let (_dir, state) = crate::state::fixtures::unconfigured();
            let runtime = tokio::runtime::Runtime::new().unwrap();
            assert!(runtime.block_on(self_record(&state)).is_none());
            Ok(())
        });
    }
}
