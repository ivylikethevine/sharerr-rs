//! Handing content to qBittorrent without disturbing what is already there.
//!
//! The README's "preserve any existing torrents" requirement comes down to four
//! rules, all enforced here or in [`sharerr_qbit::QbitClient::add_torrent`]:
//!
//! 1. **Detect first.** If a torrent already contains this file, reuse its infohash
//!    instead of creating a second one. Cross-seeding is well supported, but a
//!    duplicate sharerr keeps re-adding every sync is just noise. Reusing one
//!    sharerr did not create means *adopting* it — see [`Seeder::adopt`] for
//!    the two things that takes, and for why sharerr's tracker goes in
//!    alongside the torrent's own rather than over them.
//! 2. **`autoTMM=false`.** Automatic Torrent Management relocates content by
//!    category the instant a torrent is added. Enforced in the qbit client, with no
//!    override.
//! 3. **`savepath` is the file's existing parent directory**, in qBittorrent's view
//!    of the filesystem, so it finds the data in place.
//! 4. **Never delete.** Unsharing removes the torrent and leaves the media alone.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use sharerr_client::{AddRequest, TorrentClient, TorrentFileEntry, TorrentSummary};
use sharerr_core::MediaMeta;
use sharerr_core::paths::ResolvedPaths;
use sharerr_torrent::{AnnounceSet, TorrentFactory, TorrentRequest, torrent_file_path};

/// What [`Seeder::refresh_announce`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnounceRefresh {
    /// The cached `.torrent` already announced to the current endpoint; the
    /// client was not touched.
    Current,
    /// The client's tracker list and the cached file were both brought up to
    /// date.
    Updated,
    /// There is no cached `.torrent` to compare against, so nothing was
    /// checked or changed — the client may well still announce with an old
    /// token, and recording it as confirmed would show "Valid" for a torrent
    /// nobody has looked at.
    NoCachedTorrent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedOutcome {
    /// A torrent already covering this file was found; nothing was added.
    /// Whether it is private is not knowable here without an extra export
    /// call per item — an operator's own torrent or a cross-seed, not one
    /// sharerr built — so the caller does not update `shared_items.private`
    /// for this outcome and whatever the row already holds stands.
    Reused { info_hash: String },
    /// A new torrent was built and handed to qBittorrent. `private` is
    /// decoded from the actual bytes just built or reused from sharerr's own
    /// cache — see [`Seeder::seed`].
    Added { info_hash: String, private: bool },
}

impl SeedOutcome {
    pub fn info_hash(&self) -> &str {
        match self {
            Self::Reused { info_hash } | Self::Added { info_hash, .. } => info_hash,
        }
    }
}

pub struct Seeder {
    pub qbit: Arc<dyn TorrentClient>,
    pub category: String,
    pub tag: String,
    pub skip_checking: bool,
    /// Per-torrent upload cap in KiB/s, applied at add time and restated by
    /// `Syncer::reconcile_limits` on torrents sharerr already created.
    /// `None` leaves the client's own default in effect. See `[seeding]` in
    /// `sharerr.toml`.
    pub upload_limit_kib: Option<u64>,
    /// Seed-ratio goal, same lifecycle as `upload_limit_kib`. `None` leaves
    /// the client's own default/global ratio setting in effect.
    pub ratio_limit: Option<f64>,
    /// Whether newly built torrents set BEP 27's private flag. See
    /// `sharerr_core::config::SeedingConfig::private`. Read once per
    /// `.torrent` build, so a config change only affects torrents built
    /// after it lands — existing shares keep whatever they were built with.
    pub private: bool,
    /// Where sharerr keeps a copy of each `.torrent` it builds.
    pub torrent_dir: PathBuf,
}

impl std::fmt::Debug for Seeder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Seeder")
            .field("category", &self.category)
            .field("tag", &self.tag)
            .field("skip_checking", &self.skip_checking)
            .field("upload_limit_kib", &self.upload_limit_kib)
            .field("ratio_limit", &self.ratio_limit)
            .field("private", &self.private)
            .field("torrent_dir", &self.torrent_dir)
            .finish_non_exhaustive()
    }
}

impl Seeder {
    /// Ensure the file at `paths` is being seeded, building a torrent only if
    /// nothing already covers it.
    ///
    /// `torrents` is the client's full list, fetched and indexed once per
    /// reconciliation pass by the caller — refetching it here per item would
    /// cost a first sync of a large library one full-library round trip per
    /// file. A snapshot is
    /// sound: torrents this pass adds are single-file and each discovered item is
    /// a distinct file, so a later item can never be covered by an earlier add.
    ///
    /// `known_info_hash` is the hash sharerr last recorded for this item, if
    /// any — set when the client no longer has the torrent (a reinstall, a
    /// wiped session) but sharerr's own cache under `torrent_dir` might still.
    /// Reusing that cached `.torrent` skips re-hashing the file, which for a
    /// media library is gigabytes of CPU `find_existing` above cannot save
    /// this call from, since the whole reason this path is reached is that
    /// `torrents` no longer contains a match.
    pub async fn seed(
        &self,
        paths: &ResolvedPaths,
        announce: &AnnounceSet,
        torrents: &KnownTorrents,
        known_info_hash: Option<&str>,
        media: Option<&MediaMeta>,
    ) -> Result<SeedOutcome> {
        if let Some(existing) = self.find_existing(torrents, &paths.qbit).await? {
            tracing::info!(
                file = %paths.qbit.display(),
                info_hash = %existing,
                "already covered by an existing torrent — reusing it"
            );
            self.adopt(&existing, announce).await?;
            return Ok(SeedOutcome::Reused {
                info_hash: existing,
            });
        }

        let (info_hash, data) = match self.reuse_cached(known_info_hash, announce, paths).await {
            Some(reused) => reused,
            None => {
                let built = self.build(paths, announce, media).await?;
                (built.info_hash, built.data)
            }
        };

        // qBittorrent needs the directory the content sits in, not the file.
        let save_path = paths
            .qbit
            .parent()
            .map(Path::to_path_buf)
            .with_context(|| format!("{} has no parent directory", paths.qbit.display()))?;

        let filename = format!("{info_hash}.torrent");
        let save_path = save_path.to_string_lossy();
        let mut request = AddRequest::new(&data, &info_hash, &filename, &save_path)
            .category(&self.category)
            .tags(&self.tag)
            .skip_checking(self.skip_checking);
        if let Some(kib) = self.upload_limit_kib {
            request = request.upload_limit_kib(kib);
        }
        if let Some(ratio) = self.ratio_limit {
            request = request.ratio_limit(ratio);
        }
        self.qbit
            .add(&request)
            .await
            .with_context(|| format!("adding {} to {}", paths.qbit.display(), self.qbit.kind()))?;

        // Decoded from the bytes just built (or reused from sharerr's own
        // cache) rather than taken from `self.private`: a cached `.torrent`
        // may predate a later config change, and this is what actually got
        // handed to the client. Falls back to the conservative `true` if the
        // bytes this function itself just produced somehow fail to decode —
        // should not happen, but privateness is not something to guess public.
        let private = sharerr_torrent::Torrent::read_from_bytes(&data)
            .map_or(true, |torrent| torrent.is_private());

        Ok(SeedOutcome::Added { info_hash, private })
    }

