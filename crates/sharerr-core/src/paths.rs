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
    /// Sorted longest-prefix-first so nested mounts resolve to the most specific rule.
    maps: Vec<PathMapping>,
}

impl PathResolver {
    pub fn new(mut maps: Vec<PathMapping>) -> Self {
        maps.sort_by_key(|m| std::cmp::Reverse(m.arr.components().count()));
        Self { maps }
    }

    /// Resolve an *arr-reported path into all three views.
    pub fn resolve(&self, arr_path: &Path) -> Result<ResolvedPaths, PathError> {
        if !arr_path.is_absolute() {
            return Err(PathError::NotAbsolute(arr_path.to_path_buf()));
        }

        for map in &self.maps {
            // `strip_prefix` compares whole components, so `/tv` does not match
            // `/tv-archive/...` the way a string prefix check would.
            if let Ok(rest) = arr_path.strip_prefix(&map.arr) {
                let sharerr = map.sharerr.join(rest);
                let qbit = map.qbit.as_ref().unwrap_or(&map.sharerr).join(rest);
                return Ok(ResolvedPaths {
                    arr: arr_path.to_path_buf(),
                    sharerr,
                    qbit,
                    mapping_applied: true,
                });
            }
        }

        Ok(ResolvedPaths {
            arr: arr_path.to_path_buf(),
            sharerr: arr_path.to_path_buf(),
            qbit: arr_path.to_path_buf(),
            mapping_applied: false,
        })
    }
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
    }
}
