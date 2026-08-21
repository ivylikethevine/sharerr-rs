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
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use sharerr_client::error_chain;
use sharerr_core::config::secret_keys;
use sharerr_core::endpoint::now_epoch;
use sharerr_store::{EndpointKind, ObservedVia, Store};

use crate::state::ServeState;
use crate::torznab::Caller;

/// How often the outbound exchange runs against friends with a configured URL.
const EXCHANGE_INTERVAL: Duration = Duration::from_secs(900);

/// Cap on records accepted in one POST — a friend relays a friend list, not a
/// crawl of the internet.
const MAX_RECORDS: usize = 64;

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

/// One address inside a record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordEndpoint {
    /// One of [`EndpointKind`]'s names. Unknown kinds are skipped on ingest, so
    /// a newer sharerr can add kinds without breaking older friends.
    pub kind: String,
    pub addr: String,
    pub observed_at: i64,
}

/// One peer's self-described endpoints, signed by them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointRecord {
    /// Hex Ed25519 public key — the subject's identity.
    pub pubkey: String,
    pub endpoints: Vec<RecordEndpoint>,
    /// Unix seconds. A record never replaces a stored one with a newer
    /// `signed_at`, which is what stops replays rewinding an address.
    pub signed_at: i64,
    /// Hex Ed25519 signature over [`signable_bytes`].
    pub signature: String,
}

/// The wire shape of both gossip endpoints' bodies.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct RecordBatch {
    pub records: Vec<EndpointRecord>,
}

/// The bytes a record's signature covers: the JSON of everything except the
/// signature itself, field order fixed by this struct.
///
/// Canonicalised by construction rather than by a canonical-JSON scheme: both
/// ends are sharerr serialising the same struct with the same serde, and the
/// verifier re-derives the bytes from the parsed fields rather than trusting
/// any bytes off the wire.
fn signable_bytes(
    pubkey: &str,
    endpoints: &[RecordEndpoint],
    signed_at: i64,
) -> Result<Vec<u8>, serde_json::Error> {
    #[derive(Serialize)]
    struct Signable<'a> {
        pubkey: &'a str,
        endpoints: &'a [RecordEndpoint],
        signed_at: i64,
    }
    serde_json::to_vec(&Signable {
        pubkey,
        endpoints,
        signed_at,
    })
}

/// Check a record's signature against the key it names.
///
/// This proves the record was produced by whoever holds the *private* half of
/// `record.pubkey` — whether that pubkey belongs to anyone we know is the
/// caller's question, answered against the peers table.
pub fn verify(record: &EndpointRecord) -> Result<(), &'static str> {
    let mut key_bytes = [0u8; 32];
    hex::decode_to_slice(&record.pubkey, &mut key_bytes)
        .map_err(|_| "pubkey is not 32 hex bytes")?;
    let key =
        VerifyingKey::from_bytes(&key_bytes).map_err(|_| "pubkey is not a valid Ed25519 key")?;

    let mut sig_bytes = [0u8; 64];
    hex::decode_to_slice(&record.signature, &mut sig_bytes)
        .map_err(|_| "signature is not 64 hex bytes")?;
    let signature = Signature::from_bytes(&sig_bytes);

    let bytes = signable_bytes(&record.pubkey, &record.endpoints, record.signed_at)
        .map_err(|_| "record could not be serialised")?;
    key.verify(&bytes, &signature)
        .map_err(|_| "signature does not verify")
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
        if let Ok(Some(stored)) = vault.get(secret_keys::IDENTITY_SIGNING_KEY) {
            let mut bytes = [0u8; 32];
            hex::decode_to_slice(stored.expose_secret(), &mut bytes)
                .map_err(|_| "the stored identity key is not 32 hex bytes".to_owned())?;
            return Ok(Self {
                signing: SigningKey::from_bytes(&bytes),
            });
        }

        let seed = crate::secrets::random_bytes::<32>()
            .map_err(|err| format!("generating an identity key: {err}"))?;
        let signing = SigningKey::from_bytes(&seed);
        vault
            .put(
                secret_keys::IDENTITY_SIGNING_KEY,
                &SecretString::from(hex::encode(seed)),
            )
            .map_err(|err| format!("storing the identity key: {err}"))?;
        tracing::info!(pubkey = %hex::encode(signing.verifying_key().to_bytes()), "minted a gossip identity");
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
    // Unlike Tracker/Api, Client is genuinely independent — see
    // `docs/ROADMAP.md`'s "a peer with two addresses". Present only once
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
#[derive(Debug, Default, PartialEq, Eq, Serialize)]
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

    for record in records.into_iter().take(MAX_RECORDS) {
        if let Err(reason) = verify(&record) {
            tracing::debug!(reason, "rejected a gossip record");
            summary.invalid += 1;
            continue;
        }

        // Who is this record about? Identity is the pubkey, nothing else.
        let subject_id = match peers
            .iter()
            .find(|p| p.pubkey.as_deref() == Some(record.pubkey.as_str()))
        {
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

        // Freshness: never let an older record rewind a newer one.
        let stored_signed_at = store
            .peer_gossip_record(subject_id)
            .await
            .ok()
            .flatten()
            .and_then(|raw| serde_json::from_str::<EndpointRecord>(&raw).ok())
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
                .record_peer_endpoint(subject_id, kind, &endpoint.addr, endpoint.observed_at, via)
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
        for peer in peers.iter().filter(|p| !p.is_revoked()) {
            let Some(pubkey) = peer.pubkey.as_deref() else {
                continue;
            };
            // The intersection rule: only relay records for peers the caller
            // proved they already know by naming the pubkey.
            if !wanted.contains(pubkey) {
                continue;
            }
            if let Ok(Some(raw)) = store.peer_gossip_record(peer.id).await
                && let Ok(record) = serde_json::from_str::<EndpointRecord>(&raw)
            {
                batch.records.push(record);
            }
        }
    }

    axum::Json(batch).into_response()
}

/// `POST /api/gossip/endpoints` — the push side, for a friend whose address
/// changed and who can therefore no longer be pulled from.
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
    loop {
        if let Err(reason) = run_exchange(&state).await {
            tracing::debug!(reason, "gossip exchange skipped");
        }
        tokio::time::sleep(EXCHANGE_INTERVAL).await;
    }
}

async fn run_exchange(state: &Arc<ServeState>) -> Result<(), String> {
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

    let vault = state.open_vault().await?;
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;

    // Concurrently: the friends are independent hosts behind independent keys,
    // and each exchange can sit on the 15s timeout above. In series, one friend
    // behind a dead tunnel delayed every friend after them — with five friends
    // and two unreachable, a single pass burned 30s of wall time doing nothing.
    let (http, store, own, known) = (&http, &store, own.as_ref(), &known);
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
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use sharerr_store::PeerScope;

    fn identity(seed: u8) -> Identity {
        Identity {
            signing: SigningKey::from_bytes(&[seed; 32]),
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
}
