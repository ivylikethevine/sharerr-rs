//! The reconciliation loop.
//!
//! One pass is: discover what carries the tag, diff it against what sharerr has
//! recorded, share what is new, and withdraw what is no longer tagged. It is
//! **idempotent** — running it twice in a row makes no second change — and
//! **additive** — withdrawing a share removes a torrent and never a file.
//!
//! Failures are per item. One unreadable file, one rejected torrent, or one series
//! that vanished mid-run marks that item `Failed` with the reason and the pass
//! continues. A library of five hundred files must not be held hostage by one.

pub mod seed;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use sharerr_arr::{ArrClient, Discovered};
use sharerr_core::config::{TrackerBackend, secret_keys};
use sharerr_core::paths::PathResolver;
use sharerr_core::{Config, MediaSource, ShareState, SharedItem};
use sharerr_qbit::QbitClient;
use sharerr_store::{RunSummary, Store, Vault, master_key_from_env};
use sharerr_torrent::{
    BuiltinTracker, LavaTorrentFactory, QbitEmbeddedTracker, TrackerProvider, title,
};

use seed::{SeedOutcome, Seeder};

pub struct Syncer {
    config: Config,
    store: Store,
    sonarr: Option<ArrClient>,
    radarr: Option<ArrClient>,
    qbit: Arc<QbitClient>,
    tracker: Arc<dyn TrackerProvider>,
    seeder: Seeder,
    resolver: PathResolver,
}

impl std::fmt::Debug for Syncer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Syncer")
            .field("tag", &self.config.tag)
            .field("sonarr", &self.sonarr.is_some())
            .field("radarr", &self.radarr.is_some())
            .finish_non_exhaustive()
    }
}

/// What one pass did. Mirrors the `sync_runs` row, plus a count of items that were
/// already correct — the number that should be everything on a repeat run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub discovered: usize,
    pub added: usize,
    pub reused: usize,
    pub unchanged: usize,
    pub unshared: usize,
    pub failed: usize,
    /// *arr apps that could not be scanned at all. Their existing shares are left
    /// untouched, so this is a gap in coverage rather than a set of failed items.
    pub sources_failed: usize,
}

impl SyncReport {
    fn to_summary(&self) -> RunSummary {
        RunSummary {
            discovered: self.discovered as i64,
            added: (self.added + self.reused) as i64,
            unshared: self.unshared as i64,
            failed: (self.failed + self.sources_failed) as i64,
            error: None,
        }
    }

    /// Whether anything needs an operator's attention.
    pub fn has_problems(&self) -> bool {
        self.failed > 0 || self.sources_failed > 0
    }
}

impl std::fmt::Display for SyncReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} discovered, {} added, {} reused, {} unchanged, {} unshared, {} failed",
            self.discovered, self.added, self.reused, self.unchanged, self.unshared, self.failed
        )?;
        if self.sources_failed > 0 {
            write!(
                f,
                " ({} *arr app(s) could not be scanned; their shares were left alone)",
                self.sources_failed
            )?;
        }
        Ok(())
    }
}

/// The outcome of asking every configured *arr app what carries the tag.
#[derive(Debug, Default)]
struct Discovery {
    items: Vec<Discovered>,
    /// Apps that answered. **Only these may have items withdrawn** — see
    /// [`Syncer::withdraw_untagged`].
    scanned: HashSet<MediaSource>,
    failures: usize,
}

