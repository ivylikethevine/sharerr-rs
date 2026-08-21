//! The friends page: who this instance shares with, and on what key.
//!
//! # Why per-friend keys
//!
//! A single `torznab.api_key` shared by everybody makes two ordinary things
//! impossible: seeing whether a friend actually got set up, and cutting one
//! person off without cutting off everyone. A peer is therefore an identity
//! with its own credential, and `last_seen` answers the first question
//! directly.
//!
//! # The key is shown once
//!
//! Same rule as the Torznab key, for the same reason: it has to be copied into
//! somebody else's Prowlarr, so a write-only field would make it useless — but only
//! a SHA-256 is stored, so sharerr genuinely cannot show it again. Losing one means
//! issuing another, which is the correct behaviour for a bearer credential.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::Form;
use secrecy::SecretString;
use serde::Deserialize;
use sharerr_core::config::secret_keys;
use sharerr_core::endpoint::now_epoch;
use sharerr_store::{Peer, PeerScope};

use super::WebState;
use super::settings::title_case;
use super::templates::{PeerEndpointView, PeerRow, PeersPage, RevealedPeer, ScopeOption, render};

pub async fn page(State(state): State<WebState>) -> Response {
    render(&build(&state, None, None).await)
}

#[derive(Debug, Deserialize)]
pub struct AddForm {
    label: String,
    /// Strictly one of `PeerScope`'s names. An unknown value fails
    /// deserialization rather than widening — see the enum's docs. Absent
    /// defaults to `all`, matching the `<select>`'s first option.
    #[serde(default)]
    scope: PeerScope,
}

#[derive(Debug, Deserialize)]
pub struct ScopeForm {
    scope: PeerScope,
}

/// Mint a key, record the peer, and reveal the key exactly once.
pub async fn add(State(state): State<WebState>, Form(form): Form<AddForm>) -> Response {
    let store = match state.store_or_503().await {
        Ok(store) => store,
        Err(response) => return response,
    };

    let key = match crate::secrets::random_hex(crate::secrets::KEY_BYTES) {
        Ok(key) => key,
        Err(reason) => return rejected(&state, &reason).await,
    };

    let label = form.label.trim().to_owned();
    match store
        .create_peer(&label, &SecretString::from(key.clone()), form.scope)
        .await
    {
        Ok(peer) => {
            tracing::info!(peer = %peer.label, scope = peer.scope.as_str(), "added a friend");
            let mut page = build(&state, None, None).await;
            page.revealed = Some(RevealedPeer {
                label: peer.label,
                key,
            });
            render(&page)
        }
        Err(sharerr_store::StoreError::UserExists { username }) => {
            rejected(
                &state,
                &format!("There is already a friend called {username:?}."),
            )
            .await
        }
        Err(sharerr_store::StoreError::InvalidUser(message)) => rejected(&state, message).await,
        Err(err) => rejected(&state, &format!("could not add that friend: {err}")).await,
    }
}

/// Stop honouring a friend's key, keeping the row so the operator can see it
/// happened. `revoke_peer`'s `false` — already revoked — is not an error worth
/// showing.
pub async fn revoke(
    State(state): State<WebState>,
    Path(id): Path<i64>,
) -> Result<Response, Response> {
    let store = state.store_or_503().await?;
    let result = store.revoke_peer(id).await;
    if result.is_ok() {
        tracing::info!(peer_id = id, "revoked a friend's key");
    }
    Ok(applied(&state, result, "revoke that key").await)
}

/// Change what a friend is allowed to see.
pub async fn set_scope(
    State(state): State<WebState>,
    Path(id): Path<i64>,
    Form(form): Form<ScopeForm>,
) -> Result<Response, Response> {
    let store = state.store_or_503().await?;
    let result = store.set_peer_scope(id, form.scope).await;
    if result.is_ok() {
        tracing::info!(
            peer_id = id,
            scope = form.scope.as_str(),
            "changed what a friend can see"
        );
    }
    Ok(applied(&state, result, "change that").await)
}

#[derive(Debug, Deserialize)]
pub struct GossipForm {
    url: String,
    key: String,
    #[serde(default)]
    clear_key: Option<String>,
}

