//! Reporting this instance's endpoint to configured lighthouses, and querying
//! them for a friend gossip cannot currently reach.
//!
//! See `docs/LIGHTHOUSE.md` for the design brief. Gossip
//! (`crate::gossip`) is the primary mechanism — friends relay each other's
//! signed endpoint records directly — but two friends whose addresses both
//! rotated while neither was watching have no path back to each other
//! through gossip alone. A lighthouse is the fallback: this instance reports
//! its current endpoint under the hash of the key it issued a given friend,
//! and looks that friend up under the hash of the key *they* issued it —
//! the one credential the two of them already share.
//!
//! # Two different key hashes
//!
//! Reporting and looking up use different keys, which is easy to get
//! backwards:
//!
//! - **Report**: [`sharerr_store::Peer::key_hash`] — the sha256 of the key
//!   *we* issued the friend — is already the exact hash a friend would
//!   compute from that same raw key to look us up. No re-hashing needed.
//! - **Lookup**: the raw key *they* issued *us*, in the vault under
//!   [`secret_keys::peer_gossip_key`], hashed with
//!   [`sharerr_lighthouse::hash_key`].
//!
//! # Trusting a lookup result
//!
//! A lighthouse never distinguishes a real record from a fabricated decoy in
//! its response — that is the whole privacy property, see
//! `sharerr-lighthouse`'s module docs. A decoy's signature is random bytes,
//! so [`sharerr_lighthouse::verify`] rejects it; and a decoy names a pubkey
//! nobody signed with, so a result is only ever recorded once it verifies
//! *and* names the peer's already-known pubkey. That known pubkey comes from
//! gossip's own trust-on-first-use binding — a peer we have never gossiped
//! with has no pubkey to check against, so a lighthouse cannot help there
//! yet; there is nothing to distinguish a decoy from the real thing.

use std::sync::Arc;
use std::time::Duration;

use secrecy::ExposeSecret;
use sharerr_client::error_chain;
use sharerr_core::config::secret_keys;
use sharerr_core::endpoint::{join_path, now_epoch};
use sharerr_lighthouse::EndpointRecord as LighthouseRecord;
use sharerr_store::{EndpointKind, ObservedVia, Peer, Store, Vault};
use url::Url;

use crate::gossip;
use crate::state::ServeState;

/// How often the report-and-lookup pass runs. Matches
/// `gossip::EXCHANGE_INTERVAL` — there is no reason for a lighthouse pass to
/// run on a different cadence than gossip's own.
const INTERVAL: Duration = Duration::from_secs(900);

/// A peer is worth a lighthouse lookup once it has been this long since they
/// were last seen — direct or gossiped. Matches the order of magnitude of
/// `notify::QUIET_CHECK_INTERVAL`: an hour costs nothing in responsiveness
/// for something that, when it happens at all, happens on the order of days.
const QUIET_THRESHOLD_SECS: i64 = 3600;

/// What the lighthouse poller last did, so the UI can say whether reporting is
/// actually working.
///
/// Modelled on [`crate::gluetun::GluetunStatus`], and for the same reason: a
/// poller that runs on a timer and logs its failures is invisible to an
/// operator who is not tailing logs. Lighthouse needed it more than gluetun
/// did — a refused report means friends who have gone quiet cannot find this
/// instance at all, and until now the only trace of that was one `warn!` line
/// every fifteen minutes.
///
/// Per-lighthouse rather than one aggregate: with two configured and one
/// refusing, an aggregate "last error" reads as though everything is broken,
/// and the fix (re-issue a key, or drop that URL) applies to exactly one of
/// them.
#[derive(Debug, Default)]
pub struct LighthouseStatus {
    inner: tokio::sync::RwLock<StatusInner>,
}