impl Syncer {
    /// Wire everything from configuration and the vault.
    pub async fn build(config: &Config) -> Result<Self> {
        let master = master_key_from_env()?;
        let vault = Vault::open(config.vault_path(), &master)
            .with_context(|| format!("opening vault at {}", config.vault_path().display()))?;

        let store = Store::open(&config.database_path())
            .await
            .with_context(|| format!("opening {}", config.database_path().display()))?;

        let qbit_password = vault
            .get(secret_keys::QBITTORRENT_PASSWORD)?
            .with_context(|| format!("no {} in the vault", secret_keys::QBITTORRENT_PASSWORD))?;
        let qbit = Arc::new(QbitClient::new(
            &config.qbittorrent.url,
            &config.qbittorrent.username,
            qbit_password,
        )?);

        let sonarr = build_arr(MediaSource::Sonarr, config, &vault)?;
        let radarr = build_arr(MediaSource::Radarr, config, &vault)?;
        if sonarr.is_none() && radarr.is_none() {
            bail!("neither sonarr nor radarr is configured — there is nothing to share");
        }

        let tracker = build_tracker(config, &vault, Arc::clone(&qbit))?;

        let seeder = Seeder {
            qbit: Arc::clone(&qbit),
            factory: Arc::new(LavaTorrentFactory),
            category: config.qbittorrent.category.clone(),
            tag: config.qbittorrent.tag.clone(),
            skip_checking: config.qbittorrent.skip_checking,
            torrent_dir: config.torrent_dir(),
        };

        Ok(Self::new(
            config.clone(),
            store,
            sonarr,
            radarr,
            qbit,
            tracker,
            seeder,
        ))
    }

