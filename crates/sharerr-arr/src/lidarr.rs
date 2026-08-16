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
//! Lidarr is also on API `v1` rather than `v3` — see [`MediaSource::api_version`].

use serde::Deserialize;
use sharerr_core::{ExternalIds, MediaSource, MediaSpec};

use crate::Discovered;
use crate::client::ArrClient;
use crate::error::Result;
use crate::models::non_empty;

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
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Track {
    #[serde(default)]
    track_file_id: i64,
    #[serde(default)]
    absolute_track_number: Option<u32>,
}

pub(crate) async fn discover(client: &ArrClient, tag_id: i64) -> Result<Vec<Discovered>> {
    let artists: Vec<Artist> = client.get_list("artist", &[]).await?;
    let tagged: Vec<&Artist> = artists
        .iter()
        .filter(|a| a.tags.contains(&tag_id))
        .collect();

    tracing::debug!(
        total = artists.len(),
        tagged = tagged.len(),
        "lidarr artists scanned for the sharerr tag"
    );

    let mut discovered = Vec::new();
    for artist in tagged {
        let artist_id = artist.id.to_string();

        let albums: Vec<Album> = client
            .get_list("album", &[("artistId", artist_id.clone())])
            .await?;
        let files: Vec<TrackFile> = client
            .get_list("trackfile", &[("artistId", artist_id.clone())])
            .await?;
        if files.is_empty() {
            tracing::debug!(artist = %artist.artist_name, "tagged but has no files on disk");
            continue;
        }

        // Track numbers, so a single-track file can be named precisely. A file that
        // no track points at is still shareable — it is simply the whole album.
        let tracks: Vec<Track> = client.get_list("track", &[("artistId", artist_id)]).await?;

        for file in files {
            let Some(album) = albums.iter().find(|a| a.id == file.album_id) else {
                // A file whose album Lidarr no longer lists. Sharing it would
                // produce a release named after nothing.
                tracing::warn!(
                    artist = %artist.artist_name,
                    file_id = file.id,
                    "track file belongs to no listed album; skipping"
                );
                continue;
            };

            // Exactly one track pointing at this file means the file *is* that
            // track. More than one means it holds several, so the album is the only
            // honest name for it.
            let numbered: Vec<u32> = tracks
                .iter()
                .filter(|t| t.track_file_id == file.id)
                .filter_map(|t| t.absolute_track_number)
                .collect();
            let track = if numbered.len() == 1 {
                numbered.first().copied()
            } else {
                None
            };

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
            });
        }
    }

    Ok(discovered)
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
        let tracks = [track(7, Some(3))];
        let numbered: Vec<u32> = tracks
            .iter()
            .filter(|t| t.track_file_id == 7)
            .filter_map(|t| t.absolute_track_number)
            .collect();
        assert_eq!(numbered.len(), 1);
    }

    /// A file several tracks point at is a whole album in one file, and naming it
    /// after any one of those tracks would be wrong.
    #[test]
    fn a_file_holding_several_tracks_is_named_as_an_album() {
        let tracks = [track(7, Some(1)), track(7, Some(2))];
        let numbered: Vec<u32> = tracks
            .iter()
            .filter(|t| t.track_file_id == 7)
            .filter_map(|t| t.absolute_track_number)
            .collect();
        assert!(numbered.len() > 1, "more than one track claims this file");
    }
}
