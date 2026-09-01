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
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use sharerr_core::config::secret_keys;
use sharerr_core::endpoint::now_epoch;
use sharerr_store::{EndpointKind, Peer, PeerScope, SeedingSummary};

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
        Err(response) => return *response,
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
pub async fn revoke(State(state): State<WebState>, Path(id): Path<i64>) -> Response {
    let store = match state.store_or_503().await {
        Ok(store) => store,
        Err(response) => return *response,
    };
    let result = store.revoke_peer(id).await;
    if result.is_ok() {
        tracing::info!(peer_id = id, "revoked a friend's key");
        crate::notify::send(
            &state.serve,
            sharerr_core::config::NotificationTrigger::PeerRevoked,
            &format!("revoked friend #{id}'s key"),
        )
        .await;
    }
    applied(&state, result, "revoke that key").await
}

/// Change what a friend is allowed to see.
pub async fn set_scope(
    State(state): State<WebState>,
    Path(id): Path<i64>,
    Form(form): Form<ScopeForm>,
) -> Response {
    let store = match state.store_or_503().await {
        Ok(store) => store,
        Err(response) => return *response,
    };
    let result = store.set_peer_scope(id, form.scope).await;
    if result.is_ok() {
        tracing::info!(
            peer_id = id,
            scope = form.scope.as_str(),
            "changed what a friend can see"
        );
    }
    applied(&state, result, "change that").await
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
) -> Response {
    let store = match state.store_or_503().await {
        Ok(store) => store,
        Err(response) => return *response,
    };

    // The URL is written first, and only then the key: `set_peer_gossip_url`
    // answers whether the peer exists at all, so a `POST` for a peer deleted
    // from another tab cannot leave an orphan `peer.gossip.{id}` in the vault
    // that nothing could ever remove. The URL half is what the vault half is
    // *for*, so a peer that cannot take the URL takes no key. Parsing and
    // normalizing both live in `set_peer_gossip_url`, so a bad URL and a
    // nonexistent peer both surface here rather than one being caught earlier
    // by a second copy of the same check.
    let result = store.set_peer_gossip_url(id, Some(&form.url)).await;
    match result {
        Ok(true) => {}
        Ok(false) => return rejected(&state, "there is no friend with that id any more").await,
        Err(err) => return rejected(&state, &format!("could not save that: {err}")).await,
    }

    // The key is optional and write-only, same rules as every stored secret:
    // blank means leave alone, the checkbox means clear.
    let key = form.key.trim();
    if !key.is_empty() || form.clear_key.is_some() {
        let mut vault = match state.serve.open_vault().await {
            Ok(vault) => vault,
            Err(reason) => return rejected(&state, &reason).await,
        };
        let vault_key = secret_keys::peer_gossip_key(id);
        let result = if form.clear_key.is_some() {
            vault.remove(&vault_key).map(|_| ())
        } else {
            vault.put(&vault_key, &SecretString::from(key.to_owned()))
        };
        if let Err(err) = result {
            return rejected(&state, &format!("could not store the key: {err}")).await;
        }
    }

    tracing::info!(peer_id = id, "updated a friend's gossip settings");
    Redirect::to("/peers").into_response()
}

