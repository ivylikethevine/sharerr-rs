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
    Lidarr,
    Readarr,
    Whisparr,
}

impl MediaSource {
    /// The lowercase name used in the database and in log output.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sonarr => "sonarr",
            Self::Radarr => "radarr",
            Self::Lidarr => "lidarr",
            Self::Readarr => "readarr",
            Self::Whisparr => "whisparr",
        }
    }

    /// Which version of the *arr HTTP API this app speaks.
    ///
    /// Not cosmetic: Sonarr, Radarr and Whisparr are on `v3` while Lidarr and
    /// Readarr are on `v1`. Hardcoding one prefix is why the client could only ever
    /// have talked to the first two, and getting it wrong presents as a 404 that
    /// looks like a wrong URL rather than a wrong version.
    pub fn api_version(self) -> &'static str {
        match self {
            Self::Sonarr | Self::Radarr | Self::Whisparr => "v3",
            Self::Lidarr | Self::Readarr => "v1",
        }
    }

    /// Every source sharerr can discover from.
    pub const ALL: &'static [Self] = &[
        Self::Sonarr,
        Self::Radarr,
        Self::Lidarr,
        Self::Readarr,
        Self::Whisparr,
    ];

    /// Inverse of [`Self::as_str`], derived from it so the two cannot drift.
    ///
    /// This is *the* decoder for stored and URL-borne source names; a local
    /// string match at a call site is how the database once lost every Lidarr
    /// row to an "unknown source" error.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|s| s.as_str() == value)
    }

    /// The name as a heading or label writes it — `as_str` capitalised.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Sonarr => "Sonarr",
            Self::Radarr => "Radarr",
            Self::Lidarr => "Lidarr",
            Self::Readarr => "Readarr",
            Self::Whisparr => "Whisparr",
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
    /// MusicBrainz release-group id, for music. A string rather than a number
    /// because MusicBrainz ids are UUIDs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub musicbrainz: Option<String>,
    /// Goodreads work id, for books.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goodreads: Option<String>,
    /// ISBN, for books. Kept alongside Goodreads because indexers match on either.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isbn: Option<String>,
}

impl ExternalIds {
    /// Any IMDb id spelling reduced to the bare number.
    ///
    /// Sonarr sends `1234567`, Radarr sends `tt1234567`, and the *arr APIs
    /// return either depending on the endpoint — so every comparison and every
    /// renderer that wants the bare number goes through here rather than
    /// restating the rule.
    pub fn imdb_bare(raw: &str) -> &str {
        raw.trim().trim_start_matches("tt")
    }

    /// The stored IMDb id as [`Self::imdb_bare`] renders it, if one is known.
    pub fn imdb_numeric(&self) -> Option<&str> {
        self.imdb.as_deref().map(Self::imdb_bare)
    }
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
    /// One music file. Lidarr's unit of import is a *track file*, which may hold a
    /// whole album, so the album is the thing named and the track is optional.
    Track {
        artist: String,
        album: String,
        /// `None` when the file is the whole album rather than one track.
        track: Option<u32>,
    },
    /// One book file.
    Book {
        author: String,
        title: String,
    },
}

impl MediaSpec {
    /// The title a searcher would look for: the series, film, album, or book.
    ///
    /// For music this is the *album*, not the artist — that is what a friend's
    /// Lidarr searches on, the same way a series title is what Sonarr searches on.
    pub fn title(&self) -> &str {
        match self {
            Self::Episode { series_title, .. } => series_title,
            Self::Movie { title, .. } => title,
            Self::Track { album, .. } => album,
            Self::Book { title, .. } => title,
        }
    }

    /// The credited artist or author, when the medium has one.
    ///
    /// Music and books are searched by *creator* far more than film and television
    /// are, so this is carried separately rather than folded into the title.
    pub fn creator(&self) -> Option<&str> {
        match self {
            Self::Track { artist, .. } => Some(artist),
            Self::Book { author, .. } => Some(author),
            Self::Episode { .. } | Self::Movie { .. } => None,
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
            Self::Track {
                artist,
                album,
                track: Some(n),
            } => write!(f, "{artist} - {album} [{n:02}]"),
            Self::Track { artist, album, .. } => write!(f, "{artist} - {album}"),
            Self::Book { author, title } => write!(f, "{author} - {title}"),
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
    /// Every lifecycle state, for iteration and round-trip tests.
    pub const ALL: &'static [Self] = &[Self::Pending, Self::Seeding, Self::Unshared, Self::Failed];

    /// The lowercase name used in the database and in log output.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Seeding => "seeding",
            Self::Unshared => "unshared",
            Self::Failed => "failed",
        }
    }

    /// Inverse of [`Self::as_str`], derived from it so the two cannot drift.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|s| s.as_str() == value)
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
    /// When sharerr first recorded this file, as a Unix timestamp.
    ///
    /// Carried out of the store because the Torznab feed has to publish it: Sonarr
    /// and Radarr **reject an entire feed** whose items have no `pubDate`, so this
    /// is not decoration — without it a friend cannot add sharerr as an indexer at
    /// all. `None` on an item that has not been stored yet.
    pub created_at: Option<i64>,
}

impl SharedItem {
    /// Stable identity of the underlying file across syncs, independent of any
    /// database id. Used to diff discovery output against stored state.
    pub fn key(&self) -> (MediaSource, i64) {
        (self.source, self.file_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_source_names_round_trip() {
        for source in MediaSource::ALL {
            assert_eq!(MediaSource::parse(source.as_str()), Some(*source));
        }
        assert_eq!(MediaSource::parse("plex"), None);
    }

    #[test]
    fn share_state_names_round_trip() {
        for state in ShareState::ALL {
            assert_eq!(ShareState::parse(state.as_str()), Some(*state));
        }
        assert_eq!(ShareState::parse("paused"), None);
    }
}
