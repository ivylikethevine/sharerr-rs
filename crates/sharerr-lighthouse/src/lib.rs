//! The lighthouse: semi-anonymous `key hash -> latest endpoint` rendezvous.
//!
//! See `docs/ROADMAP.md`'s "The lighthouse" for the design brief this
//! implements. In short: two friends whose addresses both rotated while
//! neither was watching have no path back to each other through gossip alone.
//! The lighthouse is the fallback — a peer reports its current endpoint under
//! the hash of the API key it issued a given friend, and that friend looks it
//! up under the same hash, the one credential they already share.
//!
//! # Why this crate stands alone
//!
//! It is deliberately independent of the rest of the workspace — no
//! `sharerr-core`, no `sharerr-store`, no database. It knows nothing but
//! `key hash -> latest record`, so there is nothing here worth stealing and
//! nothing here that assumes it is running next to an *arr stack. That
//! independence is what lets it ship as this crate's own binary
//! ([`bin/main.rs`](../src/main.rs)) on its own port, or be mounted as a
//! handful of routes inside another axum app (which is how `sharerr serve`
//! optionally embeds it — see `crates/sharerr/src/commands/serve.rs`).
//!
//! # The privacy property
//!
//! An unauthenticated prober cannot tell a real record from a decoy: a lookup
//! for a key hash the lighthouse has never seen still returns 200 with a
//! record of the same shape, its signature and public key fabricated by a
//! keyed hash of the lighthouse's own secret and the queried key hash. That
//! makes decoys stable across repeated probes (not fresh noise that would
//! flag itself by changing) without ever confirming that a given key hash
//! belongs to a real instance.
//!
//! A friend holding a valid key can tell the difference because a genuine
//! record is *verifiable*: it is signed by the peer it describes with the
//! same Ed25519 key gossip uses, and [`verify`] checks that signature. A
//! decoy's signature is random bytes, indistinguishable on the wire from a
//! real one to anyone without the peer's public key, and it never verifies
//! for anyone.
//!
//! # What the report side does *not* hide
//!
//! The privacy property above is a property of [`lookup`], and only of
//! [`lookup`]. Reporting answers honestly: `accepted`, `stale`, or a refusal
//! naming its reason. That means someone who guesses a key hash and posts a
//! record of their own can learn whether that key hash is in use, which a
//! lookup would never tell them.
//!
//! That is a deliberate trade and not an oversight. The alternative — a report
//! endpoint that swallows every outcome into the same answer — would leave a
//! peer whose reports are being refused with no way to find out, and the two
//! reasons a report gets refused are both things an operator has to act on:
//! the table is full, or the key hash is claimed by another keypair. A silent
//! failure there means an instance that believes it is reachable and is not.
//!
//! [`LighthouseState::report`]'s own docs cover the pinning that second
//! refusal comes from.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

// ---------------------------------------------------------------------------
// Wire protocol
// ---------------------------------------------------------------------------

/// One address inside a record.
///
/// Field-for-field the same shape as gossip's `RecordEndpoint` (see
/// `crates/sharerr/src/gossip.rs`) — the lighthouse relays the identical
/// wire format rather than inventing a second one, per the design brief. The
/// type is duplicated rather than shared because gossip's lives in the
/// `sharerr` binary crate, which this crate deliberately does not depend on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = LighthouseRecordEndpoint)]
pub struct RecordEndpoint {
    /// What is reachable there. `tracker` is the only kind in use.
    #[schema(example = "tracker")]
    pub kind: String,
    /// `host:port`, as the peer sees its own reachable address.
    #[schema(example = "203.0.113.7:41234")]
    pub addr: String,
    /// Unix seconds at which the reporter last confirmed this address.
    pub observed_at: i64,
}

/// One peer's self-described endpoints, signed by them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = LighthouseEndpointRecord)]
pub struct EndpointRecord {
    /// Hex Ed25519 public key — the subject's identity.
    #[schema(example = "3b6a27bcceb6a42d62a3a8d02a6f0d73653215771de243a63ac048a18b59da29")]
    pub pubkey: String,
    pub endpoints: Vec<RecordEndpoint>,
    /// Unix seconds. A record never replaces a stored one with a newer
    /// `signed_at`, which is what stops a stale report rewinding an address.
    pub signed_at: i64,
    /// Hex Ed25519 signature over the same signable bytes gossip signs.
    pub signature: String,
}

