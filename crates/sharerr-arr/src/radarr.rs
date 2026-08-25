//! Radarr discovery.
//!
//! Simpler than Sonarr: tags are movie-level, which is naturally per-item, and the
//! file is normally embedded in the movie resource so one call covers the library.

use std::path::PathBuf;

use sharerr_core::{ExternalIds, MediaSource, MediaSpec};

use crate::client::ArrClient;
use crate::error::Result;
use crate::models::{Movie, MovieFile, non_empty, non_zero};
use crate::{Discovered, Tagged, fetch_tagged};

impl Tagged for Movie {
    fn tags(&self) -> &[i64] {
        &self.tags
    }
}

pub(crate) async fn discover(client: &ArrClient, tag_id: i64) -> Result<Vec<Discovered>> {
    let movies: Vec<Movie> = client.get("movie", &()).await?;
    // Most movies carry their file embedded and this makes no HTTP call at
    // all, but the fallback lookup for the rest runs concurrently rather than
    // one at a time — see `fetch_tagged`.
    let fetched = fetch_tagged(client, &movies, tag_id, "radarr movies", movie_file).await?;

    let mut discovered = Vec::new();
    for (movie, fetched) in fetched {
        let Some(file) = movie.movie_file.as_ref().or(fetched.as_ref()) else {
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
                tmdb: non_zero(movie.tmdb_id),
                imdb: non_empty(movie.imdb_id.clone()),
                ..ExternalIds::default()
            },
            scene_name: non_empty(file.scene_name.clone()),
            original_path: non_empty(file.original_file_path.clone()).map(PathBuf::from),
        });
    }

    Ok(discovered)
}

/// Look up `/moviefile?movieId=` only when Radarr claims a file exists but did
/// not inline it. The embedded `movieFile` is preferred by the caller, so this
/// yields `None` without a request for a movie that carries one.
async fn movie_file(client: &ArrClient, movie: &Movie) -> Result<Option<MovieFile>> {
    if movie.movie_file.is_some() || !movie.has_file {
        return Ok(None);
    }

    let files: Vec<MovieFile> = client.get("moviefile", &[("movieId", movie.id)]).await?;
    Ok(files.into_iter().next())
}
