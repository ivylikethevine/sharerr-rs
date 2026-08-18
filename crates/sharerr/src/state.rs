//! Process-wide state shared by everything the server runs.
//!
//! This lived in [`crate::commands::serve`] until the surface around it grew: the
//! tracker, the Torznab feed, and the web UI all need it, and all three are general
//! layers rather than CLI verbs. Having them reach *up* into `commands::` to find
//! it pointed the dependency arrow the wrong way — `serve` is one consumer of this
//! state, not its owner.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use secrecy::ExposeSecret;
use sharerr_core::Config;
use sharerr_core::config::secret_keys;
use sharerr_core::endpoint::AdvertisedEndpoint;
use sharerr_store::{Store, Vault, master_key_from_env};
use tokio::sync::{Notify, RwLock};

use crate::sync::Syncer;

/// How soon to retry building the syncer after the first failure.
///
/// Much shorter than any sync interval, deliberately: this is the loop an operator
/// is actively waiting on just after typing a credential, not background work. It
/// is also the figure the status page quotes to them, so it is the *first* retry
/// delay rather than the current one — see [`ServeState::recovery_delay`].
pub const RECOVERY_INTERVAL: Duration = Duration::from_secs(15);

/// Ceiling for the backoff below.
///
/// An instance that has been misconfigured for an hour is not one anybody is
/// watching, and five minutes is still far inside the window where "I fixed it and
/// it picked itself up" feels automatic.
pub const RECOVERY_INTERVAL_MAX: Duration = Duration::from_secs(300);

/// Shared between the HTTP handlers and the background loop.
///
/// The syncer is fallible-and-absent rather than simply present because it may not
/// be constructible yet. The `Err` side carries the reason, so `/ready` can report
/// *what* is wrong instead of only that something is.
///
/// The config is behind a lock because the web UI can rewrite `sharerr.toml` while
/// the process runs. Everything reads a clone rather than holding the guard: a
/// handler that kept it across an `.await` would block the settings write it is
/// racing.
#[derive(Debug)]
pub struct ServeState {
    config: RwLock<Config>,
    /// Which `sharerr.toml` the UI writes back to — the `--config` flag's value.
    config_path: PathBuf,
    /// Why the config on disk could not be loaded, when it could not be.
    ///
    /// `Some` means the `config` above is [`crate::settings::load_or_recover`]'s
    /// salvage rather than what the operator wrote, and the server is up only so
    /// that the file can be repaired. Behind a lock because a successful settings
    /// save clears it — the write proved the file parses.
    config_error: RwLock<Option<String>>,
    /// Opened independently of the syncer, because login has to work *before* any
    /// credential exists. Going through `syncer.store()` would mean nobody could
    /// log in to fix the very thing blocking the syncer.
    store: RwLock<Option<Store>>,
    syncer: RwLock<Result<Arc<Syncer>, String>>,
    /// Consecutive failed attempts to build the syncer, which is what
    /// [`Self::recovery_delay`] backs off on. Reset the moment one succeeds, and by
    /// [`Self::invalidate`] — a credential was just written, so the next attempt is
    /// a fresh question rather than a continuation of the old one.
    recovery_failures: RwLock<u32>,
    /// The builtin tracker's announce token, cached because it is consulted on
    /// every announce from every peer and reading it means an Argon2 derivation.
    ///
    /// `None` outer means "not looked up yet"; `Some(None)` means "looked up, and
    /// there is no token". Cleared by [`Self::invalidate`], so changing it through
    /// the UI takes effect without a restart.
    tracker_token: RwLock<Option<Option<String>>>,
    /// The legacy shared Torznab key, cached for the same reason as the tracker
    /// token: it is consulted on every feed request that does not match a peer —
    /// including every *wrong* key anyone sends — and reading it means an Argon2
    /// derivation. Same `None`/`Some(None)` shape, same clearing by
    /// [`Self::invalidate`].
    torznab_shared_key: RwLock<Option<Option<String>>>,
    /// Raised by [`Self::invalidate`] to cut short whatever the background loop is
    /// sleeping on.
    ///
    /// Without it the loop only notices an invalidation when its current sleep
    /// expires — and on a *working* instance that sleep is `sync.interval_secs`,
    /// 15 minutes by default. A credential typed into the UI would sit unused for
    /// a quarter of an hour while the page claimed it would be picked up in
    /// fifteen seconds. `notify_one` stores a permit, so an invalidation that
    /// lands mid-sync is not lost.
    wake: Notify,
    /// The live externally reachable endpoint. One value for the whole process:
    /// the syncer's tracker provider reads it, the gluetun poller updates it,
    /// and a settings save refreshes only its static half — so the poller's
    /// observations survive both a config change and a syncer rebuild.
    endpoint: Arc<AdvertisedEndpoint>,
    /// Raised by the gluetun push endpoint so the poller re-asks the control
    /// server *now* instead of at the next tick — the "reacted to in seconds"
    /// half of the endpoint story.
    endpoint_refresh: Notify,
    /// The tracker's live swarms. Owned here rather than by the tracker router
    /// because two consumers need one copy: however many listeners carry
    /// `/announce`, and the status page's "n peers connected" line.
    swarms: Arc<sharerr_torrent::Swarms>,
}

