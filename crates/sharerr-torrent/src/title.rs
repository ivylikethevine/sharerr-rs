//! Release titles: choosing one, and checking that it parses.
//!
//! # Why this matters more than it looks
//!
//! The friend's Sonarr/Radarr never sees sharerr's database. All it gets is a
//! release *title* from the indexer, which it parses to decide what the release
//! contains. An unparseable title is not an error on their end — it is a release
//! that is silently ignored. So a title is only usable if it parses back to the
//! episode or movie it came from, and [`resolve`] checks exactly that rather than
//! assuming.
//!
//! # This is not the torrent's internal name
//!
//! A single-file v1 torrent's `info.name` **is the filename on disk**; a client
//! told a different name looks for a file that does not exist and stalls at 0%.
//! The release title is metadata that travels alongside the torrent — in sharerr's
//! database now, and in the Torznab `<title>` element. See
//! [`crate::factory`], which deliberately takes no name argument.

use std::path::Path;

use sha2::{Digest, Sha256};
use sharerr_core::{MediaMeta, MediaSpec};
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

/// Group tag applied to titles sharerr synthesises, so they are identifiable as
/// not having come from a real scene release.
const GROUP: &str = "SHARERR";

/// What a release title claims to contain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedTitle {
    Episode {
        title: String,
        season: u32,
        episode: u32,
    },
    Movie {
        title: String,
        year: Option<u16>,
    },
    /// Nothing recognisable — the far end would ignore this release.
    Unparseable,
}

impl ParsedTitle {
    /// Whether this parse describes the same content as `spec`.
    ///
    /// Titles are compared loosely (case- and separator-insensitive) because a
    /// release name is not required to reproduce the library's punctuation.
    pub fn describes(&self, spec: &MediaSpec) -> bool {
        match (self, spec) {
            (
                Self::Episode {
                    title,
                    season,
                    episode,
                },
                MediaSpec::Episode {
                    series_title,
                    season: s,
                    episode: e,
                },
            ) => season == s && episode == e && loose_eq(title, series_title),
            (Self::Movie { title, year }, MediaSpec::Movie { title: t, year: y }) => {
                loose_eq(title, t) && (year.is_none() || y.is_none() || year == y)
            }
            // Spelled out rather than a catch-all: `Track` and `Book` have no
            // `ParsedTitle` counterpart, so filename reuse never applies to music
            // or books — and a new `MediaSpec` variant must fail to compile here
            // instead of silently losing that step of `resolve`.
            (Self::Unparseable, _)
            | (Self::Episode { .. } | Self::Movie { .. }, MediaSpec::Track { .. })
            | (Self::Episode { .. } | Self::Movie { .. }, MediaSpec::Book { .. })
            | (Self::Episode { .. }, MediaSpec::Movie { .. })
            | (Self::Movie { .. }, MediaSpec::Episode { .. }) => false,
        }
    }
}

/// Pick the release title for a file.
///
/// In order of preference:
///
/// 1. The scene name the *arr app recorded on import. This is the best possible
///    answer — it is a title that a real indexer already published and a real
///    Sonarr already parsed.
/// 2. The basename the file had when the *arr app imported it (Radarr's
///    `originalFilePath`), **only if it parses back to the right thing**. A
///    library renamed after import keeps the pre-rename name here, so this often
///    recovers a real release name that step 3 no longer sees.
/// 3. The file's own basename, but **only if it parses back to the right thing**.
///    Renamed libraries often produce names like `Show - S02E01 - Episode Name`,
///    which parse fine; but a library renamed to `01 - Episode Name` does not, and
///    using it would produce a release nobody can match.
/// 4. A synthesised scene-style name.
///
/// Steps 2 and 3 share the parse gate for the same reason: neither name is known
/// to describe this file. Only step 1 is trusted unconditionally, because the
/// scene name is one an indexer published and an *arr already parsed.
pub fn resolve(
    spec: &MediaSpec,
    scene_name: Option<&str>,
    original_path: Option<&Path>,
    media: Option<&MediaMeta>,
    path: &Path,
) -> String {
    if let Some(scene) = scene_name {
        let cleaned = strip_extension(scene.trim());
        if !cleaned.is_empty() {
            return cleaned.to_owned();
        }
    }

    for candidate in [original_path, Some(path)].into_iter().flatten() {
        if let Some(stem) = candidate.file_stem().and_then(|s| s.to_str())
            && parse(stem).describes(spec)
        {
            return stem.to_owned();
        }
    }

    synthesize(spec, media)
}

