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
use secrecy::SecretString;
use sha2::{Digest, Sha256};
use sharerr_arr::{ArrClient, Discovered};
use sharerr_client::TorrentClient;
use sharerr_core::config::secret_keys;
use sharerr_core::endpoint::AdvertisedEndpoint;
use sharerr_core::paths::PathResolver;
use sharerr_core::{Config, MediaSource, ShareState, SharedItem};
use sharerr_store::{RunSummary, Store, Vault};
use sharerr_torrent::{AnnounceSet, BuiltinTracker, TrackerProvider, title};

use crate::library::DirectoryScanner;
use seed::{AnnounceRefresh, SeedOutcome, Seeder};

/// A short, non-secret fingerprint of a tracker token — truncated SHA-256, hex.
///
/// Never the token itself: this is what gets stored per item and read back by
/// the items page purely to answer "is this the currently configured token".
/// Shared between [`token_fingerprint`] (which derives it from a built
/// announce URL) and `web::items` (which derives it from the raw token to
/// compare against).
pub(crate) fn fingerprint(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))[..12].to_owned()
}

/// [`fingerprint`] of the token embedded in `announce`'s primary URL — `None`
/// when the tracker carries no token at all. See [`Syncer::share`], which
/// records this per item as its torrent is built or confirmed.
pub(crate) fn token_fingerprint(announce: &AnnounceSet) -> Option<String> {
    let token = sharerr_torrent::token_from_announce_url(&announce.primary)?;
    Some(fingerprint(&token))
}

#[cfg(test)]
mod fingerprint_tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn announce(primary: &str) -> AnnounceSet {
        AnnounceSet::single(url::Url::parse(primary).unwrap())
    }

    #[test]
    fn the_same_token_always_fingerprints_the_same_way() {
        assert_eq!(fingerprint("s3cret"), fingerprint("s3cret"));
    }

    #[test]
    fn different_tokens_fingerprint_differently() {
        assert_ne!(fingerprint("s3cret"), fingerprint("different"));
    }

    /// Never the token itself — the whole point of storing this instead.
    #[test]
    fn the_fingerprint_does_not_contain_the_token() {
        assert!(!fingerprint("s3cret").contains("s3cret"));
    }

    #[test]
    fn an_announce_url_with_no_token_fingerprints_to_none() {
        assert_eq!(
            token_fingerprint(&announce("http://sharerr.example/announce")),
            None
        );
    }

    #[test]
    fn an_announce_url_with_a_token_fingerprints_it() {
        let with = token_fingerprint(&announce("http://sharerr.example/announce/tok1"));
        let other = token_fingerprint(&announce("http://sharerr.example/announce/tok2"));
        assert!(with.is_some());
        assert_ne!(with, other);
    }
}

/// Anything that can answer "which files should be shared right now?".
///
/// The *arr clients and the directory scanner disagree about everything except
/// that question, which is why the trait is one method wide plus a label. The
/// label matters to the loop: [`Discovery::scanned`] records which sources
/// answered, and only an answering source may have items withdrawn.
#[async_trait::async_trait]
pub trait LibrarySource: Send + Sync {
    /// Which [`MediaSource`] this source's items carry.
    fn kind(&self) -> MediaSource;
    /// Every file this source currently wants shared. The tag is the *arr
    /// share marker; sources without tags ignore it.
    async fn discover(&self, tag: &str) -> Result<SourceScan>;
}

/// What one source's discovery produced.
#[derive(Debug)]
pub struct SourceScan {
    pub items: Vec<Discovered>,
    /// Whether every corner of the source was listed. `false` — a directory
    /// walk that could not read one subtree — means the items are still shared,
    /// because sharing is additive, but nothing may be withdrawn on this
    /// source's behalf: an absent item may simply have been in the part that
    /// was not seen.
    pub complete: bool,
}

#[async_trait::async_trait]
impl LibrarySource for ArrClient {
    fn kind(&self) -> MediaSource {
        ArrClient::kind(self)
    }

    async fn discover(&self, tag: &str) -> Result<SourceScan> {
        Ok(SourceScan {
            items: ArrClient::discover(self, tag).await?,
            // An *arr answer is its whole database: there is no partial success.
            complete: true,
        })
    }
}

#[async_trait::async_trait]
impl LibrarySource for DirectoryScanner {
    fn kind(&self) -> MediaSource {
        MediaSource::Directory
    }

