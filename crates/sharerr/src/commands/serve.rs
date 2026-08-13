//! `sharerr serve` — the long-running mode: periodic reconciliation plus HTTP.
//!
//! One router carries everything: `/health` and `/ready` for the orchestrator, the
//! web UI ([`crate::web`]), sharerr's own tracker ([`crate::tracker`]), and the
//! Torznab feed a friend's Prowlarr indexes ([`crate::torznab`]). One process, one
//! port — whatever makes 8477 reachable makes all of it reachable.
//!
//! Serving is deliberately decoupled from being *configured*. An instance whose
//! vault has no `qbittorrent.password` in it yet still binds, still answers
//! `/health`, and reports the reason on `/ready`. The fix for that state is the
//! web UI (or `sharerr vault set` inside the running container), and a process
//! that exits during startup can never be reached by either — it just
//! restart-loops, and the operator has nowhere to type the password.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use secrecy::ExposeSecret;
use sharerr_core::Config;
use sharerr_core::config::secret_keys;
use sharerr_store::{Store, Vault, master_key_from_env};
use tokio::sync::{Notify, RwLock};

use crate::sync::Syncer;

/// How often to retry building the syncer while it cannot be built.
///
/// Much shorter than any sync interval, deliberately: this is the loop an operator
/// is actively waiting on just after typing a credential, not background work.
pub const RECOVERY_INTERVAL: Duration = Duration::from_secs(15);

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
    /// The builtin tracker's announce token, cached because it is consulted on
    /// every announce from every peer and reading it means an Argon2 derivation.
    ///
    /// `None` outer means "not looked up yet"; `Some(None)` means "looked up, and
    /// there is no token". Cleared by [`Self::invalidate`], so changing it through
    /// the UI takes effect without a restart.
    tracker_token: RwLock<Option<Option<String>>>,
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
}

