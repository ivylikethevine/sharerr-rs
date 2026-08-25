//! Translation between the three views of the media library.
//!
//! A single media file can have three different absolute paths at once:
//!
//! | View       | Who reports it                       |
//! |------------|--------------------------------------|
//! | `arr`      | Sonarr's `episodeFile.path`          |
//! | `sharerr`  | what this process must `open()`      |
//! | `qbit`     | what qBittorrent must be told        |
//!
//! They diverge whenever the containers mount the library at different points,
//! which is the common case rather than the exception. Mismatches here are the
//! most likely cause of "sharerr silently does nothing", so resolution reports
//! whether a mapping actually applied — see [`ResolvedPaths::mapping_applied`]
//! and the `doctor` command that surfaces it.

use std::path::{Path, PathBuf};

use crate::config::PathMapping;
use crate::model::MediaSource;

#[derive(Debug, thiserror::Error)]
pub enum PathError {
    #[error("expected an absolute path from the *arr API, got {0:?}")]
    NotAbsolute(PathBuf),
}

/// All three views of one file, plus whether a configured mapping produced them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPaths {
    pub arr: PathBuf,
    pub sharerr: PathBuf,
    pub qbit: PathBuf,
    /// `false` means no rule matched and the paths passed through unchanged.
    /// That is correct when every container shares the same mounts, and a bug
    /// otherwise — `doctor` reports it alongside a `stat` of each view.
    pub mapping_applied: bool,
}

#[derive(Debug, Clone, Default)]
/// Translates one file path between the *arr view, sharerr's view, and
/// qBittorrent's view of the same library.
pub struct PathResolver {
    maps: Vec<PathMapping>,
}

impl PathResolver {
    pub fn new(maps: Vec<PathMapping>) -> Self {
        Self { maps }
    }

    /// Resolve a path according to where it came from.
    ///
    /// The rule this encodes — a [`MediaSource::Directory`] item was scanned from
    /// sharerr's own view, so only the sharerr→qbit half of a mapping may apply
    /// to it — decides which file qBittorrent is pointed at. It lives here,
    /// beside the two resolvers it chooses between, rather than at the call
    /// sites: the reconciliation loop and the preflight checks both need it, and
    /// when each carried its own copy the invariant was one edit away from
    /// disagreeing with itself.
    pub fn resolve_for(
        &self,
        source: MediaSource,
        path: &Path,
    ) -> Result<ResolvedPaths, PathError> {
        match source {
            MediaSource::Directory => self.resolve_sharerr(path),
            _ => self.resolve(path),
        }
    }

    /// Resolve an *arr-reported path into all three views.
    pub fn resolve(&self, arr_path: &Path) -> Result<ResolvedPaths, PathError> {
        if !arr_path.is_absolute() {
            return Err(PathError::NotAbsolute(arr_path.to_path_buf()));
        }

        Ok(
            match most_specific_match(&self.maps, |m| &m.arr, arr_path) {
                Some((map, rest)) => {
                    let sharerr = map.sharerr.join(&rest);
                    let qbit = map.qbit.as_ref().unwrap_or(&map.sharerr).join(&rest);
                    ResolvedPaths {
                        arr: arr_path.to_path_buf(),
                        sharerr,
                        qbit,
                        mapping_applied: true,
                    }
                }
                None => ResolvedPaths {
                    arr: arr_path.to_path_buf(),
                    sharerr: arr_path.to_path_buf(),
                    qbit: arr_path.to_path_buf(),
                    mapping_applied: false,
                },
            },
        )
    }

