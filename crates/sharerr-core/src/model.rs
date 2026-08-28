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

crate::str_enum!(
    MediaSource {
        Sonarr => "sonarr",
        Radarr => "radarr",
        Lidarr => "lidarr",
        Readarr => "readarr",
        Whisparr => "whisparr",
        Directory => "directory",
    },
    "This is *the* decoder for stored and URL-borne source names; a local \
     string match at a call site is how a database loses every Lidarr row \
     to an \"unknown source\" error."
);

impl MediaSource {
    /// Which version of the *arr HTTP API this app speaks.
    ///
    /// Not cosmetic: Sonarr, Radarr and Whisparr are on `v3` while Lidarr and
    /// Readarr are on `v1`. Hardcoding one prefix is why the client could only ever
    /// have talked to the first two, and getting it wrong presents as a 404 that
    /// looks like a wrong URL rather than a wrong version. The arr client matches
    /// on the source itself rather than this string when building its prefix;
    /// this is the one place the split is stated for everything else.
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

/// What the file actually is, as opposed to what its name claims.
///
/// Two sources fill this in, in order of preference: the `mediaInfo` the *arr
/// apps already computed (free — it arrives in the same JSON as everything else),
/// and a direct probe of the file for the cases that have no *arr behind them —
/// a `[[library]]` directory, or a file the *arr never analysed. See
/// `sharerr_probe`.
///
/// Every field is optional and every field is a `String`. Optional because a
/// probe that recognises the container but not one of its streams must be able to
/// report what it did learn rather than nothing; `String` because these values are
/// rendered verbatim into a feed and never computed with, and because
/// `audioChannels` is `5.1` — a float, which would cost this type its `Eq`.
/// The audio pair added for music keeps that invariant even though a sample rate
/// and a bit depth are both integers: one rule for the whole type is easier to
/// hold than a rule with two exceptions, and neither value is ever arithmetic.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MediaMeta {
    /// Pixel dimensions as `WIDTHxHEIGHT`, e.g. `1920x1080`. Stored in full
    /// rather than as `1080p` because the scene shorthand throws away the aspect
    /// ratio, and [`Self::scene_resolution`] can derive the shorthand back.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video_codec: Option<String>,
    /// `HDR10`, `DV`, and so on. Empty for SDR.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_range: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_codec: Option<String>,
    /// `2.0`, `5.1`, … — a string, see the type docs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_channels: Option<String>,
    /// Slash-separated, as the *arr apps report it: `English/Japanese`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_languages: Option<String>,
    /// Slash-separated subtitle languages, same shape as [`Self::audio_languages`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitles: Option<String>,
    /// `H:MM:SS`, as the *arr apps report it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    /// Sampling frequency in hertz, e.g. `44100`. Stored raw for the same reason
    /// [`Self::resolution`] is: the shorthand is derivable from the number
    /// ([`Self::sample_rate_khz`]) and the number is not derivable back.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_sample_rate: Option<String>,
    /// Bits per sample, e.g. `16` or `24`. Absent for a lossy codec, which has no
    /// bit depth to report — see [`Self::is_lossless`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_bit_depth: Option<String>,
}

