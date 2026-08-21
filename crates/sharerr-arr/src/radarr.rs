//! Radarr discovery.
//!
//! Simpler than Sonarr: tags are movie-level, which is naturally per-item, and the
//! file is normally embedded in the movie resource so one call covers the library.

use std::path::PathBuf;

use futures::stream::{self, StreamExt, TryStreamExt};
use sharerr_core::{ExternalIds, MediaSource, MediaSpec};

use crate::client::ArrClient;
use crate::error::Result;
use crate::models::{Movie, MovieFile, non_empty, non_zero};
use crate::{DISCOVERY_CONCURRENCY, Discovered};

pub(crate) async fn discover(client: &ArrClient, tag_id: i64) -> Result<Vec<Discovered>> {
    let movies: Vec<Movie> = client.get("movie", &[]).await?;
    let tagged: Vec<&Movie> = movies.iter().filter(|m| m.tags.contains(&tag_id)).collect();

    tracing::debug!(
        total = movies.len(),
        tagged = tagged.len(),
        "radarr movies scanned for the sharerr tag"
    );

    // Most movies carry their file embedded and this makes no HTTP call at
    // all, but the fallback lookup for the rest runs concurrently rather than
    // one at a time — same pattern as `sonarr::discover`.
    let lookups: Vec<(i64, Option<MovieFile>, bool)> = tagged
        .iter()
        .map(|m| (m.id, m.movie_file.clone(), m.has_file))
        .collect();
    let fetched: Vec<Option<MovieFile>> = stream::iter(lookups)
        .map(|(id, embedded, has_file)| movie_file(client, id, embedded, has_file))
        .buffered(DISCOVERY_CONCURRENCY)
        .try_collect()
        .await?;

    let mut discovered = Vec::new();
    for (movie, file) in tagged.into_iter().zip(fetched) {
        let Some(file) = file else {
            tracing::debug!(movie = %movie.title, "tagged but has no file on disk");
            continue;
        };

        discovered.push(Discovered {
            source: MediaSource::Radarr,
            source_id: movie.id,
            file_id: file.id,
            spec: MediaSpec::Movie {
                title: movie.title.clone(),
                year: non_zero(movie.year),
            },
            arr_path: PathBuf::from(&file.path),
            size: file.size,
            ids: ExternalIds {
                tvdb: None,
                tmdb: non_zero(movie.tmdb_id),
                tvmaze: None,
                imdb: non_empty(movie.imdb_id.clone()),
                ..ExternalIds::default()
            },
            scene_name: non_empty(file.scene_name),
        });
    }

    Ok(discovered)
}

/// Prefer the embedded `movieFile`, falling back to `/moviefile?movieId=` only when
/// Radarr claims a file exists but did not inline it.
///
/// Takes the movie's fields by value rather than `&Movie`: a future borrowing
/// a per-item reference out of `tagged` is not general enough over lifetimes
/// for `StreamExt::buffered`, and owned fields sidestep the question — see
/// `sonarr::fetch_series`'s doc comment for the same trade.
async fn movie_file(
    client: &ArrClient,
    id: i64,
    embedded: Option<MovieFile>,
    has_file: bool,
) -> Result<Option<MovieFile>> {
    if let Some(file) = embedded {
        return Ok(Some(file));
    }

    if !has_file {
        return Ok(None);
    }

    let files: Vec<MovieFile> = client
        .get("moviefile", &[("movieId", id.to_string())])
        .await?;
    Ok(files.into_iter().next())
}
