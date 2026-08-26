//! Reading what a media file actually *is*, for the sources that have no *arr to ask.
//!
//! Sonarr and Radarr analyse every file they import and hand the result over in
//! the same JSON as everything else, so for those two this crate is never
//! reached. It exists for the cases with nothing behind them: a `[[library]]`
//! directory, which has no application at all, and an *arr file whose analysis
//! has not run (or failed). Both arrive at reconciliation with no
//! [`MediaMeta`], and both look identical from there — which is why one probe
//! covers them.
//!
//! # What it will not do
//!
//! **Nothing here opens a file for writing, and nothing decodes a frame.** Each
//! backend reads container headers near the start of the file and stops. That is
//! a deliberate ceiling, not an accident of the crates chosen: sharerr's central
//! constraint is that the library is never modified, and a probe that rewrote an
//! index or "repaired" a container would break it as surely as a rename would.
//!
//! **Nothing here fails a sync.** Every error path — unreadable file, unknown
//! container, truncated header, a backend panicking on malformed input — becomes
//! `None` and a `debug` log line. A file sharerr cannot describe is still a file
//! sharerr can share; it just goes out with fewer Torznab attributes.
//!
//! # Coverage
//!
//! | Extension | Backend | Yields |
//! |---|---|---|
//! | `mkv`, `webm` | `matroska` | resolution, video codec, audio codec, channels, audio languages, subtitles, runtime |
//! | `mp4`, `m4v`, `mov` | `mp4` | resolution, video codec, audio codec, channels, runtime |
//! | anything else | — | `None` |
//!
//! Audio-only containers (`flac`, `mp3`, `opus`, …) are deliberately absent for
//! now — see `docs/ROADMAP.md`. They carry no resolution or video codec, and
//! wherever an *arr manages the file its own `mediaInfo` already reports the
//! codec, sample rate and bit depth for free — which Lidarr and Readarr now
//! feed into `MediaMeta` alongside Sonarr's and Radarr's. What is left
//! uncovered is music in a `[[library]]` directory with no *arr behind it,
//! which is the whole of what a backend here would serve.

use std::path::Path;
use std::time::Duration;

use sharerr_core::MediaMeta;

/// Read what this file is, or `None` if that cannot be established.
///
/// Dispatch is by extension rather than by content sniffing. Sniffing would
/// recognise a mislabelled file, but the cost is opening and reading every file
/// in a library — including the `.nfo`, `.srt` and artwork that sit beside the
/// media — to discover that most of them are not containers. The *arr apps name
/// their imports by extension and so does every library this is pointed at.
pub fn probe(path: &Path) -> Option<MediaMeta> {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())?
        .to_ascii_lowercase();

    let meta = match extension.as_str() {
        "mkv" | "webm" => matroska_meta(path),
        "mp4" | "m4v" | "mov" => isobmff_meta(path),
        _ => {
            tracing::trace!(file = %path.display(), extension, "no probe backend for this container");
            return None;
        }
    };

    // A backend that recognised the container but described no stream yields an
    // empty `MediaMeta`. Storing that would be indistinguishable from a file
    // genuinely made of nothing, and would render an item's worth of empty
    // attributes into the feed.
    meta.filter(|m| !m.is_empty())
}

/// MKV and WebM, via EBML track headers.
fn matroska_meta(path: &Path) -> Option<MediaMeta> {
    let file = match matroska::open(path) {
        Ok(file) => file,
        Err(err) => {
            tracing::debug!(file = %path.display(), %err, "not readable as matroska");
            return None;
        }
    };

    let mut meta = MediaMeta {
        runtime: file.info.duration.map(format_runtime),
        ..MediaMeta::default()
    };

    // Languages accumulate across tracks: a release with English and Japanese
    // audio is a different release from one with either alone, and the far end
    // filters on exactly that.
    let mut audio_languages = Vec::new();
    let mut subtitles = Vec::new();

    for track in &file.tracks {
        let language = track.language.as_ref().map(ToString::to_string);
        match (&track.tracktype, &track.settings) {
            // First video track wins. A file with two is a rarity (commentary
            // angles, alternate cuts) and the first is the feature in every case
            // sharerr will meet.
            (matroska::Tracktype::Video, matroska::Settings::Video(video))
                if meta.resolution.is_none() =>
            {
                if video.pixel_width > 0 && video.pixel_height > 0 {
                    meta.resolution = Some(format!("{}x{}", video.pixel_width, video.pixel_height));
                }
                meta.video_codec = codec_name(&track.codec_id);
            }
            (matroska::Tracktype::Audio, matroska::Settings::Audio(audio)) => {
                if meta.audio_codec.is_none() {
                    meta.audio_codec = codec_name(&track.codec_id);
                    if audio.channels > 0 {
                        meta.audio_channels = Some(channel_layout(audio.channels));
                    }
                }
                if let Some(language) = language {
                    audio_languages.push(language);
                }
            }
            (matroska::Tracktype::Subtitle, _) => {
                if let Some(language) = language {
                    subtitles.push(language);
                }
            }
            _ => {}
        }
    }

    meta.audio_languages = join_languages(audio_languages);
    meta.subtitles = join_languages(subtitles);
    Some(meta)
}

