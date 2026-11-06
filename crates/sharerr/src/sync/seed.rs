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
//!
//! Before building, a friend's torrent of the same file is preferred: joining
//! their swarm under their info hash keeps one swarm for one set of bytes. See
//! [`Seeder::fetch_from_friend`] for what that takes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use sharerr_client::{AddRequest, TorrentClient, TorrentFileEntry, TorrentSummary};
use sharerr_core::paths::ResolvedPaths;
use sharerr_core::{CatalogEntry, MediaMeta};
use sharerr_store::ItemOrigin;
use sharerr_torrent::{
    AnnounceSet, SidecarRequest, Torrent, TorrentFactory, TorrentRequest, torrent_file_path,
};
use url::Url;

/// The largest `.torrent` taken from a friend. A hybrid torrent's piece layers
/// put a large file's at a few megabytes.
const MAX_FRIEND_TORRENT_BYTES: usize = 16 * 1024 * 1024;

/// What the sync loop knows about friends' torrents of the file being seeded.
#[derive(Debug, Default, Clone, Copy)]
pub struct FriendTorrents<'a> {
    /// Friends' catalog entries matching the file, with each friend's peer id,
    /// in preference order.
    pub matches: &'a [(i64, CatalogEntry)],
    /// The friend's swarm this item joined on an earlier pass, if it did.
    pub joined: Option<&'a ItemOrigin>,
}

/// What sharerr knows about a torrent it handed to the client itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddedTorrent {
    /// Decoded from the bytes handed over; see [`Seeder::seed`].
    pub private: bool,
    /// The v2 identity of a hybrid torrent, read from the same bytes.
    pub info_hash_v2: Option<String>,
}

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
    Reused {
        info_hash: String,
        /// The trackers the client's copy came with, other than ours, kept
        /// after ours when an endpoint change rewrites its list. Empty when
        /// sharerr's own cache answered and the client was not asked.
        upstream: Vec<String>,
    },
    /// A new torrent was built and handed to qBittorrent. `private` is
    /// decoded from the actual bytes just built or reused from sharerr's own
    /// cache — see [`Seeder::seed`].
    Added {
        info_hash: String,
        private: bool,
        /// The v2 identity of a hybrid build, read from the same bytes.
        info_hash_v2: Option<String>,
    },
    /// A friend's torrent of the same file: their swarm, joined under their
    /// info hash.
    Joined {
        info_hash: String,
        origin: ItemOrigin,
        /// `Some` when sharerr handed the friend's torrent to the client, so it
        /// is sharerr's to withdraw; `None` when the client already held it,
        /// usually as the download the file came from.
        added: Option<AddedTorrent>,
    },
}

impl SeedOutcome {
    pub fn info_hash(&self) -> &str {
        match self {
            Self::Reused { info_hash, .. }
            | Self::Added { info_hash, .. }
            | Self::Joined { info_hash, .. } => info_hash,
        }
    }
}

pub struct Seeder {
    pub qbit: Arc<dyn TorrentClient>,
    pub category: String,
    pub tag: String,
    pub skip_checking: bool,
    /// `[seeding]` in `sharerr.toml`. The upload cap and ratio goal apply at add
    /// time and are restated by `Syncer::reconcile_limits` on torrents sharerr
    /// already created; `None` in either leaves the client's own default in
    /// effect. `private` is read once per `.torrent` build, so a config change only
    /// affects torrents built after it lands.
    pub seeding: sharerr_core::config::SeedingConfig,
    /// Directory libraries whose files' torrents also carry the subtitles beside
    /// them, from `SeedingConfig::sidecars`. Read once per `.torrent` build, the
    /// same as `private`.
    pub sidecar_roots: Vec<PathBuf>,
    /// Where sharerr keeps a copy of each `.torrent` it builds.
    pub torrent_dir: PathBuf,
    /// The announce set each cached `.torrent` was last confirmed or rewritten
    /// to, by info hash, so [`Self::refresh_announce`] reads a file once per
    /// announce change rather than once per pass. Lives as long as this
    /// `Seeder` (a syncer rebuild starts it empty), which is also how long a
    /// cache file deleted underneath goes unnoticed.
    pub confirmed: Mutex<HashMap<String, AnnounceSet>>,
    /// Fetches a friend's `.torrent` from the link their catalog gave.
    pub friends_http: reqwest::Client,
}