impl MediaMeta {
    /// Whether this carries nothing at all.
    ///
    /// A probe that recognised no stream yields `Some(MediaMeta::default())`
    /// rather than `None`, which would render an item's worth of empty attributes.
    /// Callers store `None` instead when this is true.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The scene shorthand for [`Self::resolution`] — `1920x1080` becomes `1080p`.
    ///
    /// Keyed on height, and on the *standard* heights only: a release named
    /// `816p` communicates nothing a parser on the far end can use, so anything
    /// off-ladder yields `None` and the caller omits the token rather than
    /// inventing one. Widescreen encodes are cropped vertically (a 1920x816
    /// scope film is universally called 1080p), so the ladder matches on width
    /// first and falls back to height.
    pub fn scene_resolution(&self) -> Option<&'static str> {
        let (width, height) = self.dimensions()?;
        let ladder = [
            (7680, 4320, "4320p"),
            (3840, 2160, "2160p"),
            (1920, 1080, "1080p"),
            (1280, 720, "720p"),
            (720, 576, "576p"),
            (720, 480, "480p"),
        ];
        ladder
            .iter()
            .find(|(w, h, _)| width == *w || height == *h)
            .map(|(_, _, name)| *name)
    }

    /// [`Self::resolution`] split into numbers, when it has the expected shape.
    fn dimensions(&self) -> Option<(u32, u32)> {
        let raw = self.resolution.as_deref()?;
        let (width, height) = raw.split_once(['x', 'X'])?;
        Some((width.trim().parse().ok()?, height.trim().parse().ok()?))
    }

    /// The video codec as a release title spells it.
    ///
    /// The *arr apps and the containers themselves disagree on spelling for what
    /// is one codec to a parser — `V_MPEG4/ISO/AVC`, `avc1`, `h264` and `x264` all
    /// mean the same release token. Anything unrecognised yields `None`: an
    /// unknown string dropped into a release title is worse than no token, since
    /// the far end parses it as part of the name.
    pub fn scene_video_codec(&self) -> Option<&'static str> {
        // Separators are stripped, not just case: `H.264`, `H-264` and `h264`
        // are one codec written three ways, and matching on the raw string would
        // recognise only the third.
        let raw: String = self
            .video_codec
            .as_deref()?
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .map(|c| c.to_ascii_lowercase())
            .collect();
        for (needle, token) in [
            ("av1", "AV1"),
            ("hevc", "x265"),
            ("h265", "x265"),
            ("x265", "x265"),
            ("avc", "x264"),
            ("h264", "x264"),
            ("x264", "x264"),
            ("vp9", "VP9"),
            ("mpeg2", "MPEG2"),
            ("xvid", "XviD"),
        ] {
            if raw.contains(needle) {
                return Some(token);
            }
        }
        None
    }

    /// The audio codec as a release title spells it.
    ///
    /// The audio counterpart of [`Self::scene_video_codec`], and unrecognised
    /// values yield `None` for the same reason. A music release is matched on this
    /// token far more strictly than a video one is: a Lidarr quality profile
    /// distinguishes FLAC from MP3 and nothing else in the name substitutes.
    pub fn scene_audio_format(&self) -> Option<&'static str> {
        Self::audio_format(self.audio_codec.as_deref()?).map(|(token, _)| token)
    }

    /// Whether the audio is stored losslessly, when the codec says so.
    ///
    /// Derived from [`Self::audio_codec`] rather than stored beside it: a stored
    /// flag can end up contradicting the codec it sits next to, and no producer
    /// this project reads reports the two independently anyway.
    pub fn is_lossless(&self) -> Option<bool> {
        Self::audio_format(self.audio_codec.as_deref()?).map(|(_, lossless)| lossless)
    }

    /// One lookup behind [`Self::scene_audio_format`] and [`Self::is_lossless`], so
    /// the token and the lossless flag cannot disagree about the same codec.
    ///
    /// Matching is `contains` over the codec with separators stripped, because the
    /// same codec arrives spelled several ways: `A_AC3` from a Matroska header,
    /// `EAC3` from Sonarr, `DTS-HD MA` from a remux. Order is therefore load
    /// bearing — a needle that is a substring of another codec's name must be
    /// tried after it, which is why `eac3` precedes `ac3` and `wavpack` precedes
    /// `wav`.
    fn audio_format(codec: &str) -> Option<(&'static str, bool)> {
        let raw: String = codec
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .map(|c| c.to_ascii_lowercase())
            .collect();
        for (needle, token, lossless) in [
            ("wavpack", "WV", true),
            ("truehd", "TrueHD", true),
            ("dtshd", "DTS-HD", true),
            ("flac", "FLAC", true),
            ("alac", "ALAC", true),
            ("eac3", "EAC3", false),
            ("ac3", "AC3", false),
            ("dts", "DTS", false),
            ("mp3", "MP3", false),
            // `A_MPEG/L3` from a Matroska header, `MPEG Audio (Layer 3)` from
            // MediaInfo — the same codec, and neither spelling contains `mp3`.
            ("mpegl3", "MP3", false),
            ("layer3", "MP3", false),
            ("aac", "AAC", false),
            ("opus", "OPUS", false),
            ("vorbis", "VORBIS", false),
            ("wma", "WMA", false),
            ("ape", "APE", true),
            ("pcm", "PCM", true),
            ("wav", "WAV", true),
        ] {
            if raw.contains(needle) {
                return Some((token, lossless));
            }
        }
        None
    }

    /// [`Self::audio_sample_rate`] as a release and a reader both write it —
    /// `44100` becomes `44.1 kHz`.
    ///
    /// Display only: a trailing `.0` is dropped, so `48000` is `48 kHz` rather
    /// than `48.0 kHz`. Anything that does not parse as a whole number of hertz
    /// yields `None` rather than being echoed through unformatted.
    pub fn sample_rate_khz(&self) -> Option<String> {
        let hz: u32 = self.audio_sample_rate.as_deref()?.trim().parse().ok()?;
        let khz = f64::from(hz) / 1000.0;
        if khz.fract() == 0.0 {
            Some(format!("{} kHz", hz / 1000))
        } else {
            Some(format!("{khz:.1} kHz"))
        }
    }
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

