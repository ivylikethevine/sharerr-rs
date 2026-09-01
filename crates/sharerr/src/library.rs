//! The plain-directory library source: share what is in a folder, no *arr app.
//!
//! Each `[[library]]` entry is a directory scanned recursively for media files
//! of its declared [`LibraryKind`]. There is no tag and no metadata service —
//! being in the directory *is* the tag, the filename is all the metadata there
//! is, and every item ships with [`ExternalIds::default`], so a friend's app
//! can only parse the release name. That is the documented trade of the
//! zero-dependency path.
//!
//! The reconciliation loop withdraws items a *successful* scan no longer
//! reports, so a listing that silently dropped anything would read as
//! "untagged" and tear its shares down. That shapes every failure mode here:
//! an entry whose root is missing, unreadable, or empty fails the whole scan
//! and the loop leaves everything alone — same contract as an *arr app that
//! did not answer — while a corner of the tree that cannot be listed (a
//! root-owned `lost+found`, a walk cut short) marks the scan **incomplete**:
//! what was found is still shared, because sharing is additive, but nothing is
//! withdrawn on the evidence of a partial inventory.
//!
//! An *empty* root fails rather than scanning to nothing because it is the
//! exact signature of a bind mount that has not come up — a successful empty
//! scan would withdraw every share this source owns in one pass.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use sha2::{Digest, Sha256};
use sharerr_core::config::{LibraryConfig, LibraryKind};
use sharerr_core::{Discovered, ExternalIds, MediaSource, MediaSpec};
use sharerr_torrent::title::{self, ParsedTitle};

/// Directories deeper than this are not descended into. A media library is a
/// few levels deep; a walk that is fifty levels down has hit a cycle the
/// symlink check missed or a pathological tree, and either way should stop
/// rather than spin.
///
/// Shared with [`crate::pathsuggest`], which indexes the search root under
/// the same bound for the same reason.
pub(crate) const MAX_DEPTH: usize = 16;

/// What scanning one library produced.
#[derive(Debug, Default)]
pub struct ScanOutcome {
    pub items: Vec<Discovered>,
    /// Media files left out because their names carry nothing a release of
    /// this library's kind could honestly advertise — a tv file with no
    /// `SxxEyy`, a movie named like an episode, a music file with no
    /// artist or album directory. Inventing metadata would publish a release
    /// that downloads the wrong thing or that nothing can search for.
    pub skipped: usize,
    /// Corners of the tree that could not be listed — an unreadable
    /// subdirectory, a walk cut short at [`MAX_DEPTH`]. The items found are
    /// still worth sharing, but the scan is not a complete inventory, so
    /// nothing may be withdrawn on its evidence.
    pub incomplete: usize,
}

/// Why a `[[library]]` root could not be scanned at all.
///
/// Typed so callers that classify the root — `checks::check_library` — can
/// match on the condition instead of re-statting the directory to rediscover
/// it. Every variant names the root, so the `Display` text is a complete,
/// self-contained message.
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("library {} is not an absolute path", .0.display())]
    NotAbsolute(PathBuf),
    #[error("library {} does not exist", .0.display())]
    Missing(PathBuf),
    #[error("library {} is not a directory", .0.display())]
    NotADirectory(PathBuf),
    #[error(
        "library {} is empty — nothing to share yet, or the mount is not up; \
         existing shares are left alone",
        .0.display()
    )]
    Empty(PathBuf),
    #[error("could not read {}", .path.display())]
    Unreadable {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Scan one `[[library]]` directory.
///
/// Synchronous on purpose — it is filesystem-bound and callers run it on a
/// blocking thread. Items come back in a deterministic (sorted) order so runs
/// are comparable.
pub fn scan(library: &LibraryConfig) -> Result<ScanOutcome, ScanError> {
    let root = &library.path;
    let unreadable = |source: io::Error| ScanError::Unreadable {
        path: root.clone(),
        source,
    };
    // Relative paths would scan fine against the current directory and then
    // fail at share time, when the path resolver refuses them — a green doctor
    // followed by a failing sync. Refuse where the operator can see why.
    if !root.is_absolute() {
        return Err(ScanError::NotAbsolute(root.clone()));
    }
    let metadata = match fs::metadata(root) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Err(ScanError::Missing(root.clone()));
        }
        Err(err) => return Err(unreadable(err)),
    };
    if !metadata.is_dir() {
        return Err(ScanError::NotADirectory(root.clone()));
    }
    // A directory with no entries at all is what an unmounted bind mount looks
    // like, and a successful empty scan would withdraw every share this source
    // owns. Failing keeps them: a genuinely new library starts scanning the
    // moment it holds anything.
    if fs::read_dir(root).map_err(unreadable)?.next().is_none() {
        return Err(ScanError::Empty(root.clone()));
    }

    let source_id = path_id(root);
    let mut outcome = ScanOutcome::default();
    walk(root, root, library, source_id, 0, &mut outcome)?;
    outcome.items.sort_by(|a, b| a.arr_path.cmp(&b.arr_path));
    Ok(outcome)
}