impl ServeState {
    fn new(config: Config, config_path: impl Into<PathBuf>, config_error: Option<String>) -> Self {
        Self {
            config: RwLock::new(config),
            config_path: config_path.into(),
            config_error: RwLock::new(config_error),
            store: RwLock::new(None),
            // Replaced by the first `ensure_ready`, which the background loop runs
            // the moment `run` starts polling it. Only observable in the sliver
            // between binding the listener and that first attempt finishing.
            syncer: RwLock::new(Err("still starting up".to_owned())),
            tracker_token: RwLock::new(None),
            wake: Notify::new(),
        }
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

    /// Adopt a freshly written configuration and force the syncer to be rebuilt.
    ///
    /// Clears any recorded load error: the caller only has a `Config` because
    /// `settings::validate` accepted the document it just wrote, so whatever was
    /// wrong with the file on disk no longer is.
    pub async fn replace_config(&self, config: Config) {
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
        // The token is a vault value too, so a credential change has to drop it or
        // the tracker keeps enforcing the old one.
        *self.tracker_token.write().await = None;
        self.wake.notify_one();
    }

    /// Sleep for `interval`, or until [`Self::invalidate`] cuts it short.
    ///
    /// The timer is still the floor — a `Syncer::build` that failed because
    /// qBittorrent is down has nothing to wait on, since the service will not
    /// announce its return — but a settings write no longer has to wait for it.
    async fn sleep_or_wake(&self, interval: Duration) {
        tokio::select! {
            () = tokio::time::sleep(interval) => {}
            () = self.wake.notified() => {}
        }
    }

    /// The syncer, building it on first success and caching it until invalidated.
    ///
    /// Retried every [`RECOVERY_INTERVAL`] while absent. That repetition is the
    /// whole mechanism: [`Syncer::build`] re-reads the vault file on each attempt,
    /// so a credential written through the UI is picked up without a restart.
    async fn ensure_ready(&self) -> Option<Arc<Syncer>> {
        if let Ok(syncer) = &*self.syncer.read().await {
            return Some(Arc::clone(syncer));
        }

        // Short-circuit rather than letting `Syncer::build` run against the salvage.
        // It would fail anyway, but with "neither sonarr nor radarr is configured",
        // which sends the operator looking for a missing URL instead of the typo
        // that actually stopped the file loading.
        if let Some(reason) = self.config_error().await {
            *self.syncer.write().await = Err(format!("{}: {reason}", self.config_path.display()));
            return None;
        }

        let config = self.config().await;
        match Syncer::build(&config).await {
            Ok(syncer) => {
                let syncer = Arc::new(syncer);
                tracing::info!("credentials accepted; reconciliation is available");
                *self.syncer.write().await = Ok(Arc::clone(&syncer));
                Some(syncer)
            }
            Err(err) => {
                // `warn`, not `error`: an unpopulated vault is the expected state of
                // a container on its very first start, not a malfunction.
                let reason = format!("{err:#}");
                tracing::warn!(
                    reason,
                    retry_secs = RECOVERY_INTERVAL.as_secs(),
                    "cannot reconcile yet — fix this and it will be picked up without a restart"
                );
                *self.syncer.write().await = Err(reason);
                None
            }
        }
    }
}

pub async fn run(config: &Config, config_path: &Path, config_error: Option<String>) -> Result<()> {
    let state = Arc::new(ServeState::new(
        config.clone(),
        config_path,
        config_error.clone(),
    ));

    // The probes keep their own state and stay outside the web UI's auth layer.
    // `/health` in particular is what the Dockerfile's HEALTHCHECK curls, with no
    // cookie and no intention of getting one.
    let app = Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .with_state(Arc::clone(&state))
        .merge(crate::tracker::routes(Arc::clone(&state)))
        .merge(crate::torznab::routes(Arc::clone(&state)))
        .merge(crate::web::routes(Arc::clone(&state)));

    let listener = tokio::net::TcpListener::bind(config.server.bind)
        .await
        .with_context(|| format!("binding {}", config.server.bind))?;
    tracing::info!(bind = %config.server.bind, "http server listening");

    // Stated at startup rather than from inside the loop: it is true regardless of
    // whether any credentials load, and an operator reading the first few lines of
    // the log should not have to wait for a vault fix to learn it.
    if config_error.is_some() {
        tracing::warn!(
            config = %config_path.display(),
            "serving only so the configuration can be repaired — open the web UI"
        );
    } else if !config.sync.enabled {
        tracing::info!("periodic sync is disabled; serving http only");
    }

    // Run both until the *server* stops. A sync that fails — or a syncer that
    // cannot be built at all — is logged and retried rather than taking the process
    // down; the HTTP endpoint staying up is what lets an operator see and repair
    // the problem.
    // `into_make_service_with_connect_info` rather than a bare service: the
    // tracker records the address a peer actually reached us from, because a
    // client behind NAT reports a private address that no other peer can dial.
    let service = app.into_make_service_with_connect_info::<std::net::SocketAddr>();

    tokio::select! {
        result = axum::serve(listener, service) => result.context("http server failed"),
        () = background(state) => Ok(()),
    }
}

/// Keeps the syncer alive and reconciles on a timer.
///
/// Never returns, and never stops re-checking. It cannot simply build the syncer
/// once and settle into a sync loop, because a settings or credential write calls
/// [`ServeState::invalidate`] and puts the syncer back to absent — so readiness is
/// re-established every pass, not just at the start.
///
/// The retry runs even when periodic sync is disabled, because `/ready` should
/// still start telling the truth once the configuration is repaired.
async fn background(state: Arc<ServeState>) {
    loop {
        let Some(syncer) = state.ensure_ready().await else {
            // Still polling here, not purely waiting: a failed `Syncer::build` has
            // no event to hang off — the service it cannot reach will not tell us
            // when it comes back — so the retry has to be on a timer.
            state.sleep_or_wake(RECOVERY_INTERVAL).await;
            continue;
        };

        // Re-read every pass rather than once: the UI can enable sync, or change
        // the interval, without a restart.
        let sync = state.config().await.sync;
        if !sync.enabled {
            // Parking on the notify alone would be wrong even here: `ensure_ready`
            // is what makes `/ready` start telling the truth, and it should keep
            // being retried on an instance that never syncs on a timer.
            state.sleep_or_wake(RECOVERY_INTERVAL).await;
            continue;
        }

        match syncer.run(false).await {
            Ok(report) => tracing::info!(%report, "sync complete"),
            Err(err) => tracing::error!(error = format!("{err:#}"), "sync failed"),
        }

        // Sleeping after the pass rather than on a fixed schedule, so a slow sync is
        // never followed by a burst of catch-up runs.
        state
            .sleep_or_wake(Duration::from_secs(sync.interval_secs.max(60)))
            .await;
    }
}

/// Liveness, not correctness — this answers "should this container be restarted?",
/// and the answer is no even when the vault is empty, because a restart cannot
/// populate it. The Dockerfile's HEALTHCHECK is wired here, so anything conditional
/// in this handler turns a fixable configuration gap into a restart loop.
async fn health() -> &'static str {
    "ok"
}