/// The bytes a record's signature covers — identical construction to
/// gossip's `signable_bytes`, so a record signed for gossip verifies here
/// unchanged and vice versa.
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

/// A key hash is a lowercase-hex SHA-256 digest — the form both sides derive
/// from the shared API key without either needing to send it.
fn valid_key_hash(key_hash: &str) -> bool {
    key_hash.len() == 64 && key_hash.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn hash_key(raw_key: &str) -> String {
    hex::encode(Sha256::digest(raw_key.as_bytes()))
}

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as i64
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

struct Stored {
    record: EndpointRecord,
}

/// The whole lighthouse: a map of key hash to latest record, plus the secret
/// that makes decoys deterministic.
pub struct LighthouseState {
    records: RwLock<HashMap<String, Stored>>,
    decoy_secret: [u8; 32],
}

impl std::fmt::Debug for LighthouseState {
    /// Hand-written so the decoy secret cannot reach a log — it is as
    /// sensitive as any other server key, since holding it lets someone tell
    /// decoys from real records without a peer's public key.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LighthouseState").finish_non_exhaustive()
    }
}

/// What a report attempt amounted to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportOutcome {
    Accepted,
    /// No newer than what is already stored under this key hash.
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportError {
    BadKeyHash,
    InvalidRecord,
    /// `signed_at` is further in the future than any clock skew explains.
    /// Freshness is decided by comparing `signed_at` values, so without this
    /// a single record stamped `i64::MAX` would lock its slot forever.
    FutureTimestamp,
    /// The table is at capacity and this key hash is not in it. The POST is
    /// unauthenticated, so an unbounded map is a memory DoS; refusing new
    /// keys rather than evicting old ones means a flood cannot displace the
    /// records real peers depend on.
    Full,
    /// A record already stands under this key hash, signed by a *different*
    /// keypair. See [`LighthouseState::report`] — the first keypair to claim
    /// a key hash keeps it.
    PubkeyMismatch,
}

/// How far ahead of this host's clock a `signed_at` may be and still be
/// accepted.
pub const MAX_FUTURE_SKEW_SECS: i64 = 5 * 60;

/// How long a record is kept after it was signed. A sharerr instance
/// re-reports on a timer measured in minutes; anything this old belongs to an
/// instance that has gone away.
pub const RECORD_TTL_SECS: i64 = 30 * 24 * 60 * 60;

/// The most key hashes held at once. Each entry is bounded by the request
/// body limit; this bounds the count.
pub const MAX_RECORDS: usize = 10_000;

impl LighthouseState {
    pub fn new(decoy_secret: [u8; 32]) -> Self {
        Self {
            records: RwLock::new(HashMap::new()),
            decoy_secret,
        }
    }

