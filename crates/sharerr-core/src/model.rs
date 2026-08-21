//! Domain types shared across discovery, torrent creation, and persistence.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Where a shared item was discovered: one of the *arr apps, or a plain
/// tagged directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaSource {
    Sonarr,
    Radarr,
    Lidarr,
    Readarr,
    Whisparr,
    /// A `[[library]]` directory scanned straight from disk — no app, no API,
    /// no external ids. Everything URL-or-API-key shaped iterates
    /// [`Self::ARRS`] instead of [`Self::ALL`] to leave this variant out.
    Directory,
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
            Self::Directory => "directory",
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
            // Directory does not speak the *arr API; only `ArrClient` reads
            // this, and one is never built for it. The arm exists because the
            // function is total, and panicking here would let a future caller
            // take the whole process down over a label.
            Self::Directory => "none",
        }
    }

    /// Every source sharerr can discover from.
    pub const ALL: &'static [Self] = &[
        Self::Sonarr,
        Self::Radarr,
        Self::Lidarr,
        Self::Readarr,
        Self::Whisparr,
        Self::Directory,
    ];

    /// The sources whose items are admitted to a narrow peer scope by the
    /// declared kind in their spec rather than by which app produced them —
    /// because a directory has no single kind: it is whatever the operator
    /// declared in `[[library]]`.
    pub const KIND_SCOPED: &'static [Self] = &[Self::Directory];

    /// Only the *arr apps — the sources that have a URL, an API key, and a
    /// settings section shaped like one. Everything that loops "the configured
    /// apps" iterates this; [`Self::ALL`] additionally carries
    /// [`Self::Directory`], which has none of those.
    pub const ARRS: &'static [Self] = &[
        Self::Sonarr,
        Self::Radarr,
        Self::Lidarr,
        Self::Readarr,
        Self::Whisparr,
    ];

    /// Whether this app's tags apply above the level of one shared item —
    /// series-level for Sonarr and Whisparr (Whisparr is Sonarr's codebase, see
    /// [`Self::api_version`]), artist-level for Lidarr, author-level for
    /// Readarr — so tagging one thing shares its whole run at once. Radarr's
    /// tags are movie-level, which is naturally per-item, and a directory
    /// library has no tags to begin with.
    pub fn has_coarse_tagging(self) -> bool {
        matches!(
            self,
            Self::Sonarr | Self::Whisparr | Self::Lidarr | Self::Readarr
        )
    }

    /// Inverse of [`Self::as_str`], derived from it so the two cannot drift.
    ///
    /// This is *the* decoder for stored and URL-borne source names; a local
    /// string match at a call site is how a database loses every Lidarr row to
    /// an "unknown source" error.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|s| s.as_str() == value)
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
    /// Every value the `kind` tag can take, in variant order.
    ///
    /// Exists so [`Self::kind_tag`] can be round-tripped against serde in a test:
    /// these strings are compared against `json_extract(spec_json, '$.kind')` in
    /// the store, so a rename that separated the two would silently narrow what a
    /// scoped peer can see rather than fail to compile.
    pub const KIND_TAGS: [&'static str; 4] = ["episode", "movie", "track", "book"];

    /// The `kind` discriminant serde writes into `spec_json`.
    ///
    /// The store filters directory-sourced items by comparing this against the
    /// stored JSON, so it must stay in step with the `#[serde(tag = "kind",
    /// rename_all = "lowercase")]` attribute above. Deriving it here — next to the
    /// attribute, with a test that checks it against real serialization — is what
    /// keeps the two from drifting apart.
    pub fn kind_tag(&self) -> &'static str {
        match self {
            Self::Episode { .. } => "episode",
            Self::Movie { .. } => "movie",
            Self::Track { .. } => "track",
            Self::Book { .. } => "book",
        }
    }

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
    /// A short fingerprint of the announce token this item's torrent was last
    /// *confirmed* to be announcing with — set at creation, and refreshed each
    /// time a sync pass verifies (or fixes) it. `None` before a torrent exists,
    /// or if it predates this field. Compared against the currently configured
    /// token to answer "is this specific torrent still using it" — see
    /// `sharerr_torrent::token_from_announce_url`.
    pub announce_token_fp: Option<String>,
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