#[derive(Debug, Default, Clone)]
struct StatusInner {
    last_pass_at: Option<i64>,
    /// Keyed by lighthouse URL. A `BTreeMap` so the rendered order is stable
    /// between page loads rather than following a hash seed.
    reports: std::collections::BTreeMap<String, ReportOutcome>,
    last_recovery_at: Option<i64>,
    last_recovery_peer: Option<String>,
    lookups_attempted: usize,
}

#[derive(Debug, Default, Clone)]
struct ReportOutcome {
    last_success_at: Option<i64>,
    last_error: Option<String>,
}

/// A read-only snapshot of a [`LighthouseStatus`], for rendering.
#[derive(Debug, Clone, Default)]
pub struct LighthouseSnapshot {
    /// When the report-and-lookup pass last ran to completion. `None` before
    /// the first one, which — with a 15 minute interval — is a real state an
    /// operator can land on right after configuring one.
    pub last_pass_at: Option<i64>,
    /// One row per lighthouse this instance has actually tried, newest state.
    pub lighthouses: Vec<LighthouseReport>,
    /// When a lookup last recovered a friend's address, and whose. The only
    /// evidence a lighthouse has ever earned its keep — everything else here
    /// says the machinery is running, not that it has helped.
    pub last_recovery_at: Option<i64>,
    pub last_recovery_peer: Option<String>,
    /// Lookups attempted in the last pass. Zero is the normal, healthy case:
    /// it means no friend has gone quiet.
    pub lookups_attempted: usize,
}

/// One lighthouse's report state.
#[derive(Debug, Clone)]
pub struct LighthouseReport {
    pub url: String,
    pub last_success_at: Option<i64>,
    /// The refusal, verbatim — a 403 (the key hash is pinned to a different
    /// keypair) and a 503 (that lighthouse is full) have entirely different
    /// fixes, so the status code is the useful half.
    pub last_error: Option<String>,
}

impl LighthouseStatus {
    async fn record_pass(&self) {
        self.inner.write().await.last_pass_at = Some(now_epoch());
    }

    async fn record_report_ok(&self, url: &Url) {
        let mut inner = self.inner.write().await;
        let entry = inner.reports.entry(url.to_string()).or_default();
        entry.last_success_at = Some(now_epoch());
        entry.last_error = None;
    }

    async fn record_report_err(&self, url: &Url, reason: String) {
        let mut inner = self.inner.write().await;
        inner.reports.entry(url.to_string()).or_default().last_error = Some(reason);
    }

    async fn record_lookups(&self, attempted: usize) {
        self.inner.write().await.lookups_attempted = attempted;
    }

    async fn record_recovery(&self, peer_label: &str) {
        let mut inner = self.inner.write().await;
        inner.last_recovery_at = Some(now_epoch());
        inner.last_recovery_peer = Some(peer_label.to_owned());
    }

    pub async fn snapshot(&self) -> LighthouseSnapshot {
        let inner = self.inner.read().await;
        LighthouseSnapshot {
            last_pass_at: inner.last_pass_at,
            lighthouses: inner
                .reports
                .iter()
                .map(|(url, outcome)| LighthouseReport {
                    url: url.clone(),
                    last_success_at: outcome.last_success_at,
                    last_error: outcome.last_error.clone(),
                })
                .collect(),
            last_recovery_at: inner.last_recovery_at,
            last_recovery_peer: inner.last_recovery_peer.clone(),
            lookups_attempted: inner.lookups_attempted,
        }
    }
}

/// Report to, and query, every configured lighthouse on a timer. Never
/// returns.
pub async fn sync_loop(state: Arc<ServeState>) {
    // Built once and reused for the life of the poller — same reasoning as
    // `gossip::exchange_loop` and `gluetun::poll_loop`: `reqwest::Client` is
    // an Arc internally, so rebuilding one every pass discards a live
    // connection pool for nothing. A build failure disables lighthouse
    // reporting/lookup for the process's life rather than retrying every
    // fifteen minutes; nothing here can fix a broken TLS backend by trying
    // again.
    let http = match sharerr_client::http_client_with_timeout(Duration::from_secs(15)) {
        Ok(http) => http,
        Err(err) => {
            tracing::warn!(error = %err, "could not build the lighthouse HTTP client — lighthouse is disabled");
            return;
        }
    };

    loop {
        run(&state, &http).await;
        tokio::time::sleep(INTERVAL).await;
    }
}

