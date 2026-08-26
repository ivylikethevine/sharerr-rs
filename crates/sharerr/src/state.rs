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

use secrecy::{ExposeSecret, SecretString};
use sharerr_core::Config;
use sharerr_core::config::secret_keys;
use sharerr_core::endpoint::AdvertisedEndpoint;
use sharerr_store::{Store, Vault};
use tokio::sync::{Notify, RwLock};

use crate::gluetun::{GluetunStatus, GluetunTarget};
use crate::notify::QuietNotified;
use crate::sync::Syncer;
use crate::system_stats::SystemStatus;
use crate::tracker::LegacyTokenStatus;

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
    /// Serialises every read-modify-write of `sharerr.toml` through the web
    /// UI. The per-field `RwLock`s above protect the in-memory `Config`; this
    /// protects the *file*: two section saves (two tabs, wizard plus
    /// settings, an htmx double-submit) each open the document, apply only
    /// their own edits, and rename over each other — the loser's edits vanish
    /// and the last `replace_config` installs a `Config` missing them.
    config_write: tokio::sync::Mutex<()>,
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
    /// The tracker token a rotation just replaced, cached the same way and
    /// for the same reason as `tracker_token` — see
    /// `crate::web::settings::rotate_tracker_token`.
    tracker_token_previous: RwLock<Option<Option<String>>>,
    /// This instance's gossip signing identity, cached for the same reason as
    /// `tracker_token` — loading it means opening the vault, and the pull side of
    /// gossip is asked on every friend's poll.
    gossip_identity: RwLock<Option<Option<Arc<crate::gossip::Identity>>>>,
    /// The embedded lighthouse's state, built lazily the first time
    /// `[lighthouse] enabled = true` is actually observed — see
    /// [`Self::lighthouse_state`]. Same `Option<Option<_>>` shape as
    /// `gossip_identity`: outer `None` is "not looked up yet".
    lighthouse_state: RwLock<Option<Option<Arc<sharerr_lighthouse::LighthouseState>>>>,
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
    /// The tracker-facing gluetun poller's slot. Its endpoint is the live
    /// externally reachable address a friend reaches this instance on — one
    /// value for the whole process: the syncer's tracker provider reads it,
    /// the poller updates it, and a settings save refreshes only its static
    /// half, so the poller's observations survive both a config change and a
    /// syncer rebuild.
    tracker: GluetunSlot,
    /// The client-facing poller's slot. Its endpoint is the live address of
    /// the torrent *client* — separate from the tracker's because the two can
    /// sit behind independent tunnels (see `docker/deploy/dual-vpn/`) that
    /// rotate on unrelated schedules. Populated only by the `[gluetun_client]`
    /// poller; there is no static configuration for it, since nothing else in
    /// `sharerr.toml` describes the torrent client's own reachable address.
    /// Read today by gossip's self-record.
    client: GluetunSlot,
    /// What the lighthouse poller last reported and looked up. Lighthouse was
    /// the one background subsystem with no observability at all — a refused
    /// report means quiet friends cannot find this instance, and the only
    /// trace of it was a log line every fifteen minutes.
    lighthouse_status: Arc<crate::lighthouse_client::LighthouseStatus>,
    /// When the previous shared tracker token was last actually used — see
    /// [`LegacyTokenStatus`].
    legacy_token_status: Arc<LegacyTokenStatus>,
    /// The tracker's live swarms. Owned here rather than by the tracker router
    /// because two consumers need one copy: however many listeners carry
    /// `/announce`, and the status page's "n peers connected" line.
    swarms: Arc<sharerr_torrent::Swarms>,
    /// Dedupe for the peer-quiet notification — see [`crate::notify`].
    quiet_notified: Arc<QuietNotified>,
    /// What the CPU/memory/disk sampler last measured — see
    /// [`crate::system_stats`].
    system_status: Arc<SystemStatus>,
}

/// Everything one gluetun poller reads and writes, one per [`GluetunTarget`].
///
/// The two pollers are independent — see the `tracker`/`client` fields on
/// [`ServeState`] — and differ only in which slot they are handed, so every
/// accessor dispatches through [`ServeState::slot`] rather than matching on
/// the target itself.
#[derive(Debug)]
struct GluetunSlot {
    /// The live externally reachable address this poller keeps in step.
    endpoint: Arc<AdvertisedEndpoint>,
    /// Raised by the gluetun push endpoint so the poller re-asks its control
    /// server *now* instead of at the next tick — the "reacted to in seconds"
    /// half of the endpoint story.
    refresh: Notify,
    /// What the poller last saw and last failed with — the "when did gluetun
    /// last actually tell sharerr something" the Diagnostics page answers.
    status: Arc<GluetunStatus>,
    /// The poller's control-server API key, cached like `tracker_token` and
    /// for a sharper version of the same reason: the poller re-read it every
    /// interval, so a default 60-second poll paid 1,440 Argon2 derivations a
    /// day, and both pollers run. Cleared by [`ServeState::invalidate`], so a
    /// key saved through Settings still takes effect on the next pass without
    /// a restart.
    api_key: RwLock<Option<Option<SecretString>>>,
}