/// Build a scene-style name from metadata alone.
///
/// A title with no quality at all parses to "unknown quality" on the far end,
/// which many profiles reject outright — so this always emits *some* quality
/// tokens. Which ones depends on how much is known about the file:
///
/// - With [`MediaMeta`], the resolution and video codec are the file's real
///   ones — or, for music, the real audio format — and only the source token
///   stays invented (see below).
/// - Without it, the tokens are the flat guess this function has always
///   produced. `1080p.WEB-DL.x264` is not a claim about the file so much as the
///   most common shape of one, chosen because it is the least likely to be
///   filtered out. `FLAC` is the same kind of guess for a track: the format a
///   friend's Lidarr is least likely to reject, not something read off the file.
///
/// **The source token is a fiction either way.** `WEB-DL` versus `BluRay` versus
/// `HDTV` describes where a release came from, and no amount of reading the file
/// reveals that — the container of a BluRay remux and a web download are
/// identical. It stays `WEB-DL` because that is the least restrictive assumption
/// a downstream quality profile can be handed.
pub fn synthesize(spec: &MediaSpec, media: Option<&MediaMeta>) -> String {
    // Real values where they are known, the historical guess where they are not.
    let resolution = media
        .and_then(MediaMeta::scene_resolution)
        .unwrap_or("1080p");
    let codec = media
        .and_then(MediaMeta::scene_video_codec)
        .unwrap_or("x264");
    let audio = media
        .and_then(MediaMeta::scene_audio_format)
        .unwrap_or("FLAC");
    match spec {
        MediaSpec::Episode {
            series_title,
            season,
            episode,
        } => {
            format!(
                "{}.S{season:02}E{episode:02}.{resolution}.WEB-DL.{codec}-{GROUP}",
                dotted(series_title)
            )
        }
        MediaSpec::Movie { title, year } => match year {
            Some(year) => format!(
                "{}.{year}.{resolution}.WEB-DL.{codec}-{GROUP}",
                dotted(title)
            ),
            None => format!("{}.{resolution}.WEB-DL.{codec}-{GROUP}", dotted(title)),
        },
        // Music convention is `Artist-Album-FORMAT-GROUP`, hyphen-separated rather
        // than dotted, and with an audio format instead of a video codec. A music
        // release named like a TV episode is rejected by the profiles that matter,
        // and one named `FLAC` that is really an MP3 is rejected on arrival — the
        // format token is the one thing a Lidarr quality profile reads closely.
        MediaSpec::Track {
            artist,
            album,
            track,
        } => match track {
            Some(track) => format!(
                "{}-{}-{track:02}-{audio}-{GROUP}",
                dotted(artist),
                dotted(album)
            ),
            None => format!("{}-{}-{audio}-{GROUP}", dotted(artist), dotted(album)),
        },
        // Books have no scene convention worth imitating; author and title are what
        // a reader and a parser both look for.
        MediaSpec::Book { author, title } => {
            format!("{}-{}-EPUB-{GROUP}", dotted(author), dotted(title))
        }
    }
}

/// Parse a release title the way a downstream *arr app would.
///
/// This is a deliberately small subset — `SxxEyy` for episodes, a bare four-digit
/// year for movies — but it is the part every parser agrees on, so a title this
/// accepts is one the real implementations accept too.
pub fn parse(title: &str) -> ParsedTitle {
    let normalised = humanize(title);
    let tokens: Vec<&str> = normalised.split_whitespace().collect();

    for (index, token) in tokens.iter().enumerate() {
        if let Some((season, episode)) = season_episode(token) {
            let name = join_title(&tokens[..index]);
            if name.is_empty() {
                return ParsedTitle::Unparseable;
            }
            return ParsedTitle::Episode {
                title: name,
                season,
                episode,
            };
        }
    }

    // No SxxEyy: look for a year, which is what marks a movie release. Skip the
    // first token so a title that *is* a year ("1917") is not consumed as its own
    // release year.
    for (index, token) in tokens.iter().enumerate().skip(1) {
        if let Some(year) = release_year(token) {
            let name = join_title(&tokens[..index]);
            if name.is_empty() {
                return ParsedTitle::Unparseable;
            }
            return ParsedTitle::Movie {
                title: name,
                year: Some(year),
            };
        }
    }

    ParsedTitle::Unparseable
}