/// Configure the outbound half of a friendship: where their sharerr is, and the
/// key they issued us. The URL lands in the database; the key in the vault —
/// unlike our own peers' key *hashes*, it is a secret sharerr replays.
pub async fn set_gossip(
    State(state): State<WebState>,
    Path(id): Path<i64>,
    Form(form): Form<GossipForm>,
) -> Result<Response, Response> {
    let store = state.store_or_503().await?;

    let url = form.url.trim();
    let url = if url.is_empty() {
        None
    } else {
        match url::Url::parse(url) {
            Ok(parsed) => Some(parsed.to_string()),
            Err(err) => {
                return Ok(rejected(&state, &format!("{url:?} is not a valid URL: {err}")).await);
            }
        }
    };

    // The key is optional and write-only, same rules as every stored secret:
    // blank means leave alone, the checkbox means clear.
    let key = form.key.trim();
    if !key.is_empty() || form.clear_key.is_some() {
        let mut vault = match state.serve.open_vault().await {
            Ok(vault) => vault,
            Err(reason) => return Ok(rejected(&state, &reason).await),
        };
        let vault_key = secret_keys::peer_gossip_key(id);
        let result = if form.clear_key.is_some() {
            vault.remove(&vault_key).map(|_| ())
        } else {
            vault.put(&vault_key, &SecretString::from(key.to_owned()))
        };
        if let Err(err) = result {
            return Ok(rejected(&state, &format!("could not store the key: {err}")).await);
        }
    }

    let result = store.set_peer_gossip_url(id, url.as_deref()).await;
    if result.is_ok() {
        tracing::info!(peer_id = id, "updated a friend's gossip settings");
    }
    Ok(applied(&state, result, "save that").await)
}

/// Remove a friend entirely, freeing the name for reuse.
pub async fn delete(
    State(state): State<WebState>,
    Path(id): Path<i64>,
) -> Result<Response, Response> {
    let store = state.store_or_503().await?;
    let result = store.delete_peer(id).await;
    if result.is_ok() {
        tracing::info!(peer_id = id, "deleted a friend");
    }
    Ok(applied(&state, result, "delete that friend").await)
}

/// The literal XML this friend's Prowlarr would fetch: their scope, their
/// links, their key's own view. Scope filtering happens per key, so "why
/// can't Sam find the album" otherwise means hand-crafting a Torznab query
/// with Sam's key — this button answers it in one click.
///
/// Rendered through [`crate::torznab::render_feed`], the same function the
/// real `/torznab` route calls — not a second, hand-built HTML table that
/// could show a field the real feed gets right (or wrong) differently. The
/// honest test of scoping is not what the rules *say* a friend can see, but
/// literally what the feed *serves* them, byte for byte.
pub async fn feed_preview(
    State(state): State<WebState>,
    Path(id): Path<i64>,
) -> Result<Response, Response> {
    let store = state.store_or_503().await?;

    let peers = store
        .list_peers()
        .await
        .map_err(|err| rejected_response(&format!("could not list friends: {err}")))?;
    let Some(peer) = peers.into_iter().find(|p| p.id == id) else {
        return Err((StatusCode::NOT_FOUND, "no such friend").into_response());
    };

    let matched = crate::torznab::collect(
        &state.serve,
        &crate::torznab::SearchQuery::default(),
        peer.scope,
        &peer.key_hash,
    )
    .await
    .map_err(|(status, reason)| (status, reason).into_response())?;

    Ok(crate::torznab::xml(crate::torznab::render_feed(&matched)))
}

fn rejected_response(message: &str) -> Response {
    (StatusCode::BAD_REQUEST, message.to_owned()).into_response()
}

/// The shared tail of every peer mutation: back to the list on success, or the
/// page again with the failure sentence.
async fn applied<T>(
    state: &WebState,
    result: Result<T, sharerr_store::StoreError>,
    action: &str,
) -> Response {
    match result {
        Ok(_) => Redirect::to("/peers").into_response(),
        Err(err) => rejected(state, &format!("could not {action}: {err}")).await,
    }
}

async fn rejected(state: &WebState, message: &str) -> Response {
    tracing::warn!(message, "rejected a peers change");
    let page = build(state, Some(message.to_owned()), None).await;
    (axum::http::StatusCode::BAD_REQUEST, render(&page)).into_response()
}