    /// Resolve a path sharerr itself discovered — a `[[library]]` file.
    ///
    /// The path is already the sharerr view, so no arr-side rule may rewrite it:
    /// a `[[path_map]]` whose `arr` prefix happens to match the library would
    /// otherwise translate the path into one that exists nowhere. The only
    /// translation that can apply is sharerr→qbit, taken from the most specific
    /// rule whose `sharerr` prefix matches — the same most-specific-wins choice
    /// [`Self::resolve`] makes on the arr side, via the same `most_specific_match`.
    /// A more specific rule that leaves `qbit` unset still wins the match and
    /// still means "no translation" rather than falling through to a less
    /// specific rule that happens to define one: two sources of the same file
    /// (an *arr-reported path and a directory scan) must resolve to the same
    /// qBittorrent path, or a `skip_checking` add can point at the wrong one.
    /// With no matching rule at all the path passes through to all three views,
    /// which is correct when qBittorrent shares sharerr's mounts.
    pub fn resolve_sharerr(&self, path: &Path) -> Result<ResolvedPaths, PathError> {
        if !path.is_absolute() {
            return Err(PathError::NotAbsolute(path.to_path_buf()));
        }

        let mapped = most_specific_match(&self.maps, |m| &m.sharerr, path)
            .and_then(|(map, rest)| map.qbit.as_ref().map(|prefix| prefix.join(&rest)));
        let mapping_applied = mapped.is_some();
        let qbit = mapped.unwrap_or_else(|| path.to_path_buf());
        Ok(ResolvedPaths {
            arr: path.to_path_buf(),
            sharerr: path.to_path_buf(),
            qbit,
            mapping_applied,
        })
    }
}

