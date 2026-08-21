//! Handing content to qBittorrent without disturbing what is already there.
//!
//! The README's "preserve any existing torrents" requirement comes down to four
//! rules, all enforced here or in [`sharerr_qbit::QbitClient::add_torrent`]:
//!
//! 1. **Detect first.** If a torrent already contains this file, reuse its infohash
//!    instead of creating a second one. Cross-seeding is well supported, but a
//!    duplicate sharerr keeps re-adding every sync is just noise.
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
use sharerr_core::paths::ResolvedPaths;
use sharerr_torrent::{AnnounceSet, LavaTorrentFactory, TorrentRequest, torrent_file_path};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedOutcome {
    /// A torrent already covering this file was found; nothing was added.
    Reused { info_hash: String },
    /// A new torrent was built and handed to qBittorrent.
    Added { info_hash: String },
}

impl SeedOutcome {
    pub fn info_hash(&self) -> &str {
        match self {
            Self::Reused { info_hash } | Self::Added { info_hash } => info_hash,
        }
    }
}

pub struct Seeder {
    pub qbit: Arc<dyn TorrentClient>,
    pub category: String,
    pub tag: String,
    pub skip_checking: bool,
    /// Where sharerr keeps a copy of each `.torrent` it builds.
    pub torrent_dir: PathBuf,
}

impl std::fmt::Debug for Seeder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Seeder")
            .field("category", &self.category)
            .field("tag", &self.tag)
            .field("skip_checking", &self.skip_checking)
            .field("torrent_dir", &self.torrent_dir)
            .finish_non_exhaustive()
    }
}

impl Seeder {
    /// Ensure the file at `paths` is being seeded, building a torrent only if
    /// nothing already covers it.
    ///
    /// `torrents` is the client's full list, fetched once per reconciliation pass
    /// by the caller — refetching it here per item would cost a first sync of a
    /// large library one full-library round trip per file. A snapshot is
    /// sound: torrents this pass adds are single-file and each discovered item is
    /// a distinct file, so a later item can never be covered by an earlier add.
    pub async fn seed(
        &self,
        paths: &ResolvedPaths,
        announce: &AnnounceSet,
        torrents: &[TorrentSummary],
    ) -> Result<SeedOutcome> {
        if let Some(existing) = self.find_existing(torrents, &paths.qbit).await? {
            tracing::info!(
                file = %paths.qbit.display(),
                info_hash = %existing,
                "already covered by an existing torrent — reusing it"
            );
            return Ok(SeedOutcome::Reused {
                info_hash: existing,
            });
        }

        let built = self.build(paths, announce).await?;

        // qBittorrent needs the directory the content sits in, not the file.
        let save_path = paths
            .qbit
            .parent()
            .map(Path::to_path_buf)
            .with_context(|| format!("{} has no parent directory", paths.qbit.display()))?;

        let filename = format!("{}.torrent", built.info_hash);
        let save_path = save_path.to_string_lossy();
        self.qbit
            .add(
                &AddRequest::new(&built.data, &filename, &save_path)
                    .category(&self.category)
                    .tags(&self.tag)
                    .skip_checking(self.skip_checking),
            )
            .await
            .with_context(|| format!("adding {} to {}", paths.qbit.display(), self.qbit.kind()))?;

        Ok(SeedOutcome::Added {
            info_hash: built.info_hash,
        })
    }

    /// Bring one already-seeding torrent's announce URLs up to date, in both
    /// places they live: the cached `.torrent` the feed serves, and the tracker
    /// list inside the torrent client.
    ///
    /// Returns whether anything was stale. The client is updated **before** the
    /// file is rewritten: the file's announce is what the next pass compares
    /// against, so if the client update fails the comparison stays stale and the
    /// whole step is retried — the opposite order would record success it did not
    /// achieve.
    pub async fn refresh_announce(&self, info_hash: &str, announce: &AnnounceSet) -> Result<bool> {
        let path = torrent_file_path(&self.torrent_dir, info_hash);
        let data = match tokio::fs::read(&path).await {
            Ok(data) => data,
            // Nothing cached means nothing to compare or serve; the torrent in
            // the client still works, so this is not worth failing the item over.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(err) => {
                return Err(err).with_context(|| format!("reading {}", path.display()));
            }
        };

