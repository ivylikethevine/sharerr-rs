//! What the library is made of, rolled up from rows the items page already has.
//!
//! `media_json` landed with migration `0009` and until now was read only by the
//! two feed renderers — nothing aggregated it, so "is what I am sharing what I
//! think I am sharing?" was a question an operator could only answer by reading
//! the whole table. A library that is quietly 80% 720p, or one where a third of
//! the rows are `failed`, is not visible in a table sorted by date.
//!
//! Three deliberate constraints:
//!
//! * **No query of its own.** `items::page` has already fetched every row; this
//!   folds that slice. A dedicated route would re-read the whole table to say
//!   less.
//! * **The whole library, never the filter.** The tallies above the table are
//!   counted before filtering for the same reason — a summary that moved with
//!   the search box would answer a different question each time it was read.
//! * **Coordinates computed here, not in the template.** The same division of
//!   labour as `diagnostics::run_chart` and `topology::layout`: this module
//!   emits `x`/`w` in user units and the template places them. The web UI
//!   compiles every asset into the binary and reaches no CDN, so a chart is
//!   server-rendered SVG or it is nothing.

use sharerr_core::model::{MediaMeta, SharedItem};

use crate::web::items::human_size;
use crate::web::templates::{Breakdown, Composition, CompositionRow, Segment};

/// User units for the stacked bars. The viewBox scales to whatever width the
/// panel gets, so this is a coordinate space rather than a pixel size — but it
/// is kept close to the rendered width so a rounded `x` lands on a whole pixel
/// more often than not.
const BAR_W: i32 = 640;
const BAR_H: i32 = 14;

/// So a category that is a rounding error is still visible as *something*.
/// `run_chart` clamps its bars for the same reason; the table beneath carries
/// the exact figure either way.
const MIN_SEG_W: i32 = 2;

/// The palette, cycled. These are the diagram colours, not the health ones: a
/// composition bar means "this much of the library is 1080p", and borrowing
/// `--error-ink` for the fourth-largest codec would read as a warning about it.
const ACCENTS: [&str; 9] = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];

/// Roll a library up three ways, or `None` when there is nothing to roll up.
///
/// `None` rather than three empty bars on a fresh instance: an empty chart reads
/// as a thing that failed to load, and the items page already says "no items"
/// immediately below.
pub(crate) fn compose(items: &[SharedItem]) -> Option<Composition> {
    if items.is_empty() {
        return None;
    }

    let total_bytes: u64 = items.iter().map(|item| item.size).sum();

    Some(Composition {
        items: items.len(),
        total_size: human_size(total_bytes),
        breakdowns: vec![
            breakdown("Format", "What the files actually are", items, format_label),
            breakdown(
                "State",
                "Where each file is in the share cycle",
                items,
                |item| item.state.as_str().to_owned(),
            ),
            breakdown("Source", "Which app or directory found it", items, |item| {
                item.source.as_str().to_owned()
            }),
        ],
    })
}

/// The bucket a file falls into when asking "what is this, in the terms a
/// release is named in".
///
/// Resolution first, because a video file has both a resolution and an audio
/// codec and the resolution is what its quality is judged on. A music file has
/// no resolution, so it falls through to its audio format — which is exactly
/// what a friend's Lidarr filters on. Anything that is neither is `unknown`,
/// which is a fact worth showing rather than hiding: a large `unknown` slice
/// means the *arr apps never analysed those files and no probe covered them.
fn format_label(item: &SharedItem) -> String {
    item.media
        .as_ref()
        .and_then(|media| {
            media
                .scene_resolution()
                .or_else(|| MediaMeta::scene_audio_format(media))
        })
        .unwrap_or("unknown")
        .to_owned()
}

