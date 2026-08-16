//! The friends page: who this instance shares with, and on what key.
//!
//! # Why per-friend keys
//!
//! Until M4 the Torznab feed was guarded by a single `torznab.api_key` handed to
//! everybody. Two ordinary things were impossible with that: seeing whether a
//! friend had actually got themselves set up, and cutting one person off without
//! cutting off everyone. A peer is therefore an identity with its own credential,
//! and `last_seen` answers the first question directly.
//!
//! # The key is shown once
//!
//! Same rule as the Torznab key, for the same reason: it has to be copied into
//! somebody else's Prowlarr, so a write-only field would make it useless — but only
//! a SHA-256 is stored, so sharerr genuinely cannot show it again. Losing one means
//! issuing another, which is the correct behaviour for a bearer credential.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::Form;
use secrecy::SecretString;
use serde::Deserialize;
use sharerr_core::config::secret_keys;
use sharerr_store::{Peer, PeerScope};

use super::WebState;
use super::templates::{PeerRow, PeersPage, RevealedPeer, render};
use crate::torznab::public_base_url;

/// 160 bits, hex encoded — the same size as the Torznab and tracker secrets.
/// Long enough that guessing is not a strategy, short enough to paste into another
/// application's settings box.
const KEY_BYTES: usize = 20;

pub async fn page(State(state): State<WebState>) -> Response {
    render(&build(&state, None, None).await)
}

#[derive(Debug, Deserialize)]
pub struct AddForm {
    label: String,
    /// `all`, `tv` or `movies`. Anything else widens to `all` rather than failing —
    /// see `PeerScope::parse`.
    #[serde(default)]
    scope: String,
}

#[derive(Debug, Deserialize)]
pub struct ScopeForm {
    scope: String,
}

/// Mint a key, record the peer, and reveal the key exactly once.
pub async fn add(State(state): State<WebState>, Form(form): Form<AddForm>) -> Response {
    let store = match state.serve.store().await {
        Ok(store) => store,
        Err(reason) => return service_unavailable(&reason),
    };

    let key = match crate::secrets::random_hex(KEY_BYTES) {
        Ok(key) => key,
        Err(reason) => return rejected(&state, &reason).await,
    };

    let label = form.label.trim().to_owned();
    let scope = PeerScope::parse(&form.scope);
    match store
        .create_peer(&label, &SecretString::from(key.clone()), scope)
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
/// happened.
pub async fn revoke(State(state): State<WebState>, Path(id): Path<i64>) -> Response {
    let store = match state.serve.store().await {
        Ok(store) => store,
        Err(reason) => return service_unavailable(&reason),
    };

    match store.revoke_peer(id).await {
        // `false` means it was already revoked, which is not an error worth showing.
        Ok(_) => {
            tracing::info!(peer_id = id, "revoked a friend's key");
            Redirect::to("/peers").into_response()
        }
        Err(err) => rejected(&state, &format!("could not revoke that key: {err}")).await,
    }
}

/// Change what a friend is allowed to see.
pub async fn set_scope(
    State(state): State<WebState>,
    Path(id): Path<i64>,
    Form(form): Form<ScopeForm>,
) -> Response {
    let store = match state.serve.store().await {
        Ok(store) => store,
        Err(reason) => return service_unavailable(&reason),
    };

    let scope = PeerScope::parse(&form.scope);
    match store.set_peer_scope(id, scope).await {
        Ok(_) => {
            tracing::info!(
                peer_id = id,
                scope = scope.as_str(),
                "changed what a friend can see"
            );
            Redirect::to("/peers").into_response()
        }
        Err(err) => rejected(&state, &format!("could not change that: {err}")).await,
    }
}

/// Remove a friend entirely, freeing the name for reuse.
pub async fn delete(State(state): State<WebState>, Path(id): Path<i64>) -> Response {
    let store = match state.serve.store().await {
        Ok(store) => store,
        Err(reason) => return service_unavailable(&reason),
    };

    match store.delete_peer(id).await {
        Ok(_) => {
            tracing::info!(peer_id = id, "deleted a friend");
            Redirect::to("/peers").into_response()
        }
        Err(err) => rejected(&state, &format!("could not delete that friend: {err}")).await,
    }
}

async fn rejected(state: &WebState, message: &str) -> Response {
    tracing::warn!(message, "rejected a peers change");
    let page = build(state, Some(message.to_owned()), None).await;
    (axum::http::StatusCode::BAD_REQUEST, render(&page)).into_response()
}

fn service_unavailable(reason: &str) -> Response {
    (
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        reason.to_owned(),
    )
        .into_response()
}

async fn build(
    state: &WebState,
    error: Option<String>,
    revealed: Option<RevealedPeer>,
) -> PeersPage {
    let config = state.serve.config().await;

    let (peers, list_error) = match state.serve.store().await {
        Ok(store) => match store.list_peers().await {
            Ok(peers) => (peers, None),
            Err(err) => (Vec::new(), Some(format!("could not list friends: {err}"))),
        },
        Err(reason) => (Vec::new(), Some(reason)),
    };

    // Whether the legacy shared key is still set. While it is, revoking a peer does
    // not actually cut them off, and a page that implied otherwise would be lying
    // about a security control.
    let shared_key_set = match state.serve.open_vault().await {
        Ok(vault) => vault
            .get(secret_keys::TORZNAB_API_KEY)
            .ok()
            .flatten()
            .is_some(),
        // A vault that will not open cannot be holding a usable shared key.
        Err(_) => false,
    };

    PeersPage {
        signed_in: true,
        peers: peers.iter().map(row).collect(),
        error: error.or(list_error),
        revealed,
        feed_url: format!("{}/api", public_base_url(&config)),
        shared_key_set,
    }
}

fn row(peer: &Peer) -> PeerRow {
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
    }
}

/// Coarse relative time.
///
/// Relative rather than absolute because the question is always "recently?", never
/// "at what o'clock?" — and a relative string needs no timezone, which a container
/// usually does not have configured.
fn ago(epoch_secs: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let seconds = now.saturating_sub(epoch_secs);

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
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        assert_eq!(ago(now), "just now");
        assert_eq!(ago(now - 120), "2 minute(s) ago");
        assert_eq!(ago(now - 7_200), "2 hour(s) ago");
        assert_eq!(ago(now - 172_800), "2 day(s) ago");
    }

    /// A container whose clock jumps backwards must not render a timestamp from the
    /// future as an enormous negative age.
    #[test]
    fn a_timestamp_in_the_future_is_not_rendered_as_nonsense() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

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
        };

        assert_eq!(row(&peer).last_seen, "never");
        assert!(!row(&peer).revoked);
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
        };

        assert!(row(&peer).revoked);
    }
}