    /// Take responsibility for a torrent the client already had, without
    /// taking it over.
    ///
    /// Reusing an existing torrent is the right call — a second torrent of the
    /// same file is noise — but reusing it and doing nothing else leaves a
    /// share that only looks like one. Two things are missing, and each breaks
    /// a different half of a friend's download:
    ///
    /// 1. **The client is not announcing to sharerr's tracker.** Whatever
    ///    trackers the torrent came with are the only ones it has, so the local
    ///    client never registers in the swarm sharerr introduces friends to,
    ///    and they arrive to find nobody seeding.
    /// 2. **Nothing is cached under this infohash.** `tracker::torrent_file`
    ///    serves downloads out of `torrent_dir` and nowhere else, so the feed
    ///    advertises a release that 404s.
    ///
    /// The tracker goes in **additively**: this torrent is the operator's, and
    /// `set_trackers` would drop the trackers it came with. Ordering matches
    /// [`Self::refresh_announce`] — the client is corrected before the cache
    /// is written, so a failure leaves the next pass with the same work to do
    /// rather than a cache that claims it is done.
    ///
    /// Failure here fails the item. An adopted torrent that friends cannot
    /// download from is worse than one the items page marks Failed with a
    /// reason: the feed would carry it either way, and only one of those is
    /// visible to the operator.
    async fn adopt(&self, info_hash: &str, announce: &AnnounceSet) -> Result<()> {
        self.qbit
            .add_trackers(info_hash, &announce.tiers)
            .await
            .with_context(|| {
                format!(
                    "adding sharerr's tracker to {info_hash} in {}",
                    self.qbit.kind()
                )
            })?;

        let path = torrent_file_path(&self.torrent_dir, info_hash);
        let cached = match tokio::fs::read(&path).await {
            Ok(data) => Some(data),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
        };

        // A cache hit is the common case and not really an adoption at all:
        // sharerr built this torrent on an earlier run and is only rediscovering
        // it, which is also why it must not be exported over.
        let (rewritten, actual) = match cached {
            Some(data) => {
                match sharerr_torrent::retarget_announce(&data, announce)
                    .with_context(|| format!("parsing {}", path.display()))?
                {
                    // Already correct — a prior `adopt` already verified and
                    // wrote this cache entry, so there is nothing left to do.
                    sharerr_torrent::Retargeted::Current { .. } => return Ok(()),
                    sharerr_torrent::Retargeted::Updated {
                        data, info_hash, ..
                    } => (data, info_hash),
                }
            }
            None => {
                let exported = self
                    .qbit
                    .export(info_hash)
                    .await
                    .with_context(|| format!("exporting {info_hash} from {}", self.qbit.kind()))?
                    .with_context(|| {
                        format!(
                            "{} already has a torrent covering this file ({info_hash}) but cannot \
                             hand back its .torrent, and sharerr has none cached — so there would \
                             be nothing to serve a friend who asked for it. Remove that torrent, \
                             or let sharerr add its own",
                            self.qbit.kind()
                        )
                    })?;
                // Unlike the cache-hit path, this one is reached regardless of
                // whether the exported announce already matches: sharerr has
                // never cached this torrent before, so it still needs
                // verifying and writing either way.
                match sharerr_torrent::retarget_announce(&exported, announce)
                    .with_context(|| format!("rewriting the announce URLs of {info_hash}"))?
                {
                    sharerr_torrent::Retargeted::Current { info_hash, .. } => (exported, info_hash),
                    sharerr_torrent::Retargeted::Updated {
                        data, info_hash, ..
                    } => (data, info_hash),
                }
            }
        };

        // What is about to be filed under `info_hash` must actually *be*
        // `info_hash`. These bytes came from the client, not from
        // `TorrentFactory`, so nothing so far has checked that — and a
        // mismatch would hand every friend a torrent for a different swarm than
        // the feed pointed them at, which fails in a much more confusing place
        // than here.
        anyhow::ensure!(
            actual.eq_ignore_ascii_case(info_hash),
            "{} handed back a .torrent for {actual}, not {info_hash}",
            self.qbit.kind()
        );

        // Same writer as `build`'s cache, off the runtime for the same reason;
        // unlike there, a failure is returned — nothing else holds these bytes.
        let cached = path.clone();
        tokio::task::spawn_blocking(move || write_torrent_file(&cached, &rewritten))
            .await
            .context("cache write task panicked")?
            .with_context(|| format!("writing {}", path.display()))?;

        tracing::info!(
            info_hash,
            path = %path.display(),
            "adopted an existing torrent: sharerr's tracker added and its .torrent cached"
        );
        Ok(())
    }

    /// Read back the `.torrent` sharerr cached for `known_info_hash`, bringing
    /// its announce up to date if the endpoint has since moved, instead of
    /// rebuilding it from the media file (see [`Self::seed`]).
    ///
    /// `None` covers every reason to fall back to a full rebuild — no known
    /// hash, nothing cached under it, the cached file being unreadable or
    /// unparseable, or the cache describing a different size than the file
    /// now has — uniformly and without failing the item: none of these are
    /// worth more than a warning, since a rebuild recovers all of them.
    async fn reuse_cached(
        &self,
        known_info_hash: Option<&str>,
        announce: &AnnounceSet,
        paths: &ResolvedPaths,
    ) -> Option<(String, Vec<u8>)> {
        let hash = known_info_hash?;
        let path = torrent_file_path(&self.torrent_dir, hash);

        let data = match tokio::fs::read(&path).await {
            Ok(data) => data,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
            Err(err) => {
                tracing::warn!(
                    info_hash = hash,
                    path = %path.display(),
                    %err,
                    "could not read the cached .torrent — rebuilding instead"
                );
                return None;
            }
        };

        let outcome = match sharerr_torrent::retarget_announce(&data, announce) {
            Ok(outcome) => outcome,
            Err(err) => {
                tracing::warn!(
                    info_hash = hash,
                    path = %path.display(),
                    %err,
                    "cached .torrent could not be parsed — rebuilding instead"
                );
                return None;
            }
        };

        // Checked before acting on either branch below: a cache that parses
        // fine but describes different bytes is the in-place-upgrade case —
        // same `file_id`, same recorded info hash, a torrent that would seed
        // the wrong content. Catching it here means the next scheduled sync
        // pass rebuilds it automatically, instead of the operator needing to
        // notice and click Force rebuild.
        let length = match &outcome {
            sharerr_torrent::Retargeted::Current { length, .. }
            | sharerr_torrent::Retargeted::Updated { length, .. } => *length,
        };
        if self
            .cached_size_is_stale(&paths.sharerr, length, hash, &path)
            .await
        {
            return None;
        }

        match outcome {
            sharerr_torrent::Retargeted::Current { .. } => {
                tracing::info!(
                    info_hash = hash,
                    "reusing the cached .torrent for a vanished torrent — skipping the rebuild"
                );
                Some((hash.to_owned(), data))
            }
            sharerr_torrent::Retargeted::Updated {
                previous,
                data: rewritten,
                ..
            } => {
                if let Err(err) = tokio::fs::write(&path, &rewritten).await {
                    // The rewritten bytes are still handed to the client below; only
                    // the on-disk cache failed to update, and the next pass to touch
                    // this item will simply try the write again.
                    tracing::warn!(
                        info_hash = hash,
                        path = %path.display(),
                        %err,
                        "could not update the cached .torrent on disk"
                    );
                }

                tracing::info!(
                    info_hash = hash,
                    from = previous.as_deref().unwrap_or("(none)"),
                    to = %announce.primary,
                    "reusing the cached .torrent for a vanished torrent, with its announce refreshed"
                );
                Some((hash.to_owned(), rewritten))
            }
        }
    }