    /// Store a peer-signed record under a key hash, unless it is no newer
    /// than what is already there. Rejects anything that does not verify —
    /// no unsigned garbage is ever stored, only ever fabricated on lookup.
    ///
    /// # Why a signature alone is not enough
    ///
    /// [`verify`] only establishes that a record is self-consistent: signed by
    /// whatever pubkey it happens to carry. It says nothing about whether that
    /// pubkey has any business being under *this* key hash — and a key hash is
    /// a URL path segment, so it travels through every proxy log between a
    /// peer and the lighthouse. Anyone who reads one could mint a record under
    /// their own keypair and displace the genuine one. The friend looking it
    /// up would not be fooled — they compare `pubkey` against the identity
    /// they already hold and read a mismatch as a decoy — but the real record
    /// would be gone, and rendezvous for that pair simply stops working.
    ///
    /// So the first keypair to report under a key hash **pins** it, and later
    /// reports must carry the same `pubkey` or be refused. That is the same
    /// trust-on-first-use gossip already uses to bind a peer's identity, and
    /// unlike the alternative — deriving the key hash from the pubkey as well
    /// as the key — it needs no change to what goes over the wire, so it does
    /// not break every instance already reporting.
    ///
    /// Two consequences worth stating rather than discovering:
    ///
    /// * **A pin lapses when its record does.** Once a record is older than
    ///   [`RECORD_TTL_SECS`] its slot is free again, pubkey included. That is
    ///   what makes key rotation possible at all: stop reporting, wait out the
    ///   TTL, report with the new keypair. An instance reporting on its normal
    ///   timer never comes close to it.
    /// * **Whoever reports first wins, including an attacker.** Someone who
    ///   learns a key hash before the legitimate peer has ever reported can
    ///   claim the slot and hold it. They still cannot impersonate anyone —
    ///   the lookup side catches that — but they can deny that pair the
    ///   rendezvous. The remedy is the operator's: issue that friend a new
    ///   key, which is a new key hash. Trust-on-first-use has no better answer
    ///   than that, and the window is the gap between issuing a key and the
    ///   instance's next report.
    pub async fn report(
        &self,
        key_hash: &str,
        record: EndpointRecord,
    ) -> Result<ReportOutcome, ReportError> {
        // Lowercased before anything else: `lookup` lowercases too, and a
        // mixed-case entry would be unreachable *and* bypass the per-hash
        // staleness check (2^64 case variants of one logical hash).
        let key_hash = key_hash.to_ascii_lowercase();
        if !valid_key_hash(&key_hash) {
            return Err(ReportError::BadKeyHash);
        }
        verify(&record).map_err(|_| ReportError::InvalidRecord)?;
        let now = now_epoch();
        if record.signed_at > now.saturating_add(MAX_FUTURE_SKEW_SECS) {
            return Err(ReportError::FutureTimestamp);
        }

        let mut records = self.records.write().await;
        if let Some(existing) = records.get(&key_hash) {
            // An expired record no longer holds its pubkey — see the rotation
            // path in the doc comment. Decided here rather than left to the
            // capacity sweep below, which only runs when the table is full and
            // would make rotation depend on how busy the lighthouse is.
            let expired = now.saturating_sub(existing.record.signed_at) >= RECORD_TTL_SECS;

            // Before the staleness check, not after: a displacement attempt
            // that also happens to be stale is still a displacement attempt,
            // and an operator reading `stale` would go looking at clocks.
            if !expired && !existing.record.pubkey.eq_ignore_ascii_case(&record.pubkey) {
                return Err(ReportError::PubkeyMismatch);
            }

            // Applied whether or not the slot expired: `signed_at` only ever
            // moves forward for a given reporter, and a record that has aged
            // out is still no reason to accept an older one over it.
            if existing.record.signed_at >= record.signed_at {
                return Ok(ReportOutcome::Stale);
            }
        }
        if !records.contains_key(&key_hash) && records.len() >= MAX_RECORDS {
            records
                .retain(|_, stored| now.saturating_sub(stored.record.signed_at) < RECORD_TTL_SECS);
            if records.len() >= MAX_RECORDS {
                return Err(ReportError::Full);
            }
        }
        records.insert(key_hash, Stored { record });
        Ok(ReportOutcome::Accepted)
    }

    /// The record for a key hash: the real one if it has ever been reported,
    /// otherwise a fabricated one of the same shape, stable across repeated
    /// probes of the same key hash. Never fails and never distinguishes the
    /// two cases in its return type — that is the entire privacy property.
    pub async fn lookup(&self, key_hash: &str) -> EndpointRecord {
        if let Some(stored) = self.records.read().await.get(key_hash) {
            return stored.record.clone();
        }
        self.decoy(key_hash)
    }