    async fn discover(&self, _tag: &str) -> Result<SourceScan> {
        // A directory has no tag — being in it is the tag. The walk is
        // filesystem-bound, so it runs off the async loop; a library on a slow
        // or remote mount must not stall /health while it is listed.
        let scanner = self.clone();
        let outcome = tokio::task::spawn_blocking(move || scanner.scan_all()).await??;
        Ok(SourceScan {
            items: outcome.items,
            complete: outcome.incomplete == 0,
        })
    }
}

pub struct Syncer {
    config: Config,
    store: Store,
    /// Every configured library source — the *arr apps, then the directory
    /// scanner when any `[[library]]` is set — in a stable order.
    ///
    /// A list rather than one field per source: five named `Option`s would mean
    /// five places to edit for a sixth, and the discovery loop treats them
    /// identically anyway — the differences live behind [`LibrarySource`].
    sources: Vec<Box<dyn LibrarySource>>,
    tracker: Arc<dyn TrackerProvider>,
    /// Owns the torrent client too — the syncer reads it through
    /// [`Seeder::qbit`], so there is exactly one handle to keep consistent.
    seeder: Seeder,
    resolver: PathResolver,
}

impl std::fmt::Debug for Syncer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Syncer")
            .field("tag", &self.config.tag)
            .field(
                "sources",
                &self.sources.iter().map(|s| s.kind()).collect::<Vec<_>>(),
            )
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
    /// Library sources that could not be scanned at all. Their existing shares
    /// are left untouched, so this is a gap in coverage rather than a set of
    /// failed items.
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
                " ({} source(s) could not be scanned; their shares were left alone)",
                self.sources_failed
            )?;
        }
        Ok(())
    }
}

/// The outcome of asking every configured library source what carries the tag.
#[derive(Debug, Default)]
struct Discovery {
    items: Vec<Discovered>,
    /// Sources that answered at all — the pass is abandoned when this is empty.
    scanned: HashSet<MediaSource>,
    /// Sources that answered *completely*. **Only these may have items
    /// withdrawn** — see [`Syncer::withdraw_untagged`]. A source that answered
    /// but could not list part of itself shares what it found and withdraws
    /// nothing.
    withdrawable: HashSet<MediaSource>,
    failures: usize,
}

impl Syncer {
    /// Wire everything from configuration and the vault.
    ///
    /// `endpoint` is the *live* advertised endpoint, shared with whatever keeps
    /// it fresh — `serve` hands in the one its gluetun poller updates, so a
    /// rotated forwarded port reaches the next announce URL without rebuilding
    /// the syncer. One-shot commands build a static one from configuration.
    pub async fn build(config: &Config, endpoint: Arc<AdvertisedEndpoint>) -> Result<Self> {
        // Off the runtime — `serve` calls this on a timer *while already serving
        // HTTP*. See `secrets::open_vault_async`.
        let vault = crate::secrets::open_vault_async(config).await?;

        // Every credential is read before anything is *opened*. `serve` retries this
        // whole function on a timer while the vault is incomplete, and opening the
        // store is the expensive half: it creates the data directory, builds a
        // connection pool, and runs the migrations. Ordering it after the cheap
        // lookups keeps a not-yet-configured instance from doing that work — and
        // from materialising sharerr.db — every time round the loop.
        let qbit = build_client(config, &vault)?;

        let mut sources: Vec<Box<dyn LibrarySource>> = Vec::new();
        for source in MediaSource::ARRS.iter().copied() {
            if let Some(client) = build_arr(source, config, &vault)? {
                sources.push(Box::new(client));
            }
        }
        if !config.library.is_empty() {
            sources.push(Box::new(DirectoryScanner::new(config.library.clone())));
        }
        if sources.is_empty() {
            bail!(
                "no library source is configured — there is nothing to share. Set at \
                 least one of sonarr, radarr, lidarr, readarr or whisparr, or a \
                 [[library]] directory."
            );
        }

        let tracker = build_tracker(endpoint, &vault)?;

        // Resolved here rather than inside the seeder, which should not have to
        // know which client it is driving.
        let client_config = config.torrent_client();
        let seeder = Seeder {
            qbit: Arc::clone(&qbit),
            category: client_config.category.to_owned(),
            tag: client_config.tag.to_owned(),
            skip_checking: client_config.skip_checking,
            upload_limit_kib: client_config.upload_limit_kib,
            ratio_limit: client_config.ratio_limit,
            torrent_dir: config.torrent_dir(),
        };

        let store = Store::open(&config.database_path())
            .await
            .with_context(|| format!("opening {}", config.database_path().display()))?;

        Ok(Self::new(config.clone(), store, sources, tracker, seeder))
    }