fn walk(
    root: &Path,
    dir: &Path,
    library: &LibraryConfig,
    source_id: i64,
    depth: usize,
    outcome: &mut ScanOutcome,
) -> Result<(), ScanError> {
    if depth > MAX_DEPTH {
        tracing::warn!(
            dir = %dir.display(),
            "library walk stopped at depth {MAX_DEPTH}; this subtree is not shared and \
             nothing is withdrawn while it cannot be listed"
        );
        outcome.incomplete += 1;
        return Ok(());
    }

    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        // The root must be readable — scan() proved it exists — but one
        // unreadable subdirectory (a root-owned lost+found, say) must not stop
        // every other file in the library from being shared, pass after pass.
        Err(err) if depth > 0 => {
            tracing::warn!(
                dir = %dir.display(),
                %err,
                "could not list directory; its contents are not shared and nothing is \
                 withdrawn while it cannot be listed"
            );
            outcome.incomplete += 1;
            return Ok(());
        }
        Err(err) => {
            return Err(ScanError::Unreadable {
                path: dir.to_path_buf(),
                source: err,
            });
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                tracing::warn!(dir = %dir.display(), %err, "could not read a directory entry");
                outcome.incomplete += 1;
                continue;
            }
        };
        let path = entry.path();
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }

        // `DirEntry::metadata` does not traverse symlinks, so a link is seen as
        // a link first and resolved deliberately below.
        let meta = match entry.metadata() {
            Ok(meta) => meta,
            // Deleted between listing and stat: genuinely gone, same as never
            // listed — an actively managed library must not fail the pass over it.
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => {
                tracing::warn!(file = %path.display(), %err, "could not stat; not shared and not withdrawn");
                outcome.incomplete += 1;
                continue;
            }
        };
        // A symlinked directory is skipped (it is how walks loop forever), but a
        // symlinked file is shared via the path the operator gave — following the
        // link here is what makes a hand-curated directory of symlinks work.
        let meta = if meta.is_symlink() {
            match fs::metadata(&path) {
                Ok(target) if target.is_dir() => {
                    tracing::debug!(path = %path.display(), "skipping symlinked directory");
                    continue;
                }
                Ok(target) => target,
                Err(err) => {
                    tracing::warn!(path = %path.display(), %err, "skipping broken symlink");
                    continue;
                }
            }
        } else {
            meta
        };
        if meta.is_dir() {
            walk(root, &path, library, source_id, depth + 1, outcome)?;
            continue;
        }
        if !meta.is_file() || !has_media_extension(&path, library.kind) {
            continue;
        }

        match spec_for(root, &path, library.kind) {
            Some(spec) => outcome.items.push(Discovered {
                source: MediaSource::Directory,
                source_id,
                file_id: path_id(&path),
                spec,
                arr_path: path,
                size: meta.len(),
                ids: ExternalIds::default(),
                scene_name: None,
                original_path: None,
                // A directory has no *arr behind it to have analysed anything.
                // The sync pass probes the file, which is the one path that also
                // covers an *arr file the *arr itself never analysed.
                media: None,
            }),
            None => {
                outcome.skipped += 1;
                tracing::warn!(
                    file = %path.display(),
                    kind = library.kind.as_str(),
                    "skipped: the name carries nothing a release of this kind could advertise"
                );
            }
        }
    }
    Ok(())
}

/// The extensions worth sharing for each kind — a case-insensitive allowlist,
/// so artwork, subtitles and `.nfo` files are never turned into releases.
fn has_media_extension(path: &Path, kind: LibraryKind) -> bool {
    let allowed: &[&str] = match kind {
        LibraryKind::Tv | LibraryKind::Movie => {
            &["mkv", "mp4", "avi", "m4v", "mov", "wmv", "ts", "webm"]
        }
        LibraryKind::Music => &["flac", "mp3", "m4a", "ogg", "opus", "wav", "aac"],
        LibraryKind::Book => &["epub", "mobi", "azw", "azw3", "pdf", "cbz", "cbr"],
    };
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| allowed.iter().any(|a| ext.eq_ignore_ascii_case(a)))
}

