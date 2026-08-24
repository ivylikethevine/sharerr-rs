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
    /// Per-torrent upload cap in KiB/s, applied at add time. `None` leaves
    /// the client's own default in effect. See `[seeding]` in
    /// `sharerr.toml`.
    pub upload_limit_kib: Option<u64>,
    /// Seed-ratio goal, applied at add time. `None` leaves the client's own
    /// default/global ratio setting in effect.
    pub ratio_limit: Option<f64>,
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
        let mut request = AddRequest::new(&built.data, &filename, &save_path)
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

        Ok(SeedOutcome::Added {
            info_hash: built.info_hash,
        })
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

        let current = sharerr_torrent::read_announce(&data)
            .with_context(|| format!("parsing {}", path.display()))?;
        if current.as_deref() == Some(announce.primary.as_str()) {
            return Ok(AnnounceRefresh::Current);
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
        Ok(AnnounceRefresh::Updated)
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
        async fn add(&self, _request: &AddRequest<'_>) -> sharerr_client::Result<()> {
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
    }

    fn seeder(qbit: Arc<dyn TorrentClient>, torrent_dir: PathBuf) -> Seeder {
        Seeder {
            qbit,
            category: "sharerr".to_owned(),
            tag: "sharerr".to_owned(),
            skip_checking: true,
            upload_limit_kib: None,
            ratio_limit: None,
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
        let torrents = [
            torrent("aa", "/downloads/tv", ""),
            torrent("bb", "/downloads/tv", ""),
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
        let built = LavaTorrentFactory
            .create(&TorrentRequest {
                path: &media,
                announce: &old_announce,
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
