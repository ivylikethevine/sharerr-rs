//! The plain-directory library source: share what is in a folder, no *arr app.
//!
//! Each `[[library]]` entry is a directory scanned recursively for media files
//! of its declared [`LibraryKind`]. There is no tag and no metadata service —
//! being in the directory *is* the tag, the filename is all the metadata there
//! is, and every item ships with [`ExternalIds::default`], so a friend's app
//! can only parse the release name. That is the documented trade of the
//! zero-dependency path.
//!
//! The scan is deliberately all-or-nothing across every configured entry: the
//! reconciliation loop withdraws items a *successful* scan no longer reports,
//! so a partial listing that silently dropped one unreadable directory would
//! read as "untagged" and tear its shares down. One entry failing fails the
//! whole scan, and the loop leaves everything alone — same contract as an *arr
//! app that did not answer.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use sharerr_core::config::{LibraryConfig, LibraryKind};
use sharerr_core::{Discovered, ExternalIds, MediaSource, MediaSpec};
use sharerr_torrent::title::{self, ParsedTitle};

/// Directories deeper than this are not descended into. A media library is a
/// few levels deep; a walk that is fifty levels down has hit a cycle the
/// symlink check missed or a pathological tree, and either way should stop
/// rather than spin.
const MAX_DEPTH: usize = 16;

/// What scanning one library produced.
#[derive(Debug, Default)]
pub struct ScanOutcome {
    pub items: Vec<Discovered>,
    /// Media files left out because their names could not be classified — a tv
    /// file with no `SxxEyy` has no episode to advertise, and inventing one
    /// would publish a release that downloads the wrong thing.
    pub skipped: usize,
}

/// Scan one `[[library]]` directory.
///
/// Synchronous on purpose — it is filesystem-bound and callers run it on a
/// blocking thread. Items come back in a deterministic (sorted) order so runs
/// are comparable.
pub fn scan(library: &LibraryConfig) -> Result<ScanOutcome> {
    let root = &library.path;
    let metadata = fs::metadata(root)
        .with_context(|| format!("library {} is not readable", root.display()))?;
    if !metadata.is_dir() {
        bail!("library {} is not a directory", root.display());
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
) -> Result<()> {
    if depth > MAX_DEPTH {
        tracing::warn!(
            dir = %dir.display(),
            "library walk stopped at depth {MAX_DEPTH}; not descending further"
        );
        return Ok(());
    }

    let entries =
        fs::read_dir(dir).with_context(|| format!("could not read {}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("could not read {}", dir.display()))?;
        let path = entry.path();
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }

        // `symlink_metadata` so a link is seen as a link: a symlinked directory
        // is skipped (it is how walks loop forever), and a symlinked file is
        // shared via the path the operator gave, not its target.
        let meta = entry
            .metadata()
            .with_context(|| format!("could not stat {}", path.display()))?;
        if meta.is_dir() {
            if fs::symlink_metadata(&path)
                .with_context(|| format!("could not stat {}", path.display()))?
                .is_symlink()
            {
                tracing::debug!(path = %path.display(), "skipping symlinked directory");
                continue;
            }
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
                arr_path: path.clone(),
                size: meta.len(),
                ids: ExternalIds::default(),
                scene_name: None,
            }),
            None => {
                outcome.skipped += 1;
                tracing::warn!(
                    file = %path.display(),
                    "skipped: the name has no SxxEyy, so there is no episode to advertise"
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
            // Still listable: a title without a year searches fine, it just
            // cannot be pinned to a release year on the far end.
            ParsedTitle::Episode { .. } | ParsedTitle::Unparseable => Some(MediaSpec::Movie {
                title: humanize(&stem),
                year: None,
            }),
        },
        // Music libraries are conventionally artist/album/track on disk, so the
        // directory names are the closest thing to metadata a bare file has.
        LibraryKind::Music => Some(MediaSpec::Track {
            artist: dir_name(root, path, 2).unwrap_or_else(|| "Unknown Artist".to_owned()),
            album: dir_name(root, path, 1).unwrap_or_else(|| "Unknown Album".to_owned()),
            track: leading_number(&stem),
        }),
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
fn humanize(stem: &str) -> String {
    stem.replace(['.', '_'], " ").trim().to_owned()
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
    pub fn scan_all(&self) -> Result<Vec<Discovered>> {
        let mut items = Vec::new();
        for library in &self.libraries {
            let outcome = scan(library)?;
            if outcome.skipped > 0 {
                tracing::warn!(
                    library = %library.path.display(),
                    skipped = outcome.skipped,
                    "some files were skipped because their names could not be classified"
                );
            }
            items.extend(outcome.items);
        }
        Ok(items)
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
        touch(&root.join("Vermilion Choir/Salt Orchard/03 - Tidewrack.flac"), 16);
        touch(&root.join("stray.flac"), 16);

        let outcome = scan(&library(root, LibraryKind::Music)).unwrap();
        assert_eq!(outcome.items.len(), 2);
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
        let stray = outcome
            .items
            .iter()
            .find(|i| i.arr_path.ends_with("stray.flac"))
            .unwrap();
        assert_eq!(
            stray.spec,
            MediaSpec::Track {
                artist: "Unknown Artist".to_owned(),
                album: "Unknown Album".to_owned(),
                track: None,
            }
        );
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

    #[test]
    fn an_empty_library_scans_to_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = scan(&library(dir.path(), LibraryKind::Movie)).unwrap();
        assert!(outcome.items.is_empty());
        assert_eq!(outcome.skipped, 0);
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
}
