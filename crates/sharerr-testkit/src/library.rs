//! A synthetic media library, plus the *arr API responses that describe it.
//!
//! Keeping the files and the JSON in one place is deliberate: an end-to-end test
//! needs both to agree, and letting them drift apart produces failures that look
//! like path-mapping bugs.
//!
//! Every title is invented. Every release group is `FAKEGRP`.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::media::write_media_file;

/// The tag id the fixtures mark shareable content with.
///
/// A mock-server detail, not a fact about the tag. Anything writing to a *real*
/// Sonarr or Radarr must let it assign the id and resolve [`TAG_LABEL`] instead —
/// which is what sharerr itself does.
pub const TAG_ID: i64 = 3;
/// The tag label sharerr is configured to watch. This one *is* a fact: it is
/// matched case-insensitively against what the *arr apps report.
pub const TAG_LABEL: &str = "sharerr";
/// The prefix the *arr apps report — deliberately different from where the files
/// really are, so every test exercises path mapping rather than assuming identity.
pub const ARR_TV_PREFIX: &str = "/tv";
pub const ARR_MOVIE_PREFIX: &str = "/movies";
pub const ARR_MUSIC_PREFIX: &str = "/music";

/// The tagged series, as the *arr apps record it.
///
/// Constants rather than literals inside [`Library::series_json`] because the
/// compose stack seeds these same values into Sonarr's own database, and the two
/// descriptions of one series drifting apart is exactly the failure this module
/// exists to prevent.
pub const SERIES_ID: i64 = 11;
pub const SERIES_TITLE: &str = "Lanternwick Hollow";
pub const SERIES_FOLDER: &str = "Lanternwick Hollow";
pub const SERIES_TVDB_ID: i64 = 918_273;
pub const SERIES_TVMAZE_ID: i64 = 4242;
pub const SERIES_IMDB_ID: &str = "tt7654321";

/// Season, episode, and the file id it points at. `0` means aired but not
/// downloaded — an episode with nothing to share.
pub const EPISODES: [(i64, i64, i64); 3] = [(2, 1, 501), (2, 2, 502), (2, 3, 0)];

/// The tagged movie, as Radarr records it.
pub const MOVIE_ID: i64 = 31;
pub const MOVIE_TITLE: &str = "The Gilded Ferry";
pub const MOVIE_FOLDER: &str = "The Gilded Ferry (2019)";
pub const MOVIE_YEAR: i64 = 2019;
pub const MOVIE_TMDB_ID: i64 = 555_444;
pub const MOVIE_IMDB_ID: &str = "tt1234567";

/// The tagged artist, as Lidarr records it. Lidarr tags the *artist*, not the
/// album, so one tagged artist shares their whole discography — the same
/// surprise Sonarr's series-level tags produce.
pub const ARTIST_ID: i64 = 51;
pub const ARTIST_NAME: &str = "Marigold Static";
pub const ARTIST_FOLDER: &str = "Marigold Static";
pub const ARTIST_FOREIGN_ID: &str = "3f6b1a2e-9c4d-4e7a-8f1b-2d5c6e7a8b9c";

pub const ALBUM_ID: i64 = 71;
pub const ALBUM_TITLE: &str = "Copper Wire Choir";
pub const ALBUM_FOREIGN_ID: &str = "7a8b9c1d-2e3f-4a5b-6c7d-8e9f0a1b2c3d";
pub const ALBUM_RELEASE_FOREIGN_ID: &str = "1c2d3e4f-5a6b-7c8d-9e0f-1a2b3c4d5e6f";

/// The two track files.
pub const TRACK_FILE_IDS: [i64; 2] = [601, 602];