/// `S02E01` / `s02e01` / `S2E1` -> `(2, 1)`.
fn season_episode(token: &str) -> Option<(u32, u32)> {
    let bytes = token.as_bytes();
    if bytes.first().is_none_or(|b| !b.eq_ignore_ascii_case(&b'S')) {
        return None;
    }

    let rest = &token[1..];
    let split = rest.find(['e', 'E'])?;
    let (season, episode) = (&rest[..split], &rest[split + 1..]);

    if season.is_empty() || episode.is_empty() {
        return None;
    }
    // Trailing junk such as `S02E01E02` is not a plain single-episode token.
    Some((season.parse().ok()?, episode.parse().ok()?))
}

fn release_year(token: &str) -> Option<u16> {
    // Bracketed years are common: `(2019)`.
    let trimmed = token.trim_matches(['(', ')', '[', ']']);
    if trimmed.len() != 4 {
        return None;
    }
    let year: u16 = trimmed.parse().ok()?;
    (1900..=2099).contains(&year).then_some(year)
}

/// Join `tokens` back into a display title, trimming a leading or trailing
/// `-` or space a token boundary can leave behind.
///
/// `pub` rather than crate-private: `sharerr::library`'s directory-source
/// scan needs to cut a token list down to a display title the same way a
/// parsed release does, just from a differently-filtered token list (see
/// [`humanize`], shared for the same reason).
pub fn join_title(tokens: &[&str]) -> String {
    tokens.join(" ").trim_matches(['-', ' ']).to_owned()
}

/// Dots and underscores back to spaces — release names use them where a
/// display title uses a space. `pub` for the same reason [`join_title`] is.
pub fn humanize(stem: &str) -> String {
    stem.replace(['.', '_'], " ").trim().to_owned()
}

/// Decompose accented and other compatibility characters into a base letter
/// plus combining marks (NFKD) — `é` becomes `e` + a combining acute accent —
/// and drop the marks, so the ASCII filters below keep the bare base letter
/// rather than either discarding the whole character or, worse, treating the
/// combining mark as a word-breaking separator and splitting the base letter
/// away from the rest of the word (`Amélie` would otherwise transliterate to
/// `Ame.lie`, not `Amelie`). This recovers Latin-script titles but not
/// scripts with no ASCII-equivalent base letter at all (CJK, Cyrillic, ...),
/// which still fall through to [`ascii_fallback`].
fn transliterate(s: &str) -> String {
    s.nfkd().filter(|c| !is_combining_mark(*c)).collect()
}

/// A short, stable placeholder for a title with nothing ASCII-transliterable
/// in it, derived from the title's bytes so different untransliterable
/// titles get different placeholders rather than colliding on one literal
/// `"unknown"`. Shared between [`dotted`] and [`loose_eq`] so a title
/// sharerr synthesises from this placeholder is recognised by `loose_eq` as
/// describing the title it came from.
fn ascii_fallback(title: &str) -> String {
    let digest = Sha256::digest(title.to_lowercase().as_bytes());
    format!("unknown{}", hex::encode(&digest[..4]))
}

/// Scene names are dot-separated with no spaces.
fn dotted(title: &str) -> String {
    let cleaned: String = transliterate(title)
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(".");

    if cleaned.is_empty() {
        // `ascii_fallback` already starts with "unknown"; capitalise to match
        // this function's own style ("Unknown", not "unknown") without
        // changing the value `loose_eq`'s ASCII filter and lowercasing later
        // reduce it back to.
        let id = ascii_fallback(title);
        format!("Unknown.{}", &id["unknown".len()..])
    } else {
        cleaned
    }
}

/// *arr apps sometimes record `sceneName` with the container extension attached.
fn strip_extension(name: &str) -> &str {
    const CONTAINERS: &[&str] = &[".mkv", ".mp4", ".avi", ".ts", ".m2ts", ".wmv", ".mov"];
    for ext in CONTAINERS {
        let Some(cut) = name.len().checked_sub(ext.len()) else {
            continue;
        };
        // `get` rather than a slice: `cut` may fall inside a multi-byte char.
        if cut > 0
            && name
                .get(cut..)
                .is_some_and(|tail| tail.eq_ignore_ascii_case(ext))
        {
            return &name[..cut];
        }
    }
    name
}

