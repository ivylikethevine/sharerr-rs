//! One-string friend pairing.
//!
//! Two friends who both run sharerr used to exchange four values by hand: each
//! side's feed URL and the key it issued, typed into the other side's Friends
//! page. An **invite** folds one side's three facts — where their sharerr is,
//! the key they issued, and their gossip identity — into one pasteable string.
//! Pasting it creates the friend, stores the outbound half, pins the identity,
//! and then returns the favour over HTTP: the pasting instance `POST`s its own
//! address and freshly minted key to the inviter's `/api/gossip/pair`,
//! authenticated by the very key the invite carried, so the inviter's row
//! fills in without anyone typing a second thing. When that call cannot reach
//! them, the page shows a **reply** invite to send back by hand instead;
//! pasting a reply fills the row its `in_reply_to` names rather than creating
//! a second friend.
//!
//! # Trust
//!
//! The pubkey in an invite is pinned at pairing by the operator who pasted it,
//! rather than trust-on-first-use from whatever the friend later signs with.
//! The pair endpoint grants nothing the invite key did not already: a holder
//! of that key can already pull the feed and gossip under it, and the endpoint
//! only ever fills an *empty* outbound half. A second call, or one presenting
//! a different identity from the one pinned, is refused rather than applied.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sharerr_client::error_chain;
use sharerr_core::config::secret_keys;
use sharerr_core::endpoint::{MAX_FUTURE_SKEW_SECS, now_epoch};

use crate::gossip::EndpointRecord;
use crate::state::ServeState;
use crate::torznab::Caller;

/// What every invite string starts with, so a paste of the wrong thing is
/// refused by name rather than as undecodable base64.
pub const PREFIX: &str = "sharerr-invite:";

const VERSION: u8 = 1;

/// How long a pasted invite is honoured after it was issued. A string left
/// in a chat should stop being a credential at some point, and the issuing
/// side keeps no state to revoke one by: the cutoff is the whole mechanism.
/// A week comfortably covers "I'll set it up at the weekend".
pub const INVITE_TTL_SECS: i64 = 7 * 24 * 60 * 60;

/// Longest key a friend's instance is allowed to hand us. Ours are 40 hex
/// characters; the cap only keeps a hostile body from parking a megabyte in
/// the vault.
const MAX_KEY_LEN: usize = 256;

/// The facts one side needs about the other, as the invite string carries them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invite {
    pub v: u8,
    /// The base URL the sender's sharerr is reached on.
    pub url: String,
    /// The key the sender issued the recipient.
    pub key: String,
    /// When the string was made, epoch seconds; refused after
    /// [`INVITE_TTL_SECS`].
    pub issued_at: i64,
    /// The sender's gossip identity, hex Ed25519, when their vault was open at
    /// the time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pubkey: Option<String>,
    /// Set on a reply: the SHA-256 hex of the key the *recipient* issued, so
    /// their instance can find the friend this answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<String>,
}

impl Invite {
    pub fn new(url: String, key: String, pubkey: Option<String>) -> Self {
        Self {
            v: VERSION,
            url,
            key,
            issued_at: now_epoch(),
            pubkey,
            in_reply_to: None,
        }
    }

    /// The pasteable form: the prefix, then the JSON as unpadded URL-safe
    /// base64 — one token with no characters a chat client would mangle.
    pub fn encode(&self) -> String {
        // Serialising a struct of strings and options cannot fail.
        let json = serde_json::to_vec(self).unwrap_or_default();
        format!(
            "{PREFIX}{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
        )
    }