/// Remove a friend entirely, freeing the name for reuse.
pub async fn delete(State(state): State<WebState>, Path(id): Path<i64>) -> Response {
    let store = match state.store_or_503().await {
        Ok(store) => store,
        Err(response) => return *response,
    };
    let result = store.delete_peer(id).await;
    if result.is_ok() {
        tracing::info!(peer_id = id, "deleted a friend");
        // The row is gone; the friend's gossip key must not outlive it as an
        // orphan only `sharerr vault list` can see. Best-effort: a vault that
        // will not open now is a startup problem the operator already knows
        // about, not a reason to fail a delete that has already happened.
        // `peers.id` is AUTOINCREMENT, so a leftover can never re-attach.
        match state.serve.open_vault().await {
            Ok(mut vault) => {
                if let Err(err) = vault.remove(&secret_keys::peer_gossip_key(id)) {
                    tracing::warn!(peer_id = id, error = %err, "could not remove the friend's gossip key");
                }
            }
            Err(err) => {
                tracing::warn!(peer_id = id, error = %err, "could not open the vault to remove the friend's gossip key");
            }
        }
    }
    applied(&state, result, "delete that friend").await
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
pub async fn feed_preview(State(state): State<WebState>, Path(id): Path<i64>) -> Response {
    let store = match state.store_or_503().await {
        Ok(store) => store,
        Err(response) => return *response,
    };

    let peers = match store.list_peers().await {
        Ok(peers) => peers,
        Err(err) => return rejected_response(&format!("could not list friends: {err}")),
    };
    let Some(peer) = peers.into_iter().find(|p| p.id == id) else {
        return (StatusCode::NOT_FOUND, "no such friend").into_response();
    };

    let matched = match crate::torznab::collect(
        &state.serve,
        &crate::torznab::SearchQuery::default(),
        peer.scope,
        &peer.key_hash,
    )
    .await
    {
        Ok(matched) => matched,
        Err((status, reason)) => return (status, reason).into_response(),
    };

    crate::torznab::xml(crate::torznab::render_feed(&matched))
}

/// The most recently observed sighting of one [`EndpointKind`], relying on
/// [`sharerr_store::Store::peer_endpoints_for`]'s own
/// `observed_at DESC, id DESC` ordering — so the *first* match here is
/// already the newest, and re-deriving that with `max_by_key` would in fact
/// pick the *oldest* of a tied `observed_at`.
fn latest_endpoint(
    endpoints: &[sharerr_store::PeerEndpoint],
    kind: EndpointKind,
) -> Option<&sharerr_store::PeerEndpoint> {
    endpoints.iter().find(|endpoint| endpoint.kind == kind)
}

/// Download every active friend as a one-time `[[peers]]` restore block —
/// the export half of `sharerr_core::config::PeerImport`; see
/// `SETTINGS.md`'s "Restoring friends after a full data-directory
/// loss". Meant to be saved somewhere outside sharerr (a password manager,
/// an offline backup) and hand-pasted back into `sharerr.toml` only if the
/// data directory is ever lost — this does not write anywhere itself.
///
/// A revoked friend is deliberately excluded: importing this block always
/// creates an *active* peer, and a revoked friend flowing back through it
/// would silently un-revoke them on the next restore. No friend's own key
/// *into* this instance ever appears here, revoked or not — only a one-way
/// hash of it was ever stored, so there is nothing to export; a restore
/// always mints a fresh one, exactly like adding a friend normally.
pub async fn export(State(state): State<WebState>) -> Response {
    let store = match state.store_or_503().await {
        Ok(store) => store,
        Err(response) => return *response,
    };

    let peers = match store.list_peers().await {
        Ok(peers) => peers,
        Err(err) => return rejected(&state, &format!("could not list friends: {err}")).await,
    };
    let active: Vec<Peer> = peers
        .into_iter()
        .filter(|peer| !peer.is_revoked())
        .collect();

    let peer_ids: Vec<i64> = active.iter().map(|peer| peer.id).collect();
    let endpoints_by_peer = store
        .peer_endpoints_for(&peer_ids)
        .await
        .unwrap_or_default();

    // Only opened when at least one active friend actually has a gossip key
    // stored — `Vault::key_names` (via `secrets_present`, the same call the
    // settings page's "stored"/"not set" badges use) answers that without
    // deriving the master key, so the common case — nobody has an outbound
    // gossip relationship configured yet — costs nothing beyond a file read.
    // A failure to then open the vault degrades the export rather than
    // failing it outright: every other field is still useful on its own,
    // and the missing keys are called out in the file.
    let stored_secrets = super::settings::secrets_present(&state.serve.config().await).await;
    let any_gossip_key = active
        .iter()
        .any(|peer| stored_secrets.contains(&secret_keys::peer_gossip_key(peer.id)));
    let (vault, vault_unavailable) = if !any_gossip_key {
        (None, false)
    } else {
        match state.serve.open_vault().await {
            Ok(vault) => (Some(vault), false),
            Err(_) => (None, true),
        }
    };

    let imports: Vec<sharerr_core::config::PeerImport> = active
        .iter()
        .map(|peer| {
            let last_addr = endpoints_by_peer
                .get(&peer.id)
                .map(Vec::as_slice)
                .and_then(|endpoints| latest_endpoint(endpoints, EndpointKind::Api))
                .map(|endpoint| endpoint.addr.clone());
            let gossip_key = vault.as_ref().and_then(|vault| {
                vault
                    .get(&secret_keys::peer_gossip_key(peer.id))
                    .ok()
                    .flatten()
                    .map(|secret| secret.expose_secret().to_owned())
            });

            sharerr_core::config::PeerImport {
                label: peer.label.clone(),
                scope: peer.scope.as_str().to_owned(),
                last_addr,
                gossip_url: peer.gossip_url.clone(),
                gossip_key,
            }
        })
        .collect();

    let mut text = match (sharerr_core::config::PeerImportDocument { peers: imports }).to_toml() {
        Ok(text) => text,
        Err(err) => return rejected(&state, &format!("could not export friends: {err}")).await,
    };
    if vault_unavailable {
        text = format!(
            "# The vault could not be opened during this export, so no gossip keys are\n\
             # included below even for friends that have one configured. Re-export once\n\
             # the vault is reachable to capture them.\n\n{text}"
        );
    }

    tracing::info!(count = active.len(), "exported a [[peers]] restore block");

    super::toml_download("sharerr-peers-export.toml", text)
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
                // Every friend's endpoint history in one round trip rather
                // than one per friend. The seeding count per scope rides
                // alongside, once per distinct scope rather than once per
                // friend: it is the concrete answer to what "Can see: TV
                // only" means.
                let mut scopes: Vec<PeerScope> = Vec::new();
                for peer in &peers {
                    if !scopes.contains(&peer.scope) {
                        scopes.push(peer.scope);
                    }
                }
                let peer_ids: Vec<i64> = peers.iter().map(|peer| peer.id).collect();
                let (endpoints_by_peer, counts) =
                    tokio::join!(
                        store.peer_endpoints_for(&peer_ids),
                        futures::future::join_all(scopes.iter().map(|scope| async {
                            (*scope, store.seeding_summary(*scope).await.ok())
                        })),
                    );
                let endpoints_by_peer = endpoints_by_peer.unwrap_or_default();
                let endpoints = peers
                    .iter()
                    .map(|peer| {
                        let sharing = counts
                            .iter()
                            .find(|(scope, _)| *scope == peer.scope)
                            .and_then(|(_, sharing)| *sharing);
                        let endpoints =
                            endpoints_by_peer.get(&peer.id).cloned().unwrap_or_default();
                        (endpoints, sharing)
                    })
                    .collect::<Vec<_>>();
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
            .map(|(peer, (endpoints, sharing))| {
                let key_set = stored_secrets.contains(&secret_keys::peer_gossip_key(peer.id));
                row(peer, endpoints, key_set, *sharing)
            })
            .collect(),
        error: error.or(list_error),
        revealed,
        // The live endpoint, not `config.public_base_url()` — see
        // `ServeState::public_base_url`'s docs: a gluetun-only deployment
        // must advertise the resolved address here too.
        feed_url: format!("{}/api", state.serve.public_base_url().await),
    }
}

