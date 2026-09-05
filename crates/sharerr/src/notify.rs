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
//! # Several triggers, one sender
//!
//! [`send`] is the one place a request actually goes out for every trigger but
//! one — [`quiet_peers_loop`] here calls `Webhook::post` directly instead, since
//! it already resolves the webhook once per tick and reuses it across every
//! quiet peer found, where `send` would resolve it again per call. Every
//! trigger checks [`sharerr_core::config::NotificationsConfig::triggers`]
//! before anything else, so a disabled one costs nothing beyond an in-memory
//! read. Callers span `commands::serve`'s background loop (a sync that failed,
//! or a digest of what it added/failed), [`quiet_peers_loop`] itself (a peer
//! whose `last_seen_at` has not moved in longer than
//! `notifications.peer_quiet_secs`), `gluetun::poll_once` (the advertised
//! endpoint rotating), `web::peers::revoke` (a friend's key revoked),
//! `torznab::record_sighting` via [`peer_first_contact`] (a friend's very
//! first sighting), [`reachability_loop`] here (this instance's own tracker
//! address no longer accepting connections), and `commands::serve::background`
//! again for a `[[library]]` path that could not be read. None block on each
//! other, and none failing to reach the webhook stops anything else sharerr
//! does — a notification is best-effort by nature.
//!
//! # The heartbeat is the odd one out
//!
//! [`heartbeat_loop`] never posts to the webhook. It fetches a separate
//! Uptime-Kuma-style push URL on a timer while the instance is healthy, so the
//! monitor on the other end notices *silence*. It shares the trigger list so
//! one tickbox turns it off, but nothing else here — different URL, different
//! verb, different failure semantics (a missed push is the signal).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use secrecy::ExposeSecret;
use sharerr_core::config::{NotificationTrigger, NotifyKind, secret_keys};
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

/// The HTTP client every notification shares, built once for the life of the
/// process rather than once per call.
///
/// `webhook()` is the single choke point both callers reach a client
/// through — [`send`], a per-event notification, and `check_quiet_peers`, a
/// per-tick batch — so caching it here covers both the same way
/// `gossip::exchange_loop` and `lighthouse_client::sync_loop` cache theirs
/// for their own loops. Unlike those, `send` has no enclosing loop to build
/// the client ahead of, hence the lazy static instead of a hoisted local.
fn http_client() -> Option<&'static reqwest::Client> {
    static CLIENT: std::sync::OnceLock<Option<reqwest::Client>> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(
            || match sharerr_client::http_client_with_timeout(Duration::from_secs(10)) {
                Ok(client) => Some(client),
                Err(err) => {
                    tracing::warn!(error = %err, "could not build the notification HTTP client");
                    None
                }
            },
        )
        .as_ref()
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

    Some(Webhook {
        url,
        kind: state.with_config(|c| c.notifications.kind).await,
        client: http_client()?.clone(),
    })
}

/// Send one notification, if a webhook is configured and `trigger` is
/// enabled. Never fails outward: a misconfigured or unreachable webhook is
/// logged and otherwise ignored, the same as any other best-effort side
/// channel in this codebase.
pub async fn send(state: &ServeState, trigger: NotificationTrigger, message: &str) {
    if !trigger_enabled(state, trigger).await {
        return;
    }
    let Some(webhook) = webhook(state).await else {
        return;
    };
    webhook.post(trigger.label(), message).await;
}

