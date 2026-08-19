//! Outbound notifications: a webhook fired on sync failure or a peer going quiet.
//!
//! # Why the URL is a vault secret, not a config field
//!
//! A Discord webhook URL embeds its own bearer token in the path — posting it is
//! indistinguishable from handing someone your credential. `sharerr.toml` is
//! rewritten in place by the web UI and is the kind of file operators paste into
//! a bug report; the vault is the one place this project already keeps that class
//! of value. See [`sharerr_core::config::secret_keys::NOTIFICATIONS_WEBHOOK_URL`].
//!
//! # Two triggers, one sender
//!
//! [`send`] is the one place a request actually goes out, called from two very
//! different callers: `commands::serve`'s background loop, on a sync that failed,
//! and [`quiet_peers_loop`] here, on a timer, for a peer whose `last_seen_at` has
//! not moved in longer than `notifications.peer_quiet_secs`. Neither trigger
//! blocks on the other, and neither failing to reach the webhook stops anything
//! else sharerr does — a notification is best-effort by nature.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use secrecy::ExposeSecret;
use sharerr_core::config::{NotifyKind, secret_keys};
use sharerr_core::endpoint::now_epoch;
use tokio::sync::RwLock;

use crate::state::ServeState;

/// How often the peer-quiet check runs. Peers do not go quiet on a schedule
/// worth polling faster than this — the threshold itself is measured in days —
/// so an hour costs nothing in responsiveness.
const QUIET_CHECK_INTERVAL: Duration = Duration::from_secs(3600);

/// Per-peer dedupe for the quiet-peer notification: which `last_seen_at` this
/// peer was last notified as stale for.
///
/// Not persisted. A restart before the peer reappears costs one duplicate
/// notification for what is, at most, a weekly event — cheaper than a migration
/// for state that is naturally reconstructed the next time the peer is seen.
#[derive(Debug, Default)]
pub struct QuietNotified {
    inner: RwLock<HashMap<i64, i64>>,
}

impl QuietNotified {
    /// Whether `peer_id` being stale as of `last_seen_at` is new information —
    /// `false` if this exact staleness was already reported. Recording happens
    /// here too, so a caller cannot check and forget to mark in one step.
    async fn should_notify(&self, peer_id: i64, last_seen_at: i64) -> bool {
        let mut map = self.inner.write().await;
        if map.get(&peer_id) == Some(&last_seen_at) {
            return false;
        }
        map.insert(peer_id, last_seen_at);
        true
    }
}

/// Where notifications go, and in what shape — resolved once.
///
/// Reading the URL means opening the vault, an Argon2 derivation, so a caller
/// with several notifications to send resolves this once and reuses it rather
/// than paying that per message.
struct Webhook {
    url: url::Url,
    kind: NotifyKind,
    client: reqwest::Client,
}

/// The configured webhook, or `None` when there is none to send through.
async fn webhook(state: &ServeState) -> Option<Webhook> {
    let vault = state.open_vault().await.ok()?;
    let Ok(Some(configured)) = vault.get(secret_keys::NOTIFICATIONS_WEBHOOK_URL) else {
        // Not configured — the ordinary state for most instances, so this is
        // silent rather than a warning on every sync.
        return None;
    };
    let Ok(url) = url::Url::parse(configured.expose_secret()) else {
        tracing::warn!("notifications.webhook_url is not a valid URL — check Settings");
        return None;
    };

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            tracing::warn!(error = %err, "could not build the notification client");
            return None;
        }
    };

    Some(Webhook {
        url,
        kind: state.config().await.notifications.kind,
        client,
    })
}

/// Send one notification, if a webhook is configured. Never fails outward: a
/// misconfigured or unreachable webhook is logged and otherwise ignored, the
/// same as any other best-effort side channel in this codebase.
pub async fn send(state: &ServeState, event: &str, message: &str) {
    let Some(webhook) = webhook(state).await else {
        return;
    };
    webhook.post(event, message).await;
}