/// The single most specific configured mapping whose prefix (as picked by
/// `prefix_of`) matches `path`, and the path remaining after stripping it.
///
/// "Most specific" means the longest prefix by component count, not by
/// configuration order — [`PathResolver::resolve`] and
/// [`PathResolver::resolve_sharerr`] used to each implement their own version
/// of this, and disagreed on what "most specific" meant when a matching rule
/// didn't define the field the caller needed: `resolve` already let its most
/// specific *arr* match win even without a `qbit` override, but
/// `resolve_sharerr` skipped a specific match lacking `qbit` and fell through
/// to a less specific rule that had one — producing two different
/// qBittorrent paths for the same file depending on whether it was reached
/// via an *arr report or a library scan. One function, used by both, removes
/// the chance of that drifting apart again.
fn most_specific_match<'m>(
    maps: &'m [PathMapping],
    prefix_of: impl Fn(&PathMapping) -> &Path,
    path: &Path,
) -> Option<(&'m PathMapping, PathBuf)> {
    maps.iter()
        // `strip_prefix` compares whole components, so `/tv` does not match
        // `/tv-archive/...` the way a string prefix check would.
        .filter_map(|map| {
            let prefix = prefix_of(map);
            let rest = path.strip_prefix(prefix).ok()?;
            Some((prefix.components().count(), map, rest.to_path_buf()))
        })
        .max_by_key(|(specificity, _, _)| *specificity)
        .map(|(_, map, rest)| (map, rest))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn map(arr: &str, sharerr: &str, qbit: Option<&str>) -> PathMapping {
        PathMapping {
            arr: PathBuf::from(arr),
            sharerr: PathBuf::from(sharerr),
            qbit: qbit.map(PathBuf::from),
        }
    }

    #[test]
    fn identity_when_no_mappings_configured() {
        let r = PathResolver::default();
        let out = r.resolve(Path::new("/tv/Show/ep.mkv")).unwrap();
        assert_eq!(out.sharerr, PathBuf::from("/tv/Show/ep.mkv"));
        assert_eq!(out.qbit, PathBuf::from("/tv/Show/ep.mkv"));
        assert!(!out.mapping_applied);
    }

    #[test]
    fn rewrites_all_three_views() {
        let r = PathResolver::new(vec![map("/tv", "/media/tv", Some("/downloads/tv"))]);
        let out = r.resolve(Path::new("/tv/Show/ep.mkv")).unwrap();
        assert_eq!(out.arr, PathBuf::from("/tv/Show/ep.mkv"));
        assert_eq!(out.sharerr, PathBuf::from("/media/tv/Show/ep.mkv"));
        assert_eq!(out.qbit, PathBuf::from("/downloads/tv/Show/ep.mkv"));
        assert!(out.mapping_applied);
    }

    #[test]
    fn qbit_defaults_to_sharerr_view_when_omitted() {
        let r = PathResolver::new(vec![map("/tv", "/media/tv", None)]);
        let out = r.resolve(Path::new("/tv/Show/ep.mkv")).unwrap();
        assert_eq!(out.qbit, PathBuf::from("/media/tv/Show/ep.mkv"));
    }

    #[test]
    fn longest_prefix_wins_for_nested_mounts() {
        let r = PathResolver::new(vec![
            map("/media", "/a", None),
            map("/media/tv/anime", "/c", None),
            map("/media/tv", "/b", None),
        ]);
        assert_eq!(
            r.resolve(Path::new("/media/movies/x.mkv")).unwrap().sharerr,
            PathBuf::from("/a/movies/x.mkv")
        );
        assert_eq!(
            r.resolve(Path::new("/media/tv/x.mkv")).unwrap().sharerr,
            PathBuf::from("/b/x.mkv")
        );
        assert_eq!(
            r.resolve(Path::new("/media/tv/anime/x.mkv"))
                .unwrap()
                .sharerr,
            PathBuf::from("/c/x.mkv")
        );
    }

    #[test]
    fn component_boundaries_are_respected() {
        // A naive string-prefix check would rewrite this; component-wise must not.
        let r = PathResolver::new(vec![map("/tv", "/media/tv", None)]);
        let out = r.resolve(Path::new("/tv-archive/Show/ep.mkv")).unwrap();
        assert_eq!(out.sharerr, PathBuf::from("/tv-archive/Show/ep.mkv"));
        assert!(!out.mapping_applied);
    }

    #[test]
    fn relative_paths_are_rejected() {
        let r = PathResolver::default();
        assert!(matches!(
            r.resolve(Path::new("tv/ep.mkv")),
            Err(PathError::NotAbsolute(_))
        ));
        assert!(matches!(
            r.resolve_sharerr(Path::new("tv/ep.mkv")),
            Err(PathError::NotAbsolute(_))
        ));
    }

    /// The trap this method exists to avoid: an arr-side rule whose prefix
    /// happens to match a library path must not rewrite a path that is already
    /// the sharerr view.
    #[test]
    fn a_sharerr_view_path_is_never_rewritten_by_an_arr_rule() {
        let r = PathResolver::new(vec![map("/media", "/mnt/media", None)]);
        let out = r.resolve_sharerr(Path::new("/media/extras/x.mkv")).unwrap();
        assert_eq!(out.sharerr, PathBuf::from("/media/extras/x.mkv"));
        assert_eq!(out.qbit, PathBuf::from("/media/extras/x.mkv"));
        assert!(!out.mapping_applied);
    }

    #[test]
    fn a_sharerr_view_path_still_gets_its_qbit_translation() {
        let r = PathResolver::new(vec![
            map("/tv", "/media", Some("/downloads")),
            map("/tv/extras", "/media/extras", Some("/downloads/extras")),
        ]);
        let out = r.resolve_sharerr(Path::new("/media/extras/x.mkv")).unwrap();
        assert_eq!(out.sharerr, PathBuf::from("/media/extras/x.mkv"));
        // The most specific sharerr prefix wins, exactly as it does arr-side.
        assert_eq!(out.qbit, PathBuf::from("/downloads/extras/x.mkv"));
        assert!(out.mapping_applied);
    }

    #[test]
    fn a_rule_without_a_qbit_view_leaves_a_sharerr_path_alone() {
        let r = PathResolver::new(vec![map("/arr/tv", "/media", None)]);
        let out = r.resolve_sharerr(Path::new("/media/x.mkv")).unwrap();
        assert_eq!(out.qbit, PathBuf::from("/media/x.mkv"));
        assert!(!out.mapping_applied);
    }

    /// The bug `resolve` and `resolve_sharerr` used to disagree on: a specific
    /// rule with no `qbit` override sits ahead of a general rule that has one.
    /// `resolve` already let the specific arr-side match win outright, with no
    /// fallback to the general rule's `qbit`; `resolve_sharerr` used to skip
    /// the specific match (it has no `qbit`) and fall through to the general
    /// one instead, producing a second, different qBittorrent path for the
    /// same file. Both entry points must now agree.
    #[test]
    fn a_specific_rule_without_qbit_wins_over_a_general_rule_with_one() {
        let r = PathResolver::new(vec![
            map("/tv", "/media", Some("/downloads")),
            map("/tv/extras", "/media/extras", None),
        ]);

        let via_arr = r.resolve(Path::new("/tv/extras/x.mkv")).unwrap();
        assert_eq!(via_arr.qbit, PathBuf::from("/media/extras/x.mkv"));

        let via_directory_scan = r.resolve_sharerr(Path::new("/media/extras/x.mkv")).unwrap();
        assert_eq!(
            via_directory_scan.qbit, via_arr.qbit,
            "the same file must resolve to the same qBittorrent path regardless of entry point"
        );
        assert!(!via_directory_scan.mapping_applied);
    }
}