impl GluetunSlot {
    fn new(static_base: Option<url::Url>) -> Self {
        Self {
            endpoint: Arc::new(AdvertisedEndpoint::new(static_base)),
            refresh: Notify::new(),
            status: Arc::new(GluetunStatus::default()),
            api_key: RwLock::new(None),
        }
    }
}

impl ServeState {
    pub fn new(
        config: Config,
        config_path: impl Into<PathBuf>,
        config_error: Option<String>,
    ) -> Self {
        // Only the tracker-facing endpoint has a static half: nothing in
        // `sharerr.toml` describes the torrent client's own address.
        let tracker = GluetunSlot::new(static_base(&config));
        Self {
            config: RwLock::new(config),
            config_path: config_path.into(),
            config_error: RwLock::new(config_error),
            config_write: tokio::sync::Mutex::new(()),
            store: RwLock::new(None),
            // Replaced by the first `ensure_ready`, which the background loop runs
            // the moment `run` starts polling it. Only observable in the sliver
            // between binding the listener and that first attempt finishing.
            syncer: RwLock::new(Err("still starting up".to_owned())),
            recovery_failures: RwLock::new(0),
            tracker_token: RwLock::new(None),
            tracker_token_previous: RwLock::new(None),
            gossip_identity: RwLock::new(None),
            lighthouse_state: RwLock::new(None),
            wake: Notify::new(),
            tracker,
            client: GluetunSlot::new(None),
            lighthouse_status: Arc::new(crate::lighthouse_client::LighthouseStatus::default()),
            legacy_token_status: Arc::new(LegacyTokenStatus::default()),
            swarms: Arc::new(sharerr_torrent::Swarms::default()),
            quiet_notified: Arc::new(QuietNotified::default()),
            system_status: Arc::new(SystemStatus::default()),
        }
    }

    /// The tracker's live swarms — one copy for every listener and the status
    /// page alike.
    pub fn swarms(&self) -> Arc<sharerr_torrent::Swarms> {
        Arc::clone(&self.swarms)
    }

    /// Dedupe state for the peer-quiet notification.
    pub fn quiet_notified(&self) -> Arc<QuietNotified> {
        Arc::clone(&self.quiet_notified)
    }

    /// What the CPU/memory/disk sampler last measured.
    pub fn system_status(&self) -> Arc<SystemStatus> {
        Arc::clone(&self.system_status)
    }

    /// The live advertised endpoint this whole process shares — where friends
    /// reach the tracker and the feed.
    pub fn endpoint(&self) -> Arc<AdvertisedEndpoint> {
        self.endpoint_for(GluetunTarget::Tracker)
    }

    /// The slot `target`'s poller reads and writes.
    fn slot(&self, target: GluetunTarget) -> &GluetunSlot {
        match target {
            GluetunTarget::Tracker => &self.tracker,
            GluetunTarget::Client => &self.client,
        }
    }

    /// The base URL clients should fetch `.torrent` files and feed endpoints
    /// from right now.
    ///
    /// Unlike [`Config::public_base_url`], which only ever knows the
    /// statically configured address, this reflects gluetun's live
    /// resolution the same way the magnet tiers a torznab response carries
    /// already do via `endpoint().recent()` — the feed and the tracker share
    /// one advertised address, so a `.torrent` download link must track it
    /// too. Falls back to the static address, then to
    /// `http://localhost:<bind port>`, in the same order
    /// `Config::public_base_url` does.
    pub async fn public_base_url(&self) -> String {
        match self.endpoint().current() {
            Some(base) => sharerr_core::endpoint::base_string(&base),
            // With no live or static address, `Config::public_base_url` can
            // only land on its own localhost fallback — reused rather than
            // spelled a second time here.
            None => self.with_config(Config::public_base_url).await,
        }
    }