impl ServeState {
    pub fn new(
        config: Config,
        config_path: impl Into<PathBuf>,
        config_error: Option<String>,
    ) -> Self {
        let endpoint = Arc::new(static_endpoint(&config));
        Self {
            config: RwLock::new(config),
            config_path: config_path.into(),
            config_error: RwLock::new(config_error),
            store: RwLock::new(None),
            // Replaced by the first `ensure_ready`, which the background loop runs
            // the moment `run` starts polling it. Only observable in the sliver
            // between binding the listener and that first attempt finishing.
            syncer: RwLock::new(Err("still starting up".to_owned())),
            recovery_failures: RwLock::new(0),
            tracker_token: RwLock::new(None),
            torznab_shared_key: RwLock::new(None),
            wake: Notify::new(),
            endpoint,
            endpoint_refresh: Notify::new(),
            swarms: Arc::new(sharerr_torrent::Swarms::default()),
        }
    }

    /// The tracker's live swarms — one copy for every listener and the status
    /// page alike.
    pub fn swarms(&self) -> Arc<sharerr_torrent::Swarms> {
        Arc::clone(&self.swarms)
    }

    /// The live advertised endpoint this whole process shares.
    pub fn endpoint(&self) -> Arc<AdvertisedEndpoint> {
        Arc::clone(&self.endpoint)
    }

    /// Ask the background loop to run a pass soon, without invalidating the
    /// syncer. Used when something the syncer reads *through a shared handle*
    /// changed — the advertised endpoint above — so a rebuild would be waste.
    pub fn request_sync(&self) {
        self.wake.notify_one();
    }

    /// Ask the gluetun poller to re-resolve the endpoint now.
    pub fn nudge_endpoint(&self) {
        self.endpoint_refresh.notify_one();
    }

    /// Park until [`Self::nudge_endpoint`] is called.
    pub async fn endpoint_refresh_requested(&self) {
        self.endpoint_refresh.notified().await;
    }

    /// A snapshot of the current configuration.
    pub async fn config(&self) -> Config {
        self.config.read().await.clone()
    }

    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// Why `sharerr.toml` could not be loaded, or `None` when it was.
    pub async fn config_error(&self) -> Option<String> {
        self.config_error.read().await.clone()
    }

    /// The database, opening it on first use and caching it thereafter.
    ///
    /// Lazy rather than opened in `run` so that an unwritable `/data` is a legible
    /// error on the page that needed it, not a startup failure that stops the
    /// operator reaching any page at all.
    pub async fn store(&self) -> Result<Store, String> {
        if let Some(store) = &*self.store.read().await {
            return Ok(store.clone());
        }

        let path = self.config.read().await.database_path();
        match Store::open(&path).await {
            Ok(store) => {
                *self.store.write().await = Some(store.clone());
                Ok(store)
            }
            Err(err) => Err(format!("opening {}: {err}", path.display())),
        }
    }