/// Build the [`MediaSpec`] a bare file can support, or `None` when it cannot
/// support one at all.
///
/// The stem is parsed the way a friend's app would parse the release title,
/// which makes the two agree by construction: a spec derived from the parse is
/// exactly what `title::resolve` will verify the filename against, so a
/// parseable filename becomes its own release title.
fn spec_for(root: &Path, path: &Path, kind: LibraryKind) -> Option<MediaSpec> {
    let stem = path.file_stem()?.to_string_lossy();
    match kind {
        LibraryKind::Tv => match title::parse(&stem) {
            ParsedTitle::Episode {
                title,
                season,
                episode,
            } => Some(MediaSpec::Episode {
                series_title: title,
                season,
                episode,
            }),
            // A movie-shaped or unparseable name carries no episode numbering,
            // and an episode release without one cannot be matched downstream.
            ParsedTitle::Movie { .. } | ParsedTitle::Unparseable => None,
        },
        LibraryKind::Movie => match title::parse(&stem) {
            ParsedTitle::Movie { title, year } => Some(MediaSpec::Movie { title, year }),
            // An episode-shaped name is misfiled television: advertising it as
            // a movie would publish numbering no Radarr can match.
            ParsedTitle::Episode { .. } => None,
            // Still listable: a title without a year searches fine, it just
            // cannot be pinned to a release year on the far end. But only the
            // title part — a stem that is all release cruft has no title, and
            // publishing "Film Title 1080p BluRay x264-GROUP" as the *title*
            // makes a release nobody's search ever matches.
            ParsedTitle::Unparseable => {
                let title = display_title(&stem);
                (!title.is_empty()).then_some(MediaSpec::Movie { title, year: None })
            }
        },
        // Music libraries are conventionally artist/album/track on disk, so the
        // directory names are the closest thing to metadata a bare file has. A
        // file at the root has neither, and every such file would synthesize
        // the same byte-identical "Unknown Artist" release — skipped instead.
        LibraryKind::Music => {
            let artist = dir_name(root, path, 2);
            let album = dir_name(root, path, 1);
            if artist.is_none() && album.is_none() {
                return None;
            }
            Some(MediaSpec::Track {
                artist: artist.unwrap_or_else(|| "Unknown Artist".to_owned()),
                album: album.unwrap_or_else(|| "Unknown Album".to_owned()),
                track: leading_number(&stem),
            })
        }
        LibraryKind::Book => Some(MediaSpec::Book {
            author: dir_name(root, path, 1).unwrap_or_else(|| "Unknown Author".to_owned()),
            title: humanize(&stem),
        }),
    }
}

/// The name of the directory `levels` above `path`, as long as it is still
/// strictly inside the library root. A file sitting at the root has no such
/// directory, and the root's own name is not an artist or an author.
fn dir_name(root: &Path, path: &Path, levels: usize) -> Option<String> {
    let mut dir = path.parent()?;
    for _ in 1..levels {
        dir = dir.parent()?;
    }
    if !dir.starts_with(root) || dir == root {
        return None;
    }
    Some(dir.file_name()?.to_string_lossy().into_owned())
}