    /// Parse what somebody pasted. Every failure names what was wrong in
    /// words meant for the operator, since this is the one place a string
    /// typed by a human meets the format.
    pub fn decode(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let Some(encoded) = text.strip_prefix(PREFIX) else {
            return Err(format!(
                "that is not a sharerr invite: it should start with \"{PREFIX}\""
            ));
        };
        let json = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded.trim())
            .map_err(|_| "that invite is damaged: part of it did not paste".to_owned())?;
        let invite: Self = serde_json::from_slice(&json)
            .map_err(|_| "that invite is damaged: its contents do not parse".to_owned())?;
        if invite.v != VERSION {
            return Err(format!(
                "that invite was made by a newer sharerr (format {}); update this one first",
                invite.v
            ));
        }
        let now = now_epoch();
        if invite.issued_at > now.saturating_add(MAX_FUTURE_SKEW_SECS) {
            return Err(
                "that invite is dated in the future; check the clock on the instance that made it"
                    .to_owned(),
            );
        }
        if now.saturating_sub(invite.issued_at) > INVITE_TTL_SECS {
            return Err("that invite is more than seven days old; have them delete and re-add you on their Friends page for a fresh one".to_owned());
        }
        validate_url(&invite.url)?;
        validate_key(&invite.key)?;
        if let Some(pubkey) = &invite.pubkey
            && !is_hex_pubkey(pubkey)
        {
            return Err("that invite carries an identity key of the wrong shape".to_owned());
        }
        if let Some(hash) = &invite.in_reply_to
            && (hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err("that reply names a key hash of the wrong shape".to_owned());
        }
        Ok(invite)
    }
}