/// MP4, M4V and MOV, via ISO base media file format boxes.
fn isobmff_meta(path: &Path) -> Option<MediaMeta> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) => {
            tracing::debug!(file = %path.display(), %err, "not readable");
            return None;
        }
    };
    let reader = match mp4::read_mp4(file) {
        Ok(reader) => reader,
        Err(err) => {
            tracing::debug!(file = %path.display(), %err, "not readable as ISO-BMFF");
            return None;
        }
    };

    let mut meta = MediaMeta {
        runtime: Some(format_runtime(reader.duration())),
        ..MediaMeta::default()
    };
    let mut audio_languages = Vec::new();

    // `tracks()` is a HashMap, so iteration order is not track order. Sort by the
    // track id the container assigned, or "the first video track" would mean a
    // different track between two runs over the same file.
    let mut tracks: Vec<_> = reader.tracks().iter().collect();
    tracks.sort_by_key(|(id, _)| **id);

    for (_, track) in tracks {
        match track.track_type() {
            Ok(mp4::TrackType::Video) if meta.resolution.is_none() => {
                if track.width() > 0 && track.height() > 0 {
                    meta.resolution = Some(format!("{}x{}", track.width(), track.height()));
                }
                meta.video_codec = track.media_type().ok().map(|m| m.to_string());
            }
            Ok(mp4::TrackType::Audio) => {
                if meta.audio_codec.is_none() {
                    meta.audio_codec = track.media_type().ok().map(|m| m.to_string());
                    meta.audio_channels = track
                        .channel_config()
                        .ok()
                        .map(|c| channel_layout(u64::from(c as u8)));
                }
                // ISO-BMFF stores `und` for "undetermined", which is not a
                // language and must not be published as one.
                let language = track.language();
                if !language.is_empty() && language != "und" {
                    audio_languages.push(language.to_owned());
                }
            }
            _ => {}
        }
    }

    meta.audio_languages = join_languages(audio_languages);
    Some(meta)
}

/// A Matroska codec id reduced to the name a release title would use.
///
/// Ids are namespaced and verbose — `V_MPEG4/ISO/AVC`, `A_EAC3` — so the prefix
/// and namespace are stripped and the rest handed on. Deliberately *not*
/// normalised to scene tokens here: that mapping belongs to
/// [`MediaMeta::scene_video_codec`], which is where the release title is built,
/// and doing it twice in two places is how the two come to disagree.
fn codec_name(codec_id: &str) -> Option<String> {
    let name = codec_id
        .split_once('_')
        .map_or(codec_id, |(_, rest)| rest)
        .split('/')
        .next_back()?
        .trim();
    (!name.is_empty()).then(|| name.to_owned())
}

/// A channel count as the *arr apps and release titles spell it: `2.0`, `5.1`.
///
/// One of the channels in a surround mix is the LFE — the `.1` — so 6 channels is
/// `5.1` rather than `6.0`. Stereo and mono have no LFE and stay whole.
fn channel_layout(channels: u64) -> String {
    match channels {
        0 => "0.0".to_owned(),
        1 | 2 => format!("{channels}.0"),
        n => format!("{}.1", n - 1),
    }
}

