//! Domain types shared across discovery, torrent creation, and persistence.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Which *arr app a shared item came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaSource {
    Sonarr,
    Radarr,
}

impl MediaSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sonarr => "sonarr",
            Self::Radarr => "radarr",
        }
    }
}

impl fmt::Display for MediaSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Metadata IDs carried through to the friend's Sonarr/Radarr.
///
/// These are what make a shared release *matchable* on the far end — without
/// them the friend's app has only a filename to go on.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalIds {
    pub tvdb: Option<i64>,
    pub tmdb: Option<i64>,
    pub tvmaze: Option<i64>,
    pub imdb: Option<String>,
}

/// What a shared file actually contains.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum MediaSpec {
    Episode {
        series_title: String,
        season: u32,
        episode: u32,
    },
    Movie {
        title: String,
        year: Option<u16>,
    },
}

impl MediaSpec {
    pub fn title(&self) -> &str {
        match self {
            Self::Episode { series_title, .. } => series_title,
            Self::Movie { title, .. } => title,
        }
    }
}

impl fmt::Display for MediaSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Episode {
                series_title,
                season,
                episode,
            } => {
                write!(f, "{series_title} S{season:02}E{episode:02}")
            }
            Self::Movie {
                title,
                year: Some(y),
            } => write!(f, "{title} ({y})"),
            Self::Movie { title, year: None } => write!(f, "{title}"),
        }
    }
}

/// Lifecycle of a shared item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShareState {
    /// Discovered and recorded, torrent not yet created.
    Pending,
    /// Torrent created and handed to qBittorrent.
    Seeding,
    /// Tag was removed upstream; torrent withdrawn. The file is never touched.
    Unshared,
    /// Last attempt failed; see `last_error`. Retried on the next sync.
    Failed,
}

impl ShareState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Seeding => "seeding",
            Self::Unshared => "unshared",
            Self::Failed => "failed",
        }
    }
}

/// One file that has been (or is being) shared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedItem {
    pub id: Option<i64>,
    pub source: MediaSource,
    /// Series or movie id within the *arr app.
    pub source_id: i64,
    /// `episodeFile` / `movieFile` id within the *arr app.
    pub file_id: i64,
    pub spec: MediaSpec,
    /// Scene-style name the friend's Sonarr/Radarr will parse. Getting this
    /// wrong means the release is silently rejected on the far end.
    pub release_title: String,
    /// Path exactly as the *arr app reported it, before any mapping.
    pub arr_path: PathBuf,
    pub size: u64,
    pub ids: ExternalIds,
    pub info_hash: Option<String>,
    pub state: ShareState,
    pub last_error: Option<String>,
}

impl SharedItem {
    /// Stable identity of the underlying file across syncs, independent of any
    /// database id. Used to diff discovery output against stored state.
    pub fn key(&self) -> (MediaSource, i64) {
        (self.source, self.file_id)
    }
}