/// One tagged file, as its library source describes it.
///
/// This is everything discovery can know. The fields a [`SharedItem`] adds — the
/// database id, the info hash, the share state — belong to sharerr, not to the
/// source, and are filled in by the reconciliation loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    pub source: MediaSource,
    /// Series or movie id within the *arr app; for a directory, a stable hash
    /// of the library root.
    pub source_id: i64,
    /// `episodeFile` / `movieFile` id — the natural key sharerr diffs against.
    /// For a directory, a stable hash of the file's path.
    pub file_id: i64,
    pub spec: MediaSpec,
    /// The path **exactly as the source reported it**, before any mapping is
    /// applied. Stored verbatim so that changing a path mapping later does not
    /// orphan existing rows.
    pub arr_path: PathBuf,
    pub size: u64,
    pub ids: ExternalIds,
    /// The original scene release name, when the file was imported from one. This
    /// is the best possible release title — it is already known to parse.
    pub scene_name: Option<String>,
}

impl Discovered {
    /// Stable identity of the underlying file, matching [`SharedItem::key`].
    pub fn key(&self) -> (MediaSource, i64) {
        (self.source, self.file_id)
    }

    /// Promote to a storable item. The release title is resolved separately
    /// because it needs rules this crate does not own.
    pub fn into_shared_item(self, release_title: String) -> SharedItem {
        SharedItem {
            id: None,
            source: self.source,
            source_id: self.source_id,
            file_id: self.file_id,
            spec: self.spec,
            release_title,
            arr_path: self.arr_path,
            size: self.size,
            ids: self.ids,
            info_hash: None,
            announce_token_fp: None,
            state: ShareState::Pending,
            last_error: None,
            // Assigned by the store on insert; a discovered item has not been
            // recorded yet, so it has no publication date to report.
            created_at: None,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn media_source_names_round_trip() {
        for source in MediaSource::ALL {
            assert_eq!(MediaSource::parse(source.as_str()), Some(*source));
        }
        assert_eq!(MediaSource::parse("plex"), None);
    }

    /// `ARRS` is `ALL` minus the sources that do not speak the *arr API — the
    /// loops that build `ArrClient`s depend on that being exact.
    #[test]
    fn arrs_is_all_without_the_non_arr_sources() {
        assert!(!MediaSource::ARRS.contains(&MediaSource::Directory));
        let mut expected: Vec<MediaSource> = MediaSource::ALL.to_vec();
        expected.retain(|s| *s != MediaSource::Directory);
        assert_eq!(MediaSource::ARRS, expected.as_slice());
    }

    #[test]
    fn share_state_names_round_trip() {
        for state in ShareState::ALL {
            assert_eq!(ShareState::parse(state.as_str()), Some(*state));
        }
        assert_eq!(ShareState::parse("paused"), None);
    }

    /// `kind_tag` must agree with what serde actually writes, because the store
    /// filters a scoped peer's feed by comparing the two. Asserting against real
    /// serialization is the point: a `#[serde(rename)]` on any variant breaks this
    /// test rather than silently emptying a friend's feed.
    #[test]
    fn kind_tags_match_what_serde_serializes() {
        let specs = [
            MediaSpec::Episode {
                series_title: "Lanternwick Hollow".to_owned(),
                season: 1,
                episode: 2,
            },
            MediaSpec::Movie {
                title: "Copper Vale".to_owned(),
                year: Some(1999),
            },
            MediaSpec::Track {
                artist: "*".to_owned(),
                album: "*".to_owned(),
                track: None,
            },
            MediaSpec::Book {
                author: "*".to_owned(),
                title: "*".to_owned(),
            },
        ];

        for spec in &specs {
            let json: serde_json::Value = serde_json::to_value(spec).expect("a spec serializes");
            let serialized = json
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .expect("serde writes a kind tag");
            assert_eq!(serialized, spec.kind_tag());
        }

        let tags: Vec<&str> = specs.iter().map(MediaSpec::kind_tag).collect();
        assert_eq!(tags, MediaSpec::KIND_TAGS, "KIND_TAGS lists every variant");
    }
}
