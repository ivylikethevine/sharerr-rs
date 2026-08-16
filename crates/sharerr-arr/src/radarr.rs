//! Radarr discovery.
//!
//! Simpler than Sonarr: tags are movie-level, which is naturally per-item, and the
//! file is normally embedded in the movie resource so one call covers the library.

use std::path::PathBuf;

use sharerr_core::{ExternalIds, MediaSource, MediaSpec};

use crate::Discovered;
use crate::client::ArrClient;
use crate::error::Result;
use crate::models::{Movie, MovieFile, non_empty, non_zero};

pub(crate) async fn discover(client: &ArrClient, tag_id: i64) -> Result<Vec<Discovered>> {
    let movies: Vec<Movie> = client.get("movie", &[]).await?;
    let tagged: Vec<&Movie> = movies.iter().filter(|m| m.tags.contains(&tag_id)).collect();

    tracing::debug!(
        total = movies.len(),
        tagged = tagged.len(),
        "radarr movies scanned for the sharerr tag"
    );

    let mut discovered = Vec::new();
    for movie in tagged {
        let Some(file) = movie_file(client, movie).await? else {
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
async fn movie_file(client: &ArrClient, movie: &Movie) -> Result<Option<MovieFile>> {
    if let Some(file) = &movie.movie_file {
        return Ok(Some(file.clone()));
    }

    if !movie.has_file {
        return Ok(None);
    }

    let files: Vec<MovieFile> = client
        .get("moviefile", &[("movieId", movie.id.to_string())])
        .await?;
    Ok(files.into_iter().next())
}