async fn run(state: &Arc<ServeState>, http: &reqwest::Client) {
    let urls = state.with_config(|c| c.lighthouse.urls.clone()).await;
    if urls.is_empty() {
        return;
    }
    let Ok(store) = state.store().await else {
        return;
    };
    let Ok(vault) = state.open_vault().await else {
        return;
    };
    let Ok(peers) = store.list_peers().await else {
        return;
    };

    let own = gossip::self_record(state).await;
    let status = state.lighthouse_status();
    // Publishing this instance's record and looking up quiet friends touch
    // disjoint state (own record vs. friends' pubkeys) — independent, so run
    // them together rather than one after the other.
    tokio::join!(
        report(http, &urls, &peers, own.as_ref(), &status),
        lookup_quiet(http, &urls, &peers, &vault, &store, &status)
    );
    // Stamped after both halves finish, so "last pass" means a completed one
    // rather than one that started and is still in flight.
    status.record_pass().await;
}

/// Publish this instance's own signed record to every configured lighthouse,
/// once under each active friend's issued-key hash — a lighthouse indexes by
/// key hash alone, so a distinct report is needed per friend even though the
/// record itself is identical every time. A `None` record (no identity or no
/// advertised endpoint yet, same condition gossip already handles) skips the
/// pass entirely rather than reporting nothing meaningful.
async fn report(
    http: &reqwest::Client,
    urls: &[Url],
    peers: &[Peer],
    own: Option<&LighthouseRecord>,
    status: &LighthouseStatus,
) {
    let Some(record) = own else {
        tracing::debug!("no self-record available yet — skipping lighthouse report");
        return;
    };

    let mut attempts = Vec::new();
    for peer in peers.iter().filter(|p| !p.is_revoked()) {
        for url in urls {
            attempts.push(report_one(http, url, &peer.key_hash, record, status));
        }
    }
    futures::future::join_all(attempts).await;
}

/// Publish one record, and — unlike a fire-and-forget POST — say something
/// when the lighthouse refuses it.
///
/// A refusal is easy to lose here: logging only transport errors would make
/// a 403 or a 503 look exactly like a successful report. Both of the answers
/// that mean "your endpoint is not being published" need an operator, and an
/// instance that believes it is reachable and is not is the worst shape this
/// can fail in:
///
/// * **403** — the key hash is pinned to a different keypair. Either this
///   instance's identity was regenerated, or somebody else claimed the slot
///   first. Issuing that friend a new key is the way out; see
///   `sharerr_lighthouse::LighthouseState::report`.
/// * **503** — that lighthouse is at capacity and has no room for a key hash
///   it has not seen before.
///
/// A transport failure stays at `debug`: a lighthouse being briefly
/// unreachable is ordinary, and this runs on a timer.
async fn report_one(
    http: &reqwest::Client,
    base: &Url,
    key_hash: &str,
    record: &LighthouseRecord,
    tracked: &LighthouseStatus,
) {
    let endpoint = join_path(base, &format!("/lighthouse/v1/report/{key_hash}"));
    let response = match http.post(&endpoint).json(record).send().await {
        Ok(response) => response,
        Err(err) => {
            let reason = error_chain(&err);
            tracing::debug!(url = %base, reason = %reason, "lighthouse report failed");
            // Recorded even though it stays at `debug`: a lighthouse that has
            // been unreachable for a day is worth seeing on a page, even
            // though any single failure is ordinary.
            tracked.record_report_err(base, reason).await;
            return;
        }
    };

    let status = response.status();
    if status.is_success() {
        tracked.record_report_ok(base).await;
        return;
    }
    // The body is a short reason string; a lighthouse that answered at all
    // will have one, and it is the most useful half of this line.
    let reason = response.text().await.unwrap_or_default();
    tracked
        .record_report_err(base, format!("answered {status}: {}", reason.trim()))
        .await;
    tracing::warn!(
        url = %base,
        %status,
        reason = reason.trim(),
        "a lighthouse refused this instance's endpoint report — friends who have \
         gone quiet will not find it there"
    );
}

