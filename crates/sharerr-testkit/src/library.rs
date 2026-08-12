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
pub const TAG_ID: i64 = 3;
/// The prefix the *arr apps report — deliberately different from where the files
/// really are, so every test exercises path mapping rather than assuming identity.
pub const ARR_TV_PREFIX: &str = "/tv";
pub const ARR_MOVIE_PREFIX: &str = "/movies";

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

#[derive(Debug, Clone)]
pub struct TvLibrary {
    pub root: PathBuf,
    pub files: Vec<MediaFile>,
}

#[derive(Debug, Clone)]
pub struct MovieLibrary {
    pub root: PathBuf,
    pub files: Vec<MediaFile>,
}

/// Two tagged episodes of one series, plus an untagged series that must never
/// appear in discovery.
pub fn tv_library(root: &Path) -> std::io::Result<TvLibrary> {
    let relative = [
        (
            501i64,
            "Lanternwick Hollow/Season 02/lanternwick.s02e01.mkv",
            Some("Lanternwick.Hollow.S02E01.1080p.WEB-DL.DD5.1.H.264-FAKEGRP"),
        ),
        // No scene name: forces the release title through basename resolution.
        (
            502,
            "Lanternwick Hollow/Season 02/lanternwick.s02e02.mkv",
            None,
        ),
    ];

    let mut files = Vec::new();
    for (index, (file_id, suffix, scene_name)) in relative.into_iter().enumerate() {
        let disk_path = root.join("tv").join(suffix);
        write_media_file(&disk_path, FILE_SIZE, 1000 + index as u64)?;

        files.push(MediaFile {
            file_id,
            source_id: 11,
            arr_path: format!("{ARR_TV_PREFIX}/{suffix}"),
            disk_path,
            size: FILE_SIZE as u64,
            scene_name: scene_name.map(str::to_owned),
        });
    }

    Ok(TvLibrary {
        root: root.to_path_buf(),
        files,
    })
}

/// One tagged movie with a file, one untagged movie, and one tagged movie that has
/// not been downloaded yet.
pub fn movie_library(root: &Path) -> std::io::Result<MovieLibrary> {
    let suffix = "The Gilded Ferry (2019)/gilded.ferry.2019.mkv";
    let disk_path = root.join("movies").join(suffix);
    write_media_file(&disk_path, FILE_SIZE, 2000)?;

    Ok(MovieLibrary {
        root: root.to_path_buf(),
        files: vec![MediaFile {
            file_id: 900,
            source_id: 31,
            arr_path: format!("{ARR_MOVIE_PREFIX}/{suffix}"),
            disk_path,
            size: FILE_SIZE as u64,
            scene_name: Some("The.Gilded.Ferry.2019.1080p.BluRay.x264-FAKEGRP".to_owned()),
        }],
    })
}

/// `GET /api/v3/tag`, shared by both apps. Includes a decoy.
pub fn tag_json() -> Value {
    json!([
        { "id": 1, "label": "anime" },
        { "id": TAG_ID, "label": "sharerr" },
    ])
}

pub fn system_status_json(app: &str) -> Value {
    json!({ "appName": app, "version": "4.0.15.2941", "instanceName": app })
}

impl TvLibrary {
    pub fn series_json(&self) -> Value {
        json!([
            {
                "id": 11,
                "title": "Lanternwick Hollow",
                "tvdbId": 918273,
                "tvMazeId": 4242,
                "imdbId": "tt7654321",
                "tags": [TAG_ID],
            },
            {
                // Untagged: discovery must never return this.
                "id": 12,
                "title": "Copper Vale Station",
                "tvdbId": 112233,
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
        json!([
            { "seasonNumber": 2, "episodeNumber": 1, "episodeFileId": 501 },
            { "seasonNumber": 2, "episodeNumber": 2, "episodeFileId": 502 },
            // Aired but not downloaded — no file to share.
            { "seasonNumber": 2, "episodeNumber": 3, "episodeFileId": 0 },
        ])
    }

    /// Look a fixture up by its *arr file id.
    pub fn file(&self, file_id: i64) -> &MediaFile {
        self.files
            .iter()
            .find(|f| f.file_id == file_id)
            .unwrap_or_else(|| panic!("no fixture with file_id {file_id}"))
    }
}

impl MovieLibrary {
    pub fn movie_json(&self) -> Value {
        let file = &self.files[0];
        json!([
            {
                "id": 31,
                "title": "The Gilded Ferry",
                "year": 2019,
                "tmdbId": 555444,
                "imdbId": "tt1234567",
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
}
