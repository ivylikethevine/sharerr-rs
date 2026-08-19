//! Suggests `[[path_map]]` rules instead of making the operator derive them —
//! `sharerr doctor --suggest-paths`.
//!
//! # The chicken-and-egg problem, and how this avoids it
//!
//! sharerr is told an *arr app's file path (`/tv/Show/S01/ep.mkv`) but does not
//! know where under its *own* filesystem the bytes actually live — that is
//! exactly the unconfigured mapping. The tempting fix, walking the whole
//! container from `/` looking for a same-named file, is the kind of guess this
//! project explicitly refuses to make: a wrong match here is worse than no
//! match, since sharerr would confidently propose the wrong root. So this
//! module never searches anywhere the operator has not already named — one
//! `search_root`, given explicitly (`--search-root`) or defaulted to `/media`,
//! the path every deployment example in this repository already mounts
//! sharerr's view of the library at.
//!
//! # The match itself
//!
//! Basename and exact byte size, matched against an index of `search_root`.
//! Neither is proof — a re-encode or a same-sized extras file could collide —
//! so a name+size pair that matches more than one file under the root is
//! treated as *no* match rather than a coin flip, and every suggestion is
//! surfaced as a proposal with its agreement count, never written to
//! `sharerr.toml` automatically. Turning a proposal into a real rule is still
//! the operator pasting it into Settings or the config file, same as every
//! other suggestion this codebase makes (`ArrOutcome`, `DirOutcome`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use sharerr_core::Discovered;

/// Directories deeper than this are not descended into while indexing the
/// search root — the same bound `library::scan` uses, for the same reason: a
/// walk this deep has hit a cycle or a pathological tree, not a real library.
const MAX_DEPTH: usize = 16;

/// One proposed `arr = "..."` / `sharerr = "..."` pair, derived from matching
/// discovered files against what actually exists under the search root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub arr: PathBuf,
    pub sharerr: PathBuf,
    /// How many discovered files independently produced this exact pair. One
    /// matching file could be a name/size coincidence; a dozen agreeing on the
    /// same prefix pair are not — this is the confidence signal a caller
    /// should sort and report by, not any single match.
    pub agreement: usize,
}

/// Propose mappings for `discovered` by matching each item's basename and
/// size against an index of `search_root`.
///
/// Every discovered item is tried, including ones that already resolve
/// correctly under an existing rule — a matching item just reproduces that
/// rule, which is harmless, and it is the caller's job (see
/// `commands::doctor`) to drop suggestions that duplicate configured rules
/// before printing anything.
pub fn suggest(discovered: &[Discovered], search_root: &Path) -> Vec<Suggestion> {
    let index = index(search_root);
    let mut agreement: HashMap<(PathBuf, PathBuf), usize> = HashMap::new();

    for item in discovered {
        let Some(name) = item.arr_path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(candidates) = index.get(&(name.to_owned(), item.size)) else {
            continue;
        };
        // More than one file under the root shares this name and size —
        // not a signal to guess from, so this item contributes nothing.
        let [only] = candidates.as_slice() else {
            continue;
        };
        if let Some((arr, sharerr)) = common_suffix_prefixes(&item.arr_path, only) {
            *agreement.entry((arr, sharerr)).or_insert(0) += 1;
        }
    }

    let mut suggestions: Vec<Suggestion> = agreement
        .into_iter()
        .map(|((arr, sharerr), agreement)| Suggestion {
            arr,
            sharerr,
            agreement,
        })
        .collect();
    // Most-agreed-on first: what a caller should show and trust first.
    suggestions.sort_by(|a, b| b.agreement.cmp(&a.agreement).then(a.arr.cmp(&b.arr)));
    suggestions
}

/// Strip the longest common trailing run of path components from both paths,
/// returning what is left of each as the proposed prefix pair.
///
/// Deliberately does not extend the match all the way to the root even when
/// components keep agreeing — a directory name is often reused at the exact
/// boundary that matters (`/tv/...` on the *arr side, `/media/tv/...` on
/// sharerr's), and matching straight through it would strip `tv` as if it
/// were part of the shared suffix, proposing the degenerate `/` → `/media`
/// instead of `/tv` → `/media/tv`. At least one real component is kept on
/// the *arr side for exactly this reason.
///
/// `None` when there is nothing to strip (the basename did not actually
/// match — unreachable given the caller's index lookup, but checked rather
/// than assumed) or when stripping would leave the sharerr side with no root
/// at all (the whole found path *is* the matched suffix, so there is no
/// prefix left to propose).
fn common_suffix_prefixes(arr_path: &Path, sharerr_path: &Path) -> Option<(PathBuf, PathBuf)> {
    // Already the same path — sharerr's pass-through already resolves it, so
    // there is nothing to propose.
    if arr_path == sharerr_path {
        return None;
    }

    let arr_components: Vec<_> = arr_path.components().collect();
    let sharerr_components: Vec<_> = sharerr_path.components().collect();

    // Leave room for at least two components on the *arr side (its root, plus
    // one real directory name) — see the doc comment above.
    let max_matched = arr_components.len().saturating_sub(2);

    let mut matched = 0;
    while matched < max_matched
        && matched < sharerr_components.len()
        && arr_components[arr_components.len() - 1 - matched]
            == sharerr_components[sharerr_components.len() - 1 - matched]
    {
        matched += 1;
    }

    if matched == 0 || matched == sharerr_components.len() {
        return None;
    }

    let arr_prefix: PathBuf = arr_components[..arr_components.len() - matched]
        .iter()
        .collect();
    let sharerr_prefix: PathBuf = sharerr_components[..sharerr_components.len() - matched]
        .iter()
        .collect();
    Some((arr_prefix, sharerr_prefix))
}