/// One roll-up: bucket, sort, lay out the bar, and pre-render every string.
fn breakdown(
    title: &'static str,
    hint: &'static str,
    items: &[SharedItem],
    label_of: impl Fn(&SharedItem) -> String,
) -> Breakdown {
    // A `Vec` of pairs rather than a map: the bucket count is small (six
    // resolutions, four states, six sources), and this keeps insertion order
    // available as the tie-break below.
    let mut buckets: Vec<(String, usize, u64)> = Vec::new();
    for item in items {
        let label = label_of(item);
        match buckets.iter_mut().find(|(name, _, _)| *name == label) {
            Some((_, count, bytes)) => {
                *count += 1;
                *bytes += item.size;
            }
            None => buckets.push((label, 1, item.size)),
        }
    }

    // Biggest share first, so the bar reads left to right in the order the
    // question is asked. `unknown` sinks to the end regardless of size: it is
    // the absence of an answer, not one of the answers.
    buckets.sort_by(|a, b| {
        let unknown = |name: &str| name == "unknown";
        unknown(&a.0)
            .cmp(&unknown(&b.0))
            .then(b.2.cmp(&a.2))
            .then(a.0.cmp(&b.0))
    });

    let total_bytes: u64 = buckets.iter().map(|(_, _, bytes)| bytes).sum();
    let percentages = whole_percentages(&buckets, total_bytes);

    let mut segments = Vec::with_capacity(buckets.len());
    let mut rows = Vec::with_capacity(buckets.len());
    let mut x = 0;

    for (index, (label, count, bytes)) in buckets.iter().enumerate() {
        let accent = ACCENTS[index % ACCENTS.len()];
        let percent = percentages[index];
        // The bar keeps the exact fraction; only the printed figure is rounded.
        #[allow(clippy::cast_precision_loss)]
        let share = if total_bytes > 0 {
            *bytes as f64 / total_bytes as f64 * 100.0
        } else {
            0.0
        };
        // A bucket that rounds to nothing still holds files, and `0%` beside a
        // non-zero count reads as a bug rather than as rounding.
        let printed = if percent == 0 {
            "<1%".to_owned()
        } else {
            format!("{percent}%")
        };

        // The last segment absorbs the rounding rather than each one carrying a
        // fraction of it, so the bar ends flush at `BAR_W` instead of a pixel or
        // two short of it.
        //
        // Every earlier segment leaves `MIN_SEG_W` of room for each one still to
        // come. Without that, a library that is 99.99% one bucket rounds the
        // first segment to the full width and the slivers after it have nowhere
        // to go — which is a panic, not a squashed bar, since the clamp's minimum
        // then exceeds its maximum.
        let remaining = i32::try_from(buckets.len() - index - 1).unwrap_or(i32::MAX);
        let w = if remaining == 0 {
            (BAR_W - x).max(MIN_SEG_W)
        } else {
            #[allow(clippy::cast_possible_truncation)]
            let scaled = (share / 100.0 * f64::from(BAR_W)).round() as i32;
            let headroom = (BAR_W - x - remaining * MIN_SEG_W).max(MIN_SEG_W);
            scaled.clamp(MIN_SEG_W, headroom)
        };

        // One rendering of the name for both, so the tooltip on a slice and the
        // row in the table beneath it cannot disagree about what it is called.
        let display = crate::web::settings::title_case(label);

        segments.push(Segment {
            x,
            w,
            h: BAR_H,
            accent,
            title: format!("{display} — {} ({printed})", human_size(*bytes)),
        });
        rows.push(CompositionRow {
            label: display,
            accent,
            count: *count,
            size: human_size(*bytes),
            share: printed,
        });
        x += w;
    }

    Breakdown {
        title,
        hint,
        segments,
        rows,
        width: BAR_W,
        height: BAR_H,
    }
}