impl Webhook {
    async fn post(&self, event: &str, message: &str) {
        let body = match self.kind {
            NotifyKind::Generic => serde_json::json!({ "event": event, "message": message }),
            // Discord's own webhook shape: a single "content" field, Markdown-ish.
            NotifyKind::Discord => serde_json::json!({
                "content": format!("**sharerr** — {event}\n{message}")
            }),
            // Apprise's API server shape: POST to its own /notify endpoint, which
            // fans this one call out to whatever Apprise itself is configured to
            // reach.
            NotifyKind::Apprise => serde_json::json!({
                "title": format!("sharerr — {event}"),
                "body": message,
            }),
        };

        match self.client.post(self.url.clone()).json(&body).send().await {
            Ok(response) if response.status().is_success() => {
                tracing::debug!(event, "sent a notification");
            }
            Ok(response) => tracing::warn!(
                status = %response.status(),
                event,
                "the notification webhook responded with an error"
            ),
            Err(err) => {
                tracing::warn!(error = %err, event, "could not reach the notification webhook");
            }
        }
    }
}

/// Watch every friend for having gone quiet, on a timer. Never returns.
pub async fn quiet_peers_loop(state: Arc<ServeState>) {
    loop {
        if let Err(reason) = check_quiet_peers(&state).await {
            tracing::debug!(reason, "peer-quiet check skipped");
        }
        tokio::time::sleep(QUIET_CHECK_INTERVAL).await;
    }
}

async fn check_quiet_peers(state: &Arc<ServeState>) -> Result<(), String> {
    let threshold = state.config().await.notifications.peer_quiet_secs;
    if threshold == 0 {
        return Ok(());
    }
    // A threshold configured but no webhook to report through is the same as
    // the check being off — cheaper to notice here than to run the query and
    // discard every result. Resolved once and reused for every quiet peer
    // found: this used to open the vault here and then again inside `send` per
    // peer, so an hourly pass with N quiet friends paid 1+N Argon2 derivations
    // for a value already in hand.
    let Some(webhook) = webhook(state).await else {
        return Ok(());
    };

    let store = state.store().await?;
    let peers = store.list_peers().await.map_err(|err| err.to_string())?;
    let now = now_epoch();
    let threshold = i64::try_from(threshold).unwrap_or(i64::MAX);

    for peer in peers.iter().filter(|p| !p.is_revoked()) {
        // A peer never seen at all has not "gone" quiet — it was never
        // otherwise, and there is nothing to compare a silence against.
        let Some(last_seen) = peer.last_seen_at else {
            continue;
        };
        if now - last_seen < threshold {
            continue;
        }
        if !state
            .quiet_notified()
            .should_notify(peer.id, last_seen)
            .await
        {
            continue;
        }

        webhook
            .post(
                "peer gone quiet",
                &format!(
                    "{} has not been seen since {}",
                    peer.label,
                    crate::web::peers::ago(last_seen)
                ),
            )
            .await;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[tokio::test]
    async fn the_same_staleness_is_reported_once() {
        let notified = QuietNotified::default();

        assert!(notified.should_notify(1, 1_000).await);
        assert!(!notified.should_notify(1, 1_000).await, "must not repeat");
    }

    /// A peer who was seen again and then went quiet a second time is worth a
    /// fresh notification — the point of keying on `last_seen_at` rather than a
    /// bare "already notified" flag.
    #[tokio::test]
    async fn a_later_staleness_notifies_again() {
        let notified = QuietNotified::default();

        assert!(notified.should_notify(1, 1_000).await);
        assert!(notified.should_notify(1, 2_000).await);
    }

    #[tokio::test]
    async fn different_peers_are_independent() {
        let notified = QuietNotified::default();

        assert!(notified.should_notify(1, 1_000).await);
        assert!(notified.should_notify(2, 1_000).await);
    }
}