    /// `true` when the cached `.torrent`'s recorded size no longer matches
    /// the file `sharerr_path` now points at — an in-place upgrade under the
    /// same path, with the same info hash still recorded. A metadata read
    /// failure is treated the same way: not proof of anything on its own,
    /// but not worth trusting the cache over either, so it falls back to the
    /// same rebuild that would hit the file's real state regardless.
    async fn cached_size_is_stale(
        &self,
        sharerr_path: &Path,
        cached_length: u64,
        hash: &str,
        cache_path: &Path,
    ) -> bool {
        let on_disk = match tokio::fs::metadata(sharerr_path).await {
            Ok(meta) => meta.len(),
            Err(err) => {
                tracing::warn!(
                    info_hash = hash,
                    path = %sharerr_path.display(),
                    %err,
                    "could not read the on-disk file's size — rebuilding instead"
                );
                return true;
            }
        };
        if on_disk == cached_length {
            return false;
        }
        tracing::warn!(
            info_hash = hash,
            cache_path = %cache_path.display(),
            media_path = %sharerr_path.display(),
            cached_size = cached_length,
            on_disk_size = on_disk,
            "cached .torrent describes a different size than the file on disk — rebuilding instead"
        );
        true
    }

    /// Bring one already-seeding torrent's announce URLs up to date, in both
    /// places they live: the cached `.torrent` the feed serves, and the tracker
    /// list inside the torrent client.
    ///
    /// The client is updated **before** the file is rewritten: the file's
    /// announce is what the next pass compares against, so if the client
    /// update fails the comparison stays stale and the whole step is retried
    /// — the opposite order would record success it did not achieve.
    pub async fn refresh_announce(
        &self,
        info_hash: &str,
        announce: &AnnounceSet,
    ) -> Result<AnnounceRefresh> {
        let path = torrent_file_path(&self.torrent_dir, info_hash);
        let data = match tokio::fs::read(&path).await {
            Ok(data) => data,
            // Nothing cached means nothing to compare or serve; the torrent in
            // the client still works, so this is not worth failing the item
            // over — but nothing here has confirmed what it announces to
            // either, which the caller must not mistake for "current".
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(AnnounceRefresh::NoCachedTorrent);
            }
            Err(err) => {
                return Err(err).with_context(|| format!("reading {}", path.display()));
            }
        };

        let (previous, rewritten) = match sharerr_torrent::retarget_announce(&data, announce)
            .with_context(|| format!("parsing {}", path.display()))?
        {
            sharerr_torrent::Retargeted::Current { .. } => return Ok(AnnounceRefresh::Current),
            sharerr_torrent::Retargeted::Updated { previous, data, .. } => (previous, data),
        };

        self.qbit
            .set_trackers(info_hash, &announce.tiers)
            .await
            .with_context(|| format!("updating trackers in {}", self.qbit.kind()))?;

        tokio::fs::write(&path, &rewritten)
            .await
            .with_context(|| format!("writing {}", path.display()))?;

        tracing::info!(
            info_hash,
            from = previous.as_deref().unwrap_or("(none)"),
            to = %announce.primary,
            "announce URLs refreshed"
        );
        Ok(AnnounceRefresh::Updated)
    }

    /// Build the `.torrent` and keep a copy on disk.
    async fn build(
        &self,
        paths: &ResolvedPaths,
        announce: &AnnounceSet,
        media: Option<&MediaMeta>,
    ) -> Result<sharerr_torrent::BuiltTorrent> {
        let path = paths.sharerr.clone();
        let announce = announce.clone();
        let torrent_dir = self.torrent_dir.clone();
        let private = self.private;
        // Owned: the closure below outlives this frame on a blocking thread.
        let media = media.cloned();

        // Hashing a media file is gigabytes of CPU work, and the cache write can
        // block for tens of milliseconds on the network mounts these deployments
        // sit on. Both stay off the runtime; doing either inline would stall
        // every other task — /health and /announce included — for the duration.
        let built = tokio::task::spawn_blocking(move || {
            let built = TorrentFactory.create(&TorrentRequest {
                path: &path,
                announce: &announce,
                media: media.as_ref(),
                private,
            })?;

            // Best-effort: the torrent is already in memory and about to be
            // handed over, so a failure to cache it must not fail the share.
            let cached = torrent_file_path(&torrent_dir, &built.info_hash);
            if let Err(err) = write_torrent_file(&cached, &built.data) {
                tracing::warn!(path = %cached.display(), %err, "could not cache the .torrent file");
            }

            Ok::<_, sharerr_torrent::TorrentError>(built)
        })
        .await
        .context("torrent build task panicked")?
        .with_context(|| format!("building a torrent for {}", paths.sharerr.display()))?;

        Ok(built)
    }

    /// Find a torrent the client already has that contains `target`.
    ///
    /// Two passes, cheap first. The summary list alone answers the common case,
    /// because a single-file torrent's `content_path` is the file itself. Only
    /// torrents that could plausibly contain the target — by save path — cost an
    /// extra `torrents/files` call.
    async fn find_existing(
        &self,
        torrents: &KnownTorrents,
        target: &Path,
    ) -> Result<Option<String>> {
        // Normalised once: the target is invariant across the whole scan, and
        // the torrents' own paths were normalised once per pass when the list
        // was indexed — so nothing here allocates per candidate.
        let target = &normalize_path(target);
        let torrents = &torrents.entries;

        if let Some(found) = torrents.iter().find(|t| matches_content_path(t, target)) {
            return Ok(Some(found.hash.clone()));
        }

        for torrent in torrents.iter().filter(|t| could_contain(t, target)) {
            let files = match self.qbit.files(&torrent.hash).await {
                Ok(files) => files,
                Err(err) => {
                    // One unreadable torrent must not stop the search; the worst
                    // case is a duplicate, not a moved file.
                    tracing::warn!(hash = %torrent.hash, %err, "could not list torrent files");
                    continue;
                }
            };

            let Some(save_path) = &torrent.save_path else {
                continue;
            };
            if contains_file(save_path, &files, target) {
                return Ok(Some(torrent.hash.clone()));
            }
        }

        Ok(None)
    }
}

