//! Lidarr discovery: which music files carry the sharerr tag.
//!
//! # How this differs from Sonarr
//!
//! Lidarr's shape is the same *kind* of thing — tagged entities, then their files —
//! but two details differ and both change what a release can be called:
//!
//! * **Tags live on the artist**, not the album. Tagging an artist therefore shares
//!   their whole discography, the same surprise Sonarr's series-level tags produce.
//! * **The unit of import is a `trackFile`**, which is usually one track but is
//!   sometimes a whole album in one file. The album is what a friend's Lidarr
//!   searches for, so it is what gets named; the track number is carried when there
//!   is one and omitted when the file is the album.
//!
//! Lidarr is also on API `v1` rather than `v3`.

use serde::Deserialize;
use sharerr_core::{ExternalIds, MediaSource, MediaSpec};

use crate::client::ArrClient;
use crate::error::Result;
use crate::models::{MediaInfo, non_empty};
use crate::{Discovered, Tagged, fetch_tagged};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Artist {
    id: i64,
    #[serde(default)]
    artist_name: String,
    #[serde(default)]
    tags: Vec<i64>,
    #[serde(default)]
    foreign_artist_id: Option<String>,
}

impl Tagged for Artist {
    fn tags(&self) -> &[i64] {
        &self.tags
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Album {
    id: i64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    foreign_album_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TrackFile {
    id: i64,
    #[serde(default)]
    album_id: i64,
    #[serde(default)]
    path: String,
    #[serde(default)]
    size: u64,
    /// Present when the file was imported from a scene release.
    #[serde(default)]
    scene_name: Option<String>,
    #[serde(default)]
    media_info: Option<MediaInfo>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Track {
    #[serde(default)]
    track_file_id: i64,
    #[serde(default)]
    absolute_track_number: Option<u32>,
}

/// What one tagged artist needs fetching for it.
type ArtistPayload = (Vec<Album>, Vec<TrackFile>, Vec<Track>);

async fn fetch_artist(client: &ArrClient, artist: &Artist) -> Result<ArtistPayload> {
    // Independent lookups, so they run concurrently — per tagged artist this
    // costs one round trip's latency instead of three. The track list carries
    // numbers so a single-track file can be named precisely; a file no track
    // points at is still shareable — it is simply the whole album.
    let by_artist = [("artistId", artist.id)];
    let (albums, files, tracks) = tokio::try_join!(
        client.get::<Vec<Album>, _>("album", &by_artist),
        client.get::<Vec<TrackFile>, _>("trackfile", &by_artist),
        client.get::<Vec<Track>, _>("track", &by_artist),
    )?;
    Ok((albums, files, tracks))
}

pub(crate) async fn discover(client: &ArrClient, tag_id: i64) -> Result<Vec<Discovered>> {
    let artists: Vec<Artist> = client.get("artist", &()).await?;
    // Concurrent across artists as well as within one — see `fetch_tagged`.
    let fetched = fetch_tagged(client, &artists, tag_id, "lidarr artists", fetch_artist).await?;

    let mut discovered = Vec::new();
    for (artist, (albums, files, tracks)) in fetched {
        if files.is_empty() {
            tracing::debug!(artist = %artist.artist_name, "tagged but has no files on disk");
            continue;
        }

        // Indexed once, then each file's lookups are O(1) — a 400-track
        // discography was ~160k linear-scan comparisons per artist without this.
        let album_by_id: std::collections::HashMap<i64, &Album> =
            albums.iter().map(|a| (a.id, a)).collect();
        let numbers_by_file = numbers_by_file(&tracks);

        for file in files {
            let Some(album) = album_by_id.get(&file.album_id).copied() else {
                // A file whose album Lidarr no longer lists. Sharing it would
                // produce a release named after nothing.
                tracing::warn!(
                    artist = %artist.artist_name,
                    file_id = file.id,
                    "track file belongs to no listed album; skipping"
                );
                continue;
            };

            let track = track_number(numbers_by_file.get(&file.id).map(Vec::as_slice));

            discovered.push(Discovered {
                source: MediaSource::Lidarr,
                source_id: artist.id,
                file_id: file.id,
                spec: MediaSpec::Track {
                    artist: artist.artist_name.clone(),
                    album: album.title.clone(),
                    track,
                },
                arr_path: file.path.clone().into(),
                size: file.size,
                ids: ExternalIds {
                    // Lidarr's foreign ids are MusicBrainz. The album's is the one a
                    // friend's Lidarr matches on; the artist's is the fallback.
                    musicbrainz: non_empty(album.foreign_album_id.clone())
                        .or_else(|| non_empty(artist.foreign_artist_id.clone())),
                    ..ExternalIds::default()
                },
                scene_name: non_empty(file.scene_name.clone()),
                // Lidarr and Readarr report no pre-rename path.
                original_path: None,
                // Lidarr has already analysed the file, so this costs nothing:
                // it arrives in the same JSON as everything else above.
                media: file.media_info.and_then(MediaInfo::into_meta),
            });
        }
    }

    Ok(discovered)
}

/// Group each file's track numbers by the file they belong to.
///
/// Tracks with no absolute number contribute nothing — the file is still
/// shareable, it simply cannot be named after one track.
fn numbers_by_file(tracks: &[Track]) -> std::collections::HashMap<i64, Vec<u32>> {
    let mut by_file: std::collections::HashMap<i64, Vec<u32>> = std::collections::HashMap::new();
    for track in tracks {
        if let Some(number) = track.absolute_track_number {
            by_file.entry(track.track_file_id).or_default().push(number);
        }
    }
    by_file
}

/// The track number to name a file after, if any.
///
/// Exactly one track pointing at this file means the file *is* that track. More
/// than one means it holds several, so the album is the only honest name for it —
/// and so does none, which is a file no track claims.
fn track_number(numbers: Option<&[u32]>) -> Option<u32> {
    match numbers {
        Some([number]) => Some(*number),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn track(file_id: i64, number: Option<u32>) -> Track {
        Track {
            track_file_id: file_id,
            absolute_track_number: number,
        }
    }

    /// A file holding one track is named with that track's number.
    #[test]
    fn a_single_track_file_keeps_its_number() {
        let by_file = numbers_by_file(&[track(7, Some(3))]);
        assert_eq!(track_number(by_file.get(&7).map(Vec::as_slice)), Some(3));
    }

    /// A file several tracks point at is a whole album in one file, and naming it
    /// after any one of those tracks would be wrong.
    #[test]
    fn a_file_holding_several_tracks_is_named_as_an_album() {
        let by_file = numbers_by_file(&[track(7, Some(1)), track(7, Some(2))]);
        assert_eq!(track_number(by_file.get(&7).map(Vec::as_slice)), None);
    }

    /// A track with no absolute number contributes nothing, so a file made up
    /// only of such tracks is named as an album rather than after a phantom.
    #[test]
    fn a_file_whose_tracks_are_unnumbered_is_named_as_an_album() {
        let by_file = numbers_by_file(&[track(7, None)]);
        assert_eq!(track_number(by_file.get(&7).map(Vec::as_slice)), None);
    }
}