fn row(
    peer: &Peer,
    endpoints: &[sharerr_store::PeerEndpoint],
    gossip_key_set: bool,
    sharing: Option<SeedingSummary>,
) -> PeerRow {
    PeerRow {
        sharing: sharing.map(|summary| summary.count.unsigned_abs() as usize),
        sharing_size: sharing
            .filter(|summary| summary.size > 0)
            .map(|summary| super::items::human_size(summary.size.unsigned_abs()))
            .unwrap_or_default(),
        id: peer.id,
        label: peer.label.clone(),
        scope: peer.scope.as_str(),
        scope_label: peer.scope.label(),
        created: ago(peer.created_at),
        created_absolute: absolute(peer.created_at),
        last_seen: match peer.last_seen_at {
            Some(at) => ago(at),
            // The answer an operator is actually looking for: the friend has the
            // key but has never used it, so they have not finished setting up.
            None => "never".to_owned(),
        },
        last_seen_absolute: peer.last_seen_at.map(absolute).unwrap_or_default(),
        revoked: peer.is_revoked(),
        revoked_when: peer.revoked_at.map(ago).unwrap_or_default(),
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

/// Which relative-time bucket a moment falls into, and the whole-unit count
/// within it. `ago` and `topology::compact_ago` share this ladder and differ
/// only in the words they wrap it in — this is the one function whose
/// thresholds a single test can cover for both.
pub(crate) enum AgoBucket {
    Now,
    Minutes(i64),
    Hours(i64),
    Days(i64),
}

pub(crate) fn ago_bucket(epoch_secs: i64) -> AgoBucket {
    // Includes negative values, which mean a clock that moved backwards.
    let seconds = now_epoch().saturating_sub(epoch_secs);

    match seconds {
        s if s < 60 => AgoBucket::Now,
        s if s < 3_600 => AgoBucket::Minutes(s / 60),
        s if s < 86_400 => AgoBucket::Hours(s / 3_600),
        s => AgoBucket::Days(s / 86_400),
    }
}

/// Coarse relative time.
///
/// Relative rather than absolute because the question is always "recently?", never
/// "at what o'clock?" — and a relative string needs no timezone, which a container
/// usually does not have configured. `pub(crate)` because the status page's
/// one-glance line answers the same "recently?" question.
pub(crate) fn ago(epoch_secs: i64) -> String {
    match ago_bucket(epoch_secs) {
        AgoBucket::Now => "just now".to_owned(),
        AgoBucket::Minutes(n) => format!("{n} minute(s) ago"),
        AgoBucket::Hours(n) => format!("{n} hour(s) ago"),
        AgoBucket::Days(n) => format!("{n} day(s) ago"),
    }
}

/// The absolute instant behind a relative string, for a `title=` tooltip.
///
/// `ago` answers "recently?", which is nearly always the question — but not
/// "which of these two happened first" once both of them read "3 day(s) ago".
/// UTC with no conversion: a container's local zone is usually unset, so an
/// offset here would be a guess dressed up as a fact. Formatted by hand rather
/// than through `time`'s `format_description!`, which needs the `macros`
/// feature the workspace does not enable.
pub(crate) fn absolute(epoch_secs: i64) -> String {
    let Ok(at) = time::OffsetDateTime::from_unix_timestamp(epoch_secs) else {
        // Only reachable for a timestamp outside year ±9999 — a corrupt row
        // rather than anything an operator did. An empty title just omits the
        // tooltip, which beats rendering "invalid" next to a real time.
        return String::new();
    };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
        at.year(),
        u8::from(at.month()),
        at.day(),
        at.hour(),
        at.minute(),
        at.second()
    )
}

/// How long something took, given a start and an end.
///
/// Sync runs span seconds to minutes, so the units stay coarse: the useful
/// signal is "this pass is taking much longer than the last one", never the
/// exact millisecond.
pub(crate) fn took(started_at: i64, finished_at: i64) -> String {
    match finished_at.saturating_sub(started_at) {
        // A clock that moved backwards mid-run. Reporting a negative duration
        // would read as a bug in sharerr rather than in the host's clock.
        s if s < 0 => String::new(),
        0 => "under a second".to_owned(),
        s if s < 60 => format!("{s}s"),
        s if s < 3_600 => format!("{}m {}s", s / 60, s % 60),
        s => format!("{}h {}m", s / 3_600, (s % 3_600) / 60),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::result_large_err)]

    use axum::http::header;

    use super::*;

    #[test]
    fn a_run_duration_reads_in_the_coarsest_useful_unit() {
        assert_eq!(took(100, 100), "under a second");
        assert_eq!(took(100, 145), "45s");
        assert_eq!(took(100, 100 + 125), "2m 5s");
        assert_eq!(took(100, 100 + 7_260), "2h 1m");
        // A clock that jumped backwards reports nothing rather than "-5s".
        assert_eq!(took(200, 100), "");
    }

    #[test]
    fn an_absolute_timestamp_is_utc_and_needs_no_timezone() {
        assert_eq!(absolute(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(absolute(1_700_000_000), "2023-11-14 22:13:20 UTC");
    }

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

        assert_eq!(row(&peer, &[], false, None).last_seen, "never");
        assert!(!row(&peer, &[], false, None).revoked);
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

        assert!(row(&peer, &[], false, None).revoked);
    }

    // ------------------------------------------------------------- handlers
    //
    // All against a real `Store` on a temp `data_dir` — no vault needed for
    // any of these except `set_gossip`'s key-storage branch, which stays
    // within this project's no-live-vault-in-tests rule (see CLAUDE.md) by
    // only ever exercising the "vault would not open" path, never a real
    // open one.

    use super::super::{body_of, web_state};

    /// The `store_or_503`/`build`'s "store unavailable" branches, reached
    /// hermetically — see `state::fixtures::store_unopenable`.
    fn web_state_with_unopenable_store() -> (tempfile::TempDir, WebState) {
        let (dir, serve) = crate::state::fixtures::store_unopenable();
        (dir, web_state(serve))
    }

    #[tokio::test]
    async fn the_page_handler_renders() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);

        let response = page(State(state)).await;
        assert_eq!(response.status(), axum::http::StatusCode::OK);
    }

    /// On a gluetun-only deployment (no static `tracker.advertised_host`),
    /// `feed_url` must track the live resolved endpoint — not fall back to
    /// `http://localhost:<port>`, which only works from the box sharerr
    /// itself runs on.
    #[tokio::test]
    async fn the_feed_url_tracks_the_live_endpoint_not_localhost() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        serve
            .endpoint()
            .observe(url::Url::parse("http://203.0.113.9:41234/").unwrap());
        let state = web_state(serve);

        let page = build(&state, None, None).await;
        assert_eq!(page.feed_url, "http://203.0.113.9:41234/api");
    }

    #[tokio::test]
    async fn the_page_reports_when_the_store_will_not_open() {
        let (_dir, state) = web_state_with_unopenable_store();

        let response = page(State(state)).await;
        assert_eq!(
            response.status(),
            axum::http::StatusCode::OK,
            "still renders"
        );
    }

    #[tokio::test]
    async fn add_rejects_a_blank_label() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);

        let response = add(
            State(state),
            Form(AddForm {
                label: "   ".to_owned(),
                scope: PeerScope::All,
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn add_answers_503_when_the_store_will_not_open() {
        let (_dir, state) = web_state_with_unopenable_store();

        let response = add(
            State(state),
            Form(AddForm {
                label: "Sam".to_owned(),
                scope: PeerScope::All,
            }),
        )
        .await;

        assert_eq!(
            response.status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn revoke_answers_503_when_the_store_will_not_open() {
        let (_dir, state) = web_state_with_unopenable_store();

        let response = revoke(State(state), Path(1)).await;
        assert_eq!(
            response.status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn set_scope_answers_503_when_the_store_will_not_open() {
        let (_dir, state) = web_state_with_unopenable_store();

        let response = set_scope(
            State(state),
            Path(1),
            Form(ScopeForm {
                scope: PeerScope::Tv,
            }),
        )
        .await;
        assert_eq!(
            response.status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn set_gossip_answers_503_when_the_store_will_not_open() {
        let (_dir, state) = web_state_with_unopenable_store();

        let response = set_gossip(
            State(state),
            Path(1),
            Form(GossipForm {
                url: String::new(),
                key: String::new(),
                clear_key: None,
            }),
        )
        .await;
        assert_eq!(
            response.status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn delete_answers_503_when_the_store_will_not_open() {
        let (_dir, state) = web_state_with_unopenable_store();

        let response = delete(State(state), Path(1)).await;
        assert_eq!(
            response.status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn feed_preview_answers_503_when_the_store_will_not_open() {
        let (_dir, state) = web_state_with_unopenable_store();

        let response = feed_preview(State(state), Path(1)).await;
        assert_eq!(
            response.status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
    }

    /// A peer with a recorded endpoint must have it round-trip into the
    /// rendered row's endpoint list — the branch a peer with none (the tests
    /// above) cannot exercise.
    #[tokio::test]
    async fn build_renders_a_peer_with_a_recorded_endpoint() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let store = serve.store().await.unwrap();
        let peer = store
            .create_peer("Sam", &SecretString::from("sam-key"), PeerScope::All)
            .await
            .unwrap();
        store
            .record_peer_endpoint(
                peer.id,
                sharerr_store::EndpointKind::Client,
                "10.0.0.5:51413",
                Some(now_epoch()),
                sharerr_store::ObservedVia::Direct,
            )
            .await
            .unwrap();

        let state = web_state(serve);
        let page = build(&state, None, None).await;
        let row = page
            .peers
            .iter()
            .find(|r| r.label == "Sam")
            .expect("Sam must be listed");
        assert_eq!(row.endpoints.len(), 1);
        assert_eq!(row.endpoints[0].addr, "10.0.0.5:51413");
        assert_eq!(row.endpoints[0].kind, "client");
    }

    #[tokio::test]
    async fn add_creates_a_peer_that_list_peers_then_sees() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let store = serve.store().await.unwrap();
        let state = web_state(serve);

        let response = add(
            State(state),
            Form(AddForm {
                label: "Sam".to_owned(),
                scope: PeerScope::All,
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let peers = store.list_peers().await.unwrap();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].label, "Sam");
    }

    #[tokio::test]
    async fn adding_a_duplicate_label_is_rejected_rather_than_stored_twice() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let store = serve.store().await.unwrap();
        store
            .create_peer("Sam", &SecretString::from("first-key"), PeerScope::All)
            .await
            .unwrap();
        let state = web_state(serve);

        let response = add(
            State(state),
            Form(AddForm {
                label: "Sam".to_owned(),
                scope: PeerScope::All,
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(store.list_peers().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn revoke_marks_the_peer_revoked_and_redirects() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let store = serve.store().await.unwrap();
        let sam = store
            .create_peer("Sam", &SecretString::from("sam-key"), PeerScope::All)
            .await
            .unwrap();
        let state = web_state(serve);

        let response = revoke(State(state), Path(sam.id)).await;

        assert_eq!(response.status(), axum::http::StatusCode::SEE_OTHER);
        let peers = store.list_peers().await.unwrap();
        assert!(peers[0].revoked_at.is_some());
    }

    #[tokio::test]
    async fn revoking_an_unknown_peer_still_redirects_rather_than_erroring() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);

        let response = revoke(State(state), Path(404)).await;

        // `revoke_peer`'s `false` — nothing to revoke — is not an error worth
        // showing; see the handler's own doc comment.
        assert_eq!(response.status(), axum::http::StatusCode::SEE_OTHER);
    }

    #[tokio::test]
    async fn set_scope_changes_what_a_friend_can_see() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let store = serve.store().await.unwrap();
        let sam = store
            .create_peer("Sam", &SecretString::from("sam-key"), PeerScope::All)
            .await
            .unwrap();
        let state = web_state(serve);

        let response = set_scope(
            State(state),
            Path(sam.id),
            Form(ScopeForm {
                scope: PeerScope::Tv,
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::SEE_OTHER);
        let peers = store.list_peers().await.unwrap();
        assert_eq!(peers[0].scope, PeerScope::Tv);
    }

    #[tokio::test]
    async fn delete_removes_the_peer_entirely() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let store = serve.store().await.unwrap();
        let sam = store
            .create_peer("Sam", &SecretString::from("sam-key"), PeerScope::All)
            .await
            .unwrap();
        let state = web_state(serve);

        let response = delete(State(state), Path(sam.id)).await;

        assert_eq!(response.status(), axum::http::StatusCode::SEE_OTHER);
        assert!(store.list_peers().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn set_gossip_stores_a_valid_url_with_no_key_involved() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let store = serve.store().await.unwrap();
        let sam = store
            .create_peer("Sam", &SecretString::from("sam-key"), PeerScope::All)
            .await
            .unwrap();
        let state = web_state(serve);

        let response = set_gossip(
            State(state),
            Path(sam.id),
            Form(GossipForm {
                url: "https://sams-sharerr.example".to_owned(),
                key: String::new(),
                clear_key: None,
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::SEE_OTHER);
        let peers = store.list_peers().await.unwrap();
        assert_eq!(
            peers[0].gossip_url.as_deref(),
            Some("https://sams-sharerr.example/")
        );
    }

    #[tokio::test]
    async fn set_gossip_rejects_a_url_that_does_not_parse() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let store = serve.store().await.unwrap();
        let sam = store
            .create_peer("Sam", &SecretString::from("sam-key"), PeerScope::All)
            .await
            .unwrap();
        let state = web_state(serve);

        let response = set_gossip(
            State(state),
            Path(sam.id),
            Form(GossipForm {
                url: "not a url".to_owned(),
                key: String::new(),
                clear_key: None,
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
        assert!(store.list_peers().await.unwrap()[0].gossip_url.is_none());
    }

    /// A non-blank key routes through the vault, which cannot open with no
    /// master key set — the exact condition this suite is limited to for
    /// anything vault-shaped, per CLAUDE.md.
    ///
    /// `master_key_from_env` reads the real process environment, which
    /// several tests elsewhere in this binary legitimately mutate via
    /// `figment::Jail`. Wrapped in `Jail` too (with `clear_env`) so this is
    /// guaranteed to run with no other Jail closure's env mutation active,
    /// rather than racing the parallel runner for a var it needs absent.
    #[test]
    fn set_gossip_with_a_key_fails_cleanly_when_the_vault_will_not_open() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            let (_dir, serve) = crate::state::fixtures::unconfigured();
            let state = web_state(serve.clone());

            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async {
                let store = serve.store().await.unwrap();
                let sam = store
                    .create_peer("Sam", &SecretString::from("sam-key"), PeerScope::All)
                    .await
                    .unwrap();

                let response = set_gossip(
                    State(state),
                    Path(sam.id),
                    Form(GossipForm {
                        url: String::new(),
                        key: "a-key-alex-issued-us".to_owned(),
                        clear_key: None,
                    }),
                )
                .await;

                assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
                // The URL write must not have happened either — a rejected secret
                // write must not leave a half-applied change behind.
                assert!(store.list_peers().await.unwrap()[0].gossip_url.is_none());
            });
            Ok(())
        });
    }

    #[tokio::test]
    async fn feed_preview_answers_not_found_for_an_unknown_peer() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);

        let response = feed_preview(State(state), Path(404)).await;

        assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn feed_preview_renders_an_empty_feed_for_a_known_peer_with_nothing_shared() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let store = serve.store().await.unwrap();
        let sam = store
            .create_peer("Sam", &SecretString::from("sam-key"), PeerScope::All)
            .await
            .unwrap();
        let state = web_state(serve);

        let response = feed_preview(State(state), Path(sam.id)).await;

        assert_eq!(response.status(), axum::http::StatusCode::OK);
    }

    // -------------------------------------------------------------- export()

    use sharerr_testkit::secrets::fresh_password;

    #[tokio::test]
    async fn export_excludes_revoked_peers_and_never_carries_a_peer_key() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let store = serve.store().await.unwrap();
        let sam_key = fresh_password();
        let alex_key = fresh_password();
        store
            .create_peer("Sam", &SecretString::from(sam_key.clone()), PeerScope::Tv)
            .await
            .unwrap();
        let alex = store
            .create_peer(
                "Alex",
                &SecretString::from(alex_key.clone()),
                PeerScope::All,
            )
            .await
            .unwrap();
        store.revoke_peer(alex.id).await.unwrap();
        let state = web_state(serve);

        let response = export(State(state)).await;
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_DISPOSITION)
                .and_then(|v| v.to_str().ok()),
            Some("attachment; filename=\"sharerr-peers-export.toml\"")
        );
        let body = body_of(response).await;

        assert!(body.contains("label = \"Sam\""), "{body}");
        assert!(
            !body.contains("Alex"),
            "a revoked friend must not be exported: {body}"
        );
        assert!(
            !body.contains(&sam_key) && !body.contains(&alex_key),
            "a friend's own key into this instance must never be exported: {body}"
        );
    }

    /// The field exists to answer "where do I reach them", not "everywhere
    /// they have ever been seen" — an older sighting or a different kind
    /// (their torrent client, not their API) must not win.
    #[tokio::test]
    async fn export_picks_only_the_most_recent_api_endpoint() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let store = serve.store().await.unwrap();
        let sam = store
            .create_peer("Sam", &SecretString::from(fresh_password()), PeerScope::All)
            .await
            .unwrap();
        store
            .record_peer_endpoint(
                sam.id,
                EndpointKind::Api,
                "203.0.113.5:1",
                Some(100),
                sharerr_store::ObservedVia::Direct,
            )
            .await
            .unwrap();
        store
            .record_peer_endpoint(
                sam.id,
                EndpointKind::Api,
                "203.0.113.9:2",
                Some(200),
                sharerr_store::ObservedVia::Direct,
            )
            .await
            .unwrap();
        store
            .record_peer_endpoint(
                sam.id,
                EndpointKind::Client,
                "198.51.100.7:6881",
                Some(300),
                sharerr_store::ObservedVia::Gossip,
            )
            .await
            .unwrap();
        let state = web_state(serve);

        let body = body_of(export(State(state)).await).await;
        assert!(body.contains("203.0.113.9:2"), "{body}");
        assert!(
            !body.contains("203.0.113.5:1"),
            "must pick the newest sighting, not an older one: {body}"
        );
        assert!(
            !body.contains("198.51.100.7"),
            "must not use a non-API sighting: {body}"
        );
    }

    /// Without an openable vault, a gossip key cannot be read back at
    /// all — the export must still deliver everything else rather than
    /// failing outright, and say plainly what it left out.
    ///
    /// The realistic shape of this — a key genuinely exists but the master
    /// key is not available *right now* — needs two phases: create the
    /// friend and store the key with the vault reachable, then take the
    /// master key away before exporting. Exporting into an *empty* vault (no
    /// key ever stored) is deliberately not this test: `export` only opens
    /// the vault when `Vault::key_names` — which needs no master key — shows
    /// an active friend actually has one stored, so a friend with nothing to
    /// look up must not trigger this banner at all; that is covered by
    /// `export_picks_only_the_most_recent_api_endpoint`, which has a
    /// gossip URL but no key and asserts no such banner.
    ///
    /// `master_key_from_env` reads the real process environment with no
    /// injection point, so — per this repo's testing rule for a
    /// vault-closed outcome — this runs inside `Jail`, `clear_env()` and all,
    /// exactly as if it needed a var *set*.
    #[test]
    fn export_degrades_gracefully_without_an_openable_vault() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("SHARERR_MASTER_KEY", "a-master-key");
            let (_dir, serve) = crate::state::fixtures::unconfigured();

            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async {
                let store = serve.store().await.unwrap();
                let sam = store
                    .create_peer("Sam", &SecretString::from(fresh_password()), PeerScope::All)
                    .await
                    .unwrap();
                store
                    .set_peer_gossip_url(sam.id, Some("https://sam.example/sharerr"))
                    .await
                    .unwrap();
                let mut vault = serve.open_vault().await.unwrap();
                vault
                    .put(
                        &secret_keys::peer_gossip_key(sam.id),
                        &SecretString::from("sam-issued-us-this"),
                    )
                    .unwrap();
            });

            // The vault is unreachable for the export itself, even though it
            // genuinely holds a key for Sam.
            jail.clear_env();
            runtime.block_on(async {
                let state = web_state(serve);

                let body = body_of(export(State(state)).await).await;
                assert!(body.contains("gossip_url"), "{body}");
                assert!(!body.contains("gossip_key"), "no vault, no key: {body}");
                assert!(
                    body.contains("vault could not be opened"),
                    "must say why a key is missing: {body}"
                );
            });
            Ok(())
        });
    }

    /// The one path that actually reaches the vault: a real
    /// `SHARERR_MASTER_KEY`, via `Jail` per this repo's rule for
    /// vault-backed tests.
    #[test]
    fn export_includes_a_gossip_key_when_the_vault_is_reachable() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("SHARERR_MASTER_KEY", "a-master-key");
            let (_dir, serve) = crate::state::fixtures::unconfigured();

            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async {
                let store = serve.store().await.unwrap();
                let sam = store
                    .create_peer("Sam", &SecretString::from(fresh_password()), PeerScope::All)
                    .await
                    .unwrap();
                let mut vault = serve.open_vault().await.unwrap();
                vault
                    .put(
                        &secret_keys::peer_gossip_key(sam.id),
                        &SecretString::from("sam-issued-us-this"),
                    )
                    .unwrap();
                let state = web_state(serve);

                let body = body_of(export(State(state)).await).await;
                assert!(body.contains("sam-issued-us-this"), "{body}");
            });
            Ok(())
        });
    }

    #[tokio::test]
    async fn export_answers_503_when_the_store_will_not_open() {
        let (_dir, state) = web_state_with_unopenable_store();

        let response = export(State(state)).await;
        assert_eq!(
            response.status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
    }
}