/// The client's torrent list, indexed once per reconciliation pass for
/// [`Seeder::seed`]'s cross-seed search.
///
/// Every discovered item is matched against every torrent, and normalising a
/// torrent's paths on each comparison was O(items × torrents) `PathBuf`
/// builds per pass. Here each path is normalised exactly once; an empty
/// `save_path` or `content_path` becomes `None`, which matches nothing.
#[derive(Debug, Default)]
pub struct KnownTorrents {
    entries: Vec<KnownTorrent>,
}

impl KnownTorrents {
    pub fn index(torrents: &[TorrentSummary]) -> Self {
        Self {
            entries: torrents.iter().map(KnownTorrent::from).collect(),
        }
    }
}

/// One torrent as [`KnownTorrents`] holds it: its hash and pre-normalised paths.
#[derive(Debug)]
struct KnownTorrent {
    hash: String,
    save_path: Option<PathBuf>,
    content_path: Option<PathBuf>,
}

impl From<&TorrentSummary> for KnownTorrent {
    fn from(torrent: &TorrentSummary) -> Self {
        let normalized = |path: &str| (!path.is_empty()).then(|| normalize(path));
        Self {
            hash: torrent.hash.clone(),
            save_path: normalized(&torrent.save_path),
            content_path: normalized(&torrent.content_path),
        }
    }
}

/// Write a `.torrent` into the cache, creating `torrent_dir` if this is the
/// first one. Blocking; callers run it off the runtime.
fn write_torrent_file(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, data)
}

/// A single-file torrent points `content_path` straight at the file.
/// `target` arrives already normalised.
fn matches_content_path(torrent: &KnownTorrent, target: &Path) -> bool {
    torrent.content_path.as_deref() == Some(target)
}

/// Whether this torrent's save path could contain `target` at all.
///
/// Compared component-wise, so `/downloads/tv` does not appear to contain
/// `/downloads/tv-archive/...` the way a string prefix check would.
fn could_contain(torrent: &KnownTorrent, target: &Path) -> bool {
    torrent
        .save_path
        .as_deref()
        .is_some_and(|save_path| target.starts_with(save_path))
}

/// Whether any file in the torrent resolves to `target`. `save_path` arrives
/// already normalised.
fn contains_file(save_path: &Path, files: &[TorrentFileEntry], target: &Path) -> bool {
    files
        .iter()
        .any(|file| save_path.join(&file.name) == target)
}

fn normalize(path: &str) -> PathBuf {
    normalize_path(Path::new(path))
}