    /// The live advertised address of the torrent client — see the field
    /// comment on `client`.
    pub fn client_endpoint(&self) -> Arc<AdvertisedEndpoint> {
        self.endpoint_for(GluetunTarget::Client)
    }

    /// The endpoint gluetun keeps in step for `target`.
    pub fn endpoint_for(&self, target: GluetunTarget) -> Arc<AdvertisedEndpoint> {
        Arc::clone(&self.slot(target).endpoint)
    }

    /// What `target`'s poller last saw and last failed with.
    pub fn gluetun_status(&self, target: GluetunTarget) -> Arc<GluetunStatus> {
        Arc::clone(&self.slot(target).status)
    }

    /// What the lighthouse poller last reported and looked up.
    pub fn lighthouse_status(&self) -> Arc<crate::lighthouse_client::LighthouseStatus> {
        Arc::clone(&self.lighthouse_status)
    }

    /// Ask the background loop to run a pass soon, without invalidating the
    /// syncer. Used when something the syncer reads *through a shared handle*
    /// changed — the advertised endpoint above — so a rebuild would be waste.
    pub fn request_sync(&self) {
        self.wake.notify_one();
    }

    /// Ask `target`'s gluetun poller to re-resolve its endpoint now.
    pub fn nudge_endpoint(&self, target: GluetunTarget) {
        self.slot(target).refresh.notify_one();
    }

    /// Park until [`Self::nudge_endpoint`] is called for `target`.
    pub async fn endpoint_refresh_requested(&self, target: GluetunTarget) {
        self.slot(target).refresh.notified().await;
    }

    /// A snapshot of the current configuration.
    pub async fn config(&self) -> Config {
        self.config.read().await.clone()
    }

    /// Read one thing out of the configuration under the lock, for a caller
    /// that would otherwise clone the whole `Config` to look at a field.
    ///
    /// `f` runs synchronously under the read guard, so it must not block or
    /// `.await` — the point is to be *cheaper* than [`Self::config`], not to
    /// hold the lock across anything a settings write could be waiting on.
    pub async fn with_config<T>(&self, f: impl FnOnce(&Config) -> T) -> T {
        f(&*self.config.read().await)
    }

    /// Where the `.torrent` files live — [`Config::torrent_dir`] without the
    /// whole-`Config` clone.
    pub async fn torrent_dir(&self) -> PathBuf {
        self.with_config(Config::torrent_dir).await
    }