/// `01 - Something.flac` -> `1`; a stem with no leading digits has no track.
fn leading_number(stem: &str) -> Option<u32> {
    let digits: String = stem.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

/// Dots and underscores back to spaces — the inverse of scene-style naming,
/// for stems that are about to become display titles.
///
/// `sharerr_torrent::humanize` rather than a second copy of the same
/// substitution: that crate already owns "what a release name looks like".
fn humanize(stem: &str) -> String {
    sharerr_torrent::humanize(stem)
}

/// Tokens that mark where a filename stops being a title and starts being
/// release metadata. Compared case-insensitively, and against the part before
/// a `-` so `x264-GROUP` matches `x264`.
///
/// A distinct vocabulary from [`sharerr_core::MediaMeta::scene_video_codec`]'s
/// codec tokens on purpose: this list also has to catch source/resolution
/// tokens (`bluray`, `1080p`) that a video-codec lookup never needs to know,
/// so the two are not the same list wearing two names.
const RELEASE_TOKENS: &[&str] = &[
    "480p", "720p", "1080p", "2160p", "4k", "bluray", "blu-ray", "webrip", "web-dl", "webdl",
    "hdtv", "dvdrip", "bdrip", "remux", "x264", "x265", "h264", "h265", "hevc", "av1", "xvid",
    "proper", "repack",
];

/// The humanized stem, cut at the first release-cruft token — for movie names
/// with no year, where the whole stem would otherwise become the title.
fn display_title(stem: &str) -> String {
    let normalised = humanize(stem);
    let is_cruft = |word: &str| {
        let lowered = word.to_ascii_lowercase();
        let head = lowered.split('-').next().unwrap_or(&lowered);
        RELEASE_TOKENS
            .iter()
            .any(|t| *t == lowered.as_str() || *t == head)
    };
    let words: Vec<&str> = normalised
        .split_whitespace()
        .take_while(|word| !is_cruft(word))
        .collect();
    sharerr_torrent::join_title(&words)
}

/// A stable 63-bit id for a path, standing in for the file id an *arr app
/// would have assigned. Derived from the path bytes so it survives restarts
/// and is identical across machines that mount the library at the same point;
/// the sign bit is cleared so it can never collide with SQLite's rowid space
/// semantics or read as a sentinel.
fn path_id(path: &Path) -> i64 {
    let digest = Sha256::digest(path.as_os_str().as_encoded_bytes());
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    i64::from_le_bytes(bytes) & i64::MAX
}

/// The first pair of `[[library]]` roots where one contains the other (or they
/// are equal), if any.
///
/// Overlapping roots are rejected outright because a file reachable from both
/// is discovered twice under the same store key — `file_id` hashes the file
/// path alone — with a different spec each time, so its release title, feed
/// category, and per-friend scope would flip with config order.
pub fn overlapping_roots(libraries: &[LibraryConfig]) -> Option<(&Path, &Path)> {
    for (index, a) in libraries.iter().enumerate() {
        for b in &libraries[index + 1..] {
            if a.path.starts_with(&b.path) || b.path.starts_with(&a.path) {
                return Some((&a.path, &b.path));
            }
        }
    }
    None
}

/// Every `[[library]]` entry, scanned as one source.
///
/// One scanner rather than one per entry because the sync loop tracks success
/// per [`MediaSource`], and all directories share [`MediaSource::Directory`] —
/// see the module docs for why partial success must not look like success.
#[derive(Debug, Clone)]
pub struct DirectoryScanner {
    libraries: Vec<LibraryConfig>,
}

impl DirectoryScanner {
    pub fn new(libraries: Vec<LibraryConfig>) -> Self {
        Self { libraries }
    }

    /// Scan every entry, or fail wholesale if any entry cannot be scanned.
    pub fn scan_all(&self) -> Result<ScanOutcome> {
        if let Some((a, b)) = overlapping_roots(&self.libraries) {
            bail!(
                "[[library]] entries overlap: {} and {} — a file reachable from both \
                 would be shared under whichever kind came last",
                a.display(),
                b.display()
            );
        }

        let mut merged = ScanOutcome::default();
        for library in &self.libraries {
            let outcome = scan(library)?;
            if outcome.skipped > 0 {
                tracing::warn!(
                    library = %library.path.display(),
                    skipped = outcome.skipped,
                    "some files were skipped because their names could not be classified"
                );
            }
            merged.skipped += outcome.skipped;
            merged.incomplete += outcome.incomplete;
            merged.items.extend(outcome.items);
        }
        Ok(merged)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::path::PathBuf;

    fn library(root: &Path, kind: LibraryKind) -> LibraryConfig {
        LibraryConfig {
            path: root.to_path_buf(),
            kind,
        }
    }

    fn touch(path: &PathBuf, bytes: usize) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![0u8; bytes]).unwrap();
    }

    #[test]
    fn scans_recursively_and_ignores_non_media_and_dotfiles() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("Gilded Ferry (2019)/Gilded.Ferry.2019.mkv"), 64);
        touch(&root.join("Gilded Ferry (2019)/poster.jpg"), 8);
        touch(&root.join("Gilded Ferry (2019)/.hidden.mkv"), 8);
        touch(&root.join("notes.txt"), 8);

        let outcome = scan(&library(root, LibraryKind::Movie)).unwrap();
        assert_eq!(outcome.items.len(), 1);
        assert_eq!(outcome.skipped, 0);
        let item = &outcome.items[0];
        assert_eq!(item.source, MediaSource::Directory);
        assert_eq!(
            item.spec,
            MediaSpec::Movie {
                title: "Gilded Ferry".to_owned(),
                year: Some(2019),
            }
        );
        assert_eq!(item.size, 64);
        assert_eq!(item.ids, ExternalIds::default());
        assert!(item.scene_name.is_none());
    }

    #[test]
    fn tv_requires_episode_numbering_and_counts_what_it_skips() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("Lanternwick.Hollow.S02E01.mkv"), 16);
        touch(&root.join("some home video.mkv"), 16);

        let outcome = scan(&library(root, LibraryKind::Tv)).unwrap();
        assert_eq!(outcome.items.len(), 1);
        assert_eq!(outcome.skipped, 1);
        assert_eq!(
            outcome.items[0].spec,
            MediaSpec::Episode {
                series_title: "Lanternwick Hollow".to_owned(),
                season: 2,
                episode: 1,
            }
        );
    }

    #[test]
    fn a_movie_without_a_year_is_still_shared() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("The_Copper_Meridian.mp4"), 16);

        let outcome = scan(&library(root, LibraryKind::Movie)).unwrap();
        assert_eq!(
            outcome.items[0].spec,
            MediaSpec::Movie {
                title: "The Copper Meridian".to_owned(),
                year: None,
            }
        );
    }

    #[test]
    fn music_reads_artist_album_and_track_from_the_layout() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(
            &root.join("Vermilion Choir/Salt Orchard/03 - Tidewrack.flac"),
            16,
        );
        touch(&root.join("stray.flac"), 16);

        let outcome = scan(&library(root, LibraryKind::Music)).unwrap();
        let nested = outcome
            .items
            .iter()
            .find(|i| i.arr_path.ends_with("03 - Tidewrack.flac"))
            .unwrap();
        assert_eq!(
            nested.spec,
            MediaSpec::Track {
                artist: "Vermilion Choir".to_owned(),
                album: "Salt Orchard".to_owned(),
                track: Some(3),
            }
        );
        // A file at the root has no artist or album directory: every such file
        // would synthesize the same "Unknown Artist" release, so it is skipped
        // and counted rather than published.
        assert_eq!(outcome.items.len(), 1);
        assert_eq!(outcome.skipped, 1);
    }

    #[test]
    fn books_read_the_author_from_the_parent_directory() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("Juniper Vale/The Glass Almanac.epub"), 16);

        let outcome = scan(&library(root, LibraryKind::Book)).unwrap();
        assert_eq!(
            outcome.items[0].spec,
            MediaSpec::Book {
                author: "Juniper Vale".to_owned(),
                title: "The Glass Almanac".to_owned(),
            }
        );
    }

    #[test]
    fn ids_are_stable_across_scans_and_distinct_across_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("Ash.Verge.2021.mkv"), 16);
        touch(&root.join("Bramble.Gate.2022.mkv"), 16);

        let config = library(root, LibraryKind::Movie);
        let first = scan(&config).unwrap();
        let second = scan(&config).unwrap();
        let keys = |o: &ScanOutcome| o.items.iter().map(Discovered::key).collect::<Vec<_>>();
        assert_eq!(keys(&first), keys(&second));
        assert_ne!(first.items[0].file_id, first.items[1].file_id);
        assert!(first.items.iter().all(|i| i.file_id >= 0));
    }

    #[test]
    fn a_missing_or_non_directory_library_fails_the_scan() {
        let dir = tempfile::tempdir().unwrap();
        let missing = library(&dir.path().join("nope"), LibraryKind::Movie);
        assert!(scan(&missing).is_err());

        let file = dir.path().join("a.mkv");
        touch(&file, 8);
        assert!(scan(&library(&file, LibraryKind::Movie)).is_err());
    }

    /// An empty root is what an unmounted bind mount looks like; scanning it
    /// to nothing would withdraw every share this source owns.
    #[test]
    fn an_empty_library_fails_the_scan_rather_than_withdrawing() {
        let dir = tempfile::tempdir().unwrap();
        let err = scan(&library(dir.path(), LibraryKind::Movie)).unwrap_err();
        assert!(err.to_string().contains("empty"), "got {err:#}");
    }

    /// A root that holds *something* — even nothing shareable — is a mounted
    /// filesystem, and scanning it to nothing is an honest answer.
    #[test]
    fn a_library_with_no_media_scans_to_nothing() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("notes.txt"), 8);
        let outcome = scan(&library(dir.path(), LibraryKind::Movie)).unwrap();
        assert!(outcome.items.is_empty());
        assert_eq!(outcome.skipped, 0);
        assert_eq!(outcome.incomplete, 0);
    }

    #[test]
    fn a_relative_library_path_fails_the_scan() {
        let relative = library(Path::new("media/extras"), LibraryKind::Movie);
        let err = scan(&relative).unwrap_err();
        assert!(err.to_string().contains("absolute"), "got {err:#}");
    }

    #[test]
    fn one_broken_entry_fails_the_whole_multi_library_scan() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("ok/Ash.Verge.2021.mkv"), 16);

        let scanner = DirectoryScanner::new(vec![
            library(&root.join("ok"), LibraryKind::Movie),
            library(&root.join("missing"), LibraryKind::Tv),
        ]);
        assert!(
            scanner.scan_all().is_err(),
            "a partial scan must fail wholesale or withdrawn shares would follow"
        );
    }

    /// Overlapping roots discover the inner files twice under the same store
    /// key with conflicting specs; the configuration is rejected wholesale.
    #[test]
    fn overlapping_library_roots_fail_the_scan() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("tapes/Ash.Verge.S01E01.mkv"), 16);

        let scanner = DirectoryScanner::new(vec![
            library(root, LibraryKind::Movie),
            library(&root.join("tapes"), LibraryKind::Tv),
        ]);
        let err = scanner.scan_all().unwrap_err();
        assert!(err.to_string().contains("overlap"), "got {err:#}");
    }

    /// A hand-curated share folder is naturally built from symlinks into the
    /// real library; each must be shared via the path the operator gave.
    #[test]
    #[cfg(unix)]
    fn a_symlinked_file_is_shared_and_a_symlinked_directory_is_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("real/Gilded.Ferry.2019.mkv"), 64);
        let share = root.join("share");
        fs::create_dir(&share).unwrap();
        std::os::unix::fs::symlink(
            root.join("real/Gilded.Ferry.2019.mkv"),
            share.join("Gilded.Ferry.2019.mkv"),
        )
        .unwrap();
        // A symlinked directory is how walks loop forever; it stays skipped.
        std::os::unix::fs::symlink(root.join("real"), share.join("loop")).unwrap();
        // A dangling link has nothing to share and must not fail the pass.
        std::os::unix::fs::symlink(root.join("gone.mkv"), share.join("Bramble.Gate.2022.mkv"))
            .unwrap();

        let outcome = scan(&library(&share, LibraryKind::Movie)).unwrap();
        assert_eq!(outcome.items.len(), 1, "the symlinked file must be found");
        let item = &outcome.items[0];
        assert_eq!(item.arr_path, share.join("Gilded.Ferry.2019.mkv"));
        assert_eq!(item.size, 64, "the size is the target's, not the link's");
        assert_eq!(outcome.incomplete, 0);
    }

    /// One unreadable subdirectory marks the scan incomplete instead of failing
    /// it: the readable files are still shared, and the incomplete flag is what
    /// stops the missing subtree reading as withdrawn.
    #[test]
    #[cfg(unix)]
    fn an_unreadable_subdirectory_marks_the_scan_incomplete() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("Ash.Verge.2021.mkv"), 16);
        let locked = root.join("lost+found");
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        let outcome = scan(&library(root, LibraryKind::Movie));
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();

        let outcome = outcome.unwrap();
        assert_eq!(outcome.items.len(), 1, "the readable file must be shared");
        assert_eq!(outcome.incomplete, 1);
    }

    /// The movie fallback publishes a *title*, not a filename: release cruft is
    /// cut off, and names with no title at all are skipped, not invented.
    #[test]
    fn movie_titles_shed_release_cruft_and_junk_names_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("Film.Title.1080p.BluRay.x264-GROUP.mkv"), 16);
        // Episode-shaped: misfiled television, not a movie to advertise.
        touch(&root.join("Lanternwick.Hollow.S02E01.mkv"), 16);
        // All cruft, no title.
        touch(&root.join("1080p.x264.mkv"), 16);

        let outcome = scan(&library(root, LibraryKind::Movie)).unwrap();
        assert_eq!(outcome.items.len(), 1);
        assert_eq!(outcome.skipped, 2);
        assert_eq!(
            outcome.items[0].spec,
            MediaSpec::Movie {
                title: "Film Title".to_owned(),
                year: None,
            }
        );
    }
}
