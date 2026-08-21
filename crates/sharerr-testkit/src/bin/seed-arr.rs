//! Write the tagged fixture library straight into Sonarr's and Radarr's databases.
//!
//! ```text
//! cargo run -p sharerr-testkit --bin seed-arr -- \
//!     --sonarr docker/state/sonarr/sonarr.db \
//!     --radarr docker/state/radarr/radarr.db
//! ```
//!
//! # Why not the API
//!
//! `POST /api/v3/series` triggers a metadata lookup against `services.sonarr.tv`,
//! and `POST /api/v3/movie` one against `api.radarr.video`. The compose stack's
//! network is `internal: true` precisely so nothing can reach either, and even with
//! egress the lookup would fail: every fixture title is invented, so there is
//! nothing out there to find. Writing the rows directly is the only way to get
//! tagged content while keeping the stack sealed.
//!
//! # Two consequences
//!
//! **Sonarr and Radarr must be stopped.** Both hold their SQLite database open and
//! will not observe an external write while running.
//!
//! **This is coupled to their schemas**, which is why `compose.test.yml` pins
//! image tags rather than using `:latest`. Only the columns sharerr's four read
//! endpoints need are populated; the rows are deliberately minimal and would not
//! satisfy a metadata refresh.
//!
//! The tag id is *not* fixed. `sharerr_testkit::TAG_ID` is a mock-server detail —
//! against a real app sharerr resolves the label, case-insensitively, so whatever
//! SQLite assigns is correct.

#![allow(clippy::print_stdout)]

use std::path::PathBuf;
use std::process::ExitCode;

use sharerr_testkit::library::{
    ARR_MOVIE_PREFIX, ARR_TV_PREFIX, EPISODES, MOVIE_FOLDER, MOVIE_ID, MOVIE_IMDB_ID, MOVIE_TITLE,
    MOVIE_TMDB_ID, MOVIE_YEAR, MediaFile, SERIES_FOLDER, SERIES_ID, SERIES_IMDB_ID, SERIES_TITLE,
    SERIES_TVDB_ID, SERIES_TVMAZE_ID, TAG_LABEL, movie_files, tv_files,
};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool};
use sqlx::{Executor, Row};

/// The *arr apps store timestamps as text. Any fixed instant will do — nothing
/// sharerr reads depends on it — but it must parse, or their API throws while
/// serialising the record.
const ADDED: &str = "2020-01-01T00:00:00Z";

/// Quality id 0 is "Unknown", which is honest: these files were never graded.
/// Sharerr does not read quality, and inventing a specific one would only be a
/// lie the UI displays.
///
/// `quality` is a bare id here, not the `{"id":0,...}` object the *API* returns —
/// the column goes through `QualityIntConverter`, which reads an integer and
/// throws on anything else. Getting this wrong makes `GET /episodefile` return a
/// 500 while every other endpoint looks fine.
const QUALITY: &str = r#"{"quality":0,"revision":{"version":1,"real":0,"isRepack":false}}"#;
/// Language id 1 is English. Ids, not objects, for the same reason as `QUALITY`.
const LANGUAGES: &str = "[1]";

/// Never a root folder the *arr app itself owns — these are the read-only mounts.
const TV_ROOT: &str = ARR_TV_PREFIX;
const MOVIE_ROOT: &str = ARR_MOVIE_PREFIX;

#[tokio::main]
async fn main() -> ExitCode {
    let (sonarr, radarr) = match parse_args() {
        Ok(paths) => paths,
        Err(message) => {
            eprintln!("{message}");
            eprintln!(
                "usage: seed-arr --sonarr <sonarr.db> --radarr <radarr.db>\n\
                 \n\
                 Both apps must be stopped: they hold these databases open and will\n\
                 not see an external write while running."
            );
            return ExitCode::FAILURE;
        }
    };

    if let Err(err) = run(&sonarr, &radarr).await {
        eprintln!("seeding failed: {err}");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}

fn parse_args() -> Result<(PathBuf, PathBuf), String> {
    let mut sonarr = None;
    let mut radarr = None;
    let mut args = std::env::args().skip(1);

    while let Some(flag) = args.next() {
        let target = match flag.as_str() {
            "--sonarr" => &mut sonarr,
            "--radarr" => &mut radarr,
            other => return Err(format!("unexpected argument {other:?}")),
        };
        let value = args.next().ok_or(format!("{flag} needs a path"))?;
        *target = Some(PathBuf::from(value));
    }

    match (sonarr, radarr) {
        (Some(sonarr), Some(radarr)) => Ok((sonarr, radarr)),
        _ => Err("both --sonarr and --radarr are required".to_owned()),
    }
}

async fn run(sonarr_db: &std::path::Path, radarr_db: &std::path::Path) -> anyhow::Result<()> {
    // The fixture root is irrelevant here — only `arr_path`, `size` and
    // `scene_name` are used, and those describe how the *arr app sees the file,
    // not where it is on this host.
    let unused_root = std::path::Path::new("/");
    let tv = tv_files(unused_root);
    let movies = movie_files(unused_root);

    let sonarr = open(sonarr_db).await?;
    let tag_id = ensure_tag(&sonarr).await?;
    seed_sonarr(&sonarr, tag_id, &tv).await?;
    sonarr.close().await;

    let radarr = open(radarr_db).await?;
    let tag_id = ensure_tag(&radarr).await?;
    seed_radarr(&radarr, tag_id, &movies).await?;
    radarr.close().await;

    println!(
        "tagged {:?}: {} episode file(s) on {SERIES_TITLE:?}, {} movie file(s) on {MOVIE_TITLE:?}",
        TAG_LABEL,
        tv.len(),
        movies.len()
    );
    println!("start sonarr and radarr again to pick this up");
    Ok(())
}

/// Open an existing database, refusing to create one.
///
/// A path typo would otherwise produce an empty file, a silent success, and a
/// `doctor` run that reports the tag missing for no visible reason.
async fn open(path: &std::path::Path) -> anyhow::Result<SqlitePool> {
    if !path.exists() {
        anyhow::bail!(
            "{} does not exist — bring the stack up once so the app creates it",
            path.display()
        );
    }

    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false);
    Ok(SqlitePool::connect_with(options).await?)
}