        let current = sharerr_torrent::read_announce(&data)
            .with_context(|| format!("parsing {}", path.display()))?;
        if current.as_deref() == Some(announce.primary.as_str()) {
            return Ok(false);
        }

        self.qbit
            .set_trackers(info_hash, &announce.tiers)
            .await
            .with_context(|| format!("updating trackers in {}", self.qbit.kind()))?;

        let rewritten = sharerr_torrent::rewrite_announce(&data, announce)
            .with_context(|| format!("rewriting {}", path.display()))?;
        tokio::fs::write(&path, &rewritten)
            .await
            .with_context(|| format!("writing {}", path.display()))?;

        tracing::info!(
            info_hash,
            from = current.as_deref().unwrap_or("(none)"),
            to = %announce.primary,
            "announce URLs refreshed"
        );
        Ok(true)
    }

    /// Build the `.torrent` and keep a copy on disk.
    async fn build(
        &self,
        paths: &ResolvedPaths,
        announce: &AnnounceSet,
    ) -> Result<sharerr_torrent::BuiltTorrent> {
        let path = paths.sharerr.clone();
        let announce = announce.clone();
        let torrent_dir = self.torrent_dir.clone();

        // Hashing a media file is gigabytes of CPU work, and the cache write can
        // block for tens of milliseconds on the network mounts these deployments
        // sit on. Both stay off the runtime; doing either inline would stall
        // every other task — /health and /announce included — for the duration.
        let built = tokio::task::spawn_blocking(move || {
            let built = LavaTorrentFactory.create(&TorrentRequest {
                path: &path,
                announce: &announce,
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
        torrents: &[TorrentSummary],
        target: &Path,
    ) -> Result<Option<String>> {
        // Normalised once: the target is invariant across the whole scan, and
        // re-deriving it per candidate torrent was two allocations times the
        // client's entire list, per item.
        let target = &normalize_path(target);

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

            if contains_file(&torrent.save_path, &files, target) {
                return Ok(Some(torrent.hash.clone()));
            }
        }

        Ok(None)
    }
}

fn write_torrent_file(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, data)
}

/// A single-file torrent points `content_path` straight at the file.
/// `target` arrives already normalised.
fn matches_content_path(torrent: &TorrentSummary, target: &Path) -> bool {
    !torrent.content_path.is_empty() && normalize(&torrent.content_path) == target
}

/// Whether this torrent's save path could contain `target` at all.
///
/// Compared component-wise, so `/downloads/tv` does not appear to contain
/// `/downloads/tv-archive/...` the way a string prefix check would.
fn could_contain(torrent: &TorrentSummary, target: &Path) -> bool {
    if torrent.save_path.is_empty() {
        return false;
    }
    target.starts_with(normalize(&torrent.save_path))
}

/// Whether any file in the torrent resolves to `target`.
fn contains_file(save_path: &str, files: &[TorrentFileEntry], target: &Path) -> bool {
    let root = normalize(save_path);
    files.iter().any(|file| root.join(&file.name) == target)
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
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn torrent(hash: &str, save_path: &str, content_path: &str) -> TorrentSummary {
        TorrentSummary {
            hash: hash.to_owned(),
            name: "whatever".to_owned(),
            save_path: save_path.to_owned(),
            content_path: content_path.to_owned(),
            category: String::new(),
            tags: Vec::new(),
            is_seeding: true,
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
            "/downloads/tv",
            &files,
            Path::new("/downloads/tv/Lanternwick Hollow/lanternwick.s02e01.mkv")
        ));
        assert!(!contains_file(
            "/downloads/tv",
            &files,
            Path::new("/downloads/tv/Lanternwick Hollow/lanternwick.s02e09.mkv")
        ));
    }

    #[test]
    fn a_file_under_a_different_save_path_is_not_matched() {
        let files = [file("lanternwick.s02e01.mkv")];
        assert!(!contains_file(
            "/downloads/other",
            &files,
            Path::new("/downloads/tv/lanternwick.s02e01.mkv")
        ));
    }
}