/// Track id, the file it belongs to, absolute track number, and the foreign
/// (MusicBrainz-shaped) track id.
///
/// File 601 has exactly one track pointing at it and is named after that track.
/// File 602 has *two* tracks pointing at it — real Lidarr's actual shape for "this
/// file is the whole album, not one track": confirmed against a live container
/// that a `TrackFile` with *zero* `Tracks` rows is invisible to Lidarr's own
/// artist/album-filtered `trackfile` API (it inner-joins through `Tracks`), so
/// "no track claims this file" is not a real state to seed — the file simply
/// would not exist as far as Lidarr's API is concerned. Two tracks sharing one
/// `TrackFileId` is what actually falls back to the album name — see
/// `sharerr_arr::lidarr::track_number`.
pub const TRACKS: [(i64, i64, u32, &str); 3] = [
    (6000, 601, 1, "9d8c7b6a-5e4f-3d2c-1b0a-9f8e7d6c5b4a"),
    (6001, 602, 2, "5b4a3c2d-1e0f-9a8b-7c6d-5e4f3a2b1c0d"),
    (6002, 602, 3, "6e5d4c3b-2a1f-0e9d-8c7b-6a5f4e3d2c1b"),
];

/// Big enough to span several pieces at the 256 KiB floor, small enough to be free.
const FILE_SIZE: usize = 768 * 1024;

#[derive(Debug, Clone)]
pub struct MediaFile {
    /// `episodeFile` / `movieFile` id.
    pub file_id: i64,
    /// Series or movie id.
    pub source_id: i64,
    /// Path as the *arr app reports it.
    pub arr_path: String,
    /// Path as sharerr sees it — where the bytes actually are.
    pub disk_path: PathBuf,
    pub size: u64,
    pub scene_name: Option<String>,
}

/// A written-to-disk fixture library: TV, movie, or music, all the same
/// shape — a root directory and the files under it. Which kind it is lives
/// in which of [`tv_library`]/[`movie_library`]/[`music_library`] built it,
/// not in the type; the `*_json` methods below are grouped by the kind of
/// fixture they describe, not enforced apart by the type system.
#[derive(Debug, Clone)]
pub struct Library {
    pub root: PathBuf,
    pub files: Vec<MediaFile>,
}

/// The TV fixtures as records, without touching the disk.
///
/// Split out from [`tv_library`] for the compose stack's database seeder, which
/// needs to know what the files *are* but must not write them — `gen-fixtures`
/// owns that, and a second writer would rewrite the library out from under the
/// e2e test that asserts a sync never touches it.
///
/// The seed is the index, so order here is load-bearing: reordering these entries
/// changes the bytes on disk, and with them every recorded info hash.
pub fn tv_files(root: &Path) -> Vec<MediaFile> {
    let relative = [
        (
            501i64,
            "Season 02/lanternwick.s02e01.mkv",
            Some("Lanternwick.Hollow.S02E01.1080p.WEB-DL.DD5.1.H.264-FAKEGRP"),
        ),
        // No scene name: forces the release title through basename resolution.
        (502, "Season 02/lanternwick.s02e02.mkv", None),
    ];

    relative
        .into_iter()
        .map(|(file_id, tail, scene_name)| {
            let suffix = format!("{SERIES_FOLDER}/{tail}");
            MediaFile {
                file_id,
                source_id: SERIES_ID,
                arr_path: format!("{ARR_TV_PREFIX}/{suffix}"),
                disk_path: root.join("tv").join(&suffix),
                size: FILE_SIZE as u64,
                scene_name: scene_name.map(str::to_owned),
            }
        })
        .collect()
}

/// The movie fixtures as records, without touching the disk. See [`tv_files`].
pub fn movie_files(root: &Path) -> Vec<MediaFile> {
    let suffix = format!("{MOVIE_FOLDER}/gilded.ferry.2019.mkv");
    vec![MediaFile {
        file_id: 900,
        source_id: MOVIE_ID,
        arr_path: format!("{ARR_MOVIE_PREFIX}/{suffix}"),
        disk_path: root.join("movies").join(&suffix),
        size: FILE_SIZE as u64,
        scene_name: Some("The.Gilded.Ferry.2019.1080p.BluRay.x264-FAKEGRP".to_owned()),
    }]
}