/// Index every regular file under `root`, keyed by (filename, size).
///
/// Symlinks are never followed here, unlike `library::scan`'s deliberate
/// choice to follow a symlinked *file* — that trust is earned by the operator
/// having named the tree a `[[library]]`; a search root is comparatively
/// untrusted, so this stays conservative. Unreadable subdirectories are
/// skipped rather than failing the whole index, the same tolerance
/// `library::scan` applies, since one bad corner should not blind the search
/// to everything else under the root.
fn index(root: &Path) -> HashMap<(String, u64), Vec<PathBuf>> {
    let mut map = HashMap::new();
    index_dir(root, 0, &mut map);
    map
}

fn index_dir(dir: &Path, depth: usize, map: &mut HashMap<(String, u64), Vec<PathBuf>>) {
    if depth > MAX_DEPTH {
        tracing::warn!(dir = %dir.display(), "path-suggestion index stopped at max depth");
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_symlink() {
            continue;
        }
        if meta.is_dir() {
            index_dir(&path, depth + 1, map);
        } else if meta.is_file()
            && let Some(name) = path.file_name().and_then(|n| n.to_str())
        {
            map.entry((name.to_owned(), meta.len()))
                .or_default()
                .push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use sharerr_core::{ExternalIds, MediaSource, MediaSpec};
    use std::fs;

    fn discovered(arr_path: &str, size: u64) -> Discovered {
        Discovered {
            source: MediaSource::Sonarr,
            source_id: 1,
            file_id: 1,
            spec: MediaSpec::Episode {
                series_title: "Lanternwick Hollow".to_owned(),
                season: 1,
                episode: 1,
            },
            arr_path: PathBuf::from(arr_path),
            size,
            ids: ExternalIds::default(),
            scene_name: None,
        }
    }

    #[test]
    fn a_matching_file_proposes_the_directory_prefix_pair() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("media/tv/Lanternwick Hollow/Season 01");
        fs::create_dir_all(&media).unwrap();
        fs::write(media.join("ep.mkv"), b"hello world").unwrap();

        let items = vec![discovered(
            "/tv/Lanternwick Hollow/Season 01/ep.mkv",
            "hello world".len() as u64,
        )];
        let suggestions = suggest(&items, &dir.path().join("media"));

        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].arr, PathBuf::from("/tv"));
        assert_eq!(suggestions[0].sharerr, dir.path().join("media/tv"));
        assert_eq!(suggestions[0].agreement, 1);
    }

    #[test]
    fn agreement_counts_every_item_that_derives_the_same_pair() {
        let dir = tempfile::tempdir().unwrap();
        let season = dir.path().join("media/tv/Show/Season 01");
        fs::create_dir_all(&season).unwrap();
        fs::write(season.join("e01.mkv"), b"aaa").unwrap();
        fs::write(season.join("e02.mkv"), b"bbbb").unwrap();

        let items = vec![
            discovered("/tv/Show/Season 01/e01.mkv", 3),
            discovered("/tv/Show/Season 01/e02.mkv", 4),
        ];
        let suggestions = suggest(&items, &dir.path().join("media"));

        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].agreement, 2);
    }

    /// Two files sharing a name and size under the search root is a
    /// coincidence, not a signal — must not guess between them.
    #[test]
    fn an_ambiguous_name_and_size_match_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("media/tv/ShowA");
        let b = dir.path().join("media/tv/ShowB");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("ep.mkv"), b"same").unwrap();
        fs::write(b.join("ep.mkv"), b"same").unwrap();

        let items = vec![discovered("/tv/ShowA/ep.mkv", 4)];
        let suggestions = suggest(&items, &dir.path().join("media"));

        assert!(suggestions.is_empty(), "got {suggestions:?}");
    }

    #[test]
    fn no_match_under_the_root_proposes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("media")).unwrap();

        let items = vec![discovered("/tv/Show/ep.mkv", 4)];
        let suggestions = suggest(&items, &dir.path().join("media"));

        assert!(suggestions.is_empty());
    }

    #[test]
    fn common_suffix_prefixes_strips_the_matching_tail() {
        assert_eq!(
            common_suffix_prefixes(
                Path::new("/tv/Show/Season 01/ep.mkv"),
                Path::new("/media/tv/Show/Season 01/ep.mkv"),
            ),
            Some((PathBuf::from("/tv"), PathBuf::from("/media/tv")))
        );
    }

    #[test]
    fn identical_paths_propose_nothing() {
        assert_eq!(
            common_suffix_prefixes(Path::new("/tv/ep.mkv"), Path::new("/tv/ep.mkv")),
            None
        );
    }

    #[test]
    fn no_shared_suffix_proposes_nothing() {
        assert_eq!(
            common_suffix_prefixes(Path::new("/tv/a.mkv"), Path::new("/media/b.mkv")),
            None
        );
    }
}
