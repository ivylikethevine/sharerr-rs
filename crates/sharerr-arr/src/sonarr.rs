//! Sonarr discovery.
//!
//! Two calls per tagged series: `episodefile` carries the path and size, `episode`
//! carries the numbering. They have to be joined because Sonarr deliberately keeps
//! episode numbers off the file record — a single file can cover several episodes.
//!
//! Note the granularity this implies, which is a property of Sonarr rather than a
//! choice sharerr makes: **tags apply to a series, not an episode.** Tagging a
//! series shares every episode file it has. Per-episode selection has to come from
//! sharerr's own UI later, not from *arr tags.

use std::collections::HashMap;
use std::path::PathBuf;

use sharerr_core::{ExternalIds, MediaSpec};

use crate::Discovered;
use crate::client::ArrClient;
use crate::error::Result;
use crate::models::{Episode, EpisodeFile, Series, non_empty, non_zero};

pub(crate) async fn discover(client: &ArrClient, tag_id: i64) -> Result<Vec<Discovered>> {
    let series: Vec<Series> = client.get_list("series", &[]).await?;
    let tagged: Vec<&Series> = series.iter().filter(|s| s.tags.contains(&tag_id)).collect();

    tracing::debug!(
        total = series.len(),
        tagged = tagged.len(),
        "sonarr series scanned for the sharerr tag"
    );

    let mut discovered = Vec::new();
    for show in tagged {
        let series_id = show.id.to_string();
        let files: Vec<EpisodeFile> = client
            .get_list("episodefile", &[("seriesId", series_id.clone())])
            .await?;
        if files.is_empty() {
            tracing::debug!(series = %show.title, "tagged but has no files on disk");
            continue;
        }

        let episodes: Vec<Episode> = client
            .get_list("episode", &[("seriesId", series_id)])
            .await?;
        let numbering = numbering_by_file(&episodes);

        let ids = ExternalIds {
            tvdb: non_zero(show.tvdb_id),
            tmdb: None,
            tvmaze: non_zero(show.tv_maze_id),
            imdb: non_empty(show.imdb_id.clone()),
            ..ExternalIds::default()
        };

        for file in files {
            let Some(&(season, episode)) = numbering.get(&file.id) else {
                // A file no episode points at. Sonarr can leave these behind after a
                // failed import; sharing one would produce an unnameable release.
                tracing::warn!(
                    series = %show.title,
                    file_id = file.id,
                    path = %file.path,
                    "episode file is not linked to any episode — skipping"
                );
                continue;
            };

            discovered.push(Discovered {
                // `client.kind()`, not a hardcoded Sonarr: Whisparr shares this
                // walk, and mislabelling its items would put adult content in the
                // TV category and hand it to a friend scoped to TV.
                source: client.kind(),
                source_id: show.id,
                file_id: file.id,
                spec: MediaSpec::Episode {
                    series_title: show.title.clone(),
                    season,
                    episode,
                },
                arr_path: PathBuf::from(&file.path),
                size: file.size,
                ids: ids.clone(),
                scene_name: non_empty(file.scene_name),
            });
        }
    }

    Ok(discovered)
}

/// Map each file id to the episode that names it.
///
/// When a file covers several episodes (a double-length premiere imported as one
/// file), the lowest-numbered episode wins — that is the convention scene release
/// names follow, and the release title has to parse on the friend's Sonarr.
///
/// Season 0 is Sonarr's bucket for specials, and it sorts below every real season.
/// A file linked to both a special and a regular episode would therefore be
/// advertised as the special, describing the wrong content under a title that
/// looks perfectly well-formed — so specials are ranked last rather than first.
fn numbering_by_file(episodes: &[Episode]) -> HashMap<i64, (u32, u32)> {
    /// Sort key: real seasons before specials, then lowest season and episode.
    fn rank(season: u32, episode: u32) -> (u8, u32, u32) {
        (u8::from(season == 0), season, episode)
    }

    let mut by_file: HashMap<i64, (u32, u32)> = HashMap::new();

    for ep in episodes {
        if ep.episode_file_id == 0 {
            continue;
        }
        let candidate = (ep.season_number, ep.episode_number);
        by_file
            .entry(ep.episode_file_id)
            .and_modify(|current| {
                if rank(candidate.0, candidate.1) < rank(current.0, current.1) {
                    *current = candidate;
                }
            })
            .or_insert(candidate);
    }

    by_file
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(season: u32, episode: u32, file_id: i64) -> Episode {
        Episode {
            season_number: season,
            episode_number: episode,
            episode_file_id: file_id,
        }
    }

    #[test]
    fn episodes_without_a_file_are_ignored() {
        let map = numbering_by_file(&[ep(1, 1, 0), ep(1, 2, 55)]);
        assert_eq!(map.len(), 1);
        assert_eq!(map.get(&55), Some(&(1, 2)));
    }

    #[test]
    fn a_multi_episode_file_is_named_by_its_lowest_episode() {
        // Order deliberately reversed: the result must not depend on it.
        let map = numbering_by_file(&[ep(1, 2, 90), ep(1, 1, 90)]);
        assert_eq!(map.get(&90), Some(&(1, 1)));
    }

    #[test]
    fn numbering_is_compared_across_seasons_not_just_episodes() {
        // Within a season, the lower episode names the file.
        let map = numbering_by_file(&[ep(2, 5, 12), ep(2, 6, 12)]);
        assert_eq!(map.get(&12), Some(&(2, 5)));

        // Across seasons, the lower season wins even though its episode number is
        // higher — a plain episode-number comparison would get this backwards.
        let map = numbering_by_file(&[ep(1, 9, 13), ep(2, 1, 13)]);
        assert_eq!(map.get(&13), Some(&(1, 9)));
    }

    /// Season 0 is Sonarr's specials bucket. It sorts below every real season, so a
    /// naive minimum would let a special name a file it merely shares — publishing
    /// the wrong content under a title that parses perfectly well.
    #[test]
    fn a_special_never_names_a_file_it_shares_with_a_real_episode() {
        for order in [
            vec![ep(0, 1, 14), ep(2, 5, 14)],
            // Order must not matter.
            vec![ep(2, 5, 14), ep(0, 1, 14)],
        ] {
            let map = numbering_by_file(&order);
            assert_eq!(map.get(&14), Some(&(2, 5)), "a special hijacked the title");
        }
    }

    #[test]
    fn a_file_belonging_only_to_specials_is_still_named() {
        // Nothing else to choose, so the lowest special wins rather than nothing.
        let map = numbering_by_file(&[ep(0, 3, 15), ep(0, 1, 15)]);
        assert_eq!(map.get(&15), Some(&(0, 1)));
    }
}
