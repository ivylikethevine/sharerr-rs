//! Reporting this instance's endpoint to configured lighthouses, and querying
//! them for a friend gossip cannot currently reach.
//!
//! See `docs/ROADMAP.md`'s "The lighthouse" for the design brief. Gossip
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
use sharerr_core::endpoint::now_epoch;
use sharerr_lighthouse::EndpointRecord as LighthouseRecord;
use sharerr_lighthouse::RecordEndpoint as LighthouseRecordEndpoint;
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
    let http = match reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
    {
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
    let urls = state.config().await.lighthouse.urls;
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
    let own = own.as_ref().map(to_lighthouse_record);
    // Publishing this instance's record and looking up quiet friends touch
    // disjoint state (own record vs. friends' pubkeys) — independent, so run
    // them together rather than one after the other.
    tokio::join!(
        report(http, &urls, &peers, own.as_ref()),
        lookup_quiet(http, &urls, &peers, &vault, &store)
    );
}

/// Convert gossip's `EndpointRecord` to the lighthouse crate's field-for-field
/// duplicate of the same shape — two types by design, see
/// `sharerr-lighthouse`'s module docs on why it stands alone.
fn to_lighthouse_record(record: &gossip::EndpointRecord) -> LighthouseRecord {
    LighthouseRecord {
        pubkey: record.pubkey.clone(),
        endpoints: record
            .endpoints
            .iter()
            .map(|e| LighthouseRecordEndpoint {
                kind: e.kind.clone(),
                addr: e.addr.clone(),
                observed_at: e.observed_at,
            })
            .collect(),
        signed_at: record.signed_at,
        signature: record.signature.clone(),
    }
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
) {
    let Some(record) = own else {
        tracing::debug!("no self-record available yet — skipping lighthouse report");
        return;
    };

    let mut attempts = Vec::new();
    for peer in peers.iter().filter(|p| !p.is_revoked()) {
        for url in urls {
            attempts.push(report_one(http, url, &peer.key_hash, record));
        }
    }
    futures::future::join_all(attempts).await;
}

/// Publish one record, and — unlike a fire-and-forget POST — say something
/// when the lighthouse refuses it.
///
/// A refusal used to be invisible here: only a transport error was logged, so
/// a 403 or a 503 looked exactly like a successful report. Both of the answers
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
async fn report_one(http: &reqwest::Client, base: &Url, key_hash: &str, record: &LighthouseRecord) {
    let endpoint = format!(
        "{}/lighthouse/v1/report/{key_hash}",
        base.as_str().trim_end_matches('/')
    );
    let response = match http.post(&endpoint).json(record).send().await {
        Ok(response) => response,
        Err(err) => {
            tracing::debug!(url = %base, reason = %error_chain(&err), "lighthouse report failed");
            return;
        }
    };

    let status = response.status();
    if status.is_success() {
        return;
    }
    // The body is a short reason string; a lighthouse that answered at all
    // will have one, and it is the most useful half of this line.
    let reason = response.text().await.unwrap_or_default();
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
) {
    let now = now_epoch();

    let lookups = peers
        .iter()
        .filter(|p| !p.is_revoked())
        .map(|peer| lookup_quiet_one(http, urls, vault, store, peer, now));
    futures::future::join_all(lookups).await;
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
    now: i64,
) {
    let Some(pubkey) = peer.pubkey.as_deref() else {
        return;
    };
    let quiet = peer
        .last_seen_at
        .is_none_or(|seen| now - seen >= QUIET_THRESHOLD_SECS);
    if !quiet {
        return;
    }
    let Ok(Some(key)) = vault.get(&secret_keys::peer_gossip_key(peer.id)) else {
        return;
    };
    let hash = sharerr_lighthouse::hash_key(key.expose_secret());

    for url in urls {
        match lookup_one(http, url, &hash, pubkey).await {
            Ok(Some(record)) => {
                apply_lookup(store, peer.id, &record).await;
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
    let endpoint = format!(
        "{}/lighthouse/v1/lookup/{key_hash}",
        base.as_str().trim_end_matches('/')
    );
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
                endpoint.observed_at,
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

    use std::net::SocketAddr;

    use ed25519_dalek::{Signer, SigningKey};
    use secrecy::SecretString;
    use sharerr_store::PeerScope;

    use super::*;

    /// Sign a record the same way `sharerr_lighthouse`'s own private
    /// `signable_bytes` would — that function is not reachable from outside
    /// its crate, so this is a deliberate, small duplication rather than a
    /// shared helper; `sharerr_lighthouse`'s test suite carries the same
    /// duplication against `gossip`'s signing for the identical reason.
    fn signed_lighthouse_record(seed: u8, addr: &str, signed_at: i64) -> LighthouseRecord {
        #[derive(serde::Serialize)]
        struct Signable<'a> {
            pubkey: &'a str,
            endpoints: &'a [LighthouseRecordEndpoint],
            signed_at: i64,
        }

        let signing = SigningKey::from_bytes(&[seed; 32]);
        let pubkey = hex::encode(signing.verifying_key().to_bytes());
        let endpoints = vec![LighthouseRecordEndpoint {
            kind: "tracker".to_owned(),
            addr: addr.to_owned(),
            observed_at: signed_at,
        }];
        let bytes = serde_json::to_vec(&Signable {
            pubkey: &pubkey,
            endpoints: &endpoints,
            signed_at,
        })
        .unwrap();
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
        let state = Arc::new(sharerr_lighthouse::LighthouseState::new([7u8; 32]));
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

    fn vault_in(dir: &tempfile::TempDir) -> Vault {
        Vault::open(dir.path().join("vault.bin"), &SecretString::from("master")).unwrap()
    }

    #[tokio::test]
    async fn to_lighthouse_record_copies_every_field() {
        let record = gossip::EndpointRecord {
            pubkey: "abcd".to_owned(),
            endpoints: vec![gossip::RecordEndpoint {
                kind: "api".to_owned(),
                addr: "203.0.113.5:1".to_owned(),
                observed_at: 42,
            }],
            signed_at: 42,
            signature: "ef01".to_owned(),
        };

        let converted = to_lighthouse_record(&record);
        assert_eq!(converted.pubkey, "abcd");
        assert_eq!(converted.signed_at, 42);
        assert_eq!(converted.signature, "ef01");
        assert_eq!(converted.endpoints[0].kind, "api");
        assert_eq!(converted.endpoints[0].addr, "203.0.113.5:1");
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
        report(&http, &[url], &peers, Some(&own)).await;

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
        report(&http, &[url], &peers, Some(&own)).await;

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
        report(&http, &[url], &peers, None).await;

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
        lookup_quiet(&http, &[url], &peers, &vault, &store).await;

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
        lookup_quiet(&http, &[url], &peers, &vault, &store).await;

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
        lookup_quiet(&http, &[url], &peers, &vault, &store).await;

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