/// Create the tag if it is absent, and return whatever id it has.
async fn ensure_tag(db: &SqlitePool) -> anyhow::Result<i64> {
    db.execute(sqlx::query("INSERT OR IGNORE INTO Tags (Label) VALUES (?)").bind(TAG_LABEL))
        .await?;

    let row = sqlx::query("SELECT Id FROM Tags WHERE Label = ?")
        .bind(TAG_LABEL)
        .fetch_one(db)
        .await?;
    Ok(row.get::<i64, _>("Id"))
}

/// The relative path Sonarr and Radarr store, i.e. the part below the series or
/// movie folder. Their APIs report `path` as folder + this.
fn relative_path(arr_path: &str, folder_prefix: &str) -> String {
    arr_path
        .strip_prefix(folder_prefix)
        .unwrap_or(arr_path)
        .trim_start_matches('/')
        .to_owned()
}

async fn seed_sonarr(db: &SqlitePool, tag_id: i64, files: &[MediaFile]) -> anyhow::Result<()> {
    let series_path = format!("{TV_ROOT}/{SERIES_FOLDER}");
    let tags = format!("[{tag_id}]");
    let seasons = r#"[{"seasonNumber":2,"monitored":true}]"#;

    db.execute(sqlx::query("INSERT OR IGNORE INTO RootFolders (Path) VALUES (?)").bind(TV_ROOT))
        .await?;

    // `INSERT OR REPLACE` on a fixed id so re-running the script is a no-op rather
    // than a duplicate-key failure or a second copy of the series.
    sqlx::query(
        "INSERT OR REPLACE INTO Series (
             Id, TvdbId, TvRageId, TvMazeId, TmdbId, ImdbId, Title, TitleSlug, CleanTitle,
             SortTitle, Status, Overview, Images, Path, Monitored, SeasonFolder, Runtime,
             SeriesType, UseSceneNumbering, Year, Seasons, Ratings, Genres, QualityProfileId,
             Tags, Added, OriginalLanguage, MonitorNewItems, MalIds, AniListIds
         ) VALUES (
             ?, ?, 0, ?, 0, ?, ?, ?, ?,
             ?, 0, '', '[]', ?, 1, 1, 30,
             0, 0, 2019, ?, '{}', '[]', 1,
             ?, ?, 1, 0, '[]', '[]'
         )",
    )
    .bind(SERIES_ID)
    .bind(SERIES_TVDB_ID)
    .bind(SERIES_TVMAZE_ID)
    .bind(SERIES_IMDB_ID)
    .bind(SERIES_TITLE)
    .bind(slug(SERIES_TITLE))
    .bind(clean(SERIES_TITLE))
    .bind(SERIES_TITLE.to_lowercase())
    .bind(&series_path)
    .bind(seasons)
    .bind(&tags)
    .bind(ADDED)
    .execute(db)
    .await?;

    for file in files {
        sqlx::query(
            "INSERT OR REPLACE INTO EpisodeFiles (
                 Id, SeriesId, SeasonNumber, RelativePath, Size, DateAdded,
                 SceneName, ReleaseGroup, Quality, Languages, MediaInfo, IndexerFlags, ReleaseType
             ) VALUES (?, ?, 2, ?, ?, ?, ?, NULL, ?, ?, NULL, 0, 0)",
        )
        .bind(file.file_id)
        .bind(SERIES_ID)
        .bind(relative_path(&file.arr_path, &series_path))
        .bind(file.size as i64)
        .bind(ADDED)
        .bind(file.scene_name.as_deref())
        .bind(QUALITY)
        .bind(LANGUAGES)
        .execute(db)
        .await?;
    }

    for (season, episode, file_id) in EPISODES {
        // Ids of their own, distinct from the file ids they point at, so an episode
        // with no file (file_id 0) still gets a row.
        let id = 1000 + season * 100 + episode;
        sqlx::query(
            "INSERT OR REPLACE INTO Episodes (
                 Id, SeriesId, SeasonNumber, EpisodeNumber, Title, EpisodeFileId,
                 Monitored, UnverifiedSceneNumbering, Runtime
             ) VALUES (?, ?, ?, ?, ?, ?, 1, 0, 30)",
        )
        .bind(id)
        .bind(SERIES_ID)
        .bind(season)
        .bind(episode)
        .bind(format!("Episode {episode}"))
        .bind(file_id)
        .execute(db)
        .await?;
    }

    Ok(())
}