/// Collapse `.` segments and trailing separators so two spellings of the same path
/// compare equal. Deliberately *not* `canonicalize`: these are paths in another
/// container's filesystem, which this process cannot stat.
fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use async_trait::async_trait;
    use sharerr_client::{ClientError, ClientKind};
    use url::Url;

    /// An in-process `TorrentClient` double — no HTTP, so `Seeder`'s own logic
    /// (as opposed to any one client's wire format) can be exercised alone.
    /// `files_result` is a closure so a test can fail for one hash and succeed
    /// for another, the way `find_existing`'s multi-candidate loop needs.
    type FilesFn = dyn Fn(&str) -> sharerr_client::Result<Vec<TorrentFileEntry>> + Send + Sync;

    #[derive(Default)]
    struct StubClient {
        files_result: Option<Box<FilesFn>>,
        set_trackers_calls: std::sync::Mutex<Vec<(String, Vec<Url>)>>,
        add_trackers_calls: std::sync::Mutex<Vec<(String, Vec<Url>)>>,
        add_calls: std::sync::Mutex<Vec<(String, Vec<u8>)>>,
        set_limits_calls: std::sync::Mutex<Vec<(String, sharerr_client::SeedingLimits)>>,
        /// `.torrent` bytes `export` hands back, or `None` for a client that
        /// has no such call — the Transmission and rTorrent case.
        export_result: Option<Vec<u8>>,
    }

    impl std::fmt::Debug for StubClient {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("StubClient").finish_non_exhaustive()
        }
    }

    #[async_trait]
    impl TorrentClient for StubClient {
        fn kind(&self) -> ClientKind {
            ClientKind::QBittorrent
        }
        async fn login(&self) -> sharerr_client::Result<()> {
            Ok(())
        }
        async fn version(&self) -> sharerr_client::Result<String> {
            Ok("stub".to_owned())
        }
        async fn list(
            &self,
            _category: Option<&str>,
        ) -> sharerr_client::Result<Vec<TorrentSummary>> {
            Ok(Vec::new())
        }
        async fn files(&self, hash: &str) -> sharerr_client::Result<Vec<TorrentFileEntry>> {
            match &self.files_result {
                Some(f) => f(hash),
                None => Ok(Vec::new()),
            }
        }
        async fn add(&self, request: &AddRequest<'_>) -> sharerr_client::Result<()> {
            self.add_calls
                .lock()
                .unwrap()
                .push((request.info_hash.to_owned(), request.data.to_vec()));
            Ok(())
        }
        async fn remove(&self, _hash: &str) -> sharerr_client::Result<()> {
            Ok(())
        }
        async fn set_trackers(&self, hash: &str, urls: &[Url]) -> sharerr_client::Result<()> {
            self.set_trackers_calls
                .lock()
                .unwrap()
                .push((hash.to_owned(), urls.to_vec()));
            Ok(())
        }
        async fn add_trackers(&self, hash: &str, urls: &[Url]) -> sharerr_client::Result<()> {
            self.add_trackers_calls
                .lock()
                .unwrap()
                .push((hash.to_owned(), urls.to_vec()));
            Ok(())
        }
        async fn set_limits(
            &self,
            hash: &str,
            limits: &sharerr_client::SeedingLimits,
        ) -> sharerr_client::Result<()> {
            self.set_limits_calls
                .lock()
                .unwrap()
                .push((hash.to_owned(), *limits));
            Ok(())
        }
        async fn export(&self, _hash: &str) -> sharerr_client::Result<Option<Vec<u8>>> {
            Ok(self.export_result.clone())
        }
    }

    fn seeder(qbit: Arc<dyn TorrentClient>, torrent_dir: PathBuf) -> Seeder {
        crate::test_support::trace();
        Seeder {
            qbit,
            category: "sharerr".to_owned(),
            tag: "sharerr".to_owned(),
            skip_checking: true,
            upload_limit_kib: None,
            ratio_limit: None,
            private: true,
            torrent_dir,
        }
    }

    #[test]
    fn the_debug_impl_names_the_configuration_without_the_client() {
        let seeder = seeder(Arc::new(StubClient::default()), PathBuf::from("/torrents"));
        let text = format!("{seeder:?}");
        assert!(text.contains("sharerr"), "{text}");
        assert!(text.contains("skip_checking"), "{text}");
    }

    #[test]
    fn could_contain_is_false_for_an_empty_save_path() {
        let existing = torrent("aa", "", "");
        assert!(!could_contain(&existing, Path::new("/downloads/tv/x.mkv")));
    }

    #[tokio::test]
    async fn find_existing_skips_a_torrent_whose_files_cannot_be_listed() {
        // "aa" fails to list, "bb" succeeds and does not contain the target —
        // the search must survive the failure and keep looking rather than
        // stopping (or wrongly reporting a match) at the first error.
        let client = StubClient {
            files_result: Some(Box::new(|hash| match hash {
                "aa" => Err(ClientError::Config("boom".to_owned())),
                _ => Ok(vec![file("other.mkv")]),
            })),
            ..StubClient::default()
        };
        let seeder = seeder(Arc::new(client), PathBuf::from("/torrents"));
        let torrents = KnownTorrents::index(&[
            summary("aa", "/downloads/tv", ""),
            summary("bb", "/downloads/tv", ""),
        ]);

        let found = seeder
            .find_existing(&torrents, Path::new("/downloads/tv/target.mkv"))
            .await
            .unwrap();
        assert_eq!(found, None);
    }

    #[tokio::test]
    async fn refresh_announce_with_no_cached_file_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let seeder = seeder(Arc::new(StubClient::default()), dir.path().to_path_buf());
        let announce = AnnounceSet::single(Url::parse("http://tracker.example/announce").unwrap());

        let outcome = seeder
            .refresh_announce("deadbeef", &announce)
            .await
            .unwrap();
        assert_eq!(outcome, AnnounceRefresh::NoCachedTorrent);
    }

    #[tokio::test]
    async fn refresh_announce_updates_the_client_and_rewrites_a_stale_cached_file() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("movie.mkv");
        std::fs::write(&media, b"pretend media bytes").unwrap();

        let old_announce = AnnounceSet::single(Url::parse("http://old.example/announce").unwrap());
        let built = TorrentFactory
            .create(&TorrentRequest {
                path: &media,
                announce: &old_announce,
                media: None,
                private: true,
            })
            .unwrap();

        let torrent_dir = dir.path().join("torrents");
        std::fs::create_dir(&torrent_dir).unwrap();
        std::fs::write(
            torrent_file_path(&torrent_dir, &built.info_hash),
            &built.data,
        )
        .unwrap();

        let client = Arc::new(StubClient::default());
        let seeder = seeder(client.clone(), torrent_dir.clone());
        let new_announce = AnnounceSet::single(Url::parse("http://new.example/announce").unwrap());

        let outcome = seeder
            .refresh_announce(&built.info_hash, &new_announce)
            .await
            .unwrap();
        assert_eq!(outcome, AnnounceRefresh::Updated);
        assert_eq!(client.set_trackers_calls.lock().unwrap().len(), 1);

        let rewritten = std::fs::read(torrent_file_path(&torrent_dir, &built.info_hash)).unwrap();
        let current = sharerr_torrent::read_announce(&rewritten).unwrap();
        assert_eq!(current.as_deref(), Some("http://new.example/announce"));

        // Running it again against the now-current file must be a no-op.
        let outcome_again = seeder
            .refresh_announce(&built.info_hash, &new_announce)
            .await
            .unwrap();
        assert_eq!(outcome_again, AnnounceRefresh::Current);
        assert_eq!(client.set_trackers_calls.lock().unwrap().len(), 1);
    }

    /// A torrent the client no longer has (a reinstall, a wiped session) must
    /// not be re-hashed from the media file when sharerr's own cache under
    /// `torrent_dir` already has the very `.torrent` that was built for it.
    /// The new size check added alongside this test needs the file to still
    /// exist (it stats it), so unlike before this test cannot prove reuse by
    /// deleting the file out from under `seed` — instead it locks the file's
    /// permissions to 0, the same technique
    /// `web::probe::library_badge_reports_an_unreadable_directory` uses: a
    /// fallback to `build` would fail trying to actually read the file to
    /// hash it, while the new stat-only size check (which needs no read
    /// permission on the file itself) still succeeds. Running as root
    /// ignores permission bits and would let a rebuild through too — see
    /// that same test's comment — but a rebuild over an unchanged file with
    /// no `comment` field is deterministic, so the byte-for-byte assertion
    /// below still holds either way.
    #[cfg(unix)]
    #[tokio::test]
    async fn seed_reuses_a_cached_torrent_for_a_vanished_client_entry_with_a_current_announce() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("movie.mkv");
        std::fs::write(&media, b"pretend media bytes").unwrap();

        let announce = AnnounceSet::single(Url::parse("http://tracker.example/announce").unwrap());
        let built = TorrentFactory
            .create(&TorrentRequest {
                path: &media,
                announce: &announce,
                media: None,
                private: true,
            })
            .unwrap();

        let torrent_dir = dir.path().join("torrents");
        std::fs::create_dir(&torrent_dir).unwrap();
        std::fs::write(
            torrent_file_path(&torrent_dir, &built.info_hash),
            &built.data,
        )
        .unwrap();
        std::fs::set_permissions(&media, std::fs::Permissions::from_mode(0o000)).unwrap();

        let client = Arc::new(StubClient::default());
        let seeder = seeder(client.clone(), torrent_dir.clone());
        let paths = ResolvedPaths {
            arr: media.clone(),
            sharerr: media.clone(),
            qbit: PathBuf::from("/downloads/movie.mkv"),
            mapping_applied: false,
        };

        let outcome = seeder
            .seed(
                &paths,
                &announce,
                &KnownTorrents::default(),
                Some(&built.info_hash),
                None,
            )
            .await
            .unwrap();
        std::fs::set_permissions(&media, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            outcome,
            SeedOutcome::Added {
                info_hash: built.info_hash.clone(),
                private: true,
            }
        );

        let add_calls = client.add_calls.lock().unwrap();
        assert_eq!(add_calls.len(), 1);
        assert_eq!(add_calls[0].0, built.info_hash);
        assert_eq!(add_calls[0].1, built.data, "the cached bytes, unmodified");
    }

    /// As above, but the endpoint moved since the `.torrent` was cached: the
    /// announce must be brought up to date, in the bytes handed to the
    /// client *and* in the on-disk cache — the same guarantee
    /// `refresh_announce` gives an already-seeding torrent — without ever
    /// reading the (unreadable) media file. See the previous test's comment
    /// for why permissions rather than deletion prove that now.
    #[cfg(unix)]
    #[tokio::test]
    async fn seed_reuses_and_refreshes_a_stale_cached_torrent_for_a_vanished_client_entry() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("movie.mkv");
        std::fs::write(&media, b"pretend media bytes").unwrap();

        let old_announce = AnnounceSet::single(Url::parse("http://old.example/announce").unwrap());
        let built = TorrentFactory
            .create(&TorrentRequest {
                path: &media,
                announce: &old_announce,
                media: None,
                private: true,
            })
            .unwrap();

        let torrent_dir = dir.path().join("torrents");
        std::fs::create_dir(&torrent_dir).unwrap();
        std::fs::write(
            torrent_file_path(&torrent_dir, &built.info_hash),
            &built.data,
        )
        .unwrap();
        std::fs::set_permissions(&media, std::fs::Permissions::from_mode(0o000)).unwrap();

        let client = Arc::new(StubClient::default());
        let seeder = seeder(client.clone(), torrent_dir.clone());
        let new_announce = AnnounceSet::single(Url::parse("http://new.example/announce").unwrap());
        let paths = ResolvedPaths {
            arr: media.clone(),
            sharerr: media.clone(),
            qbit: PathBuf::from("/downloads/movie.mkv"),
            mapping_applied: false,
        };

        let outcome = seeder
            .seed(
                &paths,
                &new_announce,
                &KnownTorrents::default(),
                Some(&built.info_hash),
                None,
            )
            .await
            .unwrap();
        std::fs::set_permissions(&media, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            outcome,
            SeedOutcome::Added {
                info_hash: built.info_hash.clone(),
                private: true,
            }
        );

        let add_calls = client.add_calls.lock().unwrap();
        assert_eq!(add_calls.len(), 1);
        assert_eq!(add_calls[0].0, built.info_hash);
        let handed_to_client = sharerr_torrent::read_announce(&add_calls[0].1).unwrap();
        assert_eq!(
            handed_to_client.as_deref(),
            Some("http://new.example/announce")
        );

        let cached = std::fs::read(torrent_file_path(&torrent_dir, &built.info_hash)).unwrap();
        assert_eq!(
            sharerr_torrent::read_announce(&cached).unwrap().as_deref(),
            Some("http://new.example/announce"),
            "the on-disk cache must be refreshed too, not just the bytes sent to the client"
        );
    }

    /// The gap this covers: an in-place upgrade under the same `file_id`.
    /// Unlike the two tests above, `media` is left in place — resized rather
    /// than deleted — so the cached `.torrent` still parses and its info
    /// hash still matches `known_info_hash`, but it now describes bytes that
    /// no longer exist. `seed` must detect that and rebuild rather than
    /// hand the client a torrent for the old content.
    #[tokio::test]
    async fn seed_rebuilds_a_cached_torrent_whose_size_no_longer_matches_the_file_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("movie.mkv");
        std::fs::write(&media, b"pretend media bytes").unwrap();

        let announce = AnnounceSet::single(Url::parse("http://tracker.example/announce").unwrap());
        let stale = TorrentFactory
            .create(&TorrentRequest {
                path: &media,
                announce: &announce,
                media: None,
                private: true,
            })
            .unwrap();

        let torrent_dir = dir.path().join("torrents");
        std::fs::create_dir(&torrent_dir).unwrap();
        std::fs::write(
            torrent_file_path(&torrent_dir, &stale.info_hash),
            &stale.data,
        )
        .unwrap();

        // The upgrade: same path, different (and differently sized) bytes.
        // No delete — proving this test's point requires the file to still
        // be there for the new check to read.
        std::fs::write(&media, b"a longer, different cut of the same episode").unwrap();

        let client = Arc::new(StubClient::default());
        let seeder = seeder(client.clone(), torrent_dir.clone());
        let paths = ResolvedPaths {
            arr: media.clone(),
            sharerr: media.clone(),
            qbit: PathBuf::from("/downloads/movie.mkv"),
            mapping_applied: false,
        };

        let outcome = seeder
            .seed(
                &paths,
                &announce,
                &KnownTorrents::default(),
                Some(&stale.info_hash),
                None,
            )
            .await
            .unwrap();

        // A rebuild over the new bytes has a different info hash than the
        // stale cache entry — that difference is itself the proof the stale
        // cache was not reused.
        let rebuilt = TorrentFactory
            .create(&TorrentRequest {
                path: &media,
                announce: &announce,
                media: None,
                private: true,
            })
            .unwrap();
        assert_ne!(rebuilt.info_hash, stale.info_hash);
        assert_eq!(
            outcome,
            SeedOutcome::Added {
                info_hash: rebuilt.info_hash.clone(),
                private: true,
            }
        );

        let add_calls = client.add_calls.lock().unwrap();
        assert_eq!(add_calls.len(), 1);
        assert_eq!(add_calls[0].0, rebuilt.info_hash);
        assert_eq!(
            add_calls[0].1, rebuilt.data,
            "the client must receive a fresh torrent over the file as it is now, not the stale cache"
        );
    }

    /// No cache under the known hash (never cached, or the file was lost) —
    /// `seed` must still fall back to a full rebuild rather than failing the
    /// item, exactly as it did before `known_info_hash` existed.
    #[tokio::test]
    async fn seed_falls_back_to_building_when_the_known_hash_has_nothing_cached() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("movie.mkv");
        std::fs::write(&media, b"pretend media bytes").unwrap();

        let torrent_dir = dir.path().join("torrents");
        let client = Arc::new(StubClient::default());
        let seeder = seeder(client.clone(), torrent_dir);
        let announce = AnnounceSet::single(Url::parse("http://tracker.example/announce").unwrap());
        let paths = ResolvedPaths {
            arr: media.clone(),
            sharerr: media.clone(),
            qbit: PathBuf::from("/downloads/movie.mkv"),
            mapping_applied: false,
        };

        let outcome = seeder
            .seed(
                &paths,
                &announce,
                &KnownTorrents::default(),
                Some("stale-hash-nothing-cached"),
                None,
            )
            .await
            .unwrap();
        assert!(matches!(outcome, SeedOutcome::Added { .. }));
        assert_eq!(client.add_calls.lock().unwrap().len(), 1);
    }

    /// Adoption, end to end: a torrent the operator already had covers the
    /// file, so `seed` reuses it — and must leave behind the two things a
    /// friend's download needs, which reusing it alone did not.
    #[tokio::test]
    async fn seed_adopts_a_pre_existing_torrent_by_adding_its_tracker_and_caching_it() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("movie.mkv");
        std::fs::write(&media, b"pretend media bytes").unwrap();

        // Stand in for the operator's own torrent: built against *their*
        // tracker, and never seen by sharerr.
        let theirs = TorrentFactory
            .create(&TorrentRequest {
                path: &media,
                announce: &AnnounceSet::single(
                    Url::parse("http://their-tracker.example/announce").unwrap(),
                ),
                media: None,
                private: true,
            })
            .unwrap();

        let torrent_dir = dir.path().join("torrents");
        let client = Arc::new(StubClient {
            export_result: Some(theirs.data.clone()),
            ..StubClient::default()
        });
        let seeder = seeder(client.clone(), torrent_dir.clone());
        let announce = AnnounceSet::single(Url::parse("http://sharerr.example/announce").unwrap());
        let paths = ResolvedPaths {
            arr: media.clone(),
            sharerr: media.clone(),
            qbit: PathBuf::from("/downloads/movie.mkv"),
            mapping_applied: false,
        };
        let existing = [summary(
            &theirs.info_hash,
            "/downloads",
            "/downloads/movie.mkv",
        )];

        let outcome = seeder
            .seed(
                &paths,
                &announce,
                &KnownTorrents::index(&existing),
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            outcome,
            SeedOutcome::Reused {
                info_hash: theirs.info_hash.clone()
            }
        );
        assert!(
            client.add_calls.lock().unwrap().is_empty(),
            "adoption must not add a second torrent for the same file"
        );

        // Additive, not a replace: their tracker list is theirs.
        let added = client.add_trackers_calls.lock().unwrap();
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].0, theirs.info_hash);
        assert_eq!(added[0].1, announce.tiers);
        assert!(
            client.set_trackers_calls.lock().unwrap().is_empty(),
            "set_trackers would drop the trackers the torrent came with"
        );

        // And the feed now has something to serve, announcing to sharerr.
        let cached = std::fs::read(torrent_file_path(&torrent_dir, &theirs.info_hash)).unwrap();
        assert_eq!(
            sharerr_torrent::read_announce(&cached).unwrap().as_deref(),
            Some(announce.primary.as_str())
        );
        // Rewriting the announce must not disturb the info dict — a cached
        // file under a different infohash than the torrent being seeded is
        // exactly the 404 this is here to prevent.
        assert_eq!(
            sharerr_torrent::read_info_hash(&cached).unwrap(),
            theirs.info_hash
        );
    }

    /// The same adoption against a client that cannot export — Transmission
    /// and rTorrent both return `Ok(None)`. There is no way to serve this
    /// release, so the item fails with a message naming the choice, rather
    /// than being published as a download nobody can complete.
    #[tokio::test]
    async fn seed_fails_an_adoption_when_the_client_cannot_hand_back_the_torrent() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("movie.mkv");
        std::fs::write(&media, b"pretend media bytes").unwrap();

        let client = Arc::new(StubClient::default());
        let seeder = seeder(client.clone(), dir.path().join("torrents"));
        let announce = AnnounceSet::single(Url::parse("http://sharerr.example/announce").unwrap());
        let paths = ResolvedPaths {
            arr: media.clone(),
            sharerr: media.clone(),
            qbit: PathBuf::from("/downloads/movie.mkv"),
            mapping_applied: false,
        };
        let existing = [summary("deadbeef", "/downloads", "/downloads/movie.mkv")];

        let err = seeder
            .seed(
                &paths,
                &announce,
                &KnownTorrents::index(&existing),
                None,
                None,
            )
            .await
            .unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("cannot"), "{text}");
        assert!(text.contains("deadbeef"), "{text}");
    }

    /// Rediscovering a torrent sharerr itself added takes the same reuse
    /// branch, and must not export over its own cache — the bytes on disk are
    /// the ones the feed has been serving all along.
    #[tokio::test]
    async fn adopting_a_torrent_sharerr_already_cached_leaves_the_cached_file_alone() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("movie.mkv");
        std::fs::write(&media, b"pretend media bytes").unwrap();

        let announce = AnnounceSet::single(Url::parse("http://sharerr.example/announce").unwrap());
        let built = TorrentFactory
            .create(&TorrentRequest {
                path: &media,
                announce: &announce,
                media: None,
                private: true,
            })
            .unwrap();

        let torrent_dir = dir.path().join("torrents");
        std::fs::create_dir(&torrent_dir).unwrap();
        let cached_path = torrent_file_path(&torrent_dir, &built.info_hash);
        std::fs::write(&cached_path, &built.data).unwrap();

        // Exporting would replace the cache with these — so if they turn up on
        // disk, the cache was not consulted first.
        let client = Arc::new(StubClient {
            export_result: Some(b"not a torrent".to_vec()),
            ..StubClient::default()
        });
        let seeder = seeder(client.clone(), torrent_dir.clone());
        let paths = ResolvedPaths {
            arr: media.clone(),
            sharerr: media.clone(),
            qbit: PathBuf::from("/downloads/movie.mkv"),
            mapping_applied: false,
        };
        let existing = [summary(
            &built.info_hash,
            "/downloads",
            "/downloads/movie.mkv",
        )];

        let outcome = seeder
            .seed(
                &paths,
                &announce,
                &KnownTorrents::index(&existing),
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            outcome,
            SeedOutcome::Reused {
                info_hash: built.info_hash.clone()
            }
        );
        assert_eq!(std::fs::read(&cached_path).unwrap(), built.data);
        // The tracker still goes in: a torrent the client has and sharerr has
        // cached may still have been added by hand from that same cached file.
        assert_eq!(client.add_trackers_calls.lock().unwrap().len(), 1);
    }

    fn torrent(hash: &str, save_path: &str, content_path: &str) -> KnownTorrent {
        KnownTorrent::from(&summary(hash, save_path, content_path))
    }

    fn summary(hash: &str, save_path: &str, content_path: &str) -> TorrentSummary {
        TorrentSummary {
            hash: hash.to_owned(),
            name: "whatever".to_owned(),
            save_path: save_path.to_owned(),
            content_path: content_path.to_owned(),
            category: String::new(),
            tags: Vec::new(),
            is_seeding: true,
            ratio: None,
            ratio_limit: None,
            upload_limit_kib: None,
        }
    }

    fn file(name: &str) -> TorrentFileEntry {
        TorrentFileEntry {
            name: name.to_owned(),
            size: 1,
        }
    }

    #[test]
    fn a_single_file_torrent_is_matched_by_its_content_path() {
        let existing = torrent(
            "aa",
            "/downloads/tv",
            "/downloads/tv/lanternwick.s02e01.mkv",
        );
        assert!(matches_content_path(
            &existing,
            Path::new("/downloads/tv/lanternwick.s02e01.mkv")
        ));
        assert!(!matches_content_path(
            &existing,
            Path::new("/downloads/tv/other.mkv")
        ));
    }

    #[test]
    fn trailing_separators_and_dot_segments_do_not_defeat_matching() {
        let existing = torrent(
            "aa",
            "/downloads/tv/",
            "/downloads/tv/./lanternwick.s02e01.mkv",
        );
        assert!(matches_content_path(
            &existing,
            Path::new("/downloads/tv/lanternwick.s02e01.mkv")
        ));
    }

    #[test]
    fn an_empty_content_path_matches_nothing() {
        let existing = torrent("aa", "/downloads", "");
        assert!(!matches_content_path(&existing, Path::new("/downloads")));
    }

    /// The check that keeps a neighbouring directory from looking like a parent.
    #[test]
    fn save_path_containment_respects_component_boundaries() {
        let existing = torrent("aa", "/downloads/tv", "");
        assert!(could_contain(
            &existing,
            Path::new("/downloads/tv/Show/ep.mkv")
        ));
        assert!(!could_contain(
            &existing,
            Path::new("/downloads/tv-archive/Show/ep.mkv")
        ));
        assert!(!could_contain(
            &existing,
            Path::new("/media/tv/Show/ep.mkv")
        ));
    }

    #[test]
    fn a_multi_file_torrent_is_matched_through_its_file_list() {
        let files = [
            file("Lanternwick Hollow/lanternwick.s02e01.mkv"),
            file("Lanternwick Hollow/lanternwick.s02e02.mkv"),
        ];

        assert!(contains_file(
            Path::new("/downloads/tv"),
            &files,
            Path::new("/downloads/tv/Lanternwick Hollow/lanternwick.s02e01.mkv")
        ));
        assert!(!contains_file(
            Path::new("/downloads/tv"),
            &files,
            Path::new("/downloads/tv/Lanternwick Hollow/lanternwick.s02e09.mkv")
        ));
    }

    #[test]
    fn a_file_under_a_different_save_path_is_not_matched() {
        let files = [file("lanternwick.s02e01.mkv")];
        assert!(!contains_file(
            Path::new("/downloads/other"),
            &files,
            Path::new("/downloads/tv/lanternwick.s02e01.mkv")
        ));
    }

    // ------------------------------------------------------ the long tail

    fn built(media: &Path, announce: &str) -> sharerr_torrent::BuiltTorrent {
        TorrentFactory
            .create(&TorrentRequest {
                path: media,
                announce: &AnnounceSet::single(Url::parse(announce).unwrap()),
                media: None,
                private: true,
            })
            .unwrap()
    }

    /// Adopting a torrent sharerr had cached under a *stale* announce: the
    /// cache is rewritten in place, and the client is still only told to add
    /// the tracker, never to replace its list.
    #[tokio::test]
    async fn adopting_a_cached_torrent_with_a_stale_announce_rewrites_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("movie.mkv");
        std::fs::write(&media, b"pretend media bytes").unwrap();
        let old = built(&media, "http://old.example/announce");
        let torrent_dir = dir.path().join("torrents");
        let cached = torrent_file_path(&torrent_dir, &old.info_hash);
        write_torrent_file(&cached, &old.data).unwrap();

        let client = Arc::new(StubClient::default());
        let seeder = seeder(client.clone(), torrent_dir);
        let announce = AnnounceSet::single(Url::parse("http://sharerr.example/announce").unwrap());

        seeder.adopt(&old.info_hash, &announce).await.unwrap();

        let rewritten = std::fs::read(&cached).unwrap();
        assert_eq!(
            sharerr_torrent::read_announce(&rewritten)
                .unwrap()
                .as_deref(),
            Some(announce.primary.as_str())
        );
        assert_eq!(client.add_trackers_calls.lock().unwrap().len(), 1);
    }

    /// An exported torrent that already announces to sharerr is cached as is
    /// — never cached before, so it still has to be verified and written.
    #[tokio::test]
    async fn adopting_an_export_that_already_announces_to_sharerr_caches_it_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("movie.mkv");
        std::fs::write(&media, b"pretend media bytes").unwrap();
        let theirs = built(&media, "http://sharerr.example/announce");
        let torrent_dir = dir.path().join("torrents");
        let client = Arc::new(StubClient {
            export_result: Some(theirs.data.clone()),
            ..StubClient::default()
        });
        let seeder = seeder(client, torrent_dir.clone());
        let announce = AnnounceSet::single(Url::parse("http://sharerr.example/announce").unwrap());

        seeder.adopt(&theirs.info_hash, &announce).await.unwrap();

        let cached = std::fs::read(torrent_file_path(&torrent_dir, &theirs.info_hash)).unwrap();
        assert_eq!(cached, theirs.data);
    }

    /// The client hands back a `.torrent` for a different swarm than the one
    /// it was asked about: refused, and nothing is cached under either hash.
    #[tokio::test]
    async fn adopting_refuses_an_export_whose_info_hash_does_not_match() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("movie.mkv");
        std::fs::write(&media, b"pretend media bytes").unwrap();
        let other = dir.path().join("other.mkv");
        std::fs::write(&other, b"entirely different bytes").unwrap();
        let wrong = built(&other, "http://their-tracker.example/announce");
        let asked_for = built(&media, "http://their-tracker.example/announce");
        let torrent_dir = dir.path().join("torrents");
        let client = Arc::new(StubClient {
            export_result: Some(wrong.data),
            ..StubClient::default()
        });
        let seeder = seeder(client, torrent_dir.clone());
        let announce = AnnounceSet::single(Url::parse("http://sharerr.example/announce").unwrap());

        let err = seeder
            .adopt(&asked_for.info_hash, &announce)
            .await
            .unwrap_err();

        assert!(
            format!("{err:#}").contains("handed back a .torrent for"),
            "{err:#}"
        );
        assert!(!torrent_file_path(&torrent_dir, &asked_for.info_hash).exists());
    }

    async fn seed_with_known_hash(dir: &Path, hash: &str, client: Arc<StubClient>) -> SeedOutcome {
        let media = dir.join("movie.mkv");
        std::fs::write(&media, b"pretend media bytes").unwrap();
        let seeder = seeder(client, dir.join("torrents"));
        let announce = AnnounceSet::single(Url::parse("http://sharerr.example/announce").unwrap());
        let paths = ResolvedPaths {
            arr: media.clone(),
            sharerr: media.clone(),
            qbit: PathBuf::from("/downloads/movie.mkv"),
            mapping_applied: false,
        };
        seeder
            .seed(
                &paths,
                &announce,
                &KnownTorrents::default(),
                Some(hash),
                None,
            )
            .await
            .unwrap()
    }

    /// A cache entry that cannot be read or parsed is a reason to rebuild,
    /// not to fail the item.
    #[tokio::test]
    async fn seed_rebuilds_when_the_cached_torrent_is_unreadable_or_unparseable() {
        let dir = tempfile::tempdir().unwrap();
        let torrent_dir = dir.path().join("torrents");

        // Unreadable: the cache path is a directory.
        std::fs::create_dir_all(torrent_file_path(&torrent_dir, "unreadable")).unwrap();
        let client = Arc::new(StubClient::default());
        let outcome = seed_with_known_hash(dir.path(), "unreadable", client.clone()).await;
        assert!(matches!(outcome, SeedOutcome::Added { .. }), "{outcome:?}");

        // Unparseable: garbage where bencode should be.
        write_torrent_file(&torrent_file_path(&torrent_dir, "garbage"), b"not bencode").unwrap();
        let outcome = seed_with_known_hash(dir.path(), "garbage", client.clone()).await;
        assert!(matches!(outcome, SeedOutcome::Added { .. }), "{outcome:?}");
        assert_eq!(client.add_calls.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_media_file_that_cannot_be_stat_ed_counts_as_a_stale_cache() {
        let seeder = seeder(Arc::new(StubClient::default()), PathBuf::from("/torrents"));
        assert!(
            seeder
                .cached_size_is_stale(
                    Path::new("/nonexistent/movie.mkv"),
                    1,
                    "hash",
                    Path::new("/torrents/hash.torrent"),
                )
                .await
        );
    }

    /// A client entry with no save path cannot cover anything; the search
    /// moves on rather than matching it against an empty prefix.
    #[tokio::test]
    async fn find_existing_skips_a_torrent_with_no_save_path() {
        let seeder = seeder(Arc::new(StubClient::default()), PathBuf::from("/torrents"));
        let known = KnownTorrents::index(&[summary("abc", "", "")]);
        let found = seeder
            .find_existing(&known, Path::new("/downloads/movie.mkv"))
            .await
            .unwrap();
        assert!(found.is_none());
    }
}
