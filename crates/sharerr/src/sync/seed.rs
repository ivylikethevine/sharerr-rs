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
use sharerr_torrent::{TorrentFactory, TorrentRequest, torrent_file_path};
use url::Url;

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
    pub factory: Arc<dyn TorrentFactory>,
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
    pub async fn seed(&self, paths: &ResolvedPaths, announce: &Url) -> Result<SeedOutcome> {
        if let Some(existing) = self.find_existing(&paths.qbit).await? {
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

    /// Build the `.torrent` and keep a copy on disk.
    async fn build(
        &self,
        paths: &ResolvedPaths,
        announce: &Url,
    ) -> Result<sharerr_torrent::BuiltTorrent> {
        let factory = Arc::clone(&self.factory);
        let path = paths.sharerr.clone();
        let announce = announce.clone();

        // Hashing a media file is gigabytes of CPU work. Doing it on the runtime
        // would stall every other task for the duration.
        let built = tokio::task::spawn_blocking(move || {
            factory.create(&TorrentRequest {
                path: &path,
                announce: &announce,
            })
        })
        .await
        .context("torrent build task panicked")?
        .with_context(|| format!("building a torrent for {}", paths.sharerr.display()))?;

        // Best-effort: the torrent is already in memory and about to be handed
        // over, so a failure to cache it must not fail the share.
        let cached = torrent_file_path(&self.torrent_dir, &built.info_hash);
        if let Err(err) = write_torrent_file(&cached, &built.data) {
            tracing::warn!(path = %cached.display(), %err, "could not cache the .torrent file");
        }

        Ok(built)
    }

    /// Find a torrent qBittorrent already has that contains `target`.
    ///
    /// Two passes, cheap first. `torrents/info` alone answers the common case,
    /// because a single-file torrent's `content_path` is the file itself. Only
    /// torrents that could plausibly contain the target — by save path — cost an
    /// extra `torrents/files` call.
    pub async fn find_existing(&self, target: &Path) -> Result<Option<String>> {
        let torrents = self
            .qbit
            .list(None)
            .await
            .context("listing existing torrents in the torrent client")?;

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
fn matches_content_path(torrent: &TorrentSummary, target: &Path) -> bool {
    !torrent.content_path.is_empty() && normalize(&torrent.content_path) == normalize_path(target)
}

/// Whether this torrent's save path could contain `target` at all.
///
/// Compared component-wise, so `/downloads/tv` does not appear to contain
/// `/downloads/tv-archive/...` the way a string prefix check would.
fn could_contain(torrent: &TorrentSummary, target: &Path) -> bool {
    if torrent.save_path.is_empty() {
        return false;
    }
    normalize_path(target).starts_with(normalize(&torrent.save_path))
}

/// Whether any file in the torrent resolves to `target`.
fn contains_file(save_path: &str, files: &[TorrentFileEntry], target: &Path) -> bool {
    let root = normalize(save_path);
    let target = normalize_path(target);
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
