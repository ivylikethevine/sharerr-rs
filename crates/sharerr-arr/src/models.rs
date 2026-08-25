//! Wire types for the Sonarr/Radarr v3 APIs.
//!
//! Only the fields sharerr actually uses are declared; both APIs return a great
//! deal more, and every field is `#[serde(default)]` where the app is known to omit
//! it. A new *arr release adding fields must never break discovery.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Tag {
    pub id: i64,
    pub label: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
/// What `/system/status` reports: enough to prove the service is reachable and
/// the key is accepted.
pub struct SystemStatus {
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub app_name: String,
}

// ---------------------------------------------------------------- Sonarr

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Series {
    pub id: i64,
    pub title: String,
    #[serde(default)]
    pub tvdb_id: Option<i64>,
    #[serde(default)]
    pub tv_maze_id: Option<i64>,
    #[serde(default)]
    pub imdb_id: Option<String>,
    #[serde(default)]
    pub tags: Vec<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EpisodeFile {
    pub id: i64,
    pub path: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub scene_name: Option<String>,
}

/// An episode record. Sonarr keeps episode numbering here rather than on the file,
/// because one file can cover several episodes.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Episode {
    #[serde(default)]
    pub season_number: u32,
    #[serde(default)]
    pub episode_number: u32,
    /// `0` when the episode has no file on disk.
    #[serde(default)]
    pub episode_file_id: i64,
}

// ---------------------------------------------------------------- Radarr

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Movie {
    pub id: i64,
    pub title: String,
    #[serde(default)]
    pub year: Option<u16>,
    #[serde(default)]
    pub tmdb_id: Option<i64>,
    #[serde(default)]
    pub imdb_id: Option<String>,
    #[serde(default)]
    pub tags: Vec<i64>,
    #[serde(default)]
    pub has_file: bool,
    /// Radarr embeds the file in the movie resource; older versions and some
    /// trimmed responses omit it, hence the `moviefile` fallback in [`crate::radarr`].
    #[serde(default)]
    pub movie_file: Option<MovieFile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MovieFile {
    pub id: i64,
    pub path: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub scene_name: Option<String>,
    /// Where the file was before Radarr renamed it on import. Sonarr's
    /// `EpisodeFileResource` has no counterpart, which is why this field sits on
    /// [`MovieFile`] rather than alongside the shared ones.
    #[serde(default)]
    pub original_file_path: Option<String>,
}

/// Both apps use `""` and `0` for "unset" in places a JSON `null` would be more
/// honest. Collapse those to `None` so callers do not have to know which.
pub(crate) fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|s| !s.trim().is_empty())
}

pub(crate) fn non_zero<T: PartialEq + Default>(value: Option<T>) -> Option<T> {
    value.filter(|v| *v != T::default())
}