/// Query every configured lighthouse for every friend who has gone quiet and
/// whose identity we already know — see the module docs for why a known
/// pubkey is a prerequisite, not an optimisation.
async fn lookup_quiet(
    http: &reqwest::Client,
    urls: &[Url],
    peers: &[Peer],
    vault: &Vault,
    store: &Store,
    status: &LighthouseStatus,
) {
    let now = now_epoch();

    let quiet: Vec<&Peer> = peers
        .iter()
        .filter(|p| !p.is_revoked())
        .filter(|p| is_quiet(p, now))
        .collect();
    // Counted here rather than inside the per-peer call, so the number means
    // "friends this pass had reason to look up" — zero being the healthy case,
    // not a sign the poller did nothing.
    status.record_lookups(quiet.len()).await;

    let lookups = quiet
        .into_iter()
        .map(|peer| lookup_quiet_one(http, urls, vault, store, peer, status));
    futures::future::join_all(lookups).await;
}

/// Whether a peer has been silent long enough to be worth a lighthouse
/// lookup. Split out so [`lookup_quiet`] can count the quiet ones before
/// spawning the lookups, and so the threshold has one test to its name.
fn is_quiet(peer: &Peer, now: i64) -> bool {
    peer.last_seen_at
        .is_none_or(|seen| now - seen >= QUIET_THRESHOLD_SECS)
}

/// One friend's lookup across every configured lighthouse, stopping at the
/// first that answers — see [`lookup_quiet`] for why peers are independent
/// but a peer's own lighthouses are tried in order rather than fanned out.
async fn lookup_quiet_one(
    http: &reqwest::Client,
    urls: &[Url],
    vault: &Vault,
    store: &Store,
    peer: &Peer,
    status: &LighthouseStatus,
) {
    let Some(pubkey) = peer.pubkey.as_deref() else {
        return;
    };
    let Ok(Some(key)) = vault.get(&secret_keys::peer_gossip_key(peer.id)) else {
        return;
    };
    let hash = sharerr_lighthouse::hash_key(key.expose_secret());

    for url in urls {
        match lookup_one(http, url, &hash, pubkey).await {
            Ok(Some(record)) => {
                apply_lookup(store, peer.id, &record).await;
                status.record_recovery(&peer.label).await;
                tracing::info!(peer = peer.id, url = %url, "recorded an endpoint via lighthouse");
                break;
            }
            Ok(None) => {}
            Err(reason) => {
                tracing::debug!(url = %url, reason, "lighthouse lookup failed");
            }
        }
    }
}

/// One lookup against one lighthouse. `Ok(None)` covers both "the lighthouse
/// answered with a decoy" and "the record names a different identity" —
/// both are the same "nothing usable here" outcome to the caller.
async fn lookup_one(
    http: &reqwest::Client,
    base: &Url,
    key_hash: &str,
    expected_pubkey: &str,
) -> Result<Option<LighthouseRecord>, String> {
    let endpoint = join_path(base, &format!("/lighthouse/v1/lookup/{key_hash}"));
    let response = http
        .get(&endpoint)
        .send()
        .await
        .map_err(|e| error_chain(&e))?;
    if !response.status().is_success() {
        return Err(format!("lookup answered {}", response.status()));
    }
    let record: LighthouseRecord = response.json().await.map_err(|e| format!("body: {e}"))?;

    if record.pubkey != expected_pubkey || sharerr_lighthouse::verify(&record).is_err() {
        // Either a fabricated decoy (never verifies) or a lighthouse that
        // somehow answered for the wrong identity — both are ignored rather
        // than distinguished.
        return Ok(None);
    }
    Ok(Some(record))
}