    /// Assemble from already-built parts.
    ///
    /// [`Self::build`] is the production path; this exists so tests can inject
    /// clients pointed at a mock stack without a vault or a master key in the
    /// process environment.
    pub(crate) fn new(
        config: Config,
        store: Store,
        sonarr: Option<ArrClient>,
        radarr: Option<ArrClient>,
        qbit: Arc<QbitClient>,
        tracker: Arc<dyn TrackerProvider>,
        seeder: Seeder,
    ) -> Self {
        Self {
            resolver: config.resolver(),
            config,
            store,
            sonarr,
            radarr,
            qbit,
            tracker,
            seeder,
        }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Run one reconciliation pass.
    ///
    /// A dry run reports what would change and writes nothing — no torrents, no
    /// database rows, not even a `sync_runs` entry.
    pub async fn run(&self, dry_run: bool) -> Result<SyncReport> {
        let run_id = if dry_run {
            None
        } else {
            Some(self.store.begin_run().await?)
        };

        let outcome = self.reconcile(dry_run).await;

        if let Some(run_id) = run_id {
            let summary = match &outcome {
                Ok(report) => report.to_summary(),
                // A run that failed outright is still worth recording; an operator
                // looking at history needs to see the gap and its reason.
                Err(err) => RunSummary {
                    error: Some(format!("{err:#}")),
                    ..Default::default()
                },
            };
            if let Err(err) = self.store.finish_run(run_id, &summary).await {
                tracing::warn!(%err, "could not record the sync run");
            }
        }

        outcome
    }

    async fn reconcile(&self, dry_run: bool) -> Result<SyncReport> {
        let mut report = SyncReport::default();

        // Refuse early rather than building torrents that announce into the void.
        self.tracker.ensure_ready().await?;
        let announce = self.tracker.announce_url().await?;

        let discovery = self.discover().await;
        let discovered = &discovery.items;
        report.discovered = discovered.len();
        report.sources_failed = discovery.failures;

        // If nothing answered, every item looks untagged. Withdrawing the entire
        // library because Sonarr happened to be restarting would be far worse than
        // doing nothing, so this is the one failure that stops the pass.
        if discovery.scanned.is_empty() {
            bail!("no *arr app could be scanned — nothing was changed");
        }

        // One call, then membership tests are free. This is also what makes the
        // loop self-healing: a torrent removed behind sharerr's back is simply
        // absent here and gets re-added.
        let live: HashSet<String> = self
            .qbit
            .torrents_info(None, None)
            .await
            .context("listing torrents in qBittorrent")?
            .into_iter()
            .map(|t| t.hash.to_ascii_lowercase())
            .collect();

        let known: HashMap<(MediaSource, i64), SharedItem> = self
            .store
            .all_items()
            .await?
            .into_iter()
            .map(|item| (item.key(), item))
            .collect();

        for item in discovered {
            match self
                .share(item, &announce, &live, known.get(&item.key()), dry_run)
                .await
            {
                Ok(Step::Added) => report.added += 1,
                Ok(Step::Reused) => report.reused += 1,
                Ok(Step::Unchanged) => report.unchanged += 1,
                Err(err) => {
                    report.failed += 1;
                    // The reason has to survive to the next run, or an operator
                    // sees only a count and no cause.
                    tracing::error!(
                        item = %item.spec,
                        file = %item.arr_path.display(),
                        error = format!("{err:#}"),
                        "could not share item"
                    );
                    if !dry_run {
                        let _ = self
                            .store
                            .set_state(
                                item.source,
                                item.file_id,
                                ShareState::Failed,
                                Some(&format!("{err:#}")),
                            )
                            .await;
                    }
                }
            }
        }

        let tagged: HashSet<(MediaSource, i64)> = discovered.iter().map(Discovered::key).collect();
        report.unshared = self
            .withdraw_untagged(&known, &tagged, &discovery.scanned, dry_run)
            .await;

        Ok(report)
    }

    /// Ask every configured *arr app what carries the tag.
    ///
    /// One app failing does not abort the pass — the module's whole contract is
    /// that a healthy library is not held hostage by a broken neighbour, and
    /// "Sonarr has the tag, Radarr does not" is a routine setup that surfaces as a
    /// hard error from [`sharerr_arr::ArrError::TagNotFound`].
    async fn discover(&self) -> Discovery {
        let mut discovery = Discovery::default();

        for client in [self.sonarr.as_ref(), self.radarr.as_ref()]
            .into_iter()
            .flatten()
        {
            match client.discover(&self.config.tag).await {
                Ok(found) => {
                    discovery.scanned.insert(client.kind());
                    discovery.items.extend(found);
                }
                Err(err) => {
                    discovery.failures += 1;
                    tracing::error!(
                        service = %client.kind(),
                        error = format!("{err:#}"),
                        "discovery failed — everything already shared from this app \
                         will be left exactly as it is"
                    );
                }
            }
        }

        discovery
    }

    async fn share(
        &self,
        item: &Discovered,
        announce: &url::Url,
        live: &HashSet<String>,
        known: Option<&SharedItem>,
        dry_run: bool,
    ) -> Result<Step> {
        // Already seeding, and qBittorrent agrees. This is the branch that makes a
        // repeat run a no-op.
        if let Some(known) = known
            && known.state == ShareState::Seeding
            && let Some(hash) = &known.info_hash
            && live.contains(&hash.to_ascii_lowercase())
        {
            return Ok(Step::Unchanged);
        }

        let paths = self
            .resolver
            .resolve(&item.arr_path)
            .with_context(|| format!("resolving {}", item.arr_path.display()))?;

        // Checked before hashing: discovering a missing file only after SHA-1ing
        // gigabytes would be a needlessly expensive way to find out.
        let missing = || {
            anyhow::anyhow!(
                "{} does not exist as sharerr sees it (reported by {} as {}). Check [[path_map]]",
                paths.sharerr.display(),
                item.source,
                paths.arr.display()
            )
        };

        // Resolution only reads the path, never the file, so it works even for a
        // file that is missing — which is what lets the row below be complete.
        let release_title = title::resolve(&item.spec, item.scene_name.as_deref(), &paths.sharerr);

        if dry_run {
            if !paths.sharerr.exists() {
                return Err(missing());
            }
            tracing::info!(
                item = %item.spec,
                release = %release_title,
                file = %paths.sharerr.display(),
                "would share"
            );
            return Ok(Step::Added);
        }

        // Record before anything can fail. Two reasons: if the process dies
        // mid-add the item is on file as Pending and the next run retries it, and
        // a failure below has a row to attach its `last_error` to. Without this,
        // the most common failure — a file sharerr cannot see — would leave no
        // trace at all beyond a log line.
        let record = item.clone().into_shared_item(release_title);
        self.store.upsert(&record).await?;

        if !paths.sharerr.exists() {
            return Err(missing());
        }

        let outcome = self.seeder.seed(&paths, announce).await?;
        self.store
            .set_info_hash(item.source, item.file_id, outcome.info_hash())
            .await?;
        self.store
            .set_state(item.source, item.file_id, ShareState::Seeding, None)
            .await?;

        Ok(match outcome {
            SeedOutcome::Added { .. } => Step::Added,
            SeedOutcome::Reused { .. } => Step::Reused,
        })
    }

    /// Withdraw items whose tag was removed upstream.
    ///
    /// Removes the torrent and marks the row `Unshared`. **The file is never
    /// touched** — sharerr shares media it does not own.
    async fn withdraw_untagged(
        &self,
        known: &HashMap<(MediaSource, i64), SharedItem>,
        tagged: &HashSet<(MediaSource, i64)>,
        scanned: &HashSet<MediaSource>,
        dry_run: bool,
    ) -> usize {
        let stale = known.values().filter(|item| {
            // Only withdraw on behalf of an app that actually answered. An app
            // that failed to respond has said nothing about what it still carries,
            // and reading its silence as "untagged everything" would tear down a
            // working library because a container was restarting.
            scanned.contains(&item.source)
                && !tagged.contains(&item.key())
                && matches!(
                    item.state,
                    ShareState::Seeding | ShareState::Pending | ShareState::Failed
                )
        });

        let mut count = 0;
        for item in stale {
            if dry_run {
                tracing::info!(item = %item.spec, "would unshare");
                count += 1;
                continue;
            }

            if let Some(hash) = &item.info_hash
                && let Err(err) = self.qbit.remove_torrent(hash).await
            {
                // Worth continuing: marking the row Unshared is still correct, and
                // the torrent can be cleaned up by hand.
                tracing::warn!(%hash, %err, "could not remove torrent from qBittorrent");
            }

            match self
                .store
                .set_state(item.source, item.file_id, ShareState::Unshared, None)
                .await
            {
                Ok(()) => {
                    tracing::info!(item = %item.spec, "unshared (the file was not touched)");
                    count += 1;
                }
                Err(err) => tracing::warn!(item = %item.spec, %err, "could not mark unshared"),
            }
        }

        count
    }
}

enum Step {
    Added,
    Reused,
    Unchanged,
}

fn build_arr(kind: MediaSource, config: &Config, vault: &Vault) -> Result<Option<ArrClient>> {
    let (service, key_name) = match kind {
        MediaSource::Sonarr => (config.sonarr.as_ref(), secret_keys::SONARR_API_KEY),
        MediaSource::Radarr => (config.radarr.as_ref(), secret_keys::RADARR_API_KEY),
    };

    let Some(service) = service else {
        return Ok(None);
    };
    let api_key = vault
        .get(key_name)?
        .with_context(|| format!("{kind} is configured but {key_name} is not in the vault"))?;

    Ok(Some(ArrClient::new(kind, &service.url, api_key)?))
}

fn build_tracker(
    config: &Config,
    vault: &Vault,
    qbit: Arc<QbitClient>,
) -> Result<Arc<dyn TrackerProvider>> {
    let host = config.tracker.advertised_host.as_deref();

    Ok(match config.tracker.backend {
        TrackerBackend::QbittorrentEmbedded => {
            Arc::new(QbitEmbeddedTracker::new(qbit, host, config.tracker.port)?)
        }
        TrackerBackend::Builtin => {
            let token = vault.get(secret_keys::TRACKER_TOKEN)?;
            let token = token.as_ref().map(secrecy::ExposeSecret::expose_secret);
            let port = config
                .tracker
                .port
                .unwrap_or_else(|| config.server.bind.port());
            Arc::new(BuiltinTracker::new(host, port, token)?)
        }
    })
}