    /// Open the credential vault, off the runtime.
    ///
    /// Argon2 key derivation is tens of milliseconds of solid CPU and ~19 MiB; a
    /// container pinned to one core has one runtime worker, so doing it inline
    /// stalls `/health` for the duration. The single opener for the whole binary —
    /// the web settings pages, the connection probes, and the tracker all come
    /// through here.
    pub async fn open_vault(&self) -> Result<Vault, String> {
        let master = master_key_from_env().map_err(|err| err.to_string())?;
        let path = self.config.read().await.vault_path();

        tokio::task::spawn_blocking(move || Vault::open(&path, &master))
            .await
            .map_err(|_| "the vault task panicked".to_owned())?
            .map_err(|err| format!("opening the vault: {err}"))
    }

    /// The token the builtin tracker requires in announce URLs, if any.
    ///
    /// Cached after the first read. A vault that will not open yields `None`,
    /// which is correct rather than merely convenient: without a vault there is no
    /// stored token, so there is none to enforce.
    pub async fn tracker_token(&self) -> Option<String> {
        if let Some(cached) = &*self.tracker_token.read().await {
            return cached.clone();
        }

        let token = match self.open_vault().await {
            Ok(vault) => vault
                .get(secret_keys::TRACKER_TOKEN)
                .ok()
                .flatten()
                .map(|secret| secret.expose_secret().to_owned()),
            Err(_) => None,
        };

        *self.tracker_token.write().await = Some(token.clone());
        token
    }

    /// The legacy shared Torznab key, if one is stored. Cached after the first
    /// read; a vault that will not open yields `None`, which closes the legacy
    /// path rather than opening it.
    pub async fn torznab_shared_key(&self) -> Option<String> {
        if let Some(cached) = &*self.torznab_shared_key.read().await {
            return cached.clone();
        }

        let key = match self.open_vault().await {
            Ok(vault) => vault
                .get(secret_keys::TORZNAB_API_KEY)
                .ok()
                .flatten()
                .map(|secret| secret.expose_secret().to_owned()),
            Err(_) => None,
        };

        *self.torznab_shared_key.write().await = Some(key.clone());
        key
    }

    /// Why reconciliation is not running, or `None` when it is.
    ///
    /// `Option` rather than a `(bool, String)` pair, whose `String` would be
    /// meaningless in half its states. This is the *credential* half of what
    /// `/ready` reports — that endpoint additionally probes the database, so a
    /// green status page with a 503 from `/ready` means the database, not the
    /// configuration.
    pub async fn blocked_reason(&self) -> Option<String> {
        self.syncer.read().await.as_ref().err().cloned()
    }

    /// The cached syncer, or the reason there is not one.
    ///
    /// An accessor rather than a public field so that `/ready` — which lives with
    /// the other HTTP plumbing in [`crate::commands::serve`] — can ask without this
    /// module having to expose its locks.
    pub async fn syncer(&self) -> Result<Arc<Syncer>, String> {
        match &*self.syncer.read().await {
            Ok(syncer) => Ok(Arc::clone(syncer)),
            Err(reason) => Err(reason.clone()),
        }
    }

    /// Adopt a freshly written configuration and force the syncer to be rebuilt.
    ///
    /// Clears any recorded load error: the caller only has a `Config` because
    /// `settings::validate` accepted the document it just wrote, so whatever was
    /// wrong with the file on disk no longer is.
    pub async fn replace_config(&self, config: Config) {
        // Only the static half is refreshed: the poller's observed endpoints are
        // still true regardless of what the operator just typed.
        self.endpoint
            .set_static(match sharerr_core::endpoint::advertised_base(
                &config.tracker,
                config.server.bind.port(),
            ) {
                Ok(base) => base,
                Err(err) => {
                    tracing::warn!(%err, "the saved advertised address is unusable");
                    None
                }
            });
        *self.config.write().await = config;
        *self.config_error.write().await = None;
        self.invalidate("configuration changed").await;
    }