async fn apply_lookup(store: &Store, peer_id: i64, record: &LighthouseRecord) {
    for endpoint in &record.endpoints {
        let Some(kind) = EndpointKind::parse(&endpoint.kind) else {
            continue;
        };
        if let Err(err) = store
            .record_peer_endpoint(
                peer_id,
                kind,
                &endpoint.addr,
                Some(endpoint.observed_at),
                ObservedVia::Lighthouse,
            )
            .await
        {
            tracing::warn!(error = %err, "could not record a lighthouse-observed endpoint");
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::result_large_err)]

    /// Distinct, non-constant key material for a test identity — see the
    /// identical helper in `gossip.rs`. `seed` labels an identity; it is not
    /// the key.
    fn test_key_bytes(seed: u8) -> [u8; 32] {
        static BASE: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
        let mut bytes = *BASE.get_or_init(|| {
            let mut base = [0u8; 32];
            getrandom::fill(&mut base).expect("the OS RNG is available");
            base
        });
        bytes[0] ^= seed;
        bytes
    }

    use std::net::SocketAddr;

    use ed25519_dalek::{Signer, SigningKey};
    use secrecy::SecretString;
    use sharerr_lighthouse::RecordEndpoint as LighthouseRecordEndpoint;
    use sharerr_store::PeerScope;

    use super::*;
    use crate::test_support::vault_in;

    fn a_url(raw: &str) -> Url {
        Url::parse(raw).unwrap()
    }

    /// A refused report is the state this whole type exists for: friends who
    /// have gone quiet cannot find this instance, and before the status was
    /// tracked the only trace was one log line every fifteen minutes.
    #[tokio::test]
    async fn a_refusal_is_recorded_against_the_lighthouse_that_refused() {
        let status = LighthouseStatus::default();
        let good = a_url("https://good.example");
        let bad = a_url("https://bad.example");

        status.record_report_ok(&good).await;
        status
            .record_report_err(&bad, "answered 403: pinned elsewhere".to_owned())
            .await;

        let snapshot = status.snapshot().await;
        assert_eq!(snapshot.lighthouses.len(), 2);

        let bad_row = snapshot
            .lighthouses
            .iter()
            .find(|r| r.url.contains("bad.example"))
            .unwrap();
        assert_eq!(
            bad_row.last_error.as_deref(),
            Some("answered 403: pinned elsewhere")
        );

        // The healthy one must not inherit the other's failure — with two
        // configured, an aggregate error reads as though both are broken.
        let good_row = snapshot
            .lighthouses
            .iter()
            .find(|r| r.url.contains("good.example"))
            .unwrap();
        assert!(good_row.last_error.is_none());
        assert!(good_row.last_success_at.is_some());
    }

    /// A lighthouse that starts working again must stop reporting the old
    /// refusal, or the page accuses a working setup forever.
    #[tokio::test]
    async fn a_later_success_clears_the_previous_refusal() {
        let status = LighthouseStatus::default();
        let url = a_url("https://lighthouse.example");

        status
            .record_report_err(&url, "answered 503".to_owned())
            .await;
        status.record_report_ok(&url).await;

        let snapshot = status.snapshot().await;
        assert_eq!(snapshot.lighthouses.len(), 1);
        assert!(snapshot.lighthouses[0].last_error.is_none());
    }

    /// A failure after a success keeps the success timestamp: "last accepted 2
    /// days ago, now refusing" is a much more useful line than either half.
    #[tokio::test]
    async fn a_refusal_keeps_the_last_success_timestamp() {
        let status = LighthouseStatus::default();
        let url = a_url("https://lighthouse.example");

        status.record_report_ok(&url).await;
        status
            .record_report_err(&url, "answered 403".to_owned())
            .await;

        let snapshot = status.snapshot().await;
        assert!(snapshot.lighthouses[0].last_success_at.is_some());
        assert!(snapshot.lighthouses[0].last_error.is_some());
    }

    #[tokio::test]
    async fn a_recovery_records_who_was_found() {
        let status = LighthouseStatus::default();
        assert!(status.snapshot().await.last_recovery_at.is_none());

        status.record_recovery("Riley").await;

        let snapshot = status.snapshot().await;
        assert_eq!(snapshot.last_recovery_peer.as_deref(), Some("Riley"));
        assert!(snapshot.last_recovery_at.is_some());
    }

    /// The threshold `lookup_quiet` counts on. A friend seen a minute ago is
    /// not worth a lighthouse lookup; one never seen at all always is.
    #[tokio::test]
    async fn quietness_is_measured_against_the_threshold() {
        let now = 1_000_000;
        let (_store, mut peer) = store_with_peer("Alex", "alex-key").await;

        peer.last_seen_at = Some(now - 60);
        assert!(!is_quiet(&peer, now));

        peer.last_seen_at = Some(now - QUIET_THRESHOLD_SECS);
        assert!(is_quiet(&peer, now));

        // Never seen at all: the friend has the key but has never used it, so
        // a lighthouse is exactly the thing that might find them.
        peer.last_seen_at = None;
        assert!(is_quiet(&peer, now));
    }

    /// Sign a record via `sharerr_lighthouse::signable_bytes` directly —
    /// the same construction the lighthouse itself verifies against, since
    /// `gossip`'s signing goes through the identical, now-shared function.
    fn signed_lighthouse_record(seed: u8, addr: &str, signed_at: i64) -> LighthouseRecord {
        let signing = SigningKey::from_bytes(&test_key_bytes(seed));
        let pubkey = hex::encode(signing.verifying_key().to_bytes());
        let endpoints = vec![LighthouseRecordEndpoint {
            kind: "tracker".to_owned(),
            addr: addr.to_owned(),
            observed_at: signed_at,
        }];
        let bytes = sharerr_lighthouse::signable_bytes(&pubkey, &endpoints, signed_at).unwrap();
        let signature = hex::encode(signing.sign(&bytes).to_bytes());
        LighthouseRecord {
            pubkey,
            endpoints,
            signed_at,
            signature,
        }
    }

    /// Start a real lighthouse on a loopback port, returning its state (for
    /// pre-seeding/inspecting directly) and its base URL.
    async fn spawn_lighthouse() -> (Arc<sharerr_lighthouse::LighthouseState>, Url) {
        // A random decoy secret, never a constant: see the note on the same
        // choice in `sharerr-lighthouse`'s own tests. Nothing below depends on
        // its value.
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).expect("the OS RNG is available");
        let state = Arc::new(sharerr_lighthouse::LighthouseState::new(secret));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        let router = sharerr_lighthouse::routes(Arc::clone(&state));
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (state, Url::parse(&format!("http://{addr}")).unwrap())
    }

    async fn store_with_peer(label: &str, our_key: &str) -> (Store, Peer) {
        let store = Store::open_in_memory().await.unwrap();
        let peer = store
            .create_peer(label, &SecretString::from(our_key), PeerScope::All)
            .await
            .unwrap();
        (store, peer)
    }

    #[tokio::test]
    async fn reporting_publishes_our_record_under_every_active_peers_key_hash() {
        let (lighthouse, url) = spawn_lighthouse().await;
        let (store, alex) = store_with_peer("Alex", "alex-key").await;
        let blair = store
            .create_peer("Blair", &SecretString::from("blair-key"), PeerScope::All)
            .await
            .unwrap();
        let peers = store.list_peers().await.unwrap();

        let own = signed_lighthouse_record(1, "http://203.0.113.9:41234", 1000);
        let http = reqwest::Client::new();
        report(
            &http,
            &[url],
            &peers,
            Some(&own),
            &LighthouseStatus::default(),
        )
        .await;

        let for_alex = lighthouse.lookup(&alex.key_hash).await;
        assert_eq!(for_alex, own);
        let for_blair = lighthouse.lookup(&blair.key_hash).await;
        assert_eq!(for_blair, own);
    }

    /// A refused report must leave the pass intact: every *other* friend's
    /// key hash still gets published, and nothing panics or aborts. One
    /// lighthouse holding a pin against this instance cannot be allowed to
    /// stop it reaching the others.
    #[tokio::test]
    async fn a_refused_report_does_not_stop_the_rest_of_the_pass() {
        let (lighthouse, url) = spawn_lighthouse().await;
        let (store, alex) = store_with_peer("Alex", "alex-key").await;
        let blair = store
            .create_peer("Blair", &SecretString::from("blair-key"), PeerScope::All)
            .await
            .unwrap();
        let peers = store.list_peers().await.unwrap();

        // Somebody else got to Alex's key hash first, with their own keypair.
        let now = sharerr_core::endpoint::now_epoch();
        lighthouse
            .report(
                &alex.key_hash,
                signed_lighthouse_record(9, "198.51.100.4:6881", now),
            )
            .await
            .unwrap();

        let own = signed_lighthouse_record(1, "203.0.113.9:41234", now);
        let http = reqwest::Client::new();
        report(
            &http,
            &[url],
            &peers,
            Some(&own),
            &LighthouseStatus::default(),
        )
        .await;

        assert_eq!(
            lighthouse.lookup(&alex.key_hash).await.endpoints[0].addr,
            "198.51.100.4:6881",
            "the squatter's record stands; ours was refused"
        );
        assert_eq!(
            lighthouse.lookup(&blair.key_hash).await,
            own,
            "Blair's key hash was never claimed, so that report must still land"
        );
    }

    #[tokio::test]
    async fn a_missing_self_record_skips_reporting_entirely() {
        let (lighthouse, url) = spawn_lighthouse().await;
        let (store, alex) = store_with_peer("Alex", "alex-key").await;
        let peers = store.list_peers().await.unwrap();

        let http = reqwest::Client::new();
        report(&http, &[url], &peers, None, &LighthouseStatus::default()).await;

        // Nothing was ever reported, so a lookup surfaces only a decoy.
        let looked_up = lighthouse.lookup(&alex.key_hash).await;
        assert!(sharerr_lighthouse::verify(&looked_up).is_err());
    }

    #[tokio::test]
    async fn a_quiet_peers_lighthouse_sighting_is_recorded() {
        let (lighthouse, url) = spawn_lighthouse().await;
        let dir = tempfile::tempdir().unwrap();
        let mut vault = vault_in(&dir);

        let (store, peer) = store_with_peer("Alex", "our-key-for-alex").await;
        let record = signed_lighthouse_record(1, "http://203.0.113.9:41234", 1000);
        store
            .bind_peer_pubkey(peer.id, &record.pubkey)
            .await
            .unwrap();

        // The key Alex issued *us*, which we hash to look Alex up.
        let raw_key = "alex-issued-us-this-key";
        vault
            .put(
                &secret_keys::peer_gossip_key(peer.id),
                &SecretString::from(raw_key),
            )
            .unwrap();
        lighthouse
            .report(&sharerr_lighthouse::hash_key(raw_key), record.clone())
            .await
            .unwrap();

        let peers = store.list_peers().await.unwrap();
        let http = reqwest::Client::new();
        lookup_quiet(
            &http,
            &[url],
            &peers,
            &vault,
            &store,
            &LighthouseStatus::default(),
        )
        .await;

        let endpoints = store.peer_endpoints(peer.id).await.unwrap();
        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0].addr, "http://203.0.113.9:41234");
        assert_eq!(endpoints[0].via, ObservedVia::Lighthouse);
    }

    #[tokio::test]
    async fn a_decoy_answer_is_never_recorded() {
        let (_lighthouse, url) = spawn_lighthouse().await;
        let dir = tempfile::tempdir().unwrap();
        let mut vault = vault_in(&dir);

        let (store, peer) = store_with_peer("Alex", "our-key-for-alex").await;
        // Bound, but never reported to the lighthouse — every lookup for
        // Alex gets a decoy.
        store
            .bind_peer_pubkey(peer.id, "some-pubkey")
            .await
            .unwrap();
        vault
            .put(
                &secret_keys::peer_gossip_key(peer.id),
                &SecretString::from("alex-issued-us-this-key"),
            )
            .unwrap();

        let peers = store.list_peers().await.unwrap();
        let http = reqwest::Client::new();
        lookup_quiet(
            &http,
            &[url],
            &peers,
            &vault,
            &store,
            &LighthouseStatus::default(),
        )
        .await;

        assert!(store.peer_endpoints(peer.id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_peer_with_no_known_pubkey_is_never_looked_up() {
        let (lighthouse, url) = spawn_lighthouse().await;
        let dir = tempfile::tempdir().unwrap();
        let mut vault = vault_in(&dir);

        // Never gossiped-with: no pubkey bound at all, even though a record
        // happens to sit on the lighthouse under the right hash.
        let (store, peer) = store_with_peer("Alex", "our-key-for-alex").await;
        let raw_key = "alex-issued-us-this-key";
        vault
            .put(
                &secret_keys::peer_gossip_key(peer.id),
                &SecretString::from(raw_key),
            )
            .unwrap();
        lighthouse
            .report(
                &sharerr_lighthouse::hash_key(raw_key),
                signed_lighthouse_record(1, "http://203.0.113.9:1", 1000),
            )
            .await
            .unwrap();

        let peers = store.list_peers().await.unwrap();
        let http = reqwest::Client::new();
        lookup_quiet(
            &http,
            &[url],
            &peers,
            &vault,
            &store,
            &LighthouseStatus::default(),
        )
        .await;

        assert!(
            store.peer_endpoints(peer.id).await.unwrap().is_empty(),
            "no known pubkey means nothing to verify a result against"
        );
    }

    #[tokio::test]
    async fn run_returns_immediately_with_no_lighthouses_configured() {
        let (_dir, state) = crate::state::fixtures::unconfigured();
        assert!(state.config().await.lighthouse.urls.is_empty());

        // Nothing to assert beyond "returns instead of hanging or panicking" —
        // the point is exercising the empty-`urls` early return.
        let http = reqwest::Client::new();
        run(&state, &http).await;
    }

    /// `open_vault` reads `SHARERR_MASTER_KEY` from the real process env, and
    /// `secrets.rs` has a `#[test]` that legitimately sets it via
    /// `figment::Jail` — so asserting the var's *absence* would race that test
    /// under the parallel runner unless this is scoped by `Jail` too. `run` is
    /// async, and `Jail::expect_with`'s closure is not, hence the plain
    /// `#[test]` driving its own runtime rather than `#[tokio::test]`.
    #[test]
    fn run_returns_when_the_vault_cannot_open_even_with_lighthouses_configured() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            let config = sharerr_core::Config {
                data_dir: jail.directory().to_path_buf(),
                lighthouse: sharerr_core::config::LighthouseConfig {
                    urls: vec![Url::parse("http://lighthouse.example.invalid/").unwrap()],
                    ..Default::default()
                },
                ..Default::default()
            };
            let path = jail.directory().join("sharerr.toml");
            let state = Arc::new(ServeState::new(config, path, None));

            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async {
                // `store()` opens a plain sqlite file with no vault involved,
                // so it succeeds here; it is `open_vault()` — with no
                // `SHARERR_MASTER_KEY` set — that must be what stops `run`
                // before it ever reaches the network.
                assert!(state.store().await.is_ok());
                assert!(state.open_vault().await.is_err());

                let http = reqwest::Client::new();
                run(&state, &http).await;
            });
            Ok(())
        });
    }
}