    /// Assemble from already-built parts.
    ///
    /// [`Self::build`] is the production path; this exists so tests can inject
    /// clients pointed at a mock stack without a vault or a master key in the
    /// process environment.
    pub(crate) fn new(
        config: Config,
        store: Store,
        sources: Vec<Box<dyn LibrarySource>>,
        tracker: Arc<dyn TrackerProvider>,
        seeder: Seeder,
    ) -> Self {
        Self {
            resolver: config.resolver(),
            config,
            store,
            sources,
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
        let announce = self.tracker.announce_set().await?;

        // Three independent reads, overlapped: the *arr walk is the long pole,
        // and the torrent list plus the store snapshot ride under it instead of
        // queueing behind it. The torrent list is fetched once for the whole
        // pass — the hash set answers "is it still live", and the summaries feed
        // the seeder's cross-seed search without a refetch per item. This is
        // also what makes the loop self-healing: a torrent removed behind
        // sharerr's back is simply absent here and gets re-added.
        let (discovery, torrents, known_items) = tokio::join!(
            self.discover(),
            self.seeder.qbit.list(None),
            self.store.all_items()
        );

        let discovered = &discovery.items;
        report.discovered = discovered.len();
        report.sources_failed = discovery.failures;

        // If nothing answered, every item looks untagged. Withdrawing the entire
        // library because Sonarr happened to be restarting would be far worse than
        // doing nothing, so this is the one failure that stops the pass.
        if discovery.scanned.is_empty() {
            bail!("no library source could be scanned — nothing was changed");
        }

        let torrents =
            torrents.with_context(|| format!("listing torrents in {}", self.seeder.qbit.kind()))?;
        // Hashes are compared lowercase. Both sides are folded once here —
        // the stored side while `known` is built below — rather than per item
        // in `share`. Every writer already produces lowercase hex, so folding
        // the stored hash in place changes nothing that is looked up by it.
        let live: HashSet<String> = torrents
            .iter()
            .map(|t| t.hash.to_ascii_lowercase())
            .collect();

        let known: HashMap<(MediaSource, i64), SharedItem> = known_items?
            .into_iter()
            .map(|mut item| {
                if let Some(hash) = &mut item.info_hash {
                    hash.make_ascii_lowercase();
                }
                (item.key(), item)
            })
            .collect();

        // Indexed once per pass — see `seed::KnownTorrents`.
        let torrents = seed::KnownTorrents::index(&torrents);

        for item in discovered {
            match self
                .share(
                    item,
                    &announce,
                    &live,
                    &torrents,
                    known.get(&item.key()),
                    dry_run,
                )
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
            .withdraw_untagged(&known, &tagged, &discovery.withdrawable, dry_run)
            .await;

        Ok(report)
    }

    /// Ask every configured library source what carries the tag.
    ///
    /// One source failing does not abort the pass — the module's whole contract
    /// is that a healthy library is not held hostage by a broken neighbour, and
    /// "Sonarr has the tag, Radarr does not" is a routine setup that surfaces as a
    /// hard error from [`sharerr_arr::ArrError::TagNotFound`].
    async fn discover(&self) -> Discovery {
        // The sources are independent, so scan them concurrently: the phase
        // costs the slowest source's walk rather than the sum of all of them.
        // `join_all` preserves input order, which keeps the fold — and the log
        // lines — stable regardless of which source answers first.
        let results = futures::future::join_all(
            self.sources
                .iter()
                .map(|source| async { (source.kind(), source.discover(&self.config.tag).await) }),
        )
        .await;

        let mut discovery = Discovery::default();
        for (kind, result) in results {
            match result {
                Ok(scan) => {
                    discovery.scanned.insert(kind);
                    if scan.complete {
                        discovery.withdrawable.insert(kind);
                    } else {
                        tracing::warn!(
                            service = %kind,
                            "the scan was incomplete — new files are still shared, but \
                             nothing is withdrawn until the whole library can be listed"
                        );
                    }
                    discovery.items.extend(scan.items);
                }
                Err(err) => {
                    discovery.failures += 1;
                    tracing::error!(
                        service = %kind,
                        error = format!("{err:#}"),
                        "discovery failed — everything already shared from this source \
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
        announce: &AnnounceSet,
        live: &HashSet<String>,
        torrents: &seed::KnownTorrents,
        known: Option<&SharedItem>,
        dry_run: bool,
    ) -> Result<Step> {
        // Already seeding, and qBittorrent agrees. This is the branch that makes a
        // repeat run a no-op — except when the advertised endpoint has moved
        // since the torrent was built, in which case its announce URLs are
        // brought up to date in place. Failure there is a warning, not a failed
        // item: the torrent is still seeding, just announcing to a stale
        // address, and the comparison stays stale so the next pass retries.
        if let Some(known) = known
            && known.state == ShareState::Seeding
            && let Some(hash) = &known.info_hash
            && live.contains(hash)
        {
            if !dry_run {
                match self.seeder.refresh_announce(hash, announce).await {
                    // Either branch means the live torrent now genuinely
                    // matches `announce.primary` — a no-op refresh is still a
                    // confirmation, not merely "nothing to do".
                    Ok(AnnounceRefresh::Current | AnnounceRefresh::Updated) => {
                        let fp = token_fingerprint(announce);
                        if let Err(err) = self
                            .store
                            .set_announce_token_fp(item.source, item.file_id, fp.as_deref())
                            .await
                        {
                            tracing::warn!(
                                item = %item.spec,
                                error = %err,
                                "could not record the confirmed announce token"
                            );
                        }
                    }
                    // Nothing was compared, so nothing is confirmed: leave
                    // the stored fingerprint alone and the items page keeps
                    // showing this one as not-yet-current rather than Valid.
                    Ok(AnnounceRefresh::NoCachedTorrent) => tracing::debug!(
                        item = %item.spec,
                        info_hash = %hash,
                        "no cached .torrent to confirm the announce URL against"
                    ),
                    Err(err) => tracing::warn!(
                        item = %item.spec,
                        error = format!("{err:#}"),
                        "could not refresh announce URLs"
                    ),
                }
            }
            return Ok(Step::Unchanged);
        }

        // Path mapping only ever substitutes the prefix (`PathResolver::resolve`
        // joins the mapped root to `rest`, the part after `strip_prefix`), so the
        // file's basename is identical under `item.arr_path` and under
        // `paths.sharerr` once resolved. That means the release title can be
        // computed now, without waiting on resolution to succeed.
        let release_title = title::resolve(&item.spec, item.scene_name.as_deref(), &item.arr_path);

        if !dry_run {
            // Record before anything can fail — including resolution itself. A
            // `NotAbsolute` path (e.g. a Windows `C:\tv\…` path from Sonarr) used
            // to fail here before any row existed, so the `Failed` state set by
            // the caller below matched zero rows: the run summary said "1
            // failed" but the items page had no row and no `last_error`.
            let record = item.clone().into_shared_item(release_title.clone());
            self.store.upsert(&record).await?;
        }

        let paths = self
            .resolver
            .resolve_for(item.source, &item.arr_path)
            .with_context(|| format!("resolving {}", item.arr_path.display()))?;

        // Checked before hashing: discovering a missing file only after SHA-1ing
        // gigabytes would be a needlessly expensive way to find out.
        let missing = || {
            if item.source == MediaSource::Directory {
                // No mapping was involved, so pointing at [[path_map]] would
                // send the operator to the wrong setting.
                anyhow::anyhow!(
                    "{} does not exist as sharerr sees it — check the [[library]] mount",
                    paths.sharerr.display()
                )
            } else {
                anyhow::anyhow!(
                    "{} does not exist as sharerr sees it (reported by {} as {}). Check [[path_map]]",
                    paths.sharerr.display(),
                    item.source,
                    paths.arr.display()
                )
            }
        };

        // `try_exists` rather than a blocking `exists()`: this runs per item on
        // the async loop, against a mount that may be remote.
        if !tokio::fs::try_exists(&paths.sharerr).await.unwrap_or(false) {
            return Err(missing());
        }

        if dry_run {
            tracing::info!(
                item = %item.spec,
                release = %release_title,
                file = %paths.sharerr.display(),
                "would share"
            );
            return Ok(Step::Added);
        }

        let outcome = self
            .seeder
            .seed(
                &paths,
                announce,
                torrents,
                known.and_then(|k| k.info_hash.as_deref()),
            )
            .await?;
        self.store
            .set_seeding(
                item.source,
                item.file_id,
                outcome.info_hash(),
                token_fingerprint(announce).as_deref(),
                matches!(outcome, SeedOutcome::Added { .. }),
            )
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
    ///
    /// Nor is a torrent sharerr did not add. `Seeder::seed` reuses one that
    /// already covers the file rather than creating a duplicate, so an item can
    /// be Seeding under an infohash belonging to the operator's own torrent;
    /// removing that on withdrawal would stop a swarm sharerr was only ever a
    /// guest in. `created_by_sharerr` is what tells the two apart.
    async fn withdraw_untagged(
        &self,
        known: &HashMap<(MediaSource, i64), SharedItem>,
        tagged: &HashSet<(MediaSource, i64)>,
        withdrawable: &HashSet<MediaSource>,
        dry_run: bool,
    ) -> usize {
        let stale = known.values().filter(|item| {
            // Only withdraw on behalf of a source that answered completely. One
            // that failed to respond — or listed only part of itself — has said
            // nothing certain about what it still carries, and reading that
            // silence as "untagged everything" would tear down a working library
            // because a container was restarting.
            withdrawable.contains(&item.source)
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

            match (&item.info_hash, item.created_by_sharerr) {
                (Some(hash), true) => {
                    if let Err(err) = self.seeder.qbit.remove(hash).await {
                        // Worth continuing: marking the row Unshared is still correct, and
                        // the torrent can be cleaned up by hand.
                        // The client's own name, not a hardcoded one: this field is a
                        // `dyn TorrentClient` and may well be Transmission.
                        let client = self.seeder.qbit.kind();
                        tracing::warn!(%hash, %err, %client, "could not remove the torrent from the client");
                    }
                }
                (Some(hash), false) => tracing::info!(
                    %hash,
                    item = %item.spec,
                    "the torrent was already in the client before sharerr shared this \
                     file, so it is left running — only the share is withdrawn"
                ),
                (None, _) => {}
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
    let Some(service) = config.service(kind) else {
        return Ok(None);
    };
    // Only called over `MediaSource::ARRS`, and every *arr app has a vault key.
    let Some(key_name) = secret_keys::api_key_for(kind) else {
        return Ok(None);
    };
    let api_key = vault
        .get(key_name)?
        .with_context(|| format!("{kind} is configured but {key_name} is not in the vault"))?;

    Ok(Some(ArrClient::new(kind, &service.url, api_key)?))
}

/// Construct whichever torrent client the configuration selects.
///
/// Reads its credential from the vault under `client`'s own keys, so an operator
/// switching backends does not silently keep authenticating with the other
/// client's credential — see `checks::resolve_torrent_credential`, which is what
/// every other caller resolving a torrent-client credential goes through too.
fn build_client(config: &Config, vault: &Vault) -> Result<Arc<dyn TorrentClient>> {
    let client = config.torrent_client();
    let secret = |key: &'static str| -> Result<Option<SecretString>, String> {
        vault.get(key).map_err(|err| err.to_string())
    };

    let credential = crate::checks::resolve_torrent_credential(&client, &secret)
        .map_err(|reason| anyhow::anyhow!(reason))?
        .with_context(|| match (client.api_key_key, client.password_key) {
            (Some(api), Some(password)) => format!("no {api} or {password} in the vault"),
            (Some(api), None) => format!("no {api} in the vault"),
            (None, Some(password)) => format!("no {password} in the vault"),
            (None, None) => "no credential configured for this torrent client".to_owned(),
        })?;

    crate::checks::build_torrent_client(
        config.torrent_backend,
        client.url,
        client.username,
        credential,
    )
    .map_err(|reason| anyhow::anyhow!(reason))
}

fn build_tracker(
    endpoint: Arc<AdvertisedEndpoint>,
    vault: &Vault,
) -> Result<Arc<dyn TrackerProvider>> {
    let token = vault.get(secret_keys::TRACKER_TOKEN)?;
    let token = token.as_ref().map(secrecy::ExposeSecret::expose_secret);
    Ok(Arc::new(BuiltinTracker::new(endpoint, token)))
}