/// The music fixtures as records, without touching the disk. See [`tv_files`].
///
/// Two track files on one tagged album: one with a resolvable track number (see
/// [`TRACKS`]), one where two tracks share the file — the file is the whole
/// album rather than one track, and still shareable as such.
pub fn music_files(root: &Path) -> Vec<MediaFile> {
    let relative = [
        (
            TRACK_FILE_IDS[0],
            "01 copper wire choir.flac",
            Some("Marigold.Static-Copper.Wire.Choir-01-FAKEGRP"),
        ),
        (TRACK_FILE_IDS[1], "02 copper wire choir.flac", None),
    ];

    relative
        .into_iter()
        .map(|(file_id, tail, scene_name)| {
            let suffix = format!("{ARTIST_FOLDER}/{ALBUM_TITLE}/{tail}");
            MediaFile {
                file_id,
                source_id: ARTIST_ID,
                arr_path: format!("{ARR_MUSIC_PREFIX}/{suffix}"),
                disk_path: root.join("music").join(&suffix),
                size: FILE_SIZE as u64,
                scene_name: scene_name.map(str::to_owned),
            }
        })
        .collect()
}

/// Write `files` to disk and wrap them as a [`Library`] rooted at `root`.
///
/// `base_seed` is the deterministic-content seed the first file gets; every
/// later file gets `base_seed` plus its index, so the three library kinds —
/// seeded from 1000/2000/3000 respectively — can never produce the same
/// bytes for two different files even if a library kind grows past one file.
fn write_library(root: &Path, files: Vec<MediaFile>, base_seed: u64) -> std::io::Result<Library> {
    for (index, file) in files.iter().enumerate() {
        write_media_file(&file.disk_path, FILE_SIZE, base_seed + index as u64)?;
    }

    Ok(Library {
        root: root.to_path_buf(),
        files,
    })
}

/// Two tagged episodes of one series, plus an untagged series that must never
/// appear in discovery. Writes the files.
pub fn tv_library(root: &Path) -> std::io::Result<Library> {
    write_library(root, tv_files(root), 1000)
}

/// One tagged movie with a file, one untagged movie, and one tagged movie that has
/// not been downloaded yet. Writes the files.
pub fn movie_library(root: &Path) -> std::io::Result<Library> {
    write_library(root, movie_files(root), 2000)
}

/// One tagged artist with two track files on one album. Writes the files.
pub fn music_library(root: &Path) -> std::io::Result<Library> {
    write_library(root, music_files(root), 3000)
}

/// `GET /api/v3/tag`, shared by both apps. Includes a decoy.
pub fn tag_json() -> Value {
    json!([
        { "id": 1, "label": "anime" },
        { "id": TAG_ID, "label": TAG_LABEL },
    ])
}

pub fn system_status_json(app: &str) -> Value {
    json!({ "appName": app, "version": "4.0.15.2941", "instanceName": app })
}

impl Library {
    /// `GET /api/v3/series`: the tagged series plus an untagged decoy.
    pub fn series_json(&self) -> Value {
        json!([
            {
                "id": SERIES_ID,
                "title": SERIES_TITLE,
                "tvdbId": SERIES_TVDB_ID,
                "tvMazeId": SERIES_TVMAZE_ID,
                "imdbId": SERIES_IMDB_ID,
                "tags": [TAG_ID],
            },
            {
                // Untagged: discovery must never return this.
                "id": 12,
                "title": "Copper Vale Station",
                "tvdbId": 112_233,
                "tags": [1],
            },
        ])
    }

    pub fn episodefile_json(&self) -> Value {
        Value::Array(
            self.files
                .iter()
                .map(|f| {
                    let mut entry = json!({
                        "id": f.file_id,
                        "path": f.arr_path,
                        "size": f.size,
                    });
                    if let Some(scene) = &f.scene_name {
                        entry["sceneName"] = json!(scene);
                    }
                    entry
                })
                .collect(),
        )
    }