/// Whole-percent shares that add up to exactly 100.
///
/// Rounding each bucket independently is what a reader notices: three buckets at
/// 98.4%, 1.5% and 0.8% print as 98, 2 and 1, and a table whose shares sum to
/// 101% reads as broken arithmetic rather than as rounding. Largest remainder —
/// floor everything, then hand the leftover points to the buckets that lost the
/// most to the floor — is the standard fix and is what an apportionment does.
fn whole_percentages(buckets: &[(String, usize, u64)], total: u64) -> Vec<u32> {
    if total == 0 {
        return vec![0; buckets.len()];
    }

    #[allow(clippy::cast_precision_loss)]
    let exact: Vec<f64> = buckets
        .iter()
        .map(|(_, _, bytes)| *bytes as f64 / total as f64 * 100.0)
        .collect();

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let mut whole: Vec<u32> = exact.iter().map(|share| share.floor() as u32).collect();

    // What the floors gave away, handed back one point at a time to whoever lost
    // the most. `saturating_sub` because a float sum can land a hair over 100.
    let assigned: u32 = whole.iter().sum();
    let mut leftover = 100_u32.saturating_sub(assigned);

    let mut order: Vec<usize> = (0..buckets.len()).collect();
    order.sort_by(|a, b| {
        let remainder = |i: usize| exact[i] - exact[i].floor();
        remainder(*b)
            .partial_cmp(&remainder(*a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for index in order {
        if leftover == 0 {
            break;
        }
        whole[index] += 1;
        leftover -= 1;
    }

    whole
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use sharerr_core::model::{MediaSource, MediaSpec, ShareState};

    fn item(
        source: MediaSource,
        state: ShareState,
        size: u64,
        media: Option<MediaMeta>,
    ) -> SharedItem {
        SharedItem {
            id: None,
            source,
            source_id: 1,
            file_id: 1,
            spec: MediaSpec::Movie {
                title: "Harborlight".to_owned(),
                year: Some(2019),
            },
            release_title: "Harborlight.2019.1080p.WEB-DL.x264-SYNTH".to_owned(),
            arr_path: std::path::PathBuf::from("/data/movies/Harborlight (2019).mkv"),
            size,
            ids: sharerr_core::ExternalIds::default(),
            info_hash: None,
            announce_token_fp: None,
            created_by_sharerr: false,
            state,
            last_error: None,
            created_at: None,
            media,
        }
    }

    fn video(resolution: &str) -> MediaMeta {
        MediaMeta {
            resolution: Some(resolution.to_owned()),
            ..MediaMeta::default()
        }
    }

    fn find<'a>(composition: &'a Composition, title: &str) -> &'a Breakdown {
        composition
            .breakdowns
            .iter()
            .find(|b| b.title == title)
            .expect("every breakdown is built")
    }

    /// An empty chart reads as a thing that failed to load, and the page already
    /// says "no items" right beneath it.
    #[test]
    fn an_empty_library_composes_to_nothing_rather_than_empty_bars() {
        assert!(compose(&[]).is_none());
    }

    /// The question the panel exists to answer: a library that is quietly mostly
    /// 720p should say so.
    #[test]
    fn a_library_is_bucketed_by_what_its_files_actually_are() {
        let items = vec![
            item(
                MediaSource::Sonarr,
                ShareState::Seeding,
                100,
                Some(video("1920x1080")),
            ),
            item(
                MediaSource::Sonarr,
                ShareState::Seeding,
                300,
                Some(video("1920x1080")),
            ),
            item(
                MediaSource::Radarr,
                ShareState::Seeding,
                600,
                Some(video("1280x720")),
            ),
        ];
        let composition = compose(&items).unwrap();
        assert_eq!(composition.items, 3);

        let format = find(&composition, "Format");
        // Biggest share first, not most rows first: 720p is one file of three and
        // still 60% of the bytes.
        let labels: Vec<&str> = format.rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, vec!["720p", "1080p"]);
        assert_eq!(format.rows[0].share, "60%");
        assert_eq!(format.rows[0].count, 1);
        assert_eq!(format.rows[1].share, "40%");
        assert_eq!(format.rows[1].count, 2);
    }

    /// Music has no resolution, and the audio format is what a friend's Lidarr
    /// filters on — so it is what the bucket is named after.
    #[test]
    fn music_buckets_by_audio_format_and_unknown_files_sort_last() {
        let flac = Some(MediaMeta {
            audio_codec: Some("FLAC".to_owned()),
            ..MediaMeta::default()
        });
        let items = vec![
            item(MediaSource::Lidarr, ShareState::Seeding, 10, flac),
            // No metadata at all: the *arr never analysed it and no probe covered
            // it. That is worth showing, and worth showing last.
            item(MediaSource::Directory, ShareState::Seeding, 900, None),
            item(
                MediaSource::Sonarr,
                ShareState::Seeding,
                90,
                Some(video("1920x1080")),
            ),
        ];
        let format = compose(&items).unwrap();
        let format = find(&format, "Format");

        let labels: Vec<&str> = format.rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(
            labels,
            vec!["1080p", "FLAC", "Unknown"],
            "`unknown` is the absence of an answer, not the biggest one"
        );
    }

    /// The bar has to end flush at its own width, or it renders as a stacked bar
    /// with a gap on the right that means nothing.
    #[test]
    fn segments_tile_the_full_width_without_a_gap_or_an_overhang() {
        let items = vec![
            item(
                MediaSource::Sonarr,
                ShareState::Seeding,
                1_000_000,
                Some(video("1920x1080")),
            ),
            item(
                MediaSource::Radarr,
                ShareState::Seeding,
                3,
                Some(video("1280x720")),
            ),
            item(
                MediaSource::Lidarr,
                ShareState::Seeding,
                7,
                Some(video("720x480")),
            ),
        ];
        let composition = compose(&items).unwrap();

        for breakdown in &composition.breakdowns {
            let mut x = 0;
            for segment in &breakdown.segments {
                assert_eq!(segment.x, x, "{} has a gap", breakdown.title);
                assert!(
                    segment.w >= MIN_SEG_W,
                    "{}: a rounding-error slice must still be visible",
                    breakdown.title
                );
                x += segment.w;
            }
            assert_eq!(x, breakdown.width, "{} does not end flush", breakdown.title);
            assert_eq!(breakdown.segments.len(), breakdown.rows.len());
        }
    }

    /// A table whose shares add to 101% reads as broken arithmetic. These three
    /// are 98.4%, 1.5% and 0.8% exactly — independent rounding prints 98/2/1.
    #[test]
    fn printed_shares_add_up_to_a_hundred() {
        let items = vec![
            item(MediaSource::Sonarr, ShareState::Seeding, 984, None),
            item(MediaSource::Radarr, ShareState::Seeding, 15, None),
            item(MediaSource::Lidarr, ShareState::Seeding, 8, None),
        ];
        let composition = compose(&items).unwrap();
        let source = find(&composition, "Source");

        let shares: Vec<&str> = source.rows.iter().map(|r| r.share.as_str()).collect();
        assert_eq!(shares, vec!["98%", "1%", "1%"]);

        let total: u32 = source
            .rows
            .iter()
            .map(|r| r.share.trim_end_matches('%').parse::<u32>().unwrap_or(0))
            .sum();
        assert_eq!(total, 100);
    }

    /// A bucket that rounds away still holds files, and `0%` beside a non-zero
    /// count reads as a bug rather than as rounding.
    #[test]
    fn a_bucket_too_small_to_round_to_a_point_says_so() {
        let items = vec![
            item(MediaSource::Sonarr, ShareState::Seeding, 100_000, None),
            item(MediaSource::Radarr, ShareState::Seeding, 1, None),
        ];
        let composition = compose(&items).unwrap();
        let source = find(&composition, "Source");

        assert_eq!(source.rows[0].share, "100%");
        assert_eq!(source.rows[1].share, "<1%");
    }

    /// Every bucket keeps the colour its legend row shows, or the key beside the
    /// table points at the wrong slice.
    #[test]
    fn a_rows_accent_matches_its_segments() {
        let items = vec![
            item(
                MediaSource::Sonarr,
                ShareState::Seeding,
                100,
                Some(video("1920x1080")),
            ),
            item(
                MediaSource::Radarr,
                ShareState::Failed,
                200,
                Some(video("1280x720")),
            ),
        ];
        let composition = compose(&items).unwrap();

        for breakdown in &composition.breakdowns {
            for (segment, row) in breakdown.segments.iter().zip(&breakdown.rows) {
                assert_eq!(segment.accent, row.accent, "{}", breakdown.title);
            }
        }
    }

    /// State and source are the other two questions, and both are counted over
    /// every row rather than only the seeding ones.
    #[test]
    fn state_and_source_count_the_whole_library() {
        let items = vec![
            item(MediaSource::Sonarr, ShareState::Seeding, 100, None),
            item(MediaSource::Sonarr, ShareState::Failed, 100, None),
            item(MediaSource::Radarr, ShareState::Failed, 100, None),
        ];
        let composition = compose(&items).unwrap();

        let state = find(&composition, "State");
        let failed = state
            .rows
            .iter()
            .find(|r| r.label == "Failed")
            .expect("a failed row");
        assert_eq!(failed.count, 2);

        let source = find(&composition, "Source");
        assert_eq!(source.rows.iter().map(|r| r.count).sum::<usize>(), 3);
    }
}