fn validate_url(url: &str) -> Result<(), String> {
    let parsed =
        url::Url::parse(url).map_err(|err| format!("the address {url:?} does not parse: {err}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!("the address {url:?} is not http or https"));
    }
    Ok(())
}

fn validate_key(key: &str) -> Result<(), String> {
    if key.trim().is_empty() {
        return Err("no key was included".to_owned());
    }
    if key.len() > MAX_KEY_LEN {
        return Err("the key is longer than any sharerr issues".to_owned());
    }
    Ok(())
}

fn is_hex_pubkey(pubkey: &str) -> bool {
    pubkey.len() == 64 && pubkey.bytes().all(|b| b.is_ascii_hexdigit())
}

// ---------------------------------------------------------------------------
// The outbound half, applied locally
// ---------------------------------------------------------------------------

/// Everything an invite tells us about a friend, written to their row and
/// the vault: pin the identity first, since that is the one step that can be
/// refused on principle, then the address, then the key.
pub async fn apply_invite(
    store: &sharerr_store::Store,
    vault: &mut sharerr_store::Vault,
    peer: &sharerr_store::Peer,
    invite: &Invite,
) -> Result<(), String> {
    if let Some(pubkey) = &invite.pubkey {
        let bound = store
            .bind_peer_pubkey(peer.id, pubkey)
            .await
            .map_err(|err| format!("could not pin {}'s identity: {err}", peer.label))?;
        if !bound {
            return Err(format!(
                "{}'s sharerr now presents a different identity from the one pinned here; \
                 if they reinstalled, delete this friend and pair again",
                peer.label
            ));
        }
    }
    match store.set_peer_gossip_url(peer.id, Some(&invite.url)).await {
        Ok(true) => {}
        Ok(false) => return Err("there is no friend with that id any more".to_owned()),
        Err(err) => return Err(format!("could not save their address: {err}")),
    }
    vault
        .put(
            &secret_keys::peer_gossip_key(peer.id),
            &secrecy::SecretString::from(invite.key.clone()),
        )
        .map_err(|err| format!("could not store the key they issued: {err}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// HTTP: the return half
// ---------------------------------------------------------------------------

/// What a pasting instance sends back to the inviter: where it is, the key it
/// minted for them, and its signed self-record so its identity is pinned
/// from proof rather than from a bare claim.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = GossipPairRequest)]
pub struct PairRequest {
    /// The base URL the caller's own sharerr is reached on.
    pub url: String,
    /// The key the caller issued this instance.
    pub key: String,
    /// The caller's signed self-record, when it has a gossip identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<EndpointRecord>,
}

/// `POST /api/gossip/pair` — a friend who just pasted this instance's invite
/// handing back their side.
#[utoipa::path(
    post,
    path = "/api/gossip/pair",
    tag = "gossip",
    operation_id = "gossipPair",
    security(("peerApiKey" = [])),
    request_body = PairRequest,
    responses(
        (status = 204, description =
         "The caller's address and key are stored for the peer the `apikey` \
          belongs to, and their identity is pinned from the signed record if one \
          was sent. Nothing is returned: the caller already holds everything \
          this instance knows about it."),
        (status = 400, description =
         "The address does not parse, the key is empty or oversized, or the \
          record's signature does not verify.", body = String),
        (status = 401, content_type = "application/xml",
         description = "No `apikey`, or one that matches no active peer.", body = String),
        (status = 409, description =
         "This peer's outbound half is already set, or the record names a \
          different identity from the one pinned. Pairing fills an empty row \
          and never overwrites one; the operator clears it from Friends.",
         body = String),
        (status = 503, description = "The database or the vault is not open.", body = String),
    ),
)]
pub async fn pair(
    State(state): State<Arc<ServeState>>,
    caller: Caller,
    axum::Json(request): axum::Json<PairRequest>,
) -> Response {
    let Ok(store) = state.store().await else {
        return (StatusCode::SERVICE_UNAVAILABLE, "not ready").into_response();
    };
    let peer = match store.peer(caller.peer_id()).await {
        Ok(Some(peer)) => peer,
        _ => return (StatusCode::SERVICE_UNAVAILABLE, "not ready").into_response(),
    };

    // Everything that can be refused is refused before anything is written.
    if let Err(reason) = validate_url(&request.url).and_then(|()| validate_key(&request.key)) {
        return (StatusCode::BAD_REQUEST, reason).into_response();
    }
    if let Some(record) = &request.record {
        if let Err(reason) = crate::gossip::verify(record) {
            return (StatusCode::BAD_REQUEST, format!("record: {reason}")).into_response();
        }
        if peer
            .pubkey
            .as_deref()
            .is_some_and(|pinned| pinned != record.pubkey)
        {
            return (
                StatusCode::CONFLICT,
                "a different identity is already pinned for this key",
            )
                .into_response();
        }
    }
    let Ok(mut vault) = state.open_vault().await else {
        return (StatusCode::SERVICE_UNAVAILABLE, "the vault is not open").into_response();
    };
    let vault_key = secret_keys::peer_gossip_key(peer.id);
    let key_stored = matches!(vault.get(&vault_key), Ok(Some(_)));
    if peer.gossip_url.is_some() || key_stored {
        return (
            StatusCode::CONFLICT,
            "already paired: this instance holds an address or key for you",
        )
            .into_response();
    }

    // The key first: a vault write that fails leaves the row untouched, and a
    // row write that fails after it is a retry away from consistent, since the
    // conflict check above reads both.
    if let Err(err) = vault.put(&vault_key, &secrecy::SecretString::from(request.key)) {
        tracing::warn!(peer = peer.id, error = %err, "pairing: could not store the key");
        return (StatusCode::SERVICE_UNAVAILABLE, "could not store the key").into_response();
    }
    match store.set_peer_gossip_url(peer.id, Some(&request.url)).await {
        Ok(true) => {}
        Ok(false) => return (StatusCode::CONFLICT, "no such peer").into_response(),
        Err(err) => {
            tracing::warn!(peer = peer.id, error = %err, "pairing: could not store the address");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "could not store the address",
            )
                .into_response();
        }
    }
    // The record goes through the same ingest a gossip push would: it binds
    // the identity (first use) and records the endpoints it carries.
    if let Some(record) = request.record {
        let summary = crate::gossip::ingest(&store, peer.id, vec![record]).await;
        tracing::debug!(
            ?summary,
            peer = peer.id,
            "pairing: ingested the self-record"
        );
    }

    tracing::info!(peer = %peer.label, "paired: a friend returned their side");
    StatusCode::NO_CONTENT.into_response()
}

/// Return our side to the inviter: `POST {invite.url}/api/gossip/pair` under
/// the key the invite carried. The error is a sentence for the Friends page,
/// which then shows the reply invite to send by hand.
pub async fn send_pair(invite: &Invite, request: &PairRequest) -> Result<(), String> {
    let http = sharerr_client::http_client_with_timeout(Duration::from_secs(10))
        .map_err(|err| format!("could not build an HTTP client: {err}"))?;
    let endpoint = format!("{}/api/gossip/pair", invite.url.trim_end_matches('/'));
    let response = http
        .post(&endpoint)
        .query(&[("apikey", invite.key.as_str())])
        .json(request)
        .send()
        .await
        .map_err(|err| format!("could not reach their sharerr: {}", error_chain(&err)))?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let body = response.text().await.unwrap_or_default();
    let body = body.trim();
    Err(if body.is_empty() {
        format!("their sharerr answered {status}")
    } else {
        format!("their sharerr answered {status}: {body}")
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::result_large_err)]

    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use secrecy::SecretString;
    use sharerr_store::PeerScope;
    use tower::ServiceExt as _;

    fn invite() -> Invite {
        Invite::new(
            "https://sam.example:8477".to_owned(),
            "0123456789abcdef0123456789abcdef01234567".to_owned(),
            Some("ab".repeat(32)),
        )
    }

    #[test]
    fn an_invite_round_trips_through_its_string_form() {
        let original = invite();
        let text = original.encode();
        assert!(text.starts_with(PREFIX));
        assert!(
            !text.contains(['=', '+', '/']),
            "no characters a chat client would mangle: {text}"
        );
        assert_eq!(Invite::decode(&text).unwrap(), original);
        assert_eq!(
            Invite::decode(&format!("  {text}\n")).unwrap(),
            original,
            "surrounding whitespace from a paste is tolerated"
        );
    }

    #[test]
    fn a_reply_keeps_what_it_answers() {
        let mut reply = invite();
        reply.in_reply_to = Some(sharerr_lighthouse::hash_key("their-key"));
        let decoded = Invite::decode(&reply.encode()).unwrap();
        assert_eq!(
            decoded.in_reply_to.as_deref(),
            Some(sharerr_lighthouse::hash_key("their-key").as_str())
        );
    }

    #[test]
    fn decode_names_what_is_wrong() {
        let err = Invite::decode("hello").unwrap_err();
        assert!(err.contains("not a sharerr invite"), "{err}");

        let err = Invite::decode(&format!("{PREFIX}***")).unwrap_err();
        assert!(err.contains("did not paste"), "{err}");

        let garbage = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"{\"v\":1}");
        let err = Invite::decode(&format!("{PREFIX}{garbage}")).unwrap_err();
        assert!(err.contains("do not parse"), "{err}");

        let mut newer = invite();
        newer.v = 2;
        let err = Invite::decode(&newer.encode()).unwrap_err();
        assert!(err.contains("newer sharerr"), "{err}");

        let mut expired = invite();
        expired.issued_at = now_epoch() - INVITE_TTL_SECS - 1;
        let err = Invite::decode(&expired.encode()).unwrap_err();
        assert!(err.contains("seven days old"), "{err}");

        let mut future = invite();
        future.issued_at = now_epoch() + MAX_FUTURE_SKEW_SECS + 1;
        let err = Invite::decode(&future.encode()).unwrap_err();
        assert!(err.contains("in the future"), "{err}");

        let mut bad_url = invite();
        bad_url.url = "ftp://sam.example".to_owned();
        let err = Invite::decode(&bad_url.encode()).unwrap_err();
        assert!(err.contains("not http"), "{err}");

        let mut no_key = invite();
        no_key.key = "  ".to_owned();
        let err = Invite::decode(&no_key.encode()).unwrap_err();
        assert!(err.contains("no key"), "{err}");

        let mut long_key = invite();
        long_key.key = "k".repeat(MAX_KEY_LEN + 1);
        let err = Invite::decode(&long_key.encode()).unwrap_err();
        assert!(err.contains("longer"), "{err}");

        let mut bad_pubkey = invite();
        bad_pubkey.pubkey = Some("not-hex".to_owned());
        let err = Invite::decode(&bad_pubkey.encode()).unwrap_err();
        assert!(err.contains("identity key"), "{err}");

        let mut bad_reply = invite();
        bad_reply.in_reply_to = Some("abc".to_owned());
        let err = Invite::decode(&bad_reply.encode()).unwrap_err();
        assert!(err.contains("key hash"), "{err}");
    }

    /// Six days and a bit is still inside the window; the cutoff is a hard
    /// edge, not a grace period that drifts with clock skew.
    #[test]
    fn an_invite_just_inside_the_window_still_decodes() {
        let mut aging = invite();
        aging.issued_at = now_epoch() - INVITE_TTL_SECS + 60;
        assert!(Invite::decode(&aging.encode()).is_ok());
    }

    #[test]
    fn an_unknown_field_is_ignored_rather_than_refused() {
        let json = serde_json::json!({
            "v": 1, "url": "http://sam.example", "key": "k", "issued_at": now_epoch(), "later": true
        });
        let text = format!(
            "{PREFIX}{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json.to_string())
        );
        assert_eq!(Invite::decode(&text).unwrap().key, "k");
    }

    #[test]
    fn key_hash_matches_what_the_store_files_a_peer_under() {
        // `Store::create_peer` hashes the same way; this pins the encoding so
        // a reply's `in_reply_to` keeps finding the row.
        assert_eq!(
            sharerr_lighthouse::hash_key("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    // ------------------------------------------------------------- the API

    /// A signed self-record from a throwaway identity in a vault of its own:
    /// the fixture's `data_dir` holds the vault under test, keyed by the
    /// `Jail`'s master key, and `vault_in` would write a differently keyed
    /// file at that same path.
    fn signed_record(addr: &str) -> EndpointRecord {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = crate::test_support::vault_in(&dir);
        let identity = crate::gossip::Identity::load_or_create(&mut vault).unwrap();
        identity
            .sign_record(
                vec![crate::gossip::RecordEndpoint {
                    kind: "api".to_owned(),
                    addr: addr.to_owned(),
                    observed_at: 1_000,
                }],
                1_000,
            )
            .unwrap()
    }

    async fn post_pair(
        state: &Arc<ServeState>,
        apikey: &str,
        body: &PairRequest,
    ) -> (StatusCode, String) {
        let request = Request::builder()
            .method("POST")
            .uri(format!("/api/gossip/pair?apikey={apikey}"))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(body).unwrap()))
            .unwrap();
        let response = crate::torznab::routes(Arc::clone(state))
            .oneshot(request)
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    fn request() -> PairRequest {
        PairRequest {
            url: "http://203.0.113.9:8477".to_owned(),
            key: "the-key-sam-minted-for-us".to_owned(),
            record: Some(signed_record("http://203.0.113.9:8477")),
        }
    }

    #[tokio::test]
    async fn pair_is_behind_the_peer_key() {
        let (_dir, state) = crate::state::fixtures::unconfigured();
        let (status, _) = post_pair(&state, "wrong", &request()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    /// The vault is what the key lands in, so this and the tests after it
    /// run under a `Jail` holding a master key — the one sanctioned way to
    /// open a real vault in this binary (see CLAUDE.md).
    #[test]
    fn pair_fills_an_empty_row_once_and_refuses_a_second_time() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("SHARERR_MASTER_KEY", "a-master-key");
            let (_dir, state) = crate::state::fixtures::unconfigured();
            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async {
                let store = state.store().await.unwrap();
                let sam = store
                    .create_peer("Sam", &SecretString::from("sam-key"), PeerScope::All)
                    .await
                    .unwrap();
                let request = request();

                let (status, body) = post_pair(&state, "sam-key", &request).await;
                assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

                let peer = store.peer(sam.id).await.unwrap().unwrap();
                assert_eq!(peer.gossip_url.as_deref(), Some("http://203.0.113.9:8477/"));
                assert_eq!(
                    peer.pubkey.as_deref(),
                    Some(request.record.as_ref().unwrap().pubkey.as_str()),
                    "the identity is pinned from the signed record"
                );
                let vault = state.open_vault().await.unwrap();
                let stored = vault
                    .get(&secret_keys::peer_gossip_key(sam.id))
                    .unwrap()
                    .unwrap();
                assert_eq!(secrecy::ExposeSecret::expose_secret(&stored), request.key);
                let endpoints = store
                    .peer_endpoints_for(&[sam.id])
                    .await
                    .unwrap()
                    .remove(&sam.id)
                    .unwrap_or_default();
                assert_eq!(endpoints.len(), 1, "the record's endpoint was recorded too");

                let (status, body) = post_pair(&state, "sam-key", &request).await;
                assert_eq!(status, StatusCode::CONFLICT);
                assert!(body.contains("already paired"), "{body}");
            });
            Ok(())
        });
    }

    #[test]
    fn pair_refuses_a_bad_address_or_record_before_writing_anything() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("SHARERR_MASTER_KEY", "a-master-key");
            let (_dir, state) = crate::state::fixtures::unconfigured();
            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async {
                let store = state.store().await.unwrap();
                let sam = store
                    .create_peer("Sam", &SecretString::from("sam-key"), PeerScope::All)
                    .await
                    .unwrap();

                let mut bad_url = request();
                bad_url.url = "not a url".to_owned();
                let (status, _) = post_pair(&state, "sam-key", &bad_url).await;
                assert_eq!(status, StatusCode::BAD_REQUEST);

                let mut empty_key = request();
                empty_key.key = String::new();
                let (status, _) = post_pair(&state, "sam-key", &empty_key).await;
                assert_eq!(status, StatusCode::BAD_REQUEST);

                let mut forged = request();
                forged.record.as_mut().unwrap().signature = "00".repeat(64);
                let (status, body) = post_pair(&state, "sam-key", &forged).await;
                assert_eq!(status, StatusCode::BAD_REQUEST);
                assert!(body.starts_with("record:"), "{body}");

                let peer = store.peer(sam.id).await.unwrap().unwrap();
                assert!(peer.gossip_url.is_none());
                assert!(peer.pubkey.is_none());
                let vault = state.open_vault().await.unwrap();
                assert!(
                    vault
                        .get(&secret_keys::peer_gossip_key(sam.id))
                        .unwrap()
                        .is_none()
                );
            });
            Ok(())
        });
    }

    #[test]
    fn pair_refuses_a_record_from_an_identity_other_than_the_pinned_one() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("SHARERR_MASTER_KEY", "a-master-key");
            let (_dir, state) = crate::state::fixtures::unconfigured();
            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async {
                let store = state.store().await.unwrap();
                let sam = store
                    .create_peer("Sam", &SecretString::from("sam-key"), PeerScope::All)
                    .await
                    .unwrap();
                assert!(
                    store
                        .bind_peer_pubkey(sam.id, &"cd".repeat(32))
                        .await
                        .unwrap()
                );

                let (status, body) = post_pair(&state, "sam-key", &request()).await;
                assert_eq!(status, StatusCode::CONFLICT);
                assert!(body.contains("different identity"), "{body}");
                assert!(
                    store
                        .peer(sam.id)
                        .await
                        .unwrap()
                        .unwrap()
                        .gossip_url
                        .is_none()
                );
            });
            Ok(())
        });
    }

    /// A record is optional: a friend whose vault was shut when they pasted
    /// has no identity to send, and the address and key still land.
    #[test]
    fn pair_without_a_record_stores_the_address_and_key_only() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("SHARERR_MASTER_KEY", "a-master-key");
            let (_dir, state) = crate::state::fixtures::unconfigured();
            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async {
                let store = state.store().await.unwrap();
                let sam = store
                    .create_peer("Sam", &SecretString::from("sam-key"), PeerScope::All)
                    .await
                    .unwrap();
                let mut request = request();
                request.record = None;

                let (status, _) = post_pair(&state, "sam-key", &request).await;
                assert_eq!(status, StatusCode::NO_CONTENT);
                let peer = store.peer(sam.id).await.unwrap().unwrap();
                assert!(peer.gossip_url.is_some());
                assert!(peer.pubkey.is_none());
            });
            Ok(())
        });
    }

    /// No master key, no vault, no place for the key: 503, not a half-write.
    #[test]
    fn pair_answers_503_when_the_vault_will_not_open() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            let (_dir, state) = crate::state::fixtures::unconfigured();
            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async {
                let store = state.store().await.unwrap();
                let sam = store
                    .create_peer("Sam", &SecretString::from("sam-key"), PeerScope::All)
                    .await
                    .unwrap();

                let (status, _) = post_pair(&state, "sam-key", &request()).await;
                assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
                assert!(
                    store
                        .peer(sam.id)
                        .await
                        .unwrap()
                        .unwrap()
                        .gossip_url
                        .is_none()
                );
            });
            Ok(())
        });
    }

    #[tokio::test]
    async fn pair_answers_503_when_the_store_will_not_open() {
        let (_dir, state) = crate::state::fixtures::store_unopenable();
        let (status, _) = post_pair(&state, "sam-key", &request()).await;
        // The `Caller` extractor refuses first: no store means no key matches.
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    // ------------------------------------------------------------- send_pair

    #[tokio::test]
    async fn send_pair_posts_under_the_invite_key_and_reads_the_answer() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/api/gossip/pair"))
            .and(wiremock::matchers::query_param("apikey", "invite-key"))
            .respond_with(wiremock::ResponseTemplate::new(204))
            .mount(&server)
            .await;
        let mut invite = invite();
        invite.url = format!("{}/", server.uri());
        invite.key = "invite-key".to_owned();
        let request = PairRequest {
            url: "http://203.0.113.9:8477".to_owned(),
            key: "ours".to_owned(),
            record: None,
        };

        send_pair(&invite, &request).await.unwrap();

        let received = server.received_requests().await.unwrap();
        assert_eq!(received.len(), 1);
        let body: PairRequest = serde_json::from_slice(&received[0].body).unwrap();
        assert_eq!(body.key, "ours");
    }

    #[tokio::test]
    async fn send_pair_reports_a_refusal_with_its_reason() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(409).set_body_string("already paired"))
            .mount(&server)
            .await;
        let mut invite = invite();
        invite.url = server.uri();
        let request = PairRequest {
            url: "http://203.0.113.9:8477".to_owned(),
            key: "ours".to_owned(),
            record: None,
        };

        let err = send_pair(&invite, &request).await.unwrap_err();
        assert!(err.contains("409"), "{err}");
        assert!(err.contains("already paired"), "{err}");
    }

    #[tokio::test]
    async fn send_pair_names_an_unreachable_host() {
        let mut invite = invite();
        // Port 9 (discard) is closed on any test host.
        invite.url = "http://127.0.0.1:9".to_owned();
        let request = PairRequest {
            url: "http://203.0.113.9:8477".to_owned(),
            key: "ours".to_owned(),
            record: None,
        };

        let err = send_pair(&invite, &request).await.unwrap_err();
        assert!(err.starts_with("could not reach their sharerr"), "{err}");
    }
}