    fn decoy(&self, key_hash: &str) -> EndpointRecord {
        let pubkey = hex::encode(self.derive(b"pubkey", key_hash));
        let addr_bytes = self.derive(b"addr", key_hash);
        // Clamp away 0.x, 10.x, 127.x and 255 so a decoy looks like a public
        // unicast address rather than something a prober could dismiss as
        // obviously synthetic on sight — it still never verifies, which is
        // the actual defence.
        let octet = |b: u8| -> u8 {
            match b {
                0 | 10 | 127 | 255 => 1,
                other => other,
            }
        };
        let addr = format!(
            "{}.{}.{}.{}:{}",
            octet(addr_bytes[0]),
            addr_bytes[1],
            addr_bytes[2],
            addr_bytes[3],
            1024 + (u16::from(addr_bytes[4]) << 8 | u16::from(addr_bytes[5])) % (65535 - 1024)
        );

        // Recent-looking but stable per key hash: derived offset within the
        // last day rather than the real clock alone, so two probes minutes
        // apart still get byte-identical answers.
        let offset_bytes = self.derive(b"signed_at", key_hash);
        let offset = i64::from(
            u32::from_be_bytes([
                offset_bytes[0],
                offset_bytes[1],
                offset_bytes[2],
                offset_bytes[3],
            ]) % 86_400,
        );
        let signed_at = now_epoch() - offset;

        let mut signature = self.derive(b"sig-a", key_hash).to_vec();
        signature.extend_from_slice(&self.derive(b"sig-b", key_hash));

        EndpointRecord {
            pubkey,
            endpoints: vec![RecordEndpoint {
                kind: "tracker".to_owned(),
                addr,
                observed_at: signed_at,
            }],
            signed_at,
            signature: hex::encode(signature),
        }
    }

    /// One 32-byte keyed-hash output, domain-separated by `label` so the
    /// pubkey, address, timestamp and both signature halves of one decoy
    /// cannot be derived from each other.
    fn derive(&self, label: &[u8], key_hash: &str) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(self.decoy_secret);
        hasher.update(label);
        hasher.update(key_hash.as_bytes());
        hasher.finalize().into()
    }
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

/// `POST /lighthouse/v1/report/{key_hash}` — a peer publishing its current
/// endpoint under the hash of the key it issued the friend who will look it
/// up.
///
/// Always answers with the outcome rather than a generic success, since the
/// caller here is the legitimate reporter's own sharerr, not an anonymous
/// prober — there is nothing to hide from someone who can already produce a
/// validly signed record.
#[utoipa::path(
    post,
    path = "/lighthouse/v1/report/{key_hash}",
    tag = "lighthouse",
    operation_id = "lighthouseReport",
    params(
        ("key_hash" = String, Path,
         description = "Lowercase hex SHA-256 of the API key this peer issued the \
                        friend who will look it up. 64 characters."),
    ),
    request_body = EndpointRecord,
    responses(
        (status = 200, description = "Stored, or ignored as older than what is held. \
                                      The body is `accepted` or `stale`.",
         body = String),
        (status = 400, description = "The key hash was not 64 hex characters, the \
                                      signature did not verify, or `signed_at` is in \
                                      the future.", body = String),
        (status = 403, description = "A record signed by a different keypair already \
                                      stands under this key hash. The first keypair to \
                                      claim one keeps it until its record expires.",
         body = String),
        (status = 503, description = "At capacity and nothing was old enough to evict.",
         body = String),
    ),
)]
async fn report(
    State(state): State<Arc<LighthouseState>>,
    Path(key_hash): Path<String>,
    axum::Json(record): axum::Json<EndpointRecord>,
) -> Response {
    match state.report(&key_hash, record).await {
        Ok(ReportOutcome::Accepted) => (StatusCode::OK, "accepted").into_response(),
        Ok(ReportOutcome::Stale) => (StatusCode::OK, "stale").into_response(),
        Err(ReportError::BadKeyHash) => (
            StatusCode::BAD_REQUEST,
            "key hash must be 64 hex characters",
        )
            .into_response(),
        Err(ReportError::InvalidRecord) => {
            (StatusCode::BAD_REQUEST, "record did not verify").into_response()
        }
        Err(ReportError::FutureTimestamp) => (
            StatusCode::BAD_REQUEST,
            "signed_at is in the future — check the reporting host's clock",
        )
            .into_response(),
        Err(ReportError::Full) => {
            (StatusCode::SERVICE_UNAVAILABLE, "lighthouse is full").into_response()
        }
        Err(ReportError::PubkeyMismatch) => (
            StatusCode::FORBIDDEN,
            "this key hash is already claimed by a different keypair",
        )
            .into_response(),
    }
}