impl std::fmt::Debug for Seeder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Seeder")
            .field("category", &self.category)
            .field("tag", &self.tag)
            .field("skip_checking", &self.skip_checking)
            .field("seeding", &self.seeding)
            .field("sidecar_roots", &self.sidecar_roots)
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
    /// sound: a torrent this pass adds holds one media file, plus at most its
    /// subtitles, and each discovered item is a distinct media file, so a later
    /// item can never be covered by an earlier add.
    ///
    /// `known_info_hash` is the hash sharerr last recorded for this item, if
    /// any — set when the client no longer has the torrent (a reinstall, a
    /// wiped session) but sharerr's own cache under `torrent_dir` might still.
    /// Reusing that cached `.torrent` skips re-hashing the file, which for a
    /// media library is gigabytes of CPU `find_existing` above cannot save
    /// this call from, since the whole reason this path is reached is that
    /// `torrents` no longer contains a match.
    ///
    /// `friends` comes between the two: a friend's torrent the client already
    /// holds is adopted like any other, and one it does not is fetched and
    /// checked against the file before anything is built.
    pub async fn seed(
        &self,
        paths: &ResolvedPaths,
        announce: &AnnounceSet,
        torrents: &[TorrentSummary],
        known_info_hash: Option<&str>,
        media: Option<&MediaMeta>,
        friends: FriendTorrents<'_>,
    ) -> Result<SeedOutcome> {
        if let Some(existing) = self.find_existing(torrents, &paths.qbit).await? {
            tracing::info!(
                file = %paths.qbit.display(),
                info_hash = %existing,
                "already covered by an existing torrent — reusing it"
            );
            let upstream = self.adopt(&existing, announce).await?;
            return Ok(SeedOutcome::Reused {
                info_hash: existing,
                upstream,
            });
        }

        // A friend's torrent of this file that the client already holds.
        let held = friends.matches.iter().find(|(_, entry)| {
            torrents
                .iter()
                .any(|t| t.hash.eq_ignore_ascii_case(&entry.info_hash))
        });
        if let Some((peer_id, entry)) = held {
            tracing::info!(
                file = %paths.qbit.display(),
                info_hash = %entry.info_hash,
                "a friend's torrent of this file is already in the client — joining its swarm"
            );
            let upstream = self.adopt(&entry.info_hash, announce).await?;
            return Ok(SeedOutcome::Joined {
                info_hash: entry.info_hash.clone(),
                origin: ItemOrigin {
                    peer_id: Some(*peer_id),
                    upstream,
                },
                added: None,
            });
        }

        let (info_hash, data, joined) =
            match self.reuse_cached(known_info_hash, announce, paths).await {
                // A cached friend's torrent goes back with their tracker kept.
                Some((hash, data)) => match friends.joined {
                    Some(origin) => (
                        hash,
                        with_upstream(&data, announce, &origin.upstream)?,
                        Some(origin.clone()),
                    ),
                    None => (hash, data, None),
                },
                None => match self
                    .fetch_from_friends(paths, announce, friends.matches)
                    .await
                {
                    Some((hash, data, origin)) => (hash, data, Some(origin)),
                    None => {
                        let built = self.build(paths, announce, media).await?;
                        (built.info_hash, built.data, None)
                    }
                },
            };

        // Read from the bytes rather than the current config: a cached `.torrent`
        // may predate a later change, and this is what the client is handed. An
        // unparseable one falls back to conservative answers — it should not
        // happen, and privateness is not something to guess public.
        let parsed = sharerr_torrent::Torrent::read_from_bytes(&data).ok();
        let private = parsed
            .as_ref()
            .is_none_or(sharerr_torrent::Torrent::is_private);
        let info_hash_v2 = parsed
            .as_ref()
            .and_then(sharerr_torrent::Torrent::info_hash_v2);
        let root_folder = parsed
            .as_ref()
            .filter(|t| !t.is_single_file())
            .and_then(|t| t.name());

        // qBittorrent needs the directory the content sits in, not the file: for
        // a multi-file torrent, the parent of the root directory it names.
        let no_parent = || format!("{} has no parent directory", paths.qbit.display());
        let mut save_path = paths.qbit.parent().with_context(no_parent)?;
        if root_folder.is_some() {
            save_path = save_path.parent().with_context(no_parent)?;
        }

        let filename = sharerr_torrent::torrent_file_name(&info_hash);
        let save_path = save_path.to_string_lossy();
        let mut request = AddRequest::new(&data, &info_hash, &filename, &save_path)
            .category(&self.category)
            .tags(&self.tag)
            .skip_checking(self.skip_checking);
        if let Some(root) = root_folder {
            request = request.root_folder(root);
        }
        request = request.limits(sharerr_client::SeedingLimits {
            upload_limit_kib: self.seeding.upload_limit_kib,
            ratio_limit: self.seeding.ratio_limit,
        });
        self.qbit
            .add(&request)
            .await
            .with_context(|| format!("adding {} to {}", paths.qbit.display(), self.qbit.kind()))?;

        Ok(match joined {
            Some(origin) => SeedOutcome::Joined {
                info_hash,
                origin,
                added: Some(AddedTorrent {
                    private,
                    info_hash_v2,
                }),
            },
            None => SeedOutcome::Added {
                info_hash,
                private,
                info_hash_v2,
            },
        })
    }

    /// The first friend's torrent in `matches` that can seed the file at
    /// `paths`, as `(info hash, bytes for the client, origin)`. `None` when
    /// none can, so the caller builds; each refusal is logged, never an error.
    async fn fetch_from_friends(
        &self,
        paths: &ResolvedPaths,
        announce: &AnnounceSet,
        matches: &[(i64, CatalogEntry)],
    ) -> Option<(String, Vec<u8>, ItemOrigin)> {
        for (peer_id, entry) in matches {
            match self
                .fetch_from_friend(paths, announce, *peer_id, entry)
                .await
            {
                Ok(Some(found)) => return Some(found),
                Ok(None) => {}
                Err(err) => tracing::warn!(
                    info_hash = %entry.info_hash,
                    error = format!("{err:#}"),
                    "could not use a friend's torrent of this file — building one instead"
                ),
            }
        }
        None
    }

    /// Fetch one friend's `.torrent` and take it only if it describes this
    /// exact file where the client will look for it:
    ///
    /// - its info hash is the one the catalog named;
    /// - it is single-file and its `name` is the file's own name, since the
    ///   name is the path the client opens and cannot be changed without
    ///   changing the info hash;
    /// - it is v1-only when the client reports hybrids by their v2 hash;
    /// - every piece matches the file on disk, the same read a build costs.
    ///
    /// The cache gets the friend's torrent announcing to our tracker alone, the
    /// same as every other torrent the feed serves: their tracker tiers carry
    /// our token there and must not reach our friends. The client gets ours
    /// first and theirs after, so it announces to both swarms.
    async fn fetch_from_friend(
        &self,
        paths: &ResolvedPaths,
        announce: &AnnounceSet,
        peer_id: i64,
        entry: &CatalogEntry,
    ) -> Result<Option<(String, Vec<u8>, ItemOrigin)>> {
        let response = self
            .friends_http
            .get(&entry.download_url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            // Without the URL: its query carries our token at the friend's
            // tracker, which has no business in a log.
            .map_err(|e| anyhow::anyhow!(sharerr_client::error_chain(&e.without_url())))
            .context("fetching the friend's .torrent")?;
        if let Some(length) = response.content_length() {
            anyhow::ensure!(
                usize::try_from(length).is_ok_and(|length| length <= MAX_FRIEND_TORRENT_BYTES),
                "the friend's .torrent is {length} bytes, past the \
                 {MAX_FRIEND_TORRENT_BYTES}-byte cap"
            );
        }
        let bytes = response
            .bytes()
            .await
            .map_err(reqwest::Error::without_url)
            .context("reading the friend's .torrent")?;
        anyhow::ensure!(
            bytes.len() <= MAX_FRIEND_TORRENT_BYTES,
            "the friend's .torrent is {} bytes, past the {MAX_FRIEND_TORRENT_BYTES}-byte cap",
            bytes.len()
        );
        let torrent = Torrent::read_from_bytes(&bytes).context("parsing the friend's .torrent")?;
        let info_hash = torrent.info_hash();
        anyhow::ensure!(
            info_hash.eq_ignore_ascii_case(&entry.info_hash),
            "the friend's .torrent is {info_hash}, not the {} their catalog named",
            entry.info_hash
        );

        let file_name = paths.qbit.file_name().and_then(|name| name.to_str());
        if !torrent.is_single_file() || torrent.name() != file_name {
            tracing::info!(
                info_hash,
                theirs = torrent.name().unwrap_or("(none)"),
                ours = file_name.unwrap_or("(none)"),
                "a friend's torrent of this file names it differently — building one instead"
            );
            return Ok(None);
        }
        if torrent.info_hash_v2().is_some() && !self.qbit.kind().hybrid_torrents() {
            tracing::info!(
                info_hash,
                client = %self.qbit.kind(),
                "a friend's torrent of this file is hybrid, which this client reports by \
                 another hash — building one instead"
            );
            return Ok(None);
        }

        let path = paths.sharerr.clone();
        let parsed = torrent.clone();
        let verified = tokio::task::spawn_blocking(move || {
            sharerr_torrent::verify_single_file(&path, &parsed)
        })
        .await
        .context("verification task panicked")?
        .with_context(|| format!("verifying {}", paths.sharerr.display()))?;
        if !verified {
            tracing::warn!(
                info_hash,
                file = %paths.sharerr.display(),
                "a friend's torrent of this file does not match it piece for piece — \
                 building one instead"
            );
            return Ok(None);
        }

        let cached = sharerr_torrent::rewrite_announce(&bytes, announce)?;
        let cache = torrent_file_path(&self.torrent_dir, &info_hash);
        let target = cache.clone();
        tokio::task::spawn_blocking(move || write_torrent_file(&target, &cached))
            .await
            .context("cache write task panicked")?
            .with_context(|| format!("writing {}", cache.display()))?;

        let upstream = foreign_tiers(&torrent, announce);
        let data = with_upstream(&bytes, announce, &upstream)?;
        tracing::info!(
            info_hash,
            file = %paths.qbit.display(),
            "joining a friend's swarm for this file: their torrent verified and cached"
        );
        Ok(Some((
            info_hash,
            data,
            ItemOrigin {
                peer_id: Some(peer_id),
                upstream,
            },
        )))
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
    ///
    /// Returns the tracker tiers the client's own copy came with, other than
    /// ours, when it had to be exported; empty when sharerr's cache answered.
    async fn adopt(&self, info_hash: &str, announce: &AnnounceSet) -> Result<Vec<String>> {
        self.qbit
            .add_trackers(info_hash, announce.tiers())
            .await
            .with_context(|| {
                format!(
                    "adding sharerr's tracker to {info_hash} in {}",
                    self.qbit.kind()
                )
            })?;

        let path = torrent_file_path(&self.torrent_dir, info_hash);
        let cached = read_cached(&path)
            .await
            .with_context(|| format!("reading {}", path.display()))?;

        // A cache hit is the common case and not really an adoption at all:
        // sharerr built this torrent on an earlier run and is only rediscovering
        // it, which is also why it must not be exported over.
        let from_cache = cached.is_some();
        let bytes = match cached {
            Some(data) => data,
            None => self
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
                })?,
        };
        let outcome = sharerr_torrent::retarget_announce(&bytes, announce).with_context(|| {
            if from_cache {
                format!("parsing {}", path.display())
            } else {
                format!("rewriting the announce URLs of {info_hash}")
            }
        })?;
        let upstream = if from_cache {
            Vec::new()
        } else {
            Torrent::read_from_bytes(&bytes)
                .map(|exported| foreign_tiers(&exported, announce))
                .unwrap_or_default()
        };
        let (rewritten, actual) = match outcome {
            // Already correct: a prior `adopt` already verified and wrote this
            // cache entry, so there is nothing left to do.
            sharerr_torrent::Retargeted::Current { .. } if from_cache => return Ok(upstream),
            // Never cached before, so it still needs verifying and writing even
            // though the exported announce already matches.
            sharerr_torrent::Retargeted::Current { torrent } => (bytes, torrent.info_hash()),
            sharerr_torrent::Retargeted::Updated { data, torrent, .. } => {
                (data, torrent.info_hash())
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
        Ok(upstream)
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

        let data = match read_cached(&path).await {
            Ok(Some(data)) => data,
            Ok(None) => return None,
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
        let torrent = outcome.torrent();
        if self
            .cached_size_is_stale(
                &paths.sharerr,
                &torrent.file_paths(),
                torrent.content_length(),
                hash,
                &path,
            )
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
                    to = %announce.primary(),
                    "reusing the cached .torrent for a vanished torrent, with its announce refreshed"
                );
                Some((hash.to_owned(), rewritten))
            }
        }
    }

    /// `true` when the cached `.torrent`'s recorded size no longer matches
    /// the file `sharerr_path` now points at — an in-place upgrade under the
    /// same path, with the same info hash still recorded. `files` are a
    /// multi-file torrent's names, beside `sharerr_path`, whose sizes are summed
    /// instead; a subtitle since deleted is the same stale cache. A metadata read
    /// failure is treated the same way: not proof of anything on its own,
    /// but not worth trusting the cache over either, so it falls back to the
    /// same rebuild that would hit the file's real state regardless.
    async fn cached_size_is_stale(
        &self,
        sharerr_path: &Path,
        files: &[String],
        cached_length: u64,
        hash: &str,
        cache_path: &Path,
    ) -> bool {
        let on_disk = match content_size(sharerr_path, files).await {
            Ok(size) => size,
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
    ///
    /// A file already confirmed against this same `announce` (see
    /// [`Self::confirmed`]) is `Current` without being read again.
    ///
    /// `upstream` is a joined friend's tracker tiers ([`ItemOrigin::upstream`]),
    /// kept in the client's list after ours so the rewrite does not drop them.
    pub async fn refresh_announce(
        &self,
        info_hash: &str,
        announce: &AnnounceSet,
        upstream: &[String],
    ) -> Result<AnnounceRefresh> {
        if self.is_confirmed(info_hash, announce) {
            return Ok(AnnounceRefresh::Current);
        }
        let path = torrent_file_path(&self.torrent_dir, info_hash);
        // Nothing cached means nothing to compare or serve; the torrent in the client
        // still works, so this is not worth failing the item over, but nothing here
        // has confirmed what it announces to either, which the caller must not
        // mistake for "current".
        let Some(data) = read_cached(&path)
            .await
            .with_context(|| format!("reading {}", path.display()))?
        else {
            return Ok(AnnounceRefresh::NoCachedTorrent);
        };

        let (previous, rewritten) = match sharerr_torrent::retarget_announce(&data, announce)
            .with_context(|| format!("parsing {}", path.display()))?
        {
            sharerr_torrent::Retargeted::Current { .. } => {
                self.mark_confirmed(info_hash, announce);
                return Ok(AnnounceRefresh::Current);
            }
            sharerr_torrent::Retargeted::Updated { previous, data, .. } => (previous, data),
        };

        let mut tiers = announce.tiers().to_vec();
        tiers.extend(upstream.iter().filter_map(|url| Url::parse(url).ok()));
        self.qbit
            .set_trackers(info_hash, &tiers)
            .await
            .with_context(|| format!("updating trackers in {}", self.qbit.kind()))?;

        tokio::fs::write(&path, &rewritten)
            .await
            .with_context(|| format!("writing {}", path.display()))?;

        tracing::info!(
            info_hash,
            from = previous.as_deref().unwrap_or("(none)"),
            to = %announce.primary(),
            "announce URLs refreshed"
        );
        self.mark_confirmed(info_hash, announce);
        Ok(AnnounceRefresh::Updated)
    }

    /// The trackers an already-adopted torrent carries in the client besides
    /// ours, for one adopted before they were recorded: what the client's
    /// export lists, less every tier sharerr's cached copy or `announce` names.
    /// `None` when the client cannot export, so nothing can be learned.
    pub async fn recover_upstream(
        &self,
        info_hash: &str,
        announce: &AnnounceSet,
    ) -> Result<Option<Vec<String>>> {
        let Some(exported) = self
            .qbit
            .export(info_hash)
            .await
            .with_context(|| format!("exporting {info_hash} from {}", self.qbit.kind()))?
        else {
            return Ok(None);
        };
        let exported = Torrent::read_from_bytes(&exported)
            .with_context(|| format!("parsing {info_hash} as {} exported it", self.qbit.kind()))?;
        let path = torrent_file_path(&self.torrent_dir, info_hash);
        let ours: Vec<String> = match read_cached(&path)
            .await
            .with_context(|| format!("reading {}", path.display()))?
        {
            Some(data) => Torrent::read_from_bytes(&data)
                .map(|cached| cached.announce_tiers().into_iter().flatten().collect())
                .unwrap_or_default(),
            None => Vec::new(),
        };
        Ok(Some(
            foreign_tiers(&exported, announce)
                .into_iter()
                .filter(|url| !ours.contains(url))
                .collect(),
        ))
    }

    /// Whether `info_hash`'s cached file was last seen announcing `announce`.
    fn is_confirmed(&self, info_hash: &str, announce: &AnnounceSet) -> bool {
        self.confirmed
            .lock()
            .is_ok_and(|confirmed| confirmed.get(info_hash) == Some(announce))
    }

    /// Record that `info_hash`'s cached file announces `announce`, as of now.
    fn mark_confirmed(&self, info_hash: &str, announce: &AnnounceSet) {
        if let Ok(mut confirmed) = self.confirmed.lock() {
            confirmed.insert(info_hash.to_owned(), announce.clone());
        }
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
        let private = self.seeding.private;
        let hybrid = self.seeding.hybrid && self.qbit.kind().hybrid_torrents();
        // Owned: the closure below outlives this frame on a blocking thread.
        let media = media.cloned();
        let wants_sidecars = self.sidecar_roots.iter().any(|root| path.starts_with(root));
        // The client's view of the directory, since that is where it will look. A
        // file at that filesystem's root has no directory to name, so it stays a
        // single-file torrent.
        let root_name = paths
            .qbit
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .map(str::to_owned);

        // Hashing a media file is gigabytes of CPU work, and the cache write can
        // block for tens of milliseconds on the network mounts these deployments
        // sit on. Both stay off the runtime; doing either inline would stall
        // every other task — /health and /announce included — for the duration.
        let built = tokio::task::spawn_blocking(move || {
            let sidecars = if wants_sidecars && root_name.is_some() {
                crate::library::subtitles_beside(&path)
            } else {
                Vec::new()
            };
            let built = match root_name.as_deref().filter(|_| !sidecars.is_empty()) {
                Some(root_name) => TorrentFactory.create_with_sidecars(&SidecarRequest {
                    path: &path,
                    sidecars: &sidecars,
                    root_name,
                    announce: &announce,
                    media: media.as_ref(),
                    private,
                })?,
                None => TorrentFactory.create(&TorrentRequest {
                    path: &path,
                    announce: &announce,
                    media: media.as_ref(),
                    private,
                    hybrid,
                })?,
            };

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
        torrents: &[TorrentSummary],
        target: &Path,
    ) -> Result<Option<String>> {
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

            if contains_file(Path::new(&torrent.save_path), &files, target) {
                return Ok(Some(torrent.hash.clone()));
            }
        }

        Ok(None)
    }
}

/// The size of a torrent's content on disk: the file at `path`, or with `files`
/// (a multi-file torrent's names, in the same directory) the sum of those.
async fn content_size(path: &Path, files: &[String]) -> std::io::Result<u64> {
    if files.is_empty() {
        return Ok(tokio::fs::metadata(path).await?.len());
    }
    let dir = path.parent().unwrap_or(Path::new(""));
    let mut total = 0u64;
    for name in files {
        total = total.saturating_add(tokio::fs::metadata(dir.join(name)).await?.len());
    }
    Ok(total)
}

/// `torrent`'s announce URLs other than ours, one per tier, in order.
fn foreign_tiers(torrent: &Torrent, announce: &AnnounceSet) -> Vec<String> {
    torrent
        .announce_tiers()
        .into_iter()
        .flatten()
        .filter(|url| !announce.tiers().iter().any(|ours| ours.as_str() == url))
        .collect()
}

/// `data` announcing to our tiers first and then `upstream`'s, one URL per
/// tier: the client's copy of a joined friend's torrent.
fn with_upstream(data: &[u8], announce: &AnnounceSet, upstream: &[String]) -> Result<Vec<u8>> {
    let mut torrent = Torrent::read_from_bytes(data).context("parsing a friend's .torrent")?;
    let tiers: Vec<Vec<String>> = announce
        .tiers()
        .iter()
        .map(|url| vec![url.to_string()])
        .chain(upstream.iter().map(|url| vec![url.clone()]))
        .collect();
    torrent.announce = Some(announce.primary().to_string());
    torrent.announce_list = (tiers.len() > 1).then_some(tiers);
    Ok(torrent.encode())
}

/// Write a `.torrent` into the cache, creating `torrent_dir` if this is the
/// first one. Blocking; callers run it off the runtime.
fn write_torrent_file(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, data)
}

/// The `.torrent` at `path`, or `None` when nothing is cached there.
async fn read_cached(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    match tokio::fs::read(path).await {
        Ok(data) => Ok(Some(data)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// A single-file torrent points `content_path` straight at the file.
///
/// `Path` compares by components, so `.` segments and trailing separators are
/// two spellings of one path. Deliberately not `canonicalize`: these are paths in
/// another container's filesystem, which this process cannot stat.
fn matches_content_path(torrent: &TorrentSummary, target: &Path) -> bool {
    Path::new(&torrent.content_path) == target
}

/// Whether this torrent's save path could contain `target` at all.
///
/// Compared component-wise, so `/downloads/tv` does not appear to contain
/// `/downloads/tv-archive/...` the way a string prefix check would.
fn could_contain(torrent: &TorrentSummary, target: &Path) -> bool {
    !torrent.save_path.is_empty() && target.starts_with(&torrent.save_path)
}

/// Whether any file in the torrent resolves to `target`.
fn contains_file(save_path: &Path, files: &[TorrentFileEntry], target: &Path) -> bool {
    files
        .iter()
        .any(|file| save_path.join(&file.name) == target)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::test_support::StubClient;
    use sharerr_client::ClientError;
    use url::Url;

    fn seeder(qbit: Arc<dyn TorrentClient>, torrent_dir: PathBuf) -> Seeder {
        crate::test_support::trace();
        Seeder {
            qbit,
            category: "sharerr".to_owned(),
            tag: "sharerr".to_owned(),
            skip_checking: true,
            seeding: sharerr_core::config::SeedingConfig::default(),
            sidecar_roots: Vec::new(),
            torrent_dir,
            confirmed: Default::default(),
            friends_http: reqwest::Client::new(),
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
        let existing = summary("aa", "", "");
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
        let torrents = [
            summary("aa", "/downloads/tv", ""),
            summary("bb", "/downloads/tv", ""),
        ];

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
            .refresh_announce("deadbeef", &announce, &[])
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
                hybrid: false,
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
            .refresh_announce(&built.info_hash, &new_announce, &[])
            .await
            .unwrap();
        assert_eq!(outcome, AnnounceRefresh::Updated);
        assert_eq!(client.set_trackers_calls.lock().unwrap().len(), 1);

        let rewritten = std::fs::read(torrent_file_path(&torrent_dir, &built.info_hash)).unwrap();
        let current = sharerr_torrent::Torrent::read_from_bytes(&rewritten)
            .unwrap()
            .announce;
        assert_eq!(current.as_deref(), Some("http://new.example/announce"));

        // Running it again against the now-current file must be a no-op.
        let outcome_again = seeder
            .refresh_announce(&built.info_hash, &new_announce, &[])
            .await
            .unwrap();
        assert_eq!(outcome_again, AnnounceRefresh::Current);
        assert_eq!(client.set_trackers_calls.lock().unwrap().len(), 1);
    }

    /// Once a cached file has been confirmed against an announce set, later
    /// passes with the same set answer from memory: deleting the file is not
    /// noticed until the announce changes and the file has to be read again.
    #[tokio::test]
    async fn refresh_announce_skips_the_file_once_confirmed_until_the_announce_changes() {
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
                hybrid: false,
            })
            .unwrap();
        let torrent_dir = dir.path().join("torrents");
        std::fs::create_dir(&torrent_dir).unwrap();
        let cached = torrent_file_path(&torrent_dir, &built.info_hash);
        std::fs::write(&cached, &built.data).unwrap();

        let seeder = seeder(Arc::new(StubClient::default()), torrent_dir);
        let first = seeder
            .refresh_announce(&built.info_hash, &announce, &[])
            .await
            .unwrap();
        assert_eq!(first, AnnounceRefresh::Current, "read and confirmed");

        std::fs::remove_file(&cached).unwrap();
        let remembered = seeder
            .refresh_announce(&built.info_hash, &announce, &[])
            .await
            .unwrap();
        assert_eq!(
            remembered,
            AnnounceRefresh::Current,
            "the same announce is answered without the file"
        );

        let moved = AnnounceSet::single(Url::parse("http://moved.example/announce").unwrap());
        let reread = seeder
            .refresh_announce(&built.info_hash, &moved, &[])
            .await
            .unwrap();
        assert_eq!(
            reread,
            AnnounceRefresh::NoCachedTorrent,
            "a changed announce goes back to the file"
        );
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
                hybrid: false,
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
                &[],
                Some(&built.info_hash),
                None,
                FriendTorrents::default(),
            )
            .await
            .unwrap();
        std::fs::set_permissions(&media, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            outcome,
            SeedOutcome::Added {
                info_hash: built.info_hash.clone(),
                private: true,
                info_hash_v2: None,
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
                hybrid: false,
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
                &[],
                Some(&built.info_hash),
                None,
                FriendTorrents::default(),
            )
            .await
            .unwrap();
        std::fs::set_permissions(&media, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            outcome,
            SeedOutcome::Added {
                info_hash: built.info_hash.clone(),
                private: true,
                info_hash_v2: None,
            }
        );

        let add_calls = client.add_calls.lock().unwrap();
        assert_eq!(add_calls.len(), 1);
        assert_eq!(add_calls[0].0, built.info_hash);
        let handed_to_client = sharerr_torrent::Torrent::read_from_bytes(&add_calls[0].1)
            .unwrap()
            .announce;
        assert_eq!(
            handed_to_client.as_deref(),
            Some("http://new.example/announce")
        );

        let cached = std::fs::read(torrent_file_path(&torrent_dir, &built.info_hash)).unwrap();
        assert_eq!(
            sharerr_torrent::Torrent::read_from_bytes(&cached)
                .unwrap()
                .announce
                .as_deref(),
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
                hybrid: false,
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
                &[],
                Some(&stale.info_hash),
                None,
                FriendTorrents::default(),
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
                hybrid: false,
            })
            .unwrap();
        assert_ne!(rebuilt.info_hash, stale.info_hash);
        assert_eq!(
            outcome,
            SeedOutcome::Added {
                info_hash: rebuilt.info_hash.clone(),
                private: true,
                info_hash_v2: None,
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
                &[],
                Some("stale-hash-nothing-cached"),
                None,
                FriendTorrents::default(),
            )
            .await
            .unwrap();
        assert!(matches!(outcome, SeedOutcome::Added { .. }));
        assert_eq!(client.add_calls.lock().unwrap().len(), 1);
    }

    /// `seeding.hybrid` builds a hybrid torrent, whose v2 identity rides along
    /// in the outcome for the store, and caches the hybrid bytes.
    #[tokio::test]
    async fn seed_builds_a_hybrid_torrent_when_configured() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("movie.mkv");
        std::fs::write(&media, b"pretend media bytes").unwrap();

        let torrent_dir = dir.path().join("torrents");
        let client = Arc::new(StubClient::default());
        let mut seeder = seeder(client.clone(), torrent_dir.clone());
        seeder.seeding.hybrid = true;
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
                &[],
                None,
                None,
                FriendTorrents::default(),
            )
            .await
            .unwrap();
        let SeedOutcome::Added {
            info_hash,
            info_hash_v2,
            ..
        } = outcome
        else {
            panic!("expected a build, got {outcome:?}");
        };
        let v2 = info_hash_v2.expect("a hybrid build has a v2 identity");
        assert_eq!(v2.len(), 40);
        assert_ne!(v2, info_hash);

        let cached = std::fs::read(torrent_file_path(&torrent_dir, &info_hash)).unwrap();
        let torrent = sharerr_torrent::Torrent::read_from_bytes(&cached).unwrap();
        assert_eq!(torrent.info_hash_v2().as_deref(), Some(v2.as_str()));
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
                hybrid: false,
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
                &existing,
                None,
                None,
                FriendTorrents::default(),
            )
            .await
            .unwrap();
        assert_eq!(
            outcome,
            SeedOutcome::Reused {
                info_hash: theirs.info_hash.clone(),
                upstream: vec!["http://their-tracker.example/announce".to_owned()],
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
        assert_eq!(added[0].1, announce.tiers());
        assert!(
            client.set_trackers_calls.lock().unwrap().is_empty(),
            "set_trackers would drop the trackers the torrent came with"
        );

        // And the feed now has something to serve, announcing to sharerr.
        let cached = std::fs::read(torrent_file_path(&torrent_dir, &theirs.info_hash)).unwrap();
        assert_eq!(
            sharerr_torrent::Torrent::read_from_bytes(&cached)
                .unwrap()
                .announce
                .as_deref(),
            Some(announce.primary().as_str())
        );
        // Rewriting the announce must not disturb the info dict — a cached
        // file under a different infohash than the torrent being seeded is
        // exactly the 404 this is here to prevent.
        assert_eq!(
            sharerr_torrent::Torrent::read_from_bytes(&cached)
                .unwrap()
                .info_hash(),
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
                &existing,
                None,
                None,
                FriendTorrents::default(),
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
                hybrid: false,
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
                &existing,
                None,
                None,
                FriendTorrents::default(),
            )
            .await
            .unwrap();
        assert_eq!(
            outcome,
            SeedOutcome::Reused {
                info_hash: built.info_hash.clone(),
                upstream: Vec::new(),
            }
        );
        assert_eq!(std::fs::read(&cached_path).unwrap(), built.data);
        // The tracker still goes in: a torrent the client has and sharerr has
        // cached may still have been added by hand from that same cached file.
        assert_eq!(client.add_trackers_calls.lock().unwrap().len(), 1);
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
            is_stopped: false,
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
        let existing = summary(
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
        let existing = summary(
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
        let existing = summary("aa", "/downloads", "");
        assert!(!matches_content_path(&existing, Path::new("/downloads")));
    }

    /// The check that keeps a neighbouring directory from looking like a parent.
    #[test]
    fn save_path_containment_respects_component_boundaries() {
        let existing = summary("aa", "/downloads/tv", "");
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
                hybrid: false,
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
            sharerr_torrent::Torrent::read_from_bytes(&rewritten)
                .unwrap()
                .announce
                .as_deref(),
            Some(announce.primary().as_str())
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
                &[],
                Some(hash),
                None,
                FriendTorrents::default(),
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
                    &[],
                    1,
                    "hash",
                    Path::new("/torrents/hash.torrent"),
                )
                .await
        );
    }

    /// A media file and its subtitles, in a directory the client sees under
    /// another name, with what `seed` handed the client for it.
    async fn seed_with_sidecars(
        media_dir: &Path,
        torrent_dir: PathBuf,
        client: Arc<StubClient>,
        sidecar_roots: Vec<PathBuf>,
        known_hash: Option<&str>,
    ) -> SeedOutcome {
        let mut seeder = seeder(client, torrent_dir);
        seeder.sidecar_roots = sidecar_roots;
        let media = media_dir.join("Show.S01E01.mkv");
        let paths = ResolvedPaths {
            arr: media.clone(),
            sharerr: media,
            qbit: PathBuf::from("/downloads/tv/Season 01/Show.S01E01.mkv"),
            mapping_applied: true,
        };
        let announce = AnnounceSet::single(Url::parse("http://sharerr.example/announce").unwrap());
        seeder
            .seed(
                &paths,
                &announce,
                &[],
                known_hash,
                None,
                FriendTorrents::default(),
            )
            .await
            .unwrap()
    }

    fn media_with_subtitle(dir: &Path) {
        std::fs::write(dir.join("Show.S01E01.mkv"), b"pretend media bytes").unwrap();
        std::fs::write(dir.join("Show.S01E01.en.srt"), b"pretend subtitles").unwrap();
    }

    /// The whole feature: a multi-file torrent over the files where they sit,
    /// with the client told the parent of the directory the torrent names.
    #[tokio::test]
    async fn sidecars_build_a_multi_file_torrent_the_client_finds_in_place() {
        let dir = tempfile::tempdir().unwrap();
        media_with_subtitle(dir.path());
        let client = Arc::new(StubClient::default());

        let outcome = seed_with_sidecars(
            dir.path(),
            dir.path().join("torrents"),
            client.clone(),
            vec![dir.path().to_path_buf()],
            None,
        )
        .await;
        assert!(matches!(outcome, SeedOutcome::Added { private: true, .. }));

        let add_calls = client.add_calls.lock().unwrap();
        let torrent = sharerr_torrent::Torrent::read_from_bytes(&add_calls[0].1).unwrap();
        assert_eq!(torrent.name(), Some("Season 01"), "the client's directory");
        assert_eq!(
            torrent.file_paths(),
            vec!["Show.S01E01.mkv", "Show.S01E01.en.srt"]
        );
        assert_eq!(
            *client.add_locations.lock().unwrap(),
            vec![("/downloads/tv".to_owned(), Some("Season 01".to_owned()))]
        );
    }

    /// Opt-in: the same files outside any sidecar root build what they always did.
    #[tokio::test]
    async fn without_a_sidecar_root_the_torrent_stays_single_file() {
        let dir = tempfile::tempdir().unwrap();
        media_with_subtitle(dir.path());
        let client = Arc::new(StubClient::default());

        seed_with_sidecars(
            dir.path(),
            dir.path().join("torrents"),
            client.clone(),
            vec![PathBuf::from("/somewhere/else")],
            None,
        )
        .await;

        let add_calls = client.add_calls.lock().unwrap();
        let torrent = sharerr_torrent::Torrent::read_from_bytes(&add_calls[0].1).unwrap();
        assert!(torrent.is_single_file());
        assert_eq!(
            *client.add_locations.lock().unwrap(),
            vec![("/downloads/tv/Season 01".to_owned(), None)]
        );
    }

    /// Nothing to include means the same torrent, and so the same info hash, as
    /// with the setting off.
    #[tokio::test]
    async fn a_file_with_no_subtitles_stays_single_file_under_a_sidecar_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Show.S01E01.mkv"), b"bytes").unwrap();
        let client = Arc::new(StubClient::default());

        seed_with_sidecars(
            dir.path(),
            dir.path().join("torrents"),
            client.clone(),
            vec![dir.path().to_path_buf()],
            None,
        )
        .await;

        let add_calls = client.add_calls.lock().unwrap();
        assert!(
            sharerr_torrent::Torrent::read_from_bytes(&add_calls[0].1)
                .unwrap()
                .is_single_file()
        );
    }

    /// A client that lost the torrent gets the cached multi-file one back with the
    /// save path its shape needs, whatever the setting says now.
    #[tokio::test]
    async fn a_cached_multi_file_torrent_is_reused_with_its_own_save_path() {
        let dir = tempfile::tempdir().unwrap();
        media_with_subtitle(dir.path());
        let torrents = dir.path().join("torrents");
        let first = Arc::new(StubClient::default());
        let outcome = seed_with_sidecars(
            dir.path(),
            torrents.clone(),
            first.clone(),
            vec![dir.path().to_path_buf()],
            None,
        )
        .await;
        let hash = outcome.info_hash().to_owned();

        let second = Arc::new(StubClient::default());
        let reused = seed_with_sidecars(
            dir.path(),
            torrents,
            second.clone(),
            Vec::new(),
            Some(&hash),
        )
        .await;

        assert_eq!(reused.info_hash(), hash);
        assert_eq!(
            *second.add_locations.lock().unwrap(),
            vec![("/downloads/tv".to_owned(), Some("Season 01".to_owned()))]
        );
    }

    /// A subtitle deleted since the build leaves a torrent no client can complete,
    /// so the cache is stale the same as a resized media file.
    #[tokio::test]
    async fn a_cached_multi_file_torrent_with_a_file_gone_is_rebuilt() {
        let dir = tempfile::tempdir().unwrap();
        media_with_subtitle(dir.path());
        let torrents = dir.path().join("torrents");
        let roots = vec![dir.path().to_path_buf()];
        let outcome = seed_with_sidecars(
            dir.path(),
            torrents.clone(),
            Arc::new(StubClient::default()),
            roots.clone(),
            None,
        )
        .await;
        let hash = outcome.info_hash().to_owned();
        std::fs::remove_file(dir.path().join("Show.S01E01.en.srt")).unwrap();

        let client = Arc::new(StubClient::default());
        let rebuilt =
            seed_with_sidecars(dir.path(), torrents, client.clone(), roots, Some(&hash)).await;

        assert_ne!(rebuilt.info_hash(), hash);
        assert_eq!(
            *client.add_locations.lock().unwrap(),
            vec![("/downloads/tv/Season 01".to_owned(), None)]
        );
    }

    /// A client entry with no save path cannot cover anything; the search
    /// moves on rather than matching it against an empty prefix.
    #[tokio::test]
    async fn find_existing_skips_a_torrent_with_no_save_path() {
        let seeder = seeder(Arc::new(StubClient::default()), PathBuf::from("/torrents"));
        let known = [summary("abc", "", "")];
        let found = seeder
            .find_existing(&known, Path::new("/downloads/movie.mkv"))
            .await
            .unwrap();
        assert!(found.is_none());
    }

    fn paths_for(media: &Path) -> ResolvedPaths {
        ResolvedPaths {
            arr: media.to_path_buf(),
            sharerr: media.to_path_buf(),
            qbit: PathBuf::from("/downloads/movie.mkv"),
            mapping_applied: false,
        }
    }

    /// The client is corrected before the cache is written, so a tracker the
    /// client refuses to add fails the item and leaves nothing cached.
    #[tokio::test]
    async fn an_adoption_fails_when_the_client_refuses_the_tracker() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("movie.mkv");
        std::fs::write(&media, b"pretend media bytes").unwrap();
        let theirs = built(&media, "http://their-tracker.example/announce");
        let torrent_dir = dir.path().join("torrents");
        let client = Arc::new(StubClient {
            export_result: Some(theirs.data.clone()),
            fail_add_trackers: true,
            ..StubClient::default()
        });
        let seeder = seeder(client, torrent_dir.clone());
        let announce = AnnounceSet::single(Url::parse("http://sharerr.example/announce").unwrap());
        let existing = [summary(
            &theirs.info_hash,
            "/downloads",
            "/downloads/movie.mkv",
        )];

        let err = seeder
            .seed(
                &paths_for(&media),
                &announce,
                &existing,
                None,
                None,
                FriendTorrents::default(),
            )
            .await
            .unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("adding sharerr's tracker"), "{text}");
        assert!(!torrent_file_path(&torrent_dir, &theirs.info_hash).exists());
    }

    /// A cached `.torrent` that exists but cannot be read — a directory
    /// squatting on its path — is an error naming the path, not the
    /// "nothing cached" case, on both the refresh and the adoption paths.
    #[tokio::test]
    async fn an_unreadable_cached_torrent_is_an_error_not_an_absence() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("movie.mkv");
        std::fs::write(&media, b"pretend media bytes").unwrap();
        let theirs = built(&media, "http://their-tracker.example/announce");
        let torrent_dir = dir.path().join("torrents");
        let squatter = torrent_file_path(&torrent_dir, &theirs.info_hash);
        std::fs::create_dir_all(&squatter).unwrap();
        let client = Arc::new(StubClient {
            export_result: Some(theirs.data.clone()),
            ..StubClient::default()
        });
        let seeder = seeder(client, torrent_dir);
        let announce = AnnounceSet::single(Url::parse("http://sharerr.example/announce").unwrap());

        let err = seeder
            .refresh_announce(&theirs.info_hash, &announce, &[])
            .await
            .unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("reading"), "{text}");
        assert!(text.contains(&theirs.info_hash), "{text}");

        let existing = [summary(
            &theirs.info_hash,
            "/downloads",
            "/downloads/movie.mkv",
        )];
        let err = seeder
            .seed(
                &paths_for(&media),
                &announce,
                &existing,
                None,
                None,
                FriendTorrents::default(),
            )
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("reading"), "{err:#}");
    }

    // ------------------------------------------------------------ friends

    /// A friend's torrent over `media`, announcing to their tracker with our
    /// token there.
    fn friends_torrent(media: &Path) -> sharerr_torrent::BuiltTorrent {
        TorrentFactory
            .create(&TorrentRequest {
                path: media,
                announce: &AnnounceSet::single(
                    Url::parse("http://friend.example/announce/our-token").unwrap(),
                ),
                media: None,
                private: false,
                hybrid: false,
            })
            .unwrap()
    }

    fn entry(info_hash: &str, download_url: String) -> CatalogEntry {
        CatalogEntry {
            info_hash: info_hash.to_owned(),
            spec: sharerr_core::MediaSpec::Movie {
                title: "Copper Vale".to_owned(),
                year: Some(2001),
            },
            ids: sharerr_core::ExternalIds::default(),
            size: 0,
            private: false,
            download_url,
        }
    }

    struct FriendCase {
        _dir: tempfile::TempDir,
        media: PathBuf,
        torrent_dir: PathBuf,
        paths: ResolvedPaths,
        announce: AnnounceSet,
    }

    fn friend_case(contents: &[u8]) -> FriendCase {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("movie.mkv");
        std::fs::write(&media, contents).unwrap();
        FriendCase {
            torrent_dir: dir.path().join("torrents"),
            paths: ResolvedPaths {
                arr: media.clone(),
                sharerr: media.clone(),
                qbit: PathBuf::from("/movies/movie.mkv"),
                mapping_applied: false,
            },
            announce: AnnounceSet::single(Url::parse("http://sharerr.example/announce").unwrap()),
            media,
            _dir: dir,
        }
    }

    async fn serve(bytes: Vec<u8>) -> wiremock::MockServer {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::path_regex("^/torrents/"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(bytes))
            .mount(&server)
            .await;
        server
    }

    /// The case the catalog exists for: the client still holds the download
    /// the library file came from, so the item joins that swarm instead of
    /// starting a second one, and the torrent stays the operator's.
    #[tokio::test]
    async fn a_friends_torrent_the_client_already_holds_is_joined_by_hash() {
        let case = friend_case(b"pretend media bytes");
        let theirs = friends_torrent(&case.media);
        let client = Arc::new(StubClient {
            export_result: Some(theirs.data.clone()),
            ..StubClient::default()
        });
        let seeder = seeder(client.clone(), case.torrent_dir.clone());
        let held = [summary(
            &theirs.info_hash,
            "/downloads",
            "/downloads/movie.mkv",
        )];
        let matches = [(7, entry(&theirs.info_hash, "http://unused".to_owned()))];

        let outcome = seeder
            .seed(
                &case.paths,
                &case.announce,
                &held,
                None,
                None,
                FriendTorrents {
                    matches: &matches,
                    joined: None,
                },
            )
            .await
            .unwrap();

        assert_eq!(
            outcome,
            SeedOutcome::Joined {
                info_hash: theirs.info_hash.clone(),
                origin: ItemOrigin {
                    peer_id: Some(7),
                    upstream: vec!["http://friend.example/announce/our-token".to_owned()],
                },
                added: None,
            }
        );
        assert!(client.add_calls.lock().unwrap().is_empty());
        assert_eq!(client.add_trackers_calls.lock().unwrap().len(), 1);
        assert!(torrent_file_path(&case.torrent_dir, &theirs.info_hash).exists());
    }

    /// Fetched, checked piece for piece, cached announcing to us alone, and
    /// handed to the client announcing to us and then the friend.
    #[tokio::test]
    async fn a_friends_torrent_that_matches_the_file_is_fetched_and_joined() {
        let case = friend_case(b"pretend media bytes");
        let theirs = friends_torrent(&case.media);
        let server = serve(theirs.data.clone()).await;
        let client = Arc::new(StubClient::default());
        let seeder = seeder(client.clone(), case.torrent_dir.clone());
        let matches = [(
            7,
            entry(
                &theirs.info_hash,
                format!("{}/torrents/{}.torrent", server.uri(), theirs.info_hash),
            ),
        )];

        let outcome = seeder
            .seed(
                &case.paths,
                &case.announce,
                &[],
                None,
                None,
                FriendTorrents {
                    matches: &matches,
                    joined: None,
                },
            )
            .await
            .unwrap();

        let SeedOutcome::Joined {
            info_hash,
            origin,
            added: Some(added),
        } = outcome
        else {
            panic!("expected a joined, added torrent: {outcome:?}");
        };
        assert_eq!(info_hash, theirs.info_hash);
        assert_eq!(origin.peer_id, Some(7));
        assert!(!added.private);

        let adds = client.add_calls.lock().unwrap();
        assert_eq!(adds.len(), 1);
        let handed = sharerr_torrent::Torrent::read_from_bytes(&adds[0].1).unwrap();
        assert_eq!(handed.info_hash(), theirs.info_hash);
        assert_eq!(
            handed.announce_tiers(),
            vec![
                vec!["http://sharerr.example/announce".to_owned()],
                vec!["http://friend.example/announce/our-token".to_owned()],
            ]
        );

        let cached =
            std::fs::read(torrent_file_path(&case.torrent_dir, &theirs.info_hash)).unwrap();
        let cached = sharerr_torrent::Torrent::read_from_bytes(&cached).unwrap();
        assert_eq!(
            cached.announce_tiers(),
            vec![vec!["http://sharerr.example/announce".to_owned()]],
            "our token at the friend's tracker must never reach the feed"
        );
    }

    /// Each reason a friend's torrent cannot seed this file falls back to a
    /// build of our own, never fails the item.
    #[tokio::test]
    async fn a_friends_torrent_that_cannot_seed_the_file_falls_back_to_a_build() {
        let case = friend_case(b"pretend media bytes");
        let other = case.media.with_file_name("other.mkv");
        std::fs::write(&other, b"pretend media bytes").unwrap();
        let renamed = friends_torrent(&other);
        // Different bytes of the same size: the pieces differ.
        let same_name_dir = tempfile::tempdir().unwrap();
        let same_name = same_name_dir.path().join("movie.mkv");
        std::fs::write(&same_name, b"different bytes!!!!").unwrap();
        let different_pieces = friends_torrent(&same_name);

        for (why, theirs, catalog_hash) in [
            (
                "a different file name",
                renamed.clone(),
                renamed.info_hash.clone(),
            ),
            (
                "different pieces",
                different_pieces.clone(),
                different_pieces.info_hash.clone(),
            ),
            (
                "a hash other than the catalog's",
                different_pieces.clone(),
                "ab".repeat(20),
            ),
        ] {
            let server = serve(theirs.data.clone()).await;
            let client = Arc::new(StubClient::default());
            let seeder = seeder(client.clone(), case.torrent_dir.clone());
            let matches = [(
                7,
                entry(
                    &catalog_hash,
                    format!("{}/torrents/{catalog_hash}.torrent", server.uri()),
                ),
            )];
            let outcome = seeder
                .seed(
                    &case.paths,
                    &case.announce,
                    &[],
                    None,
                    None,
                    FriendTorrents {
                        matches: &matches,
                        joined: None,
                    },
                )
                .await
                .unwrap();
            assert!(
                matches!(outcome, SeedOutcome::Added { .. }),
                "{why}: {outcome:?}"
            );
            assert_ne!(outcome.info_hash(), theirs.info_hash, "{why}");
        }

        // And a friend who does not answer at all.
        let client = Arc::new(StubClient::default());
        let seeder = seeder(client, case.torrent_dir.clone());
        let matches = [(
            7,
            entry(&"ab".repeat(20), "http://127.0.0.1:0/x".to_owned()),
        )];
        let outcome = seeder
            .seed(
                &case.paths,
                &case.announce,
                &[],
                None,
                None,
                FriendTorrents {
                    matches: &matches,
                    joined: None,
                },
            )
            .await
            .unwrap();
        assert!(matches!(outcome, SeedOutcome::Added { .. }), "{outcome:?}");
    }

    /// A joined torrent the client lost goes back from the cache with the
    /// friend's tracker kept, and stays joined.
    #[tokio::test]
    async fn a_cached_joined_torrent_goes_back_with_the_friends_tracker() {
        let case = friend_case(b"pretend media bytes");
        let theirs = friends_torrent(&case.media);
        write_torrent_file(
            &torrent_file_path(&case.torrent_dir, &theirs.info_hash),
            &sharerr_torrent::rewrite_announce(&theirs.data, &case.announce).unwrap(),
        )
        .unwrap();
        let client = Arc::new(StubClient::default());
        let seeder = seeder(client.clone(), case.torrent_dir.clone());
        let origin = ItemOrigin {
            peer_id: Some(7),
            upstream: vec!["http://friend.example/announce/our-token".to_owned()],
        };

        let outcome = seeder
            .seed(
                &case.paths,
                &case.announce,
                &[],
                Some(&theirs.info_hash),
                None,
                FriendTorrents {
                    matches: &[],
                    joined: Some(&origin),
                },
            )
            .await
            .unwrap();

        assert!(
            matches!(&outcome, SeedOutcome::Joined { origin: o, added: Some(_), .. } if *o == origin),
            "{outcome:?}"
        );
        let adds = client.add_calls.lock().unwrap();
        let handed = sharerr_torrent::Torrent::read_from_bytes(&adds[0].1).unwrap();
        assert_eq!(handed.announce_tiers().len(), 2);
    }

    /// An endpoint change rewrites the client's list with the friend's
    /// tracker still after ours.
    #[tokio::test]
    async fn refreshing_a_joined_torrents_announce_keeps_the_friends_tracker() {
        let case = friend_case(b"pretend media bytes");
        let theirs = friends_torrent(&case.media);
        write_torrent_file(
            &torrent_file_path(&case.torrent_dir, &theirs.info_hash),
            &sharerr_torrent::rewrite_announce(&theirs.data, &case.announce).unwrap(),
        )
        .unwrap();
        let client = Arc::new(StubClient::default());
        let seeder = seeder(client.clone(), case.torrent_dir.clone());
        let moved = AnnounceSet::single(Url::parse("http://moved.example/announce").unwrap());
        let upstream = ["http://friend.example/announce/our-token".to_owned()];

        let refreshed = seeder
            .refresh_announce(&theirs.info_hash, &moved, &upstream)
            .await
            .unwrap();

        assert_eq!(refreshed, AnnounceRefresh::Updated);
        let set = client.set_trackers_calls.lock().unwrap();
        assert_eq!(
            set[0].1,
            vec![
                Url::parse("http://moved.example/announce").unwrap(),
                Url::parse("http://friend.example/announce/our-token").unwrap(),
            ]
        );
    }

    /// What an adopted torrent carries besides us: the client's export, less
    /// every tier our cached copy or the current announce names.
    #[tokio::test]
    async fn an_adopted_torrents_own_trackers_are_recovered_from_its_export() {
        let case = friend_case(b"pretend media bytes");
        let theirs = friends_torrent(&case.media);
        let old_ours = AnnounceSet::single(Url::parse("http://old.example/announce").unwrap());
        write_torrent_file(
            &torrent_file_path(&case.torrent_dir, &theirs.info_hash),
            &sharerr_torrent::rewrite_announce(&theirs.data, &old_ours).unwrap(),
        )
        .unwrap();
        let mut exported = sharerr_torrent::Torrent::read_from_bytes(&theirs.data).unwrap();
        exported.announce_list = Some(vec![
            vec!["http://old.example/announce".to_owned()],
            vec!["http://friend.example/announce/our-token".to_owned()],
            vec!["http://sharerr.example/announce".to_owned()],
        ]);
        let client = Arc::new(StubClient {
            export_result: Some(exported.encode()),
            ..StubClient::default()
        });
        let seeder = seeder(client, case.torrent_dir.clone());

        assert_eq!(
            seeder
                .recover_upstream(&theirs.info_hash, &case.announce)
                .await
                .unwrap(),
            Some(vec!["http://friend.example/announce/our-token".to_owned()])
        );

        let cannot_export = seeder_without_export(case.torrent_dir.clone());
        assert_eq!(
            cannot_export
                .recover_upstream(&theirs.info_hash, &case.announce)
                .await
                .unwrap(),
            None
        );
    }

    fn seeder_without_export(torrent_dir: PathBuf) -> Seeder {
        seeder(Arc::new(StubClient::default()), torrent_dir)
    }
}