    /// Drop the cached syncer so the recovery loop builds a new one.
    ///
    /// Every credential and settings write calls this. Without it a *changed*
    /// credential would never take effect: `ensure_ready` caches the first syncer
    /// that builds successfully and would otherwise keep using the old values until
    /// someone restarted the container.
    pub async fn invalidate(&self, reason: &str) {
        tracing::info!(reason, "rebuilding the syncer");
        *self.syncer.write().await = Err(format!("reloading — {reason}"));
        // These are vault values too, so a credential change has to drop them or
        // the tracker and the feed keep enforcing the old ones.
        *self.tracker_token.write().await = None;
        *self.torznab_shared_key.write().await = None;
        // Back to the fast retry. Someone has just changed something, so the next
        // attempt is a new question — making them wait out a backoff earned by the
        // *previous* configuration would be the opposite of what they expect.
        *self.recovery_failures.write().await = 0;
        self.wake.notify_one();
    }

    /// How long to wait before the next attempt to build the syncer.
    ///
    /// Doubles per consecutive failure from [`RECOVERY_INTERVAL`], capped at
    /// [`RECOVERY_INTERVAL_MAX`]. A flat 15s retry meant an instance with a master
    /// key but no usable credentials re-ran `Syncer::build` — and with it an Argon2
    /// derivation of ~19 MiB and tens of milliseconds — 5,760 times a day, forever,
    /// on a runtime worker.
    ///
    /// Backing off costs nothing in responsiveness because [`Self::invalidate`]
    /// both resets the counter and wakes the loop, so the moment an operator
    /// actually changes something the next attempt is immediate.
    pub async fn recovery_delay(&self) -> Duration {
        let failures = *self.recovery_failures.read().await;
        // Saturating, and capped well below the point where the shift itself could
        // overflow.
        let factor = 1_u32.checked_shl(failures.min(16)).unwrap_or(u32::MAX);
        RECOVERY_INTERVAL
            .saturating_mul(factor)
            .min(RECOVERY_INTERVAL_MAX)
    }

    /// Sleep for `interval`, or until [`Self::invalidate`] cuts it short.
    ///
    /// The timer is still the floor — a `Syncer::build` that failed because
    /// qBittorrent is down has nothing to wait on, since the service will not
    /// announce its return — but a settings write no longer has to wait for it.
    pub async fn sleep_or_wake(&self, interval: Duration) {
        tokio::select! {
            () = tokio::time::sleep(interval) => {}
            () = self.wake.notified() => {}
        }
    }

    /// The syncer, building it on first success and caching it until invalidated.
    ///
    /// Retried while absent — see [`Self::recovery_delay`] for the schedule. That
    /// repetition is the whole mechanism: [`Syncer::build`] re-reads the vault file
    /// on each attempt, so a credential written through the UI is picked up without
    /// a restart.
    pub async fn ensure_ready(&self) -> Option<Arc<Syncer>> {
        if let Ok(syncer) = &*self.syncer.read().await {
            return Some(Arc::clone(syncer));
        }

        // Short-circuit rather than letting `Syncer::build` run against the salvage.
        // It would fail anyway, but with "neither sonarr nor radarr is configured",
        // which sends the operator looking for a missing URL instead of the typo
        // that actually stopped the file loading.
        if let Some(reason) = self.config_error().await {
            *self.syncer.write().await = Err(format!("{}: {reason}", self.config_path.display()));
            *self.recovery_failures.write().await += 1;
            return None;
        }

        let config = self.config().await;
        match Syncer::build(&config, self.endpoint()).await {
            Ok(syncer) => {
                let syncer = Arc::new(syncer);
                tracing::info!("credentials accepted; reconciliation is available");
                *self.syncer.write().await = Ok(Arc::clone(&syncer));
                *self.recovery_failures.write().await = 0;
                Some(syncer)
            }
            Err(err) => {
                // `warn`, not `error`: an unpopulated vault is the expected state of
                // a container on its very first start, not a malfunction.
                let reason = format!("{err:#}");
                *self.recovery_failures.write().await += 1;
                tracing::warn!(
                    reason,
                    retry_secs = self.recovery_delay().await.as_secs(),
                    "cannot reconcile yet — fix this and it will be picked up without a restart"
                );
                *self.syncer.write().await = Err(reason);
                None
            }
        }
    }
}

