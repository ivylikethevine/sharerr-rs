//! Wire types for the Sonarr/Radarr v3 APIs.
//!
//! Only the fields sharerr actually uses are declared; both APIs return a great
//! deal more, and every field is `#[serde(default)]` where the app is known to omit
//! it. A new *arr release adding fields must never break discovery.

use serde::Deserialize;

use sharerr_core::MediaMeta;

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

/// The `mediaInfo` object Sonarr and Radarr attach to a file they have analysed.
///
/// Identical in both APIs, which is why it lives here rather than beside either.
/// Every field is optional twice over: absent when the *arr never ran its analysis,
/// and `""` when the analysis ran but found no such stream — [`MediaInfo::into_meta`]
/// collapses both to `None`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MediaInfo {
    #[serde(default)]
    pub resolution: Option<String>,
    #[serde(default)]
    pub video_codec: Option<String>,
    #[serde(default)]
    pub video_dynamic_range_type: Option<String>,
    #[serde(default)]
    pub audio_codec: Option<String>,
    /// A number in the JSON (`5.1`), not a string, so it is captured as one and
    /// formatted back — `serde_json::Value` would be heavier for no gain.
    #[serde(default)]
    pub audio_channels: Option<f32>,
    #[serde(default)]
    pub audio_languages: Option<String>,
    #[serde(default)]
    pub subtitles: Option<String>,
    #[serde(default)]
    pub run_time: Option<String>,
    /// Bits per sample. Reported by the *arr apps that manage audio — Lidarr for
    /// music, Readarr for audiobooks — and absent from Sonarr's and Radarr's copy
    /// of this object, which is why it is `Option` twice over like the rest.
    #[serde(default, deserialize_with = "number_or_string")]
    pub audio_bits: Option<String>,
    /// Sampling frequency in hertz, from the same two apps.
    #[serde(default, deserialize_with = "number_or_string")]
    pub audio_sample_rate: Option<String>,
}

impl MediaInfo {
    /// Convert to the shared shape, dropping the fields that carry nothing.
    ///
    /// Yields `None` rather than an all-empty [`MediaMeta`] when the *arr sent an
    /// object with nothing in it, which it does for a file queued for analysis.
    pub(crate) fn into_meta(self) -> Option<MediaMeta> {
        let meta = MediaMeta {
            resolution: non_empty(self.resolution),
            video_codec: non_empty(self.video_codec),
            dynamic_range: non_empty(self.video_dynamic_range_type),
            audio_codec: non_empty(self.audio_codec),
            // `5.1` must not render as `5.1000000238`, and `2` must not render as
            // `2` where every other producer writes `2.0`.
            audio_channels: self.audio_channels.map(|c| format!("{c:.1}")),
            audio_languages: non_empty(self.audio_languages),
            subtitles: non_empty(self.subtitles),
            runtime: non_empty(self.run_time),
            audio_sample_rate: self.audio_sample_rate,
            audio_bit_depth: self.audio_bits,
        };
        (!meta.is_empty()).then_some(meta)
    }
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
    #[serde(default)]
    pub media_info: Option<MediaInfo>,
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
    #[serde(default)]
    pub media_info: Option<MediaInfo>,
}

/// Both apps use `""` and `0` for "unset" in places a JSON `null` would be more
/// honest. Collapse those to `None` so callers do not have to know which.
pub(crate) fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|s| !s.trim().is_empty())
}

pub(crate) fn non_zero<T: PartialEq + Default>(value: Option<T>) -> Option<T> {
    value.filter(|v| *v != T::default())
}

/// Read a numeric `mediaInfo` field that has shipped as both a JSON number and a
/// JSON string into the one `String` shape [`MediaMeta`] stores.
///
/// Lidarr reports `audioSampleRate` as a bare number in current releases and has
/// reported it as a string. Every field on [`MediaInfo`] is `#[serde(default)]`,
/// so a type that matched only one of the two spellings would fail *silently* to
/// `None` against the other: discovery would keep working and quietly carry no
/// metadata, which is the failure this whole struct is shaped to avoid.
///
/// A `0` collapses to `None` for the same reason [`non_zero`] exists — these apps
/// use it where a `null` would be more honest, and `0 Hz` is not a sample rate.
pub(crate) fn number_or_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        Text(String),
        Number(f64),
    }

    Ok(match Option::<Either>::deserialize(deserializer)? {
        None => None,
        Some(Either::Text(text)) => non_empty(Some(text)).filter(|t| t != "0"),
        // `{n}` on an `f64` prints `44100` for `44100.0`, so a whole number that
        // arrived through a float-shaped JSON number reads back as an integer.
        Some(Either::Number(number)) => (number != 0.0).then(|| format!("{number}")),
    })
}