crate::str_enum!(ShareState {
    Pending => "pending",
    Seeding => "seeding",
    Unshared => "unshared",
    Failed => "failed",
});

/// One file that has been (or is being) shared.
#[derive(Debug, Clone, PartialEq)]
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
    /// Whether sharerr added this item's torrent to the client itself.
    ///
    /// `false` when `Seeder::seed` found a torrent that already covered the
    /// file and reused it — an operator's own torrent, or a cross-seed of the
    /// same media. Withdrawing such an item marks the row `Unshared` but
    /// leaves the torrent where it was: removing it would tear down something
    /// sharerr did not put there, which is the "preserve any existing
    /// torrents" rule the whole seeding path is built around.
    ///
    /// `false` on an item with no torrent yet — nothing has been created, so
    /// there is nothing to claim.
    pub created_by_sharerr: bool,
    pub state: ShareState,
    pub last_error: Option<String>,
    /// When sharerr first recorded this file, as a Unix timestamp.
    ///
    /// Carried out of the store because the Torznab feed has to publish it: Sonarr
    /// and Radarr **reject an entire feed** whose items have no `pubDate`, so this
    /// is not decoration — without it a friend cannot add sharerr as an indexer at
    /// all. `None` on an item that has not been stored yet.
    pub created_at: Option<i64>,
    /// What the file actually is — resolution, codecs, runtime. Published as
    /// Torznab attributes so a friend's Sonarr can filter on quality rather than
    /// guessing from the title, and folded into the release title itself when
    /// there was no real name to use. See [`MediaMeta`].
    pub media: Option<MediaMeta>,
    /// Uploaded ÷ downloaded, as the torrent client itself reports it for this
    /// specific torrent. Refreshed each sync pass by
    /// `sharerr_store::Store::set_ratio`; `None` before a torrent exists, or for
    /// a row that predates this column.
    pub achieved_ratio: Option<f64>,
    /// The per-torrent seed-ratio limit the client is actually enforcing, when
    /// it can express one as a plain number. `None` covers "no limit set on
    /// this torrent", "the client falls back to its own global default", and
    /// (rTorrent) "this backend has no per-torrent ratio-limit RPC at all" —
    /// see `sharerr_client::TorrentSummary::ratio_limit`.
    pub ratio_limit_reported: Option<f64>,
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
    /// The path the file had when it was imported, before any renaming the source
    /// applied — Radarr's `originalFilePath`. A second chance at a real release
    /// name for a library that has since been renamed; unlike [`Self::scene_name`]
    /// it is not known to parse, so it is only used if it does. `None` for every
    /// other source: Sonarr, Lidarr and Readarr do not report it.
    pub original_path: Option<PathBuf>,
    /// What the file actually is, when the source knew. `None` here does not mean
    /// "unknown for good": the sync pass probes the file itself for anything that
    /// arrives without it. See [`MediaMeta`].
    pub media: Option<MediaMeta>,
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
            media: self.media,
            info_hash: None,
            announce_token_fp: None,
            // No torrent exists yet; `Store::set_seeding` records which branch
            // of `Seeder::seed` produced one.
            created_by_sharerr: false,
            state: ShareState::Pending,
            last_error: None,
            // Assigned by the store on insert; a discovered item has not been
            // recorded yet, so it has no publication date to report.
            created_at: None,
            // A rediscovery describes a file, not a torrent, so it never knows
            // these — `Store::upsert`'s COALESCE preserves whatever
            // `Store::set_ratio` last wrote.
            achieved_ratio: None,
            ratio_limit_reported: None,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    // ------------------------------------------------------------ MediaMeta

    fn meta(resolution: &str, codec: &str) -> MediaMeta {
        MediaMeta {
            resolution: Some(resolution.to_owned()),
            video_codec: Some(codec.to_owned()),
            ..MediaMeta::default()
        }
    }

    #[test]
    fn standard_ladder_heights_get_their_scene_shorthand() {
        assert_eq!(meta("1920x1080", "").scene_resolution(), Some("1080p"));
        assert_eq!(meta("1280x720", "").scene_resolution(), Some("720p"));
        assert_eq!(meta("3840x2160", "").scene_resolution(), Some("2160p"));
    }

    /// A scope film is cropped vertically — 1920x816 is universally called 1080p,
    /// and matching on height alone would call it nothing.
    #[test]
    fn a_widescreen_crop_is_named_by_its_width() {
        assert_eq!(meta("1920x816", "").scene_resolution(), Some("1080p"));
        assert_eq!(meta("3840x1600", "").scene_resolution(), Some("2160p"));
    }

    /// Off-ladder yields nothing rather than an invented token: `816p` in a
    /// release title communicates nothing a parser on the far end can use.
    #[test]
    fn an_off_ladder_resolution_is_not_named() {
        assert_eq!(meta("1024x768", "").scene_resolution(), None);
        assert_eq!(meta("garbage", "").scene_resolution(), None);
        assert_eq!(MediaMeta::default().scene_resolution(), None);
    }

    #[test]
    fn every_spelling_of_one_codec_maps_to_one_token() {
        for spelling in ["V_MPEG4/ISO/AVC", "avc1", "h264", "x264", "H.264"] {
            assert_eq!(
                meta("", spelling).scene_video_codec(),
                Some("x264"),
                "{spelling}"
            );
        }
        for spelling in ["HEVC", "h265", "x265"] {
            assert_eq!(
                meta("", spelling).scene_video_codec(),
                Some("x265"),
                "{spelling}"
            );
        }
    }

    /// An unrecognised codec yields nothing rather than being passed through: it
    /// would land in a release title, where the far end parses it as part of the
    /// name.
    #[test]
    fn an_unknown_codec_contributes_no_token() {
        assert_eq!(meta("", "SOMETHING_NEW").scene_video_codec(), None);
        assert_eq!(MediaMeta::default().scene_video_codec(), None);
    }

    #[test]
    fn an_all_absent_meta_is_empty_and_a_populated_one_is_not() {
        assert!(MediaMeta::default().is_empty());
        assert!(!meta("1920x1080", "x264").is_empty());
    }

    /// Absent fields must not serialize, or a stored row grows a wall of nulls
    /// and every consumer has to tell "null" from "absent".
    #[test]
    fn absent_fields_are_omitted_from_the_stored_json() {
        let json = serde_json::to_string(&MediaMeta {
            resolution: Some("1920x1080".to_owned()),
            ..MediaMeta::default()
        })
        .unwrap();
        assert_eq!(json, r#"{"resolution":"1920x1080"}"#);

        // ...and a row written before a field existed still loads.
        let back: MediaMeta = serde_json::from_str(r#"{"resolution":"1920x1080"}"#).unwrap();
        assert_eq!(back.resolution.as_deref(), Some("1920x1080"));
        assert_eq!(back.audio_codec, None);
    }

    /// The audio counterpart of the video codec table, and the one the naming of a
    /// music release turns on.
    #[test]
    fn audio_codecs_reduce_to_the_token_a_release_uses() {
        for (reported, token, lossless) in [
            ("FLAC", "FLAC", true),
            ("flac", "FLAC", true),
            ("A_FLAC", "FLAC", true),
            ("ALAC", "ALAC", true),
            ("MP3", "MP3", false),
            ("MPEG Audio (Layer 3)", "MP3", false),
            ("A_MPEG/L3", "MP3", false),
            ("AAC", "AAC", false),
            ("Opus", "OPUS", false),
            ("Vorbis", "VORBIS", false),
            ("WavPack", "WV", true),
            ("TrueHD", "TrueHD", true),
            ("DTS-HD MA", "DTS-HD", true),
        ] {
            let meta = MediaMeta {
                audio_codec: Some(reported.to_owned()),
                ..MediaMeta::default()
            };
            assert_eq!(meta.scene_audio_format(), Some(token), "{reported}");
            assert_eq!(meta.is_lossless(), Some(lossless), "{reported}");
        }
    }

    /// The table is `contains`-matched, so a needle that is a substring of another
    /// codec's name has to be tried after it. These three are the pairs that
    /// actually collide, and getting the order wrong reads `A_AC3` as AAC.
    #[test]
    fn colliding_audio_codec_names_resolve_to_the_longer_match() {
        for (reported, token) in [
            ("A_AC3", "AC3"),
            ("EAC3", "EAC3"),
            ("WavPack", "WV"),
            ("DTS-HD MA", "DTS-HD"),
        ] {
            let meta = MediaMeta {
                audio_codec: Some(reported.to_owned()),
                ..MediaMeta::default()
            };
            assert_eq!(meta.scene_audio_format(), Some(token), "{reported}");
        }
    }

    /// An unknown codec yields no token, for the same reason an unknown video
    /// codec does: the string would be parsed as part of the release name.
    #[test]
    fn an_unknown_audio_codec_yields_no_token_and_no_verdict() {
        let meta = MediaMeta {
            audio_codec: Some("Some Future Codec".to_owned()),
            ..MediaMeta::default()
        };
        assert_eq!(meta.scene_audio_format(), None);
        assert_eq!(meta.is_lossless(), None);
        assert_eq!(MediaMeta::default().is_lossless(), None);
    }

    #[test]
    fn a_sample_rate_renders_in_kilohertz_without_a_trailing_zero() {
        let rate = |hz: &str| {
            MediaMeta {
                audio_sample_rate: Some(hz.to_owned()),
                ..MediaMeta::default()
            }
            .sample_rate_khz()
        };
        assert_eq!(rate("44100").as_deref(), Some("44.1 kHz"));
        assert_eq!(rate("48000").as_deref(), Some("48 kHz"));
        assert_eq!(rate("96000").as_deref(), Some("96 kHz"));
        assert_eq!(rate("192000").as_deref(), Some("192 kHz"));
        assert_eq!(
            rate("not a number"),
            None,
            "an unparseable rate is dropped rather than echoed through"
        );
        assert_eq!(MediaMeta::default().sample_rate_khz(), None);
    }

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

    #[test]
    fn coarse_tagging_is_series_artist_and_author_level_only() {
        assert!(MediaSource::Sonarr.has_coarse_tagging());
        assert!(MediaSource::Whisparr.has_coarse_tagging());
        assert!(MediaSource::Lidarr.has_coarse_tagging());
        assert!(MediaSource::Readarr.has_coarse_tagging());
        assert!(!MediaSource::Radarr.has_coarse_tagging());
        assert!(!MediaSource::Directory.has_coarse_tagging());
    }

    #[test]
    fn api_version_matches_the_arr_app() {
        assert_eq!(MediaSource::Sonarr.api_version(), "v3");
        assert_eq!(MediaSource::Radarr.api_version(), "v3");
        assert_eq!(MediaSource::Whisparr.api_version(), "v3");
        assert_eq!(MediaSource::Lidarr.api_version(), "v1");
        assert_eq!(MediaSource::Readarr.api_version(), "v1");
        assert_eq!(MediaSource::Directory.api_version(), "none");
    }

    fn episode() -> MediaSpec {
        MediaSpec::Episode {
            series_title: "Lanternwick Hollow".to_owned(),
            season: 1,
            episode: 2,
        }
    }

    fn movie(year: Option<u16>) -> MediaSpec {
        MediaSpec::Movie {
            title: "Copper Vale".to_owned(),
            year,
        }
    }

    fn track(track: Option<u32>) -> MediaSpec {
        MediaSpec::Track {
            artist: "The Verdigris".to_owned(),
            album: "Static Orchard".to_owned(),
            track,
        }
    }

    fn book() -> MediaSpec {
        MediaSpec::Book {
            author: "Marlow Finch".to_owned(),
            title: "The Quiet Ledger".to_owned(),
        }
    }

    #[test]
    fn title_is_the_searchable_name_per_kind() {
        assert_eq!(episode().title(), "Lanternwick Hollow");
        assert_eq!(movie(None).title(), "Copper Vale");
        assert_eq!(
            track(None).title(),
            "Static Orchard",
            "the album, not the artist"
        );
        assert_eq!(book().title(), "The Quiet Ledger");
    }

    #[test]
    fn creator_is_only_present_for_music_and_books() {
        assert_eq!(episode().creator(), None);
        assert_eq!(movie(None).creator(), None);
        assert_eq!(track(None).creator(), Some("The Verdigris"));
        assert_eq!(book().creator(), Some("Marlow Finch"));
    }

    #[test]
    fn display_renders_each_kind() {
        assert_eq!(episode().to_string(), "Lanternwick Hollow S01E02");
        assert_eq!(movie(Some(1999)).to_string(), "Copper Vale (1999)");
        assert_eq!(movie(None).to_string(), "Copper Vale");
        assert_eq!(
            track(Some(4)).to_string(),
            "The Verdigris - Static Orchard [04]"
        );
        assert_eq!(track(None).to_string(), "The Verdigris - Static Orchard");
        assert_eq!(book().to_string(), "Marlow Finch - The Quiet Ledger");
    }

    #[test]
    fn imdb_bare_strips_the_tt_prefix_regardless_of_source() {
        assert_eq!(ExternalIds::imdb_bare("tt1234567"), "1234567");
        assert_eq!(ExternalIds::imdb_bare("1234567"), "1234567");
        assert_eq!(ExternalIds::imdb_bare("  tt1234567  "), "1234567");
    }

    #[test]
    fn imdb_numeric_is_none_without_a_stored_id() {
        let ids = ExternalIds::default();
        assert_eq!(ids.imdb_numeric(), None);

        let ids = ExternalIds {
            imdb: Some("tt42".to_owned()),
            ..Default::default()
        };
        assert_eq!(ids.imdb_numeric(), Some("42"));
    }

    fn discovered() -> Discovered {
        Discovered {
            source: MediaSource::Radarr,
            source_id: 7,
            file_id: 99,
            spec: movie(Some(2001)),
            arr_path: PathBuf::from("/media/movies/copper-vale.mkv"),
            size: 1024,
            ids: ExternalIds::default(),
            media: None,
            scene_name: Some("Copper.Vale.2001.mkv".to_owned()),
            original_path: None,
        }
    }

    #[test]
    fn shared_item_and_discovered_keys_agree() {
        let discovered = discovered();
        let shared = discovered
            .clone()
            .into_shared_item("Copper.Vale.2001".to_owned());
        assert_eq!(discovered.key(), (MediaSource::Radarr, 99));
        assert_eq!(shared.key(), discovered.key());
    }

    #[test]
    fn promoting_a_discovered_item_starts_pending_with_no_history() {
        let shared = discovered().into_shared_item("Copper.Vale.2001".to_owned());
        assert_eq!(shared.id, None);
        assert_eq!(shared.state, ShareState::Pending);
        assert_eq!(shared.info_hash, None);
        assert_eq!(shared.announce_token_fp, None);
        assert_eq!(shared.last_error, None);
        assert_eq!(shared.created_at, None);
        assert_eq!(shared.release_title, "Copper.Vale.2001");
    }
}