/// `GET /lighthouse/v1/lookup/{key_hash}` — always `200`, always the same
/// JSON shape, real record or decoy. A malformed key hash still gets a
/// decoy rather than a `400`: a probe that can distinguish "malformed" from
/// "unknown" learns something it should not.
#[utoipa::path(
    get,
    path = "/lighthouse/v1/lookup/{key_hash}",
    tag = "lighthouse",
    operation_id = "lighthouseLookup",
    params(
        ("key_hash" = String, Path,
         description = "Lowercase hex SHA-256 of the API key the peer issued you."),
    ),
    responses(
        (status = 200, description = "A record. **Always 200, always this shape** — \
             an unknown or malformed key hash gets a fabricated record rather than an \
             error, so an unauthenticated probe cannot tell that an instance exists. \
             Verify `signature` against the `pubkey` you expect: a decoy carries \
             random bytes there and never verifies.",
         body = EndpointRecord),
    ),
)]
async fn lookup(
    State(state): State<Arc<LighthouseState>>,
    Path(key_hash): Path<String>,
) -> Response {
    let key_hash = key_hash.to_lowercase();
    let key_hash = if valid_key_hash(&key_hash) {
        key_hash
    } else {
        // Not a real lookup key, but still hashed through so the fabricated
        // answer is deterministic for this input rather than falling back to
        // some other shape of response.
        hash_key(&key_hash)
    };
    axum::Json(state.lookup(&key_hash).await).into_response()
}

/// `GET /lighthouse/v1/health` — liveness only, no state consulted. Under the
/// same `/lighthouse/v1/...` prefix as everything else here rather than a
/// bare `/health`, deliberately: `sharerr serve` already owns that path on
/// whichever listener it embeds these routes onto, and a second `/health`
/// registration on the same router panics at merge time.
#[utoipa::path(
    get,
    path = "/lighthouse/v1/health",
    tag = "lighthouse",
    operation_id = "lighthouseHealth",
    responses((status = 200, description = "Alive. No state is consulted.", body = String)),
)]
async fn health() -> &'static str {
    "ok"
}

/// The lighthouse's routes, under `/lighthouse/v1/...` regardless of whether
/// they are served by the standalone binary at the root of its own port or
/// merged into another axum app — the URL a client builds is the same
/// either way.
///
/// Mounted through [`OpenApiRouter`] rather than a plain `Router`, so a route
/// and its entry in the OpenAPI document are one declaration: the path comes
/// from the handler's own `#[utoipa::path]` attribute, and there is no second
/// place to add a route to, or forget to.
pub fn routes(state: Arc<LighthouseState>) -> Router {
    let (router, _) = api_router().with_state(state).split_for_parts();
    router
}

/// The same declaration without state, so its OpenAPI half can be read off
/// without standing a lighthouse up. `sharerr`'s own document merges this in,
/// because the frontend listener carries the lighthouse in some topologies —
/// see `sharerr::openapi`.
fn api_router() -> OpenApiRouter<Arc<LighthouseState>> {
    OpenApiRouter::new()
        .routes(routes!(health))
        .routes(routes!(report))
        .routes(routes!(lookup))
}