    /// Where the database, vault, and `.torrent` cache all live — what the
    /// system sampler measures disk usage against, since it is the one path
    /// an operator running out of room here would actually feel.
    pub async fn data_dir(&self) -> PathBuf {
        self.with_config(|c| c.data_dir.clone()).await
    }

    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// Why `sharerr.toml` could not be loaded, or `None` when it was.
    /// Hold this across an open → edit → save of `sharerr.toml` — see the
    /// field's own doc comment.
    pub async fn lock_config_write(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.config_write.lock().await
    }

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
        // Just the path, not a clone of the whole `Config` — this is called on
        // every cache miss and every settings save.
        let path = self.config.read().await.vault_path();
        crate::secrets::open_vault_at(path)
            .await
            .map_err(|err| err.to_string())
    }

    /// Read `cache`, or fill it by opening the vault and deriving a value with
    /// `load`, caching the result.
    ///
    /// The shape shared by [`Self::tracker_token`], [`Self::gossip_identity`],
    /// and [`Self::lighthouse_state`]: each differs only in which lock it
    /// touches and how it derives a value from an opened vault. The outer
    /// `None` means "not looked up yet", `Some(None)` means "looked up, and
    /// there is none".
    ///
    /// A vault that will not open is `Err` and is **not** cached: the next
    /// call tries again. Caching it as "there is none" looked correct — with
    /// no vault there is nothing to read — but for the tracker it turned a
    /// transient error (a master-key file briefly unreadable during a mount
    /// or rotation) into token enforcement silently switched off until the
    /// next settings save. The cost of not caching is one Argon2 derivation
    /// per call for as long as the vault stays broken, which is the right
    /// place to spend it.
    async fn try_cached_from_vault<T: Clone>(
        &self,
        cache: &RwLock<Option<Option<T>>>,
        load: impl FnOnce(Vault) -> Option<T>,
    ) -> Result<Option<T>, String> {
        if let Some(cached) = &*cache.read().await {
            return Ok(cached.clone());
        }

        // The write guard is held across the open, so concurrent misses queue
        // behind one Argon2 derivation instead of each paying for their own;
        // whoever wins re-checks, since a loser wakes to a filled cache.
        let mut slot = cache.write().await;
        if let Some(cached) = &*slot {
            return Ok(cached.clone());
        }
        let value = load(self.open_vault().await?);
        *slot = Some(value.clone());
        Ok(value)
    }

    /// [`Self::try_cached_from_vault`] for a plain string secret under `key`.
    async fn try_vault_string(
        &self,
        cache: &RwLock<Option<Option<String>>>,
        key: &'static str,
    ) -> Result<Option<String>, String> {
        self.try_cached_from_vault(cache, |vault| vault_string(&vault, key))
            .await
    }

    /// [`Self::try_cached_from_vault`] for the accessors where an unopenable
    /// vault genuinely means "absent" — a gossip identity or lighthouse seed
    /// that cannot be loaded is simply not there yet — rather than a reason
    /// to refuse anything.
    async fn cached_from_vault<T: Clone>(
        &self,
        cache: &RwLock<Option<Option<T>>>,
        load: impl FnOnce(Vault) -> Option<T>,
    ) -> Option<T> {
        self.try_cached_from_vault(cache, load)
            .await
            .unwrap_or_default()
    }

    /// The token the builtin tracker requires in announce URLs, if any.
    ///
    /// `None` when there is none *or* the vault could not be opened — fine
    /// for rendering ("is a token configured?"), not for admission: the
    /// tracker goes through [`Self::tracker_tokens`], which keeps the two
    /// apart.
    pub async fn tracker_token(&self) -> Option<String> {
        self.try_tracker_token().await.unwrap_or_default()
    }

    async fn try_tracker_token(&self) -> Result<Option<String>, String> {
        self.try_vault_string(&self.tracker_token, secret_keys::TRACKER_TOKEN)
            .await
    }

    /// Both announce tokens — `(current, previous)` — for the tracker's
    /// admission check, or `Err` when the vault could not be opened so the
    /// tracker can fail *closed*, the same way it already does for a store
    /// that will not open. `Ok((None, _))` is the genuine "no token
    /// required" answer and only ever comes from an opened vault.
    ///
    /// A cold miss fills both caches from one vault open rather than two —
    /// this is asked on every announce, and the miss is what every peer's
    /// first announce after a settings save hits at once.
    pub async fn tracker_tokens(&self) -> Result<(Option<String>, Option<String>), String> {
        if let (Some(current), Some(previous)) = (
            &*self.tracker_token.read().await,
            &*self.tracker_token_previous.read().await,
        ) {
            return Ok((current.clone(), previous.clone()));
        }

        // Both write guards held across the open, in the same order as the
        // reads above, so concurrent misses queue behind one derivation —
        // same reasoning as `try_cached_from_vault`.
        let mut current = self.tracker_token.write().await;
        let mut previous = self.tracker_token_previous.write().await;
        if let (Some(current), Some(previous)) = (&*current, &*previous) {
            return Ok((current.clone(), previous.clone()));
        }
        let vault = self.open_vault().await?;
        let tokens = (
            vault_string(&vault, secret_keys::TRACKER_TOKEN),
            vault_string(&vault, secret_keys::TRACKER_TOKEN_PREVIOUS),
        );
        *current = Some(tokens.0.clone());
        *previous = Some(tokens.1.clone());
        Ok(tokens)
    }

    /// The token a rotation just replaced, still accepted alongside
    /// [`Self::tracker_token`] until the operator finalizes it away — see
    /// [`crate::tracker::authenticate_token`].
    #[cfg(test)]
    pub async fn tracker_token_previous(&self) -> Option<String> {
        self.try_tracker_token_previous().await.unwrap_or_default()
    }

    #[cfg(test)]
    async fn try_tracker_token_previous(&self) -> Result<Option<String>, String> {
        self.try_vault_string(
            &self.tracker_token_previous,
            secret_keys::TRACKER_TOKEN_PREVIOUS,
        )
        .await
    }

    /// When the previous shared tracker token was last actually used.
    pub fn legacy_token_status(&self) -> Arc<LegacyTokenStatus> {
        Arc::clone(&self.legacy_token_status)
    }

    /// A gluetun poller's control-server API key, cached after the first read.
    ///
    /// Same shape and same reasoning as [`Self::tracker_token`]: the outer
    /// `None` means "not looked up yet", `Some(None)` means "looked up, and
    /// there is none". The poller calls this every interval, so without the
    /// cache each call was an Argon2 derivation on a timer.
    pub async fn gluetun_api_key(&self, target: GluetunTarget) -> Option<SecretString> {
        self.cached_from_vault(&self.slot(target).api_key, |vault| {
            vault.get(target.api_key_secret()).ok().flatten()
        })
        .await
    }

    /// This instance's gossip signing identity, cached after the first load.
    ///
    /// Loading means opening the vault — an Argon2 derivation — and
    /// `self_record` is asked on every pull request a friend makes.
    pub async fn gossip_identity(&self) -> Option<Arc<crate::gossip::Identity>> {
        self.cached_from_vault(&self.gossip_identity, |mut vault| {
            crate::gossip::Identity::load_or_create(&mut vault)
                .ok()
                .map(Arc::new)
        })
        .await
    }

    /// The embedded lighthouse's state, built on first use and cached
    /// thereafter — same reasoning as [`Self::gossip_identity`]: building it
    /// means opening the vault to load or mint the decoy seed, and `lookup`
    /// is answered on every friend's probe.
    ///
    /// `None` when `[lighthouse] enabled` is false, or when the vault cannot
    /// be opened — an unconfigured instance still binds and serves
    /// everything else, so a lighthouse that cannot get its seed yet is
    /// simply absent rather than a startup failure.
    pub async fn lighthouse_state(&self) -> Option<Arc<sharerr_lighthouse::LighthouseState>> {
        if !self.config.read().await.lighthouse.enabled {
            return None;
        }
        self.cached_from_vault(&self.lighthouse_state, |mut vault| {
            crate::secrets::load_or_create_seed(
                &mut vault,
                secret_keys::LIGHTHOUSE_DECOY_SEED,
                "lighthouse decoy seed",
            )
            .ok()
            .map(|(seed, _minted)| Arc::new(sharerr_lighthouse::LighthouseState::new(seed)))
        })
        .await
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
        self.tracker.endpoint.set_static(static_base(&config));
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
        *self.tracker_token_previous.write().await = None;
        *self.gossip_identity.write().await = None;
        *self.tracker.api_key.write().await = None;
        *self.client.api_key.write().await = None;
        // `lighthouse_state` is not cleared here: its only vault input is the
        // decoy seed, which is minted once and never written by any settings
        // page, so there is no newer value for a rebuild to pick up.
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

/// The advertised base as configuration alone resolves it, with an unusable
/// address treated as unset rather than fatal — `serve` must come up either way,
/// and the tracker provider reports the absence with the sentence that names the
/// fix.
///
/// One function for both the startup read and the post-save refresh, so this
/// match is not duplicated for a value this load-bearing.
fn static_base(config: &Config) -> Option<url::Url> {
    match sharerr_core::endpoint::advertised_base(&config.tracker, config.server.bind.port()) {
        Ok(base) => base,
        Err(err) => {
            tracing::warn!(%err, "the advertised address is unusable");
            None
        }
    }
}

/// A plain string secret under `key`, or `None` when it is absent or the
/// vault could not read it.
fn vault_string(vault: &Vault, key: &str) -> Option<String> {
    vault
        .get(key)
        .ok()
        .flatten()
        .map(|secret| secret.expose_secret().to_owned())
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
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::result_large_err)]

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
    /// Without this cutoff, `invalidate` only marks the syncer stale and the
    /// background loop notices whenever its current sleep expires — which on a
    /// *working* instance is `sync.interval_secs`, 15 minutes by default, though
    /// the status page promises fifteen seconds. The sleep here is an hour: if it
    /// is not cut short, the test hangs rather than passing slowly.
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

    /// `quiet_notified` and `swarms` both hand out one shared handle for the
    /// whole process — asserting `Arc::ptr_eq` is what proves a second call
    /// reuses it rather than building a fresh, disconnected one.
    #[tokio::test]
    async fn quiet_notified_and_swarms_are_shared_handles() {
        let (_dir, state) = unconfigured();

        assert!(Arc::ptr_eq(
            &state.quiet_notified(),
            &state.quiet_notified()
        ));
        assert!(Arc::ptr_eq(&state.swarms(), &state.swarms()));
        assert!(Arc::ptr_eq(&state.system_status(), &state.system_status()));
    }

    /// `endpoint_for`/`gluetun_status` must route `Client` to the client-facing
    /// handle, not silently alias the tracker-facing one — the whole reason
    /// the two are separate fields.
    #[tokio::test]
    async fn endpoint_for_and_gluetun_status_route_by_target() {
        let (_dir, state) = unconfigured();

        assert!(Arc::ptr_eq(
            &state.endpoint_for(GluetunTarget::Tracker),
            &state.endpoint()
        ));
        assert!(Arc::ptr_eq(
            &state.endpoint_for(GluetunTarget::Client),
            &state.client_endpoint()
        ));
        assert!(!Arc::ptr_eq(
            &state.endpoint_for(GluetunTarget::Tracker),
            &state.endpoint_for(GluetunTarget::Client)
        ));

        assert!(Arc::ptr_eq(
            &state.gluetun_status(GluetunTarget::Client),
            &state.gluetun_status(GluetunTarget::Client)
        ));
        assert!(!Arc::ptr_eq(
            &state.gluetun_status(GluetunTarget::Tracker),
            &state.gluetun_status(GluetunTarget::Client)
        ));
    }

    /// `request_sync` must not panic when nothing is parked on the wake
    /// notify yet — `notify_one` storing a permit for the next waiter is the
    /// whole point, and calling it with no waiter present is the common case.
    #[tokio::test]
    async fn request_sync_is_harmless_with_no_waiter() {
        let (_dir, state) = unconfigured();
        state.request_sync();
    }

    /// `nudge_endpoint`/`endpoint_refresh_requested` must route by target too
    /// — a tracker-facing push must not wake the client-facing poller.
    #[tokio::test]
    async fn nudge_endpoint_wakes_only_the_matching_target() {
        let (_dir, state) = unconfigured();

        let waiter = {
            let state = Arc::clone(&state);
            tokio::spawn(async move {
                state
                    .endpoint_refresh_requested(GluetunTarget::Client)
                    .await;
            })
        };
        tokio::task::yield_now().await;

        state.nudge_endpoint(GluetunTarget::Tracker);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            !waiter.is_finished(),
            "a tracker-facing nudge must not wake the client-facing waiter"
        );

        state.nudge_endpoint(GluetunTarget::Client);
        tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .expect("the client-facing nudge must wake it")
            .expect("the waiting task should not panic");
    }

    /// `lighthouse_state` must return `None` without ever touching the vault
    /// when the feature is disabled — the default for an unconfigured
    /// instance, and the branch that keeps a fresh container's `/health`
    /// from paying an Argon2 derivation it does not need.
    #[tokio::test]
    async fn lighthouse_state_is_none_when_disabled() {
        let (_dir, state) = unconfigured();
        assert!(!state.config().await.lighthouse.enabled);
        assert!(state.lighthouse_state().await.is_none());
    }

    /// Every `cached_from_vault`-backed accessor's *success* path needs a real
    /// open vault, which means a real `SHARERR_MASTER_KEY` — safe here (unlike
    /// a plain `std::env::set_var`) because `Jail` scopes it to this closure
    /// and serializes against every other Jail-based test in the binary. `Jail`
    /// itself is not async, hence the plain `#[test]` driving its own runtime
    /// rather than `#[tokio::test]`.
    #[test]
    fn vault_backed_accessors_succeed_and_cache_once_the_vault_opens() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("SHARERR_MASTER_KEY", "a-master-key");
            let config = Config {
                data_dir: jail.directory().to_path_buf(),
                lighthouse: sharerr_core::config::LighthouseConfig {
                    enabled: true,
                    ..Default::default()
                },
                ..Default::default()
            };
            let path = jail.directory().join("sharerr.toml");
            let state = ServeState::new(config, path, None);

            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async {
                // No token stored yet: `load` runs against a real, empty vault
                // and legitimately returns `None` — this still exercises the
                // `Ok(vault) => load(vault)` success arm, distinct from the
                // vault-unavailable `Err(_) => None` arm covered elsewhere.
                assert!(state.tracker_token().await.is_none());
                // Cached: a second call must not need the vault again. There is
                // no direct way to assert that from outside, so this only
                // guards against a changed return value, not the caching path
                // itself — `cached_from_vault`'s doc comment covers the intent.
                assert!(state.tracker_token().await.is_none());

                // Same shape, same reasoning, no rotation in progress yet.
                assert!(state.tracker_token_previous().await.is_none());
                assert!(state.tracker_token_previous().await.is_none());

                assert!(
                    state
                        .gluetun_api_key(GluetunTarget::Tracker)
                        .await
                        .is_none(),
                    "no gluetun key stored yet"
                );

                let identity = state.gossip_identity().await;
                assert!(identity.is_some(), "an identity is minted on first use");

                let lighthouse = state.lighthouse_state().await;
                assert!(
                    lighthouse.is_some(),
                    "a decoy seed is minted on first use once enabled"
                );
            });
            Ok(())
        });
    }
}