/// The endpoint as configuration alone resolves it, with an unusable address
/// treated as unset rather than fatal — `serve` must come up either way, and the
/// tracker provider reports the absence with the sentence that names the fix.
fn static_endpoint(config: &Config) -> AdvertisedEndpoint {
    let base =
        match sharerr_core::endpoint::advertised_base(&config.tracker, config.server.bind.port()) {
            Ok(base) => base,
            Err(err) => {
                tracing::warn!(%err, "the configured advertised address is unusable");
                None
            }
        };
    AdvertisedEndpoint::new(base)
}

#[cfg(test)]
pub(crate) mod fixtures {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// A config pointing at an empty directory: no vault, no credentials. This is a
    /// fresh container's first start, and it is independent of whether a master key
    /// happens to be set in the test process — with an empty data dir the build
    /// fails either way.
    /// The `TempDir` is returned because it must outlive the test — dropping it
    /// deletes the directory the config points at.
    pub(crate) fn unconfigured() -> (tempfile::TempDir, Arc<ServeState>) {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            data_dir: dir.path().to_path_buf(),
            ..Config::default()
        };
        let path = dir.path().join("sharerr.toml");
        (dir, Arc::new(ServeState::new(config, path, None)))
    }

    /// The same fresh container, except its `sharerr.toml` did not load at all.
    pub(crate) fn unloadable() -> (tempfile::TempDir, Arc<ServeState>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sharerr.toml");
        let state = ServeState::new(
            Config::default(),
            path,
            Some("invalid key `taag`".to_owned()),
        );
        (dir, Arc::new(state))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::fixtures::{unconfigured, unloadable};
    use super::*;

    #[tokio::test]
    async fn an_empty_vault_leaves_the_instance_blocked_rather_than_failing() {
        let (_dir, state) = unconfigured();

        assert!(state.ensure_ready().await.is_none());
        assert!(state.syncer.read().await.is_err());
    }

    /// A file that would not load must be *named* as the obstacle. Letting
    /// `Syncer::build` run against the salvaged config instead reports "neither
    /// sonarr nor radarr is configured", which is true of the defaults and sends
    /// the operator hunting for a missing URL rather than the typo.
    #[tokio::test]
    async fn an_unloadable_config_is_the_reported_reason() {
        let (_dir, state) = unloadable();

        assert!(state.ensure_ready().await.is_none());
        let reason = state.blocked_reason().await.expect("must be blocked");
        assert!(reason.contains("taag"), "got {reason:?}");
        assert!(
            !reason.contains("nothing to share"),
            "must not relay Syncer::build's misleading reason: {reason:?}"
        );
    }

    /// A successful save is proof the file parses — `settings::validate` produced
    /// the `Config` being adopted — so the banner has to clear itself. Leaving it
    /// set would tell an operator who just fixed the file that it is still broken.
    #[tokio::test]
    async fn a_successful_save_clears_the_config_error() {
        let (_dir, state) = unloadable();

        state.replace_config(Config::default()).await;

        assert_eq!(state.config_error().await, None);
    }

    /// The regression the web UI depends on. `ensure_ready` caches the first
    /// syncer that builds, so without invalidation a credential *changed* through
    /// the UI would keep using the old value until someone restarted the
    /// container — and the symptom (auth failures against a key you can see is
    /// correct on screen) is deeply confusing.
    ///
    /// Driven from the absent side because building a real syncer needs a live
    /// Sonarr and qBittorrent; what is being asserted is that the cache is cleared
    /// and the reason is replaced, which is the whole of the mechanism.
    #[tokio::test]
    async fn invalidate_clears_the_cached_syncer_and_says_why() {
        let (_dir, state) = unconfigured();
        // Stand in for a syncer that built successfully at some earlier point.
        *state.syncer.write().await = Err("some earlier reason".to_owned());

        state.invalidate("credentials changed").await;

        let reason = match &*state.syncer.read().await {
            Err(reason) => reason.clone(),
            Ok(_) => panic!("invalidate must leave the syncer absent"),
        };
        assert!(reason.contains("credentials changed"), "got {reason:?}");
    }

    /// The wait a user actually experiences after saving a credential.
    ///
    /// Before this, `invalidate` only marked the syncer stale and the background
    /// loop noticed whenever its current sleep expired — which on a *working*
    /// instance is `sync.interval_secs`, 15 minutes by default, while the status
    /// page promised fifteen seconds. The sleep here is an hour: if it is not cut
    /// short, the test hangs rather than passing slowly.
    #[tokio::test]
    async fn invalidating_cuts_short_a_long_sleep() {
        let (_dir, state) = unconfigured();

        let sleeper = {
            let state = Arc::clone(&state);
            tokio::spawn(async move { state.sleep_or_wake(Duration::from_secs(3600)).await })
        };

        // Yield so the sleeper is parked on `notified()` before the permit lands —
        // though `notify_one` stores one either way, which is what makes an
        // invalidation racing a sync impossible to lose.
        tokio::task::yield_now().await;
        state.invalidate("credentials changed").await;

        tokio::time::timeout(Duration::from_secs(5), sleeper)
            .await
            .expect("invalidate must wake the loop, not leave it sleeping")
            .expect("the sleeping task should not panic");
    }

    /// Replacing the config must both take effect and force a rebuild — swapping
    /// one without the other leaves the running syncer wired to the old settings.
    #[tokio::test]
    async fn replacing_the_config_swaps_it_and_invalidates() {
        let (dir, state) = unconfigured();

        let updated = Config {
            data_dir: dir.path().to_path_buf(),
            tag: "renamed".to_owned(),
            ..Config::default()
        };
        state.replace_config(updated).await;

        assert_eq!(state.config().await.tag, "renamed");
        assert!(state.syncer.read().await.is_err());
    }

    /// Login has to work before any credential exists, so the store must open
    /// without the syncer — and stay the same handle once opened.
    #[tokio::test]
    async fn the_store_opens_independently_of_the_syncer() {
        let (_dir, state) = unconfigured();

        state
            .store()
            .await
            .expect("store opens with an empty vault");
        assert!(
            state.syncer.read().await.is_err(),
            "the syncer is still unbuildable, which is the point"
        );
        state.store().await.expect("a second call reuses the pool");
    }

    /// The first retry must still be the fifteen seconds the status page promises;
    /// backing off is only allowed to affect *subsequent* attempts.
    #[tokio::test]
    async fn the_first_retry_is_the_advertised_interval() {
        let (_dir, state) = unconfigured();

        assert_eq!(state.recovery_delay().await, RECOVERY_INTERVAL);
    }

    /// The point of the backoff: a permanently blocked instance must stop
    /// re-deriving its Argon2 vault key every fifteen seconds forever.
    #[tokio::test]
    async fn repeated_failures_back_off_and_then_settle_at_the_cap() {
        let (_dir, state) = unloadable();

        // Two failed attempts: 15s, then 30s.
        state.ensure_ready().await;
        assert_eq!(state.recovery_delay().await, Duration::from_secs(30));
        state.ensure_ready().await;
        assert_eq!(state.recovery_delay().await, Duration::from_secs(60));

        for _ in 0..20 {
            state.ensure_ready().await;
        }
        assert_eq!(
            state.recovery_delay().await,
            RECOVERY_INTERVAL_MAX,
            "the delay must be capped, and must not overflow on the way there"
        );
    }

    /// Backoff must never make an operator wait for a change they just made.
    #[tokio::test]
    async fn invalidating_resets_the_backoff() {
        let (_dir, state) = unloadable();

        for _ in 0..5 {
            state.ensure_ready().await;
        }
        assert!(state.recovery_delay().await > RECOVERY_INTERVAL);

        state.invalidate("credentials changed").await;

        assert_eq!(state.recovery_delay().await, RECOVERY_INTERVAL);
    }
}