    pub fn episode_json(&self) -> Value {
        Value::Array(
            EPISODES
                .iter()
                .map(|&(season, episode, file_id)| {
                    json!({
                        "seasonNumber": season,
                        "episodeNumber": episode,
                        "episodeFileId": file_id,
                    })
                })
                .collect(),
        )
    }

    /// Look a fixture up by its *arr file id.
    pub fn file(&self, file_id: i64) -> &MediaFile {
        self.files
            .iter()
            .find(|f| f.file_id == file_id)
            .unwrap_or_else(|| panic!("no fixture with file_id {file_id}"))
    }

    pub fn movie_json(&self) -> Value {
        let file = &self.files[0];
        json!([
            {
                "id": MOVIE_ID,
                "title": MOVIE_TITLE,
                "year": MOVIE_YEAR,
                "tmdbId": MOVIE_TMDB_ID,
                "imdbId": MOVIE_IMDB_ID,
                "tags": [TAG_ID],
                "hasFile": true,
                "movieFile": {
                    "id": file.file_id,
                    "path": file.arr_path,
                    "size": file.size,
                    "sceneName": file.scene_name,
                },
            },
            { "id": 32, "title": "Paper Lantern Sky", "year": 2021, "tags": [1], "hasFile": false },
            // Tagged, but nothing downloaded yet.
            { "id": 33, "title": "Harrowmere", "year": 2024, "tags": [TAG_ID], "hasFile": false },
        ])
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn the_tv_library_lands_on_disk_where_it_says_it_does() {
        let dir = tempfile::tempdir().unwrap();
        let library = tv_library(dir.path()).unwrap();

        assert_eq!(library.files.len(), 2);
        for file in &library.files {
            assert!(
                file.disk_path.exists(),
                "{} was not written",
                file.disk_path.display()
            );
            assert_eq!(std::fs::metadata(&file.disk_path).unwrap().len(), file.size);
            // The arr view must differ from the disk view, or path mapping goes
            // untested everywhere these fixtures are used.
            assert!(file.arr_path.starts_with(ARR_TV_PREFIX));
            assert_ne!(Path::new(&file.arr_path), file.disk_path);
        }
    }

    #[test]
    fn the_json_matches_the_files_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let library = tv_library(dir.path()).unwrap();

        let files = library.episodefile_json();
        let entries = files.as_array().unwrap();
        assert_eq!(entries.len(), library.files.len());

        for (entry, fixture) in entries.iter().zip(&library.files) {
            assert_eq!(entry["id"].as_i64().unwrap(), fixture.file_id);
            assert_eq!(entry["path"].as_str().unwrap(), fixture.arr_path);
            assert_eq!(entry["size"].as_u64().unwrap(), fixture.size);
        }
    }

    #[test]
    fn fixture_content_is_reproducible_across_runs() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();

        let a = tv_library(first.path()).unwrap();
        let b = tv_library(second.path()).unwrap();

        for (x, y) in a.files.iter().zip(&b.files) {
            assert_eq!(
                std::fs::read(&x.disk_path).unwrap(),
                std::fs::read(&y.disk_path).unwrap()
            );
        }
    }

    #[test]
    fn the_movie_library_lands_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let library = movie_library(dir.path()).unwrap();

        assert_eq!(library.files.len(), 1);
        assert!(library.files[0].disk_path.exists());
        assert_eq!(library.movie_json().as_array().unwrap().len(), 3);
    }

    #[test]
    fn the_music_library_lands_on_disk_where_it_says_it_does() {
        let dir = tempfile::tempdir().unwrap();
        let library = music_library(dir.path()).unwrap();

        assert_eq!(library.files.len(), 2);
        for file in &library.files {
            assert!(
                file.disk_path.exists(),
                "{} was not written",
                file.disk_path.display()
            );
            assert_eq!(std::fs::metadata(&file.disk_path).unwrap().len(), file.size);
            assert!(file.arr_path.starts_with(ARR_MUSIC_PREFIX));
            assert_ne!(Path::new(&file.arr_path), file.disk_path);
        }
    }
}