/// Readiness covers the three things that stop this instance doing work: a config
/// file it could not load, credentials it could not load, and its own database. The
/// *arr apps and qBittorrent being down is a `doctor` question, not a reason to pull
/// this instance out of service.
async fn ready(State(state): State<Arc<ServeState>>) -> (StatusCode, String) {
    // Reported ahead of the syncer's reason, which would otherwise relay the same
    // string under the less specific "not configured" heading.
    if let Some(reason) = state.config_error().await {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("configuration invalid: {reason}"),
        );
    }

    let syncer = match &*state.syncer.read().await {
        Ok(syncer) => Arc::clone(syncer),
        Err(reason) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("not configured: {reason}"),
            );
        }
    };

    match syncer.store().recent_runs(1).await {
        Ok(_) => (StatusCode::OK, "ready".to_owned()),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable".to_owned(),
        ),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// A config pointing at an empty directory: no vault, no credentials. This is a
    /// fresh container's first start, and it is independent of whether a master key
    /// happens to be set in the test process — with an empty data dir the build
    /// fails either way.
    /// The `TempDir` is returned because it must outlive the test — dropping it
    /// deletes the directory the config points at.
    fn unconfigured() -> (tempfile::TempDir, Arc<ServeState>) {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            data_dir: dir.path().to_path_buf(),
            ..Config::default()
        };
        let path = dir.path().join("sharerr.toml");
        (dir, Arc::new(ServeState::new(config, path, None)))
    }

    /// The same fresh container, except its `sharerr.toml` did not load at all.
    fn unloadable() -> (tempfile::TempDir, Arc<ServeState>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sharerr.toml");
        let state = ServeState::new(
            Config::default(),
            path,
            Some("invalid key `taag`".to_owned()),
        );
        (dir, Arc::new(state))
    }

    #[tokio::test]
    async fn an_empty_vault_leaves_the_instance_blocked_rather_than_failing() {
        let (_dir, state) = unconfigured();

        assert!(state.ensure_ready().await.is_none());
        assert!(state.syncer.read().await.is_err());
    }

    /// The regression that matters most: an unconfigured instance must still look
    /// alive, or the orchestrator restarts the container the operator is trying to
    /// type a password into. `health`'s lack of state is what enforces that, so this
    /// mostly guards the signature.
    #[tokio::test]
    async fn health_is_unconditional() {
        assert_eq!(health().await, "ok");
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

    #[tokio::test]
    async fn ready_reports_the_config_error_before_anything_else() {
        let (_dir, state) = unloadable();

        let (status, body) = ready(State(state)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(body.starts_with("configuration invalid:"), "got {body:?}");
        assert!(body.contains("taag"), "got {body:?}");
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

    #[tokio::test]
    async fn ready_reports_503_and_names_what_is_missing() {
        let (_dir, state) = unconfigured();
        state.ensure_ready().await;

        let (status, body) = ready(State(state)).await;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        // The whole point of the body is to name the thing to go and fix.
        let reason = body
            .strip_prefix("not configured: ")
            .unwrap_or_else(|| panic!("unexpected body: {body}"));
        assert!(!reason.trim().is_empty(), "no reason given");
    }
}