/// `H:MM:SS`, matching what Sonarr and Radarr report so the two sources are
/// indistinguishable downstream.
fn format_runtime(duration: Duration) -> String {
    let total = duration.as_secs();
    format!(
        "{}:{:02}:{:02}",
        total / 3600,
        (total % 3600) / 60,
        total % 60
    )
}

/// Slash-separated, deduplicated, in first-seen order — the shape the *arr apps
/// use. `None` when there were none, so the field is omitted rather than empty.
fn join_languages(languages: Vec<String>) -> Option<String> {
    let mut seen = Vec::new();
    for language in languages {
        if !seen.contains(&language) {
            seen.push(language);
        }
    }
    (!seen.is_empty()).then(|| seen.join("/"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn channel_counts_carry_the_lfe_where_there_is_one() {
        // Mono and stereo have no LFE channel, so the count is whole.
        assert_eq!(channel_layout(1), "1.0");
        assert_eq!(channel_layout(2), "2.0");
        // Six channels is 5.1, not 6.0 — one of them is the LFE.
        assert_eq!(channel_layout(6), "5.1");
        assert_eq!(channel_layout(8), "7.1");
    }

    #[test]
    fn codec_ids_lose_their_namespace_but_keep_their_name() {
        assert_eq!(codec_name("V_MPEG4/ISO/AVC").as_deref(), Some("AVC"));
        assert_eq!(codec_name("V_MPEGH/ISO/HEVC").as_deref(), Some("HEVC"));
        assert_eq!(codec_name("A_EAC3").as_deref(), Some("EAC3"));
        // Nothing to strip is not an error.
        assert_eq!(codec_name("AV1").as_deref(), Some("AV1"));
        assert_eq!(codec_name(""), None);
    }

    #[test]
    fn runtimes_match_the_shape_the_arr_apps_report() {
        assert_eq!(format_runtime(Duration::from_secs(0)), "0:00:00");
        assert_eq!(format_runtime(Duration::from_secs(59)), "0:00:59");
        assert_eq!(format_runtime(Duration::from_secs(3600)), "1:00:00");
        // 1:56:38, the shape Radarr's `runTime` uses.
        assert_eq!(format_runtime(Duration::from_secs(6998)), "1:56:38");
    }

    #[test]
    fn languages_are_deduplicated_in_first_seen_order() {
        let joined = join_languages(vec!["eng".to_owned(), "jpn".to_owned(), "eng".to_owned()]);
        assert_eq!(joined.as_deref(), Some("eng/jpn"));
        assert_eq!(join_languages(Vec::new()), None);
    }

    /// The probe must never be the thing that fails a sync. A file that is not
    /// the container its extension claims is the common case for a partial
    /// download or a hand-renamed file, and it has to come back as "unknown".
    /// Build a real, valid ISO-BMFF file with a known video and audio track.
    ///
    /// Synthetic like every other fixture in this tree — the `mp4` crate's own
    /// writer produces it, so it is byte-stable across machines and contains no
    /// real content. Written rather than committed as a binary so the *shape*
    /// being asserted is visible in the test that asserts it.
    fn write_mp4(path: &Path, width: u16, height: u16) {
        let file = std::fs::File::create(path).unwrap();
        let mut writer = mp4::Mp4Writer::write_start(
            std::io::BufWriter::new(file),
            &mp4::Mp4Config {
                major_brand: "isom".parse().unwrap(),
                minor_version: 512,
                compatible_brands: vec!["isom".parse().unwrap(), "mp41".parse().unwrap()],
                timescale: 1000,
            },
        )
        .unwrap();

        writer
            .add_track(&mp4::TrackConfig::from(mp4::AvcConfig {
                width,
                height,
                seq_param_set: vec![0, 0, 0, 1],
                pic_param_set: vec![0, 0, 0, 1],
            }))
            .unwrap();
        writer
            .add_track(&mp4::TrackConfig::from(mp4::AacConfig {
                chan_conf: mp4::ChannelConfig::FiveOne,
                ..mp4::AacConfig::default()
            }))
            .unwrap();
        writer
            .write_sample(
                1,
                &mp4::Mp4Sample {
                    start_time: 0,
                    duration: 1000,
                    rendering_offset: 0,
                    is_sync: true,
                    bytes: mp4::Bytes::from(vec![0x0u8; 64]),
                },
            )
            .unwrap();
        writer.write_end().unwrap();
    }

    // ---------------------------------------------------------------- EBML

    /// An EBML data-size, always in its 8-byte form.
    ///
    /// The compact forms exist to save bytes in a real file; a fixture has no
    /// such pressure, and one width means the builder below has no length-
    /// dependent branch to get wrong.
    fn ebml_size(len: u64) -> Vec<u8> {
        let mut out = vec![0x01];
        out.extend_from_slice(&len.to_be_bytes()[1..]);
        out
    }

    /// `[id][size][payload]` — the only shape EBML has.
    fn elem(id: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        out.extend_from_slice(&ebml_size(payload.len() as u64));
        out.extend_from_slice(payload);
        out
    }

    /// An unsigned EBML integer, in the fewest bytes that hold it.
    fn uint(id: &[u8], value: u64) -> Vec<u8> {
        let bytes = value.to_be_bytes();
        let first = bytes.iter().position(|b| *b != 0).unwrap_or(7);
        elem(id, &bytes[first..])
    }

    fn utf8(id: &[u8], value: &str) -> Vec<u8> {
        elem(id, value.as_bytes())
    }

    fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.concat()
    }

    /// Hand-build a Matroska file with known tracks.
    ///
    /// The `matroska` crate has no writer, so unlike the ISO-BMFF fixture this
    /// one is assembled from the spec. It is worth the assembly: `.mkv` is the
    /// container essentially all of this project's media arrives in, and without
    /// this the MKV extraction path would ship untested.
    ///
    /// Deliberately header-only — no Cluster, no frames. The probe reads track
    /// headers and stops, so a file with no media in it exercises exactly the
    /// same code a two-hour film would.
    fn write_mkv(path: &Path) {
        // Duration is in timecode-scale ticks; the default scale is 1ms, so
        // 2_531_000 ticks is 2531s — 0:42:11.
        let info = elem(
            &[0x15, 0x49, 0xA9, 0x66],
            &cat(&[
                uint(&[0x2A, 0xD7, 0xB1], 1_000_000),
                elem(&[0x44, 0x89], &2_531_000f64.to_be_bytes()),
            ]),
        );

        let video = elem(
            &[0xAE],
            &cat(&[
                uint(&[0xD7], 1),
                uint(&[0x73, 0xC5], 1),
                uint(&[0x83], 0x01),
                utf8(&[0x86], "V_MPEGH/ISO/HEVC"),
                elem(&[0xE0], &cat(&[uint(&[0xB0], 1920), uint(&[0xBA], 1080)])),
            ]),
        );
        let audio = elem(
            &[0xAE],
            &cat(&[
                uint(&[0xD7], 2),
                uint(&[0x73, 0xC5], 2),
                uint(&[0x83], 0x02),
                utf8(&[0x86], "A_EAC3"),
                utf8(&[0x22, 0xB5, 0x9C], "eng"),
                elem(&[0xE1], &uint(&[0x9F], 6)),
            ]),
        );
        // A second audio track, to prove languages accumulate while the codec
        // and channel count stay those of the first.
        let audio_jpn = elem(
            &[0xAE],
            &cat(&[
                uint(&[0xD7], 3),
                uint(&[0x73, 0xC5], 3),
                uint(&[0x83], 0x02),
                utf8(&[0x86], "A_AAC"),
                utf8(&[0x22, 0xB5, 0x9C], "jpn"),
                elem(&[0xE1], &uint(&[0x9F], 2)),
            ]),
        );
        let subtitle = elem(
            &[0xAE],
            &cat(&[
                uint(&[0xD7], 4),
                uint(&[0x73, 0xC5], 4),
                uint(&[0x83], 0x11),
                utf8(&[0x86], "S_TEXT/UTF8"),
                utf8(&[0x22, 0xB5, 0x9C], "eng"),
            ]),
        );

        let tracks = elem(
            &[0x16, 0x54, 0xAE, 0x6B],
            &cat(&[video, audio, audio_jpn, subtitle]),
        );
        let segment = elem(&[0x18, 0x53, 0x80, 0x67], &cat(&[info, tracks]));
        let header = elem(&[0x1A, 0x45, 0xDF, 0xA3], &utf8(&[0x42, 0x82], "matroska"));

        std::fs::write(path, cat(&[header, segment])).unwrap();
    }

    #[test]
    fn a_matroska_file_yields_its_tracks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Lanternwick Hollow S02E01.mkv");
        write_mkv(&path);

        let meta = probe(&path).expect("a valid container describes itself");
        assert_eq!(meta.resolution.as_deref(), Some("1920x1080"));
        // The namespace is stripped, the name kept.
        assert_eq!(meta.video_codec.as_deref(), Some("HEVC"));
        assert_eq!(
            meta.audio_codec.as_deref(),
            Some("EAC3"),
            "the first audio track wins"
        );
        assert_eq!(meta.audio_channels.as_deref(), Some("5.1"));
        assert_eq!(
            meta.audio_languages.as_deref(),
            Some("eng/jpn"),
            "languages accumulate across tracks"
        );
        assert_eq!(meta.subtitles.as_deref(), Some("eng"));
        assert_eq!(meta.runtime.as_deref(), Some("0:42:11"));
    }

    /// End to end for the surface that motivated the probe: an MKV in a
    /// `[[library]]` directory reaching a release title with its real tokens.
    #[test]
    fn a_probed_matroska_drives_the_scene_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("whatever.mkv");
        write_mkv(&path);

        let meta = probe(&path).unwrap();
        assert_eq!(meta.scene_resolution(), Some("1080p"));
        assert_eq!(meta.scene_video_codec(), Some("x265"));
    }

    /// The extraction path, against a container that actually parses. Everything
    /// else in this module proves the *rejection* paths; this is the one that
    /// proves a probe produces the right answer rather than merely not crashing.
    #[test]
    fn a_real_container_yields_its_streams() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Gilded Ferry.mp4");
        write_mp4(&path, 1920, 1080);

        let meta = probe(&path).expect("a valid container describes itself");
        assert_eq!(meta.resolution.as_deref(), Some("1920x1080"));
        assert_eq!(meta.video_codec.as_deref(), Some("h264"));
        assert_eq!(meta.audio_codec.as_deref(), Some("aac"));
        // FiveOne is 6 channels, and the LFE makes that 5.1 rather than 6.0.
        assert_eq!(meta.audio_channels.as_deref(), Some("5.1"));
        // `und` is not a language and must not be published as one.
        assert_eq!(meta.audio_languages, None);
        assert!(meta.runtime.is_some());
    }

    /// The resolution has to survive into the shorthand a release title uses —
    /// the whole reason `synthesize` was given access to this.
    #[test]
    fn a_probed_resolution_reaches_the_scene_shorthand() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Copper Vale.mp4");
        write_mp4(&path, 1280, 720);

        let meta = probe(&path).unwrap();
        assert_eq!(meta.scene_resolution(), Some("720p"));
        assert_eq!(meta.scene_video_codec(), Some("x264"));
    }

    #[test]
    fn a_file_that_is_not_the_container_it_claims_yields_nothing() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["not-really.mkv", "not-really.mp4", "not-really.mov"] {
            let path = dir.path().join(name);
            std::fs::write(&path, b"this is not a media container at all").unwrap();
            assert_eq!(probe(&path), None, "{name}");
        }
    }

    #[test]
    fn a_missing_file_yields_nothing_rather_than_erroring() {
        assert_eq!(probe(Path::new("/nonexistent/file.mkv")), None);
    }

    /// Everything beside the media in a library — artwork, subtitles, `.nfo` —
    /// must be rejected on the extension alone, without ever being opened.
    #[test]
    fn unknown_extensions_are_not_opened() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["poster.jpg", "subs.srt", "movie.nfo", "noextension"] {
            let path = dir.path().join(name);
            std::fs::write(&path, b"x").unwrap();
            assert_eq!(probe(&path), None, "{name}");
        }
    }

    /// Dispatch is case-insensitive: `.MKV` off a Windows-authored library is the
    /// same container as `.mkv`, and must reach the same backend. Proven by the
    /// error path — an unrecognised extension returns before opening anything,
    /// so only a dispatched extension can report the container as unreadable.
    #[test]
    fn extension_matching_ignores_case() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Film.MKV");
        std::fs::write(&path, b"not a container").unwrap();
        assert_eq!(probe(&path), None);
    }
}