/// The OpenAPI half alone.
pub fn api_spec() -> utoipa::openapi::OpenApi {
    api_router().split_for_parts().1
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use ed25519_dalek::{Signer, SigningKey};
    use tower::ServiceExt;

    fn signed_record(seed: u8, addr: &str, signed_at: i64) -> EndpointRecord {
        let signing = SigningKey::from_bytes(&[seed; 32]);
        let pubkey = hex::encode(signing.verifying_key().to_bytes());
        let endpoints = vec![RecordEndpoint {
            kind: "tracker".to_owned(),
            addr: addr.to_owned(),
            observed_at: signed_at,
        }];
        let bytes = signable_bytes(&pubkey, &endpoints, signed_at).unwrap();
        let signature = hex::encode(signing.sign(&bytes).to_bytes());
        EndpointRecord {
            pubkey,
            endpoints,
            signed_at,
            signature,
        }
    }

    fn state() -> Arc<LighthouseState> {
        Arc::new(LighthouseState::new([7u8; 32]))
    }

    #[test]
    fn a_signed_record_verifies_and_a_tampered_one_does_not() {
        let record = signed_record(1, "203.0.113.9:41234", 1000);
        assert!(verify(&record).is_ok());

        let mut tampered = record.clone();
        tampered.endpoints[0].addr = "attacker.example:1".to_owned();
        assert!(verify(&tampered).is_err());
    }

    #[tokio::test]
    async fn a_valid_report_is_stored_and_a_stale_one_is_not() {
        let state = state();
        let key_hash = hash_key("friend-shared-secret");

        let outcome = state
            .report(&key_hash, signed_record(1, "203.0.113.9:41234", 1000))
            .await
            .unwrap();
        assert_eq!(outcome, ReportOutcome::Accepted);

        let stale = state
            .report(&key_hash, signed_record(1, "203.0.113.9:1", 500))
            .await
            .unwrap();
        assert_eq!(stale, ReportOutcome::Stale);

        let fresher = state
            .report(&key_hash, signed_record(1, "203.0.113.9:2", 2000))
            .await
            .unwrap();
        assert_eq!(fresher, ReportOutcome::Accepted);

        let record = state.lookup(&key_hash).await;
        assert_eq!(record.endpoints[0].addr, "203.0.113.9:2");
    }

    /// The displacement this pinning exists to stop: a second keypair, whose
    /// record is perfectly well signed, arriving under a key hash somebody
    /// else already holds.
    #[tokio::test]
    async fn a_second_keypair_cannot_displace_the_record_under_a_key_hash() {
        let state = state();
        let key_hash = hash_key("friend-shared-secret");

        // Stamped now, not at some small absolute number: a record older
        // than the TTL would exercise the rotation path below instead of the
        // pin, and pass while proving nothing.
        let now = now_epoch();
        state
            .report(&key_hash, signed_record(1, "203.0.113.9:41234", now))
            .await
            .unwrap();

        // Signed, self-consistent, and newer — everything `verify` checks. The
        // only thing wrong with it is whose key signed it.
        let attacker = signed_record(2, "198.51.100.4:6881", now + 60);
        assert!(verify(&attacker).is_ok(), "the attack is a *valid* record");
        let err = state.report(&key_hash, attacker).await.unwrap_err();
        assert_eq!(err, ReportError::PubkeyMismatch);

        let record = state.lookup(&key_hash).await;
        assert_eq!(
            record.endpoints[0].addr, "203.0.113.9:41234",
            "the genuine record must still be there"
        );
        assert_eq!(record.pubkey, signed_record(1, "x", 0).pubkey);
    }

    /// A mismatch is reported as a mismatch even when the intruding record is
    /// also stale — otherwise an operator chasing a displacement attempt reads
    /// `stale` and goes looking at clocks instead.
    #[tokio::test]
    async fn a_stale_report_from_another_keypair_is_still_a_mismatch() {
        let state = state();
        let key_hash = hash_key("k");
        let now = now_epoch();
        state
            .report(&key_hash, signed_record(1, "203.0.113.9:1", now))
            .await
            .unwrap();

        let err = state
            .report(&key_hash, signed_record(2, "198.51.100.4:1", now - 5000))
            .await
            .unwrap_err();
        assert_eq!(err, ReportError::PubkeyMismatch);
    }

    /// The rotation path: a pin lasts exactly as long as the record holding
    /// it. Without this a peer that regenerated its identity could never
    /// report under an already-issued key again, and the operator's only
    /// remedy would be to re-issue every friend's key.
    #[tokio::test]
    async fn a_new_keypair_may_claim_a_key_hash_once_the_old_record_has_expired() {
        let state = state();
        let key_hash = hash_key("k");

        // Signed a day past the TTL, so it is expired however long ago "now"
        // is — the state has no clock of its own to wind forward.
        let expired = now_epoch() - RECORD_TTL_SECS - 86_400;
        state
            .report(&key_hash, signed_record(1, "203.0.113.9:1", expired))
            .await
            .unwrap();

        let outcome = state
            .report(&key_hash, signed_record(2, "198.51.100.4:1", now_epoch()))
            .await
            .unwrap();
        assert_eq!(outcome, ReportOutcome::Accepted);
        assert_eq!(
            state.lookup(&key_hash).await.pubkey,
            signed_record(2, "x", 0).pubkey
        );
    }

    /// The same keypair reporting again is the ordinary case and must stay
    /// ordinary — this runs every few minutes for every friend.
    #[tokio::test]
    async fn the_pinned_keypair_keeps_reporting_normally() {
        let state = state();
        let key_hash = hash_key("k");

        let now = now_epoch();
        for (at, addr) in [(now - 300, "203.0.113.9:1"), (now, "203.0.113.9:2")] {
            let outcome = state
                .report(&key_hash, signed_record(1, addr, at))
                .await
                .unwrap();
            assert_eq!(outcome, ReportOutcome::Accepted);
        }
        assert_eq!(
            state.lookup(&key_hash).await.endpoints[0].addr,
            "203.0.113.9:2"
        );
    }

    /// Hex case is not identity. A record whose pubkey differs from the pinned
    /// one only in case is the same key, and refusing it would strand a peer
    /// whose encoder disagreed with ours about capitalisation.
    #[tokio::test]
    async fn a_pinned_pubkey_is_matched_regardless_of_hex_case() {
        let state = state();
        let key_hash = hash_key("k");
        let now = now_epoch();
        state
            .report(&key_hash, signed_record(1, "203.0.113.9:1", now - 300))
            .await
            .unwrap();

        let mut same_key = signed_record(1, "203.0.113.9:2", now);
        same_key.pubkey = same_key.pubkey.to_uppercase();
        // The signature covers the pubkey *as written*, so re-sign for the
        // uppercase spelling — otherwise this would be rejected as unsigned
        // and prove nothing about the pin.
        let signing = SigningKey::from_bytes(&[1u8; 32]);
        let bytes =
            signable_bytes(&same_key.pubkey, &same_key.endpoints, same_key.signed_at).unwrap();
        same_key.signature = hex::encode(signing.sign(&bytes).to_bytes());

        let outcome = state.report(&key_hash, same_key).await.unwrap();
        assert_eq!(outcome, ReportOutcome::Accepted);
    }

    /// The refusal has to reach the reporter as its own status: the client
    /// logs on it, and 403 is the one answer that means "stop retrying and
    /// look at your keys".
    #[tokio::test]
    async fn the_report_route_answers_a_mismatch_with_403() {
        let state = state();
        let key_hash = hash_key("k");
        let now = now_epoch();
        state
            .report(&key_hash, signed_record(1, "203.0.113.9:1", now))
            .await
            .unwrap();

        let app = routes(state);
        let body = serde_json::to_vec(&signed_record(2, "198.51.100.4:1", now + 60)).unwrap();
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/lighthouse/v1/report/{key_hash}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn an_unsigned_report_is_rejected() {
        let state = state();
        let mut record = signed_record(1, "203.0.113.9:1", 1000);
        record.signature = "00".repeat(64);

        let err = state.report(&hash_key("k"), record).await.unwrap_err();
        assert_eq!(err, ReportError::InvalidRecord);
    }

    #[tokio::test]
    async fn an_unknown_key_hash_gets_a_stable_decoy_that_never_verifies() {
        let state = state();
        let key_hash = hash_key("nobody-has-registered-this");

        let first = state.lookup(&key_hash).await;
        let second = state.lookup(&key_hash).await;
        assert_eq!(first, second, "a repeated probe must see the same decoy");
        assert!(
            verify(&first).is_err(),
            "a decoy must never verify for anyone"
        );

        // Different key hashes must not collide on the same fabricated answer.
        let other = state.lookup(&hash_key("a-different-key")).await;
        assert_ne!(first.endpoints[0].addr, other.endpoints[0].addr);
    }

    #[tokio::test]
    async fn a_real_record_and_a_decoy_are_indistinguishable_on_status_code() {
        let state = state();
        let real_hash = hash_key("real");
        state
            .report(&real_hash, signed_record(1, "203.0.113.9:1", 1000))
            .await
            .unwrap();

        let app = routes(state);
        let real = app
            .clone()
            .oneshot(
                Request::get(format!("/lighthouse/v1/lookup/{real_hash}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(real.status(), StatusCode::OK);

        let decoy = app
            .oneshot(
                Request::get(format!("/lighthouse/v1/lookup/{}", hash_key("unknown")))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(decoy.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn the_report_route_rejects_a_bad_signature() {
        let state = state();
        let mut record = signed_record(1, "203.0.113.9:1", 1000);
        record.signature = "00".repeat(64);

        let response = routes(state)
            .oneshot(
                Request::post(format!("/lighthouse/v1/report/{}", hash_key("k")))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&record).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