async fn seed_radarr(db: &SqlitePool, tag_id: i64, files: &[MediaFile]) -> anyhow::Result<()> {
    let movie_path = format!("{MOVIE_ROOT}/{MOVIE_FOLDER}");
    let tags = format!("[{tag_id}]");
    let file = files
        .first()
        .ok_or_else(|| anyhow::anyhow!("no movie fixtures to seed"))?;

    db.execute(sqlx::query("INSERT OR IGNORE INTO RootFolders (Path) VALUES (?)").bind(MOVIE_ROOT))
        .await?;

    // Radarr 5 keeps the descriptive half of a movie in its own table, so the row
    // has to exist before `Movies` can point at it.
    sqlx::query(
        "INSERT OR REPLACE INTO MovieMetadata (
             Id, TmdbId, ImdbId, Images, Genres, Title, SortTitle, CleanTitle,
             OriginalTitle, CleanOriginalTitle, OriginalLanguage, Status, Runtime,
             Year, Ratings, Recommendations, Overview
         ) VALUES (?, ?, ?, '[]', '[]', ?, ?, ?, ?, ?, 1, 3, 100, ?, '{}', '[]', '')",
    )
    .bind(MOVIE_ID)
    .bind(MOVIE_TMDB_ID)
    .bind(MOVIE_IMDB_ID)
    .bind(MOVIE_TITLE)
    .bind(MOVIE_TITLE.to_lowercase())
    .bind(clean(MOVIE_TITLE))
    .bind(MOVIE_TITLE)
    .bind(clean(MOVIE_TITLE))
    .bind(MOVIE_YEAR)
    .execute(db)
    .await?;

    sqlx::query(
        "INSERT OR REPLACE INTO Movies (
             Id, Path, Monitored, QualityProfileId, Added, Tags, MovieFileId,
             MinimumAvailability, MovieMetadataId
         ) VALUES (?, ?, 1, 1, ?, ?, ?, 3, ?)",
    )
    .bind(MOVIE_ID)
    .bind(&movie_path)
    .bind(ADDED)
    .bind(&tags)
    .bind(file.file_id)
    .bind(MOVIE_ID)
    .execute(db)
    .await?;

    sqlx::query(
        "INSERT OR REPLACE INTO MovieFiles (
             Id, MovieId, RelativePath, Size, DateAdded, SceneName, ReleaseGroup,
             Quality, Languages, MediaInfo, Edition, IndexerFlags
         ) VALUES (?, ?, ?, ?, ?, ?, NULL, ?, ?, NULL, '', 0)",
    )
    .bind(file.file_id)
    .bind(MOVIE_ID)
    .bind(relative_path(&file.arr_path, &movie_path))
    .bind(file.size as i64)
    .bind(ADDED)
    .bind(file.scene_name.as_deref())
    .bind(QUALITY)
    .bind(LANGUAGES)
    .execute(db)
    .await?;

    Ok(())
}

/// Sonarr's `TitleSlug`, which carries a unique index — lowercase, hyphenated.
fn slug(title: &str) -> String {
    title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

/// Both apps' `CleanTitle`: lowercase, non-alphanumerics dropped entirely.
fn clean(title: &str) -> String {
    title
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relative_path_is_what_is_left_below_the_folder() {
        assert_eq!(
            relative_path(
                "/tv/Lanternwick Hollow/Season 02/a.mkv",
                "/tv/Lanternwick Hollow"
            ),
            "Season 02/a.mkv"
        );
    }

    /// The *arr apps report `path` as folder + relative path, so a mismatch here
    /// would hand sharerr a path that resolves to nothing and look like a
    /// path-mapping bug rather than a seeding one.
    #[test]
    fn rejoining_the_folder_reproduces_the_path_the_api_reports() {
        let folder = format!("{TV_ROOT}/{SERIES_FOLDER}");
        for file in tv_files(std::path::Path::new("/")) {
            let relative = relative_path(&file.arr_path, &folder);
            assert_eq!(format!("{folder}/{relative}"), file.arr_path);
        }
    }

    #[test]
    fn titles_reduce_the_way_the_arr_apps_index_them() {
        assert_eq!(clean("The Gilded Ferry"), "thegildedferry");
        assert_eq!(slug("Lanternwick Hollow"), "lanternwick-hollow");
    }
}
