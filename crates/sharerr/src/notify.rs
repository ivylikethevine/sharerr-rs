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
    // found, so an hourly pass with N quiet friends pays one Argon2
    // derivation, not N.
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

    // ------------------------------------------------------------ Webhook
    //
    // Built by hand against a wiremock server rather than through `webhook()`
    // — that function opens the vault, which cannot in this suite (see
    // CLAUDE.md). `Webhook`'s fields are private but this module's own tests
    // can still construct one directly, which is exactly the "test the
    // store-backed logic with the secret already resolved" pattern
    // `checks::check_qbit` already uses for the same reason.

    fn webhook_to(server: &wiremock::MockServer, kind: NotifyKind) -> Webhook {
        Webhook {
            url: url::Url::parse(&server.uri()).unwrap(),
            kind,
            client: reqwest::Client::new(),
        }
    }

    #[tokio::test]
    async fn a_generic_webhook_posts_event_and_message_as_json() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::body_json(serde_json::json!({
                "event": "sync failed",
                "message": "could not reach qBittorrent"
            })))
            .respond_with(wiremock::ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        webhook_to(&server, NotifyKind::Generic)
            .post("sync failed", "could not reach qBittorrent")
            .await;
    }

    #[tokio::test]
    async fn a_discord_webhook_folds_event_and_message_into_one_content_field() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::body_json(serde_json::json!({
                "content": "**sharerr** — peer gone quiet\nSam has not been seen since 2 day(s) ago"
            })))
            .respond_with(wiremock::ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        webhook_to(&server, NotifyKind::Discord)
            .post(
                "peer gone quiet",
                "Sam has not been seen since 2 day(s) ago",
            )
            .await;
    }

    #[tokio::test]
    async fn an_apprise_webhook_sends_a_title_and_body() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::body_json(serde_json::json!({
                "title": "sharerr — sync failed",
                "body": "could not reach qBittorrent"
            })))
            .respond_with(wiremock::ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        webhook_to(&server, NotifyKind::Apprise)
            .post("sync failed", "could not reach qBittorrent")
            .await;
    }

    /// `post` must never panic or propagate an error outward — a notification
    /// is best-effort, and a webhook responding with a server error is no
    /// different from any other unreachable side channel.
    #[tokio::test]
    async fn a_failing_webhook_is_swallowed_rather_than_panicking() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(500))
            .mount(&server)
            .await;

        webhook_to(&server, NotifyKind::Generic)
            .post("sync failed", "whatever")
            .await;
    }

    #[tokio::test]
    async fn posting_to_nothing_listening_does_not_panic() {
        let port = sharerr_testkit::net::closed_port();
        let webhook = Webhook {
            url: url::Url::parse(&format!("http://127.0.0.1:{port}")).unwrap(),
            kind: NotifyKind::Generic,
            client: reqwest::Client::new(),
        };

        webhook.post("sync failed", "whatever").await;
    }

    // ------------------------------------------------------- send / quiet loop

    #[tokio::test]
    async fn send_with_no_webhook_configured_returns_without_erroring() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        // No master key is set, so `webhook()` cannot open the vault — the
        // same "not configured" outcome an operator who never set one sees.
        send(&serve, "sync failed", "whatever").await;
    }

    #[tokio::test]
    async fn check_quiet_peers_with_the_threshold_off_never_touches_the_vault_or_store() {
        let (dir, _serve) = crate::state::fixtures::unconfigured();
        let config = sharerr_core::Config {
            data_dir: dir.path().to_path_buf(),
            notifications: sharerr_core::config::NotificationsConfig {
                peer_quiet_secs: 0,
                ..Default::default()
            },
            ..Default::default()
        };
        let state = Arc::new(ServeState::new(
            config,
            dir.path().join("sharerr.toml"),
            None,
        ));

        // `0` turns the check off entirely — see `NotificationsConfig::peer_quiet_secs`.
        // No database exists at this data_dir; a store touch here would fail.
        assert_eq!(check_quiet_peers(&state).await, Ok(()));
    }

    #[tokio::test]
    async fn check_quiet_peers_with_no_webhook_configured_is_a_silent_no_op() {
        let (dir, _serve) = crate::state::fixtures::unconfigured();
        let config = sharerr_core::Config {
            data_dir: dir.path().to_path_buf(),
            notifications: sharerr_core::config::NotificationsConfig {
                peer_quiet_secs: 3600,
                ..Default::default()
            },
            ..Default::default()
        };
        let state = Arc::new(ServeState::new(
            config,
            dir.path().join("sharerr.toml"),
            None,
        ));

        // No master key, so `webhook()` finds nothing to send through and this
        // returns before ever touching the (nonexistent) database either.
        assert_eq!(check_quiet_peers(&state).await, Ok(()));
    }
}