/// Compare titles ignoring case, separators, and punctuation.
fn loose_eq(a: &str, b: &str) -> bool {
    let key = |s: &str| -> String {
        let ascii: String = transliterate(s)
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .map(|c| c.to_ascii_lowercase())
            .collect();
        if ascii.is_empty() {
            // Same placeholder `dotted` would have synthesised from this
            // title, so a title with no ASCII-transliterable content still
            // matches the release sharerr generated for it.
            ascii_fallback(s)
        } else {
            ascii
        }
    };
    key(a) == key(b)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::path::PathBuf;

    fn episode() -> MediaSpec {
        MediaSpec::Episode {
            series_title: "Lanternwick Hollow".to_owned(),
            season: 2,
            episode: 1,
        }
    }

    fn movie() -> MediaSpec {
        MediaSpec::Movie {
            title: "The Gilded Ferry".to_owned(),
            year: Some(2019),
        }
    }

    // ---------------------------------------------------------- parsing

    #[test]
    fn parses_a_scene_episode_name() {
        assert_eq!(
            parse("Lanternwick.Hollow.S02E01.1080p.WEB-DL.DD5.1.H.264-FAKEGRP"),
            ParsedTitle::Episode {
                title: "Lanternwick Hollow".to_owned(),
                season: 2,
                episode: 1
            }
        );
    }

    #[test]
    fn parses_a_renamed_library_episode_name() {
        // The shape Sonarr's default rename produces.
        assert_eq!(
            parse("Lanternwick Hollow - S02E01 - The Long Way Down"),
            ParsedTitle::Episode {
                title: "Lanternwick Hollow".to_owned(),
                season: 2,
                episode: 1
            }
        );
    }

    #[test]
    fn parses_single_digit_season_and_episode() {
        assert_eq!(
            parse("Copper.Vale.S2E4.WEB-DL"),
            ParsedTitle::Episode {
                title: "Copper Vale".to_owned(),
                season: 2,
                episode: 4
            }
        );
    }

    #[test]
    fn parses_a_scene_movie_name() {
        assert_eq!(
            parse("The.Gilded.Ferry.2019.1080p.BluRay.x264-FAKEGRP"),
            ParsedTitle::Movie {
                title: "The Gilded Ferry".to_owned(),
                year: Some(2019)
            }
        );
    }

    #[test]
    fn parses_a_bracketed_movie_year() {
        assert_eq!(
            parse("The Gilded Ferry (2019)"),
            ParsedTitle::Movie {
                title: "The Gilded Ferry".to_owned(),
                year: Some(2019)
            }
        );
    }

    #[test]
    fn a_title_that_is_only_a_year_is_not_consumed_as_its_own_year() {
        // A movie literally called "1917" must not parse to an empty title.
        assert_eq!(
            parse("1917 2019 1080p"),
            ParsedTitle::Movie {
                title: "1917".to_owned(),
                year: Some(2019)
            }
        );
    }

    #[test]
    fn unrecognisable_names_are_reported_rather_than_guessed() {
        assert_eq!(parse("01 - The Long Way Down"), ParsedTitle::Unparseable);
        assert_eq!(parse("episode1"), ParsedTitle::Unparseable);
        assert_eq!(parse(""), ParsedTitle::Unparseable);
        // A season/episode token with nothing before it names no series.
        assert_eq!(parse("S02E01"), ParsedTitle::Unparseable);
    }

    // ---------------------------------------------------------- round trips

    /// The property that matters: anything sharerr synthesises must parse back to
    /// the thing it was made from, or the friend's Sonarr silently ignores it.
    #[test]
    fn synthesized_episode_titles_round_trip() {
        for (series, season, ep) in [
            ("Lanternwick Hollow", 2u32, 1u32),
            ("Copper Vale Station", 11, 24),
            ("Harrowmere: The Second Age", 1, 1),
            ("A Show With  Odd   Spacing", 3, 9),
            ("Numbers 1917 In The Title", 4, 2),
            ("Amélie's Café", 1, 1),
            ("千と千尋の神隠し", 1, 1),
        ] {
            let spec = MediaSpec::Episode {
                series_title: series.to_owned(),
                season,
                episode: ep,
            };
            let title = synthesize(&spec, None);
            assert!(
                parse(&title).describes(&spec),
                "{title:?} did not round-trip back to {spec:?}"
            );
        }
    }

    #[test]
    fn synthesized_movie_titles_round_trip() {
        for (name, year) in [
            ("The Gilded Ferry", Some(2019u16)),
            ("Harrowmere", Some(2024)),
            ("Punctuation: A Movie!", Some(1999)),
            ("No Year Known", None),
            ("Amélie", Some(2001)),
            ("千と千尋の神隠し", Some(2001)),
        ] {
            let spec = MediaSpec::Movie {
                title: name.to_owned(),
                year,
            };
            let title = synthesize(&spec, None);
            if year.is_some() {
                assert!(
                    parse(&title).describes(&spec),
                    "{title:?} did not round-trip back to {spec:?}"
                );
            } else {
                // With no year there is nothing for a movie parser to anchor on.
                // Accepted, and the reason a year is worth having.
                assert_eq!(parse(&title), ParsedTitle::Unparseable);
            }
        }
    }

    #[test]
    fn synthesized_titles_are_dot_separated_and_tagged() {
        let title = synthesize(&episode(), None);
        assert_eq!(title, "Lanternwick.Hollow.S02E01.1080p.WEB-DL.x264-SHARERR");
        assert!(!title.contains(' '), "scene names carry no spaces: {title}");
    }

    /// The trap this exists to avoid: dropping combining marks by filtering
    /// on `char::is_ascii_alphanumeric` alone splits an accented word into
    /// two ("Am" + "lie") instead of recovering the unaccented letter.
    #[test]
    fn accented_titles_are_transliterated_not_split() {
        let spec = MediaSpec::Movie {
            title: "Amélie".to_owned(),
            year: Some(2001),
        };
        let title = synthesize(&spec, None);
        assert_eq!(title, "Amelie.2001.1080p.WEB-DL.x264-SHARERR");
    }

    /// A script with no ASCII-transliterable form at all (CJK, here) used to
    /// collapse to the literal `"Unknown"` — indistinguishable from any other
    /// untransliterable title, and a collision risk for `describes`.
    #[test]
    fn a_title_with_no_ascii_form_gets_a_stable_placeholder_not_the_literal_unknown() {
        let title = synthesize(
            &MediaSpec::Movie {
                title: "千と千尋の神隠し".to_owned(),
                year: Some(2001),
            },
            None,
        );
        assert_ne!(title, "Unknown.2001.1080p.WEB-DL.x264-SHARERR");
        assert!(
            title.starts_with("Unknown."),
            "still recognisable as a placeholder: {title}"
        );
    }

    /// Two different untransliterable titles must not collapse onto the same
    /// placeholder and therefore be treated as describing the same content.
    #[test]
    fn two_different_untransliterable_titles_get_different_placeholders() {
        let a = synthesize(
            &MediaSpec::Movie {
                title: "千と千尋の神隠し".to_owned(),
                year: Some(2001),
            },
            None,
        );
        let b = synthesize(
            &MediaSpec::Movie {
                title: "となりのトトロ".to_owned(),
                year: Some(1988),
            },
            None,
        );
        assert_ne!(a, b);
        assert!(
            !parse(&a).describes(&MediaSpec::Movie {
                title: "となりのトトロ".to_owned(),
                year: Some(1988),
            }),
            "a release synthesised for one untransliterable title must not \
             describe an unrelated one: {a:?}"
        );
    }

    // ------------------------------------------------------ synthesis + media

    fn full_meta() -> MediaMeta {
        MediaMeta {
            resolution: Some("3840x2160".to_owned()),
            video_codec: Some("HEVC".to_owned()),
            ..MediaMeta::default()
        }
    }

    #[test]
    fn known_metadata_replaces_the_guessed_quality_tokens() {
        let title = synthesize(&episode(), Some(&full_meta()));
        assert_eq!(title, "Lanternwick.Hollow.S02E01.2160p.WEB-DL.x265-SHARERR");

        let movie = synthesize(&movie(), Some(&full_meta()));
        assert_eq!(movie, "The.Gilded.Ferry.2019.2160p.WEB-DL.x265-SHARERR");
    }

    /// Metadata that describes the file but not in terms a release title uses —
    /// an off-ladder resolution, an unrecognised codec — falls back per token
    /// rather than all-or-nothing.
    #[test]
    fn unusable_metadata_falls_back_token_by_token() {
        let odd = MediaMeta {
            resolution: Some("1024x768".to_owned()),
            video_codec: Some("HEVC".to_owned()),
            ..MediaMeta::default()
        };
        assert_eq!(
            synthesize(&episode(), Some(&odd)),
            "Lanternwick.Hollow.S02E01.1080p.WEB-DL.x265-SHARERR",
            "the codec is usable even though the resolution is not"
        );
    }

    fn track() -> MediaSpec {
        MediaSpec::Track {
            artist: "Copper Vale".to_owned(),
            album: "Harrowmere".to_owned(),
            track: Some(3),
        }
    }

    /// Music and books have no resolution or video codec, so metadata must not
    /// leak into their naming conventions — and a track whose metadata says
    /// nothing about its audio keeps the historical `FLAC` guess.
    #[test]
    fn audio_and_book_titles_ignore_video_metadata() {
        assert_eq!(
            synthesize(&track(), Some(&full_meta())),
            "Copper.Vale-Harrowmere-03-FLAC-SHARERR"
        );
        assert_eq!(
            synthesize(
                &MediaSpec::Book {
                    author: "Copper Vale".to_owned(),
                    title: "Harrowmere".to_owned(),
                },
                Some(&full_meta()),
            ),
            "Copper.Vale-Harrowmere-EPUB-SHARERR"
        );
    }

    /// The whole point of reading Lidarr's `mediaInfo`: a friend's quality profile
    /// reads the format token, and calling an MP3 a FLAC gets the release rejected
    /// on arrival rather than filtered out up front.
    #[test]
    fn a_tracks_format_token_is_the_format_the_file_actually_is() {
        let lossy = MediaMeta {
            audio_codec: Some("MP3".to_owned()),
            ..MediaMeta::default()
        };
        assert_eq!(
            synthesize(&track(), Some(&lossy)),
            "Copper.Vale-Harrowmere-03-MP3-SHARERR"
        );

        // An unrecognised codec falls back rather than dropping a string nobody
        // parses into the middle of a release name.
        let unknown = MediaMeta {
            audio_codec: Some("Some Future Codec".to_owned()),
            ..MediaMeta::default()
        };
        assert_eq!(
            synthesize(&track(), Some(&unknown)),
            "Copper.Vale-Harrowmere-03-FLAC-SHARERR"
        );
    }

    /// The metadata reaches `synthesize` only through the fallback path — a real
    /// scene name still outranks everything, metadata included.
    #[test]
    fn metadata_does_not_displace_a_real_name() {
        let scene = "Lanternwick.Hollow.S02E01.720p.HDTV.x264-REALGRP";
        let resolved = resolve(
            &episode(),
            Some(scene),
            None,
            Some(&full_meta()),
            &PathBuf::from("/tv/whatever.mkv"),
        );
        assert_eq!(resolved, scene, "a published name is not ours to improve");
    }

    // ---------------------------------------------------------- resolution

    #[test]
    fn a_scene_name_is_preferred_over_everything() {
        let scene = "Lanternwick.Hollow.S02E01.1080p.WEB-DL.DD5.1.H.264-FAKEGRP";
        let resolved = resolve(
            &episode(),
            Some(scene),
            None,
            None,
            &PathBuf::from("/tv/Lanternwick Hollow/Season 02/whatever.mkv"),
        );
        assert_eq!(resolved, scene);
    }

    #[test]
    fn a_scene_name_keeps_its_extension_off() {
        let resolved = resolve(
            &episode(),
            Some("Lanternwick.Hollow.S02E01.1080p.1080p.WEB-DL.x264-FAKEGRP.mkv"),
            None,
            None,
            &PathBuf::from("/tv/x.mkv"),
        );
        assert_eq!(
            resolved,
            "Lanternwick.Hollow.S02E01.1080p.1080p.WEB-DL.x264-FAKEGRP"
        );
    }

    #[test]
    fn a_parseable_basename_is_used_when_there_is_no_scene_name() {
        let resolved = resolve(
            &episode(),
            None,
            None,
            None,
            &PathBuf::from(
                "/tv/Lanternwick Hollow/Season 02/Lanternwick Hollow - S02E01 - Pilot.mkv",
            ),
        );
        assert_eq!(resolved, "Lanternwick Hollow - S02E01 - Pilot");
    }

    #[test]
    fn an_unparseable_basename_is_replaced_rather_than_published() {
        let resolved = resolve(
            &episode(),
            None,
            None,
            None,
            &PathBuf::from("/tv/Lanternwick Hollow/Season 02/01.mkv"),
        );
        assert_eq!(
            resolved,
            "Lanternwick.Hollow.S02E01.1080p.WEB-DL.x264-SHARERR"
        );
    }

    /// The subtle one: a basename that parses, but to the *wrong* episode. Using it
    /// would advertise the wrong content under a title that looks perfectly fine.
    #[test]
    fn a_basename_describing_different_content_is_rejected() {
        let resolved = resolve(
            &episode(),
            None,
            None,
            None,
            &PathBuf::from("/tv/Copper Vale/Copper.Vale.S05E09.WEB-DL.mkv"),
        );
        assert_eq!(
            resolved,
            "Lanternwick.Hollow.S02E01.1080p.WEB-DL.x264-SHARERR"
        );
    }

    #[test]
    fn an_empty_scene_name_falls_through() {
        let resolved = resolve(
            &episode(),
            Some("   "),
            None,
            None,
            &PathBuf::from("/tv/Lanternwick/01.mkv"),
        );
        assert_eq!(
            resolved,
            "Lanternwick.Hollow.S02E01.1080p.WEB-DL.x264-SHARERR"
        );
    }

    /// The case the fallback exists for: Radarr renamed the file on import, so the
    /// basename on disk no longer carries a release name, but `originalFilePath`
    /// still does.
    #[test]
    fn an_original_path_is_used_when_the_current_basename_is_unparseable() {
        let resolved = resolve(
            &movie(),
            None,
            Some(&PathBuf::from(
                "/downloads/The.Gilded.Ferry.2019.1080p.BluRay.x264-FAKEGRP.mkv",
            )),
            None,
            &PathBuf::from("/movies/The Gilded Ferry (2019)/movie.mkv"),
        );
        assert_eq!(resolved, "The.Gilded.Ferry.2019.1080p.BluRay.x264-FAKEGRP");
    }

    /// The original path is a candidate, not a truth: it goes through the same
    /// parse gate as the basename, so an import from a junk-named download does
    /// not get published as a release title.
    #[test]
    fn an_unparseable_original_path_falls_through_to_the_basename() {
        let resolved = resolve(
            &movie(),
            None,
            Some(&PathBuf::from("/downloads/rrqx7z1p/file1.mkv")),
            None,
            &PathBuf::from("/movies/The Gilded Ferry (2019)/The Gilded Ferry (2019).mkv"),
        );
        assert_eq!(resolved, "The Gilded Ferry (2019)");
    }

    /// Same rejection the basename gets: parses cleanly, describes something else.
    #[test]
    fn an_original_path_describing_different_content_is_rejected() {
        let resolved = resolve(
            &movie(),
            None,
            Some(&PathBuf::from(
                "/downloads/Copper.Vale.2011.1080p.WEB-DL.mkv",
            )),
            None,
            &PathBuf::from("/movies/The Gilded Ferry (2019)/movie.mkv"),
        );
        assert_eq!(resolved, "The.Gilded.Ferry.2019.1080p.WEB-DL.x264-SHARERR");
    }

    #[test]
    fn a_scene_name_still_outranks_the_original_path() {
        let resolved = resolve(
            &movie(),
            Some("The.Gilded.Ferry.2019.2160p.UHD.BluRay-SCENEGRP"),
            Some(&PathBuf::from(
                "/downloads/The.Gilded.Ferry.2019.1080p.BluRay.x264-FAKEGRP.mkv",
            )),
            None,
            &PathBuf::from("/movies/The Gilded Ferry (2019)/movie.mkv"),
        );
        assert_eq!(resolved, "The.Gilded.Ferry.2019.2160p.UHD.BluRay-SCENEGRP");
    }

    #[test]
    fn movie_basenames_resolve_the_same_way() {
        let parseable = resolve(
            &movie(),
            None,
            None,
            None,
            &PathBuf::from("/movies/The Gilded Ferry (2019)/The Gilded Ferry (2019).mkv"),
        );
        assert_eq!(parseable, "The Gilded Ferry (2019)");

        let unparseable = resolve(
            &movie(),
            None,
            None,
            None,
            &PathBuf::from("/movies/The Gilded Ferry (2019)/movie.mkv"),
        );
        assert_eq!(
            unparseable,
            "The.Gilded.Ferry.2019.1080p.WEB-DL.x264-SHARERR"
        );
    }
}