async fn build(
    state: &WebState,
    error: Option<String>,
    revealed: Option<RevealedPeer>,
) -> PeersPage {
    let config = state.serve.config().await;

    let (peers, endpoints, list_error) = match state.serve.store().await {
        Ok(store) => match store.list_peers().await {
            Ok(peers) => {
                // One extra query per friend; the list is people, not rows —
                // but run concurrently rather than one round trip at a time.
                let endpoints = futures::future::join_all(
                    peers
                        .iter()
                        .map(|peer| async { store.peer_endpoints(peer.id).await.unwrap_or_default() }),
                )
                .await;
                (peers, endpoints, None)
            }
            Err(err) => (
                Vec::new(),
                Vec::new(),
                Some(format!("could not list friends: {err}")),
            ),
        },
        Err(reason) => (Vec::new(), Vec::new(), Some(reason)),
    };

    let stored_secrets = super::settings::secrets_present(&config).await;

    PeersPage {
        signed_in: true,
        scope_options: PeerScope::ALL
            .iter()
            .map(|scope| ScopeOption {
                value: scope.as_str(),
                label: title_case(scope.label()),
            })
            .collect(),
        peers: peers
            .iter()
            .zip(&endpoints)
            .map(|(peer, endpoints)| {
                let key_set = stored_secrets.contains(&secret_keys::peer_gossip_key(peer.id));
                row(peer, endpoints, key_set)
            })
            .collect(),
        error: error.or(list_error),
        revealed,
        feed_url: format!("{}/api", config.public_base_url()),
    }
}

fn row(peer: &Peer, endpoints: &[sharerr_store::PeerEndpoint], gossip_key_set: bool) -> PeerRow {
    PeerRow {
        id: peer.id,
        label: peer.label.clone(),
        scope: peer.scope.as_str(),
        scope_label: peer.scope.label(),
        created: ago(peer.created_at),
        last_seen: match peer.last_seen_at {
            Some(at) => ago(at),
            // The answer an operator is actually looking for: the friend has the
            // key but has never used it, so they have not finished setting up.
            None => "never".to_owned(),
        },
        revoked: peer.is_revoked(),
        // Truncated: this is a recogniser, not a copy source — the full key
        // travels peer-to-peer inside signed records, never through this page.
        pubkey_short: peer
            .pubkey
            .as_deref()
            .map(|pk| format!("{}…", &pk[..pk.len().min(12)])),
        gossip_url: peer.gossip_url.clone().unwrap_or_default(),
        gossip_key_set,
        endpoints: endpoints
            .iter()
            .map(|e| PeerEndpointView {
                kind: e.kind.as_str(),
                addr: e.addr.clone(),
                seen: ago(e.observed_at),
                via: e.via.as_str(),
            })
            .collect(),
    }
}

/// Coarse relative time.
///
/// Relative rather than absolute because the question is always "recently?", never
/// "at what o'clock?" — and a relative string needs no timezone, which a container
/// usually does not have configured. `pub(crate)` because the status page's
/// one-glance line answers the same "recently?" question.
pub(crate) fn ago(epoch_secs: i64) -> String {
    let seconds = now_epoch().saturating_sub(epoch_secs);

    match seconds {
        // Includes negative values, which mean a clock that moved backwards.
        s if s < 60 => "just now".to_owned(),
        s if s < 3_600 => format!("{} minute(s) ago", s / 60),
        s if s < 86_400 => format!("{} hour(s) ago", s / 3_600),
        s => format!("{} day(s) ago", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn relative_times_read_the_way_a_person_would_say_them() {
        let now = now_epoch();

        assert_eq!(ago(now), "just now");
        assert_eq!(ago(now - 120), "2 minute(s) ago");
        assert_eq!(ago(now - 7_200), "2 hour(s) ago");
        assert_eq!(ago(now - 172_800), "2 day(s) ago");
    }

    /// A container whose clock jumps backwards must not render a timestamp from the
    /// future as an enormous negative age.
    #[test]
    fn a_timestamp_in_the_future_is_not_rendered_as_nonsense() {
        let now = now_epoch();

        assert_eq!(ago(now + 10_000), "just now");
    }

    #[test]
    fn a_peer_that_has_never_connected_says_so() {
        let peer = Peer {
            id: 1,
            label: "Sam".to_owned(),
            created_at: 0,
            last_seen_at: None,
            revoked_at: None,
            scope: PeerScope::All,
            pubkey: None,
            gossip_url: None,
            key_hash: "hash".to_owned(),
        };

        assert_eq!(row(&peer, &[], false).last_seen, "never");
        assert!(!row(&peer, &[], false).revoked);
    }

    #[test]
    fn a_revoked_peer_is_marked_as_such() {
        let peer = Peer {
            id: 1,
            label: "Sam".to_owned(),
            created_at: 0,
            last_seen_at: Some(0),
            revoked_at: Some(1),
            scope: PeerScope::Tv,
            pubkey: None,
            gossip_url: None,
            key_hash: "hash".to_owned(),
        };

        assert!(row(&peer, &[], false).revoked);
    }
}