/// Whether `trigger` is in [`sharerr_core::config::NotificationsConfig::triggers`]
/// — checked ahead of resolving the webhook itself, so a disabled trigger
/// costs nothing beyond reading the in-memory config.
async fn trigger_enabled(state: &ServeState, trigger: NotificationTrigger) -> bool {
    state
        .config()
        .await
        .notifications
        .triggers
        .contains(&trigger)
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

/// A friend was just seen for the first time — `torznab::record_sighting`
/// observed [`sharerr_store::Touch::First`] for them. Fires
/// [`NotificationTrigger::PeerFirstContact`] with their label and how they
/// showed up (feed request or tracker announce).
///
/// The label is looked up here rather than passed in because the tracker's
/// announce path only knows the peer's id; one indexed read on an event that
/// happens once per friend ever is not worth widening both call sites for.
pub async fn peer_first_contact(
    state: &ServeState,
    store: &sharerr_store::Store,
    peer_id: i64,
    kind: sharerr_store::EndpointKind,
) {
    if !trigger_enabled(state, NotificationTrigger::PeerFirstContact).await {
        return;
    }
    let label = match store.peer(peer_id).await {
        Ok(Some(peer)) => peer.label,
        Ok(None) => format!("friend #{peer_id}"),
        Err(err) => {
            tracing::warn!(peer_id, error = %err, "could not look up a peer for its first-contact notification");
            format!("friend #{peer_id}")
        }
    };
    send(
        state,
        NotificationTrigger::PeerFirstContact,
        &first_contact_message(&label, kind),
    )
    .await;
}

/// The one-line body of a first-contact notification, split out so the
/// wording is testable without a store or a vault.
fn first_contact_message(label: &str, kind: sharerr_store::EndpointKind) -> String {
    let how = match kind {
        sharerr_store::EndpointKind::Api => "fetched the feed",
        sharerr_store::EndpointKind::Client => "announced to the tracker",
        sharerr_store::EndpointKind::Tracker => "made contact",
    };
    format!("{label} {how} for the first time")
}

/// How often [`reachability_loop`] dials the advertised tracker address. The
/// check is a single TCP connect bounded by `checks::REACH_TIMEOUT`, so this
/// is about not hammering one's own router, not about cost.
const REACH_CHECK_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// Watch this instance's own advertised tracker address for going
/// unreachable, on a timer. Never returns.
///
/// Gated twice: on `[checks] reachability` (the opt-in that owns the
/// NAT-hairpin caveat — see `checks::check_reachable`) and on the
/// [`NotificationTrigger::TrackerUnreachable`] trigger. It notifies only on
/// the transition from confirmed-reachable to not, so an instance whose
/// router never hairpins is never told its port is closed; it simply never
/// reaches the "reachable" state a later failure would be measured against.
pub async fn reachability_loop(state: Arc<ServeState>) {
    let mut watch = ReachWatch::default();
    loop {
        let (enabled, trigger) = (
            state.config().await.checks.reachability,
            trigger_enabled(&state, NotificationTrigger::TrackerUnreachable).await,
        );
        if enabled && trigger {
            let base = state.endpoint().current();
            let outcome = crate::checks::check_reachable(base.as_ref()).await;
            if let Some(message) = watch.observe(base.as_ref(), &outcome) {
                send(&state, NotificationTrigger::TrackerUnreachable, &message).await;
            }
        }
        tokio::time::sleep(REACH_CHECK_INTERVAL).await;
    }
}

/// The transition detector behind [`reachability_loop`]: remembers whether
/// the last dial succeeded, and yields a message exactly when a confirmed
/// address stops answering.
#[derive(Debug, Default)]
struct ReachWatch {
    was_reachable: bool,
}

impl ReachWatch {
    /// Feed one dial's outcome; `Some(message)` when it is worth notifying.
    fn observe(
        &mut self,
        base: Option<&url::Url>,
        outcome: &crate::checks::ReachOutcome,
    ) -> Option<String> {
        use crate::checks::ReachOutcome;
        let reachable = outcome.is_reachable();
        let fell = self.was_reachable && !reachable;
        self.was_reachable = reachable;
        if !fell {
            return None;
        }
        let address = base.map(ToString::to_string).unwrap_or_default();
        Some(match outcome {
            ReachOutcome::Refused(reason) => {
                format!("{address} stopped accepting connections ({reason})")
            }
            ReachOutcome::TimedOut => {
                format!("{address} stopped accepting connections (timed out)")
            }
            ReachOutcome::NotConfigured => {
                "the advertised tracker address is no longer configured".to_owned()
            }
            ReachOutcome::Unusable(reason) => {
                format!("the advertised tracker address became unusable ({reason})")
            }
            ReachOutcome::Reachable => unreachable!("a reachable outcome is never a fall"),
        })
    }
}

/// Push an Uptime-Kuma-style heartbeat on a timer. Never returns.
///
/// Each tick: `notifications.heartbeat_secs` is re-read (so the UI can change
/// it without a restart; `0` parks the loop on a one-minute re-check), the
/// [`NotificationTrigger::Heartbeat`] trigger is consulted, and the push URL
/// is read from the vault. The GET goes out only while the instance would
/// answer `/ready` with 200 — configuration loaded and the syncer built — so
/// a monitor that expects this push sees the same truth `/ready` tells.
///
/// The vault is opened every tick rather than once, the same as
/// `check_quiet_peers`: Argon2 is tens of milliseconds, the minimum tick is
/// a minute, and it means a URL pasted into Settings takes effect on the next
/// beat rather than after a restart.
pub async fn heartbeat_loop(state: Arc<ServeState>) {
    loop {
        let secs = state.config().await.notifications.heartbeat_secs;
        if secs == 0 {
            tokio::time::sleep(Duration::from_secs(60)).await;
            continue;
        }
        if trigger_enabled(&state, NotificationTrigger::Heartbeat).await
            && is_ready(&state).await
            && let Some((url, client)) = heartbeat_target(&state).await
        {
            push_heartbeat(&client, &url).await;
        }
        tokio::time::sleep(Duration::from_secs(secs.max(1))).await;
    }
}

/// The readiness `/ready` reports, without the database round-trip: the
/// configuration loaded and the syncer is built. Cheap enough to ask every
/// beat.
async fn is_ready(state: &ServeState) -> bool {
    state.config_error().await.is_none() && state.syncer().await.is_ok()
}

/// The stored push URL and the shared client, or `None` when there is
/// nothing to push to (the ordinary state — most instances have no monitor).
async fn heartbeat_target(state: &ServeState) -> Option<(url::Url, reqwest::Client)> {
    let vault = state.open_vault().await.ok()?;
    let Ok(Some(configured)) = vault.get(secret_keys::NOTIFICATIONS_HEARTBEAT_URL) else {
        return None;
    };
    let Ok(url) = url::Url::parse(configured.expose_secret()) else {
        tracing::warn!("notifications.heartbeat_url is not a valid URL — check Settings");
        return None;
    };
    Some((url, http_client()?.clone()))
}

/// One heartbeat: a bare GET, which is what Uptime Kuma's push monitor
/// expects (`/api/push/<token>?status=up&msg=OK&ping=`, all of it already in
/// the stored URL). Best-effort like every other sender here.
async fn push_heartbeat(client: &reqwest::Client, url: &url::Url) {
    match client.get(url.clone()).send().await {
        Ok(response) if response.status().is_success() => {
            tracing::debug!("sent a heartbeat");
        }
        Ok(response) => tracing::warn!(
            status = %response.status(),
            "the heartbeat push URL responded with an error"
        ),
        Err(err) => {
            tracing::warn!(error = %err, "could not reach the heartbeat push URL");
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
    if threshold == 0 || !trigger_enabled(state, NotificationTrigger::PeerQuiet).await {
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
                NotificationTrigger::PeerQuiet.label(),
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
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::result_large_err)]

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

    // ---------------------------------------------------- webhook() resolution
    //
    // `webhook()` opens a real vault, which this suite otherwise avoids (see
    // CLAUDE.md). `figment::Jail` scopes and serializes `SHARERR_MASTER_KEY`
    // to one closure at a time, the same pattern `secrets.rs` and
    // `web/mod.rs` already use, which makes exercising `webhook()` itself
    // (rather than only its "vault will not open" fallback) safe here.

    fn state_with_a_stored_webhook_url(jail: &mut figment::Jail, url: &str) -> Arc<ServeState> {
        jail.set_env("SHARERR_MASTER_KEY", "a-master-key");
        let dir = jail.directory().to_path_buf();
        let config = sharerr_core::Config {
            data_dir: dir.clone(),
            ..sharerr_core::Config::default()
        };
        let state = Arc::new(ServeState::new(config, dir.join("sharerr.toml"), None));

        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let mut vault = state.open_vault().await.unwrap();
            vault
                .put(
                    secret_keys::NOTIFICATIONS_WEBHOOK_URL,
                    &secrecy::SecretString::from(url.to_owned()),
                )
                .unwrap();
        });
        state
    }

    #[test]
    fn webhook_resolves_the_url_and_kind_from_a_real_vault_secret() {
        figment::Jail::expect_with(|jail| {
            let state = state_with_a_stored_webhook_url(jail, "https://hooks.example/abc");

            let runtime = tokio::runtime::Runtime::new().unwrap();
            let resolved = runtime.block_on(webhook(&state)).expect("must resolve");
            assert_eq!(resolved.url.as_str(), "https://hooks.example/abc");
            assert_eq!(resolved.kind, NotifyKind::Generic);
            Ok(())
        });
    }

    #[test]
    fn webhook_with_an_unparseable_stored_url_is_treated_as_unconfigured() {
        figment::Jail::expect_with(|jail| {
            let state = state_with_a_stored_webhook_url(jail, "not a url at all");

            let runtime = tokio::runtime::Runtime::new().unwrap();
            assert!(runtime.block_on(webhook(&state)).is_none());
            Ok(())
        });
    }

    #[test]
    fn send_delivers_through_a_vault_configured_webhook() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("SHARERR_MASTER_KEY", "a-master-key");
            let dir = jail.directory().to_path_buf();
            let config = sharerr_core::Config {
                data_dir: dir.clone(),
                ..sharerr_core::Config::default()
            };
            let state = Arc::new(ServeState::new(config, dir.join("sharerr.toml"), None));

            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async {
                let server = wiremock::MockServer::start().await;
                wiremock::Mock::given(wiremock::matchers::method("POST"))
                    .respond_with(wiremock::ResponseTemplate::new(200))
                    .expect(1)
                    .mount(&server)
                    .await;

                let mut vault = state.open_vault().await.unwrap();
                vault
                    .put(
                        secret_keys::NOTIFICATIONS_WEBHOOK_URL,
                        &secrecy::SecretString::from(server.uri()),
                    )
                    .unwrap();
                drop(vault);

                send(
                    &state,
                    NotificationTrigger::SyncFailed,
                    "could not reach qBittorrent",
                )
                .await;
            });
            Ok(())
        });
    }

    #[test]
    fn send_uses_the_triggers_label_as_the_event_text() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("SHARERR_MASTER_KEY", "a-master-key");
            let dir = jail.directory().to_path_buf();
            let config = sharerr_core::Config {
                data_dir: dir.clone(),
                ..sharerr_core::Config::default()
            };
            let state = Arc::new(ServeState::new(config, dir.join("sharerr.toml"), None));

            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async {
                let server = wiremock::MockServer::start().await;
                wiremock::Mock::given(wiremock::matchers::method("POST"))
                    .and(wiremock::matchers::body_json(serde_json::json!({
                        "event": "friend revoked",
                        "message": "revoked friend #7's key"
                    })))
                    .respond_with(wiremock::ResponseTemplate::new(200))
                    .expect(1)
                    .mount(&server)
                    .await;

                let mut vault = state.open_vault().await.unwrap();
                vault
                    .put(
                        secret_keys::NOTIFICATIONS_WEBHOOK_URL,
                        &secrecy::SecretString::from(server.uri()),
                    )
                    .unwrap();
                drop(vault);

                send(
                    &state,
                    NotificationTrigger::PeerRevoked,
                    "revoked friend #7's key",
                )
                .await;
            });
            Ok(())
        });
    }

    #[test]
    fn send_does_nothing_when_the_trigger_is_disabled() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("SHARERR_MASTER_KEY", "a-master-key");
            let dir = jail.directory().to_path_buf();
            let config = sharerr_core::Config {
                data_dir: dir.clone(),
                notifications: sharerr_core::config::NotificationsConfig {
                    // Every trigger but the one under test — proves `send` checks
                    // the specific trigger, not merely "is anything enabled".
                    triggers: sharerr_core::config::NotificationTrigger::ALL
                        .iter()
                        .copied()
                        .filter(|t| *t != NotificationTrigger::EndpointRotated)
                        .collect(),
                    ..Default::default()
                },
                ..sharerr_core::Config::default()
            };
            let state = Arc::new(ServeState::new(config, dir.join("sharerr.toml"), None));

            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async {
                let server = wiremock::MockServer::start().await;
                // No `.expect(1)` — any request at all fails the test via the
                // server's own drop-time verification of zero-or-more, so the
                // absence of a mount at all would also pass; mount one anyway so
                // a regression shows as a real assertion failure, not a hang.
                wiremock::Mock::given(wiremock::matchers::method("POST"))
                    .respond_with(wiremock::ResponseTemplate::new(200))
                    .expect(0)
                    .mount(&server)
                    .await;

                let mut vault = state.open_vault().await.unwrap();
                vault
                    .put(
                        secret_keys::NOTIFICATIONS_WEBHOOK_URL,
                        &secrecy::SecretString::from(server.uri()),
                    )
                    .unwrap();
                drop(vault);

                send(
                    &state,
                    NotificationTrigger::EndpointRotated,
                    "advertised endpoint is now 203.0.113.5:51413",
                )
                .await;
            });
            Ok(())
        });
    }

    // -------------------------------------------------- check_quiet_peers, live

    #[test]
    fn check_quiet_peers_notifies_a_stale_peer_once_then_dedupes() {
        figment::Jail::expect_with(|jail| {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async {
                let server = wiremock::MockServer::start().await;
                wiremock::Mock::given(wiremock::matchers::method("POST"))
                    .respond_with(wiremock::ResponseTemplate::new(200))
                    .expect(1)
                    .mount(&server)
                    .await;

                jail.set_env("SHARERR_MASTER_KEY", "a-master-key");
                let dir = jail.directory().to_path_buf();
                let config = sharerr_core::Config {
                    data_dir: dir.clone(),
                    notifications: sharerr_core::config::NotificationsConfig {
                        // A 1-second threshold, cleared with a short sleep
                        // below — there is no store API to backdate
                        // `last_seen_at` directly, only `touch_peer` (always
                        // "now"), so time is left to actually pass instead.
                        peer_quiet_secs: 1,
                        ..Default::default()
                    },
                    ..sharerr_core::Config::default()
                };
                let state = Arc::new(ServeState::new(config, dir.join("sharerr.toml"), None));

                let mut vault = state.open_vault().await.unwrap();
                vault
                    .put(
                        secret_keys::NOTIFICATIONS_WEBHOOK_URL,
                        &secrecy::SecretString::from(server.uri()),
                    )
                    .unwrap();
                drop(vault);

                let store = state.store().await.unwrap();
                let peer = store
                    .create_peer(
                        "Sam",
                        &secrecy::SecretString::from("sam-key"),
                        sharerr_store::PeerScope::All,
                    )
                    .await
                    .unwrap();
                store.touch_peer(peer.id).await.unwrap();
                tokio::time::sleep(Duration::from_millis(1500)).await;

                check_quiet_peers(&state).await.unwrap();
                // A second pass finds the same staleness and must not notify
                // again — the mock's `expect(1)` above enforces this.
                check_quiet_peers(&state).await.unwrap();
            });
            Ok(())
        });
    }

    // ------------------------------------------------------- send / quiet loop

    #[tokio::test]
    async fn send_with_no_webhook_configured_returns_without_erroring() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        // No master key is set, so `webhook()` cannot open the vault — the
        // same "not configured" outcome an operator who never set one sees.
        send(&serve, NotificationTrigger::SyncFailed, "whatever").await;
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

    // ------------------------------------------------------- first contact

    #[test]
    fn first_contact_message_says_how_the_friend_showed_up() {
        assert_eq!(
            first_contact_message("Sam", sharerr_store::EndpointKind::Api),
            "Sam fetched the feed for the first time"
        );
        assert_eq!(
            first_contact_message("Sam", sharerr_store::EndpointKind::Client),
            "Sam announced to the tracker for the first time"
        );
    }

    #[test]
    fn peer_first_contact_notifies_with_the_peers_label() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("SHARERR_MASTER_KEY", "a-master-key");
            let dir = jail.directory().to_path_buf();
            let config = sharerr_core::Config {
                data_dir: dir.clone(),
                ..sharerr_core::Config::default()
            };
            let state = Arc::new(ServeState::new(config, dir.join("sharerr.toml"), None));

            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async {
                let server = wiremock::MockServer::start().await;
                wiremock::Mock::given(wiremock::matchers::method("POST"))
                    .and(wiremock::matchers::body_json(serde_json::json!({
                        "event": "friend made first contact",
                        "message": "Sam fetched the feed for the first time"
                    })))
                    .respond_with(wiremock::ResponseTemplate::new(200))
                    .expect(1)
                    .mount(&server)
                    .await;

                let mut vault = state.open_vault().await.unwrap();
                vault
                    .put(
                        secret_keys::NOTIFICATIONS_WEBHOOK_URL,
                        &secrecy::SecretString::from(server.uri()),
                    )
                    .unwrap();
                drop(vault);

                let store = state.store().await.unwrap();
                let sam = store
                    .create_peer(
                        "Sam",
                        &secrecy::SecretString::from("sam-key"),
                        sharerr_store::PeerScope::All,
                    )
                    .await
                    .unwrap();

                peer_first_contact(&state, &store, sam.id, sharerr_store::EndpointKind::Api).await;
            });
            Ok(())
        });
    }

    #[tokio::test]
    async fn peer_first_contact_with_no_webhook_is_a_no_op_even_for_an_unknown_peer() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let store = sharerr_store::Store::open_in_memory().await.unwrap();
        peer_first_contact(&serve, &store, 42, sharerr_store::EndpointKind::Client).await;
    }

    // ------------------------------------------------------- reachability

    #[test]
    fn reach_watch_notifies_only_on_the_fall_from_reachable() {
        use crate::checks::ReachOutcome;
        let base = url::Url::parse("http://203.0.113.5:8478/").unwrap();
        let mut watch = ReachWatch::default();

        // Never confirmed reachable (a router that refuses hairpinning): silent.
        assert!(
            watch
                .observe(Some(&base), &ReachOutcome::Refused("refused".into()))
                .is_none()
        );
        assert!(
            watch
                .observe(Some(&base), &ReachOutcome::Reachable)
                .is_none()
        );
        // Confirmed, then gone: exactly one message.
        let message = watch
            .observe(Some(&base), &ReachOutcome::TimedOut)
            .expect("the fall must notify");
        assert_eq!(
            message,
            "http://203.0.113.5:8478/ stopped accepting connections (timed out)"
        );
        // Still gone: no repeat until it has been reachable again.
        assert!(
            watch
                .observe(Some(&base), &ReachOutcome::TimedOut)
                .is_none()
        );
        assert!(
            watch
                .observe(Some(&base), &ReachOutcome::Reachable)
                .is_none()
        );
        assert!(
            watch
                .observe(Some(&base), &ReachOutcome::Refused("nope".into()))
                .is_some()
        );
    }

    #[test]
    fn reach_watch_words_a_lost_or_unusable_address() {
        use crate::checks::ReachOutcome;
        let mut watch = ReachWatch::default();
        watch.observe(None, &ReachOutcome::Reachable);
        assert_eq!(
            watch.observe(None, &ReachOutcome::NotConfigured).unwrap(),
            "the advertised tracker address is no longer configured"
        );
        watch.observe(None, &ReachOutcome::Reachable);
        assert_eq!(
            watch
                .observe(None, &ReachOutcome::Unusable("no host".into()))
                .unwrap(),
            "the advertised tracker address became unusable (no host)"
        );
    }

    // ---------------------------------------------------------- heartbeat

    #[tokio::test]
    async fn a_heartbeat_is_a_bare_get_to_the_stored_url() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/push/abc"))
            .and(wiremock::matchers::query_param("status", "up"))
            .respond_with(wiremock::ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let url = url::Url::parse(&format!(
            "{}/api/push/abc?status=up&msg=OK&ping=",
            server.uri()
        ))
        .unwrap();
        push_heartbeat(&reqwest::Client::new(), &url).await;
    }

    #[tokio::test]
    async fn a_failing_heartbeat_is_swallowed() {
        let port = sharerr_testkit::net::closed_port();
        let url = url::Url::parse(&format!("http://127.0.0.1:{port}/push")).unwrap();
        push_heartbeat(&reqwest::Client::new(), &url).await;
    }

    #[test]
    fn heartbeat_target_resolves_the_stored_push_url() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("SHARERR_MASTER_KEY", "a-master-key");
            let dir = jail.directory().to_path_buf();
            let config = sharerr_core::Config {
                data_dir: dir.clone(),
                ..sharerr_core::Config::default()
            };
            let state = Arc::new(ServeState::new(config, dir.join("sharerr.toml"), None));

            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async {
                assert!(
                    heartbeat_target(&state).await.is_none(),
                    "nothing stored yet"
                );

                let mut vault = state.open_vault().await.unwrap();
                vault
                    .put(
                        secret_keys::NOTIFICATIONS_HEARTBEAT_URL,
                        &secrecy::SecretString::from("https://kuma.example/api/push/abc?status=up"),
                    )
                    .unwrap();
                drop(vault);

                let (url, _) = heartbeat_target(&state).await.expect("must resolve");
                assert_eq!(url.path(), "/api/push/abc");
            });
            Ok(())
        });
    }

    #[tokio::test]
    async fn an_unconfigured_instance_is_not_ready_to_heartbeat() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        assert!(!is_ready(&serve).await);
        assert!(heartbeat_target(&serve).await.is_none());
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
