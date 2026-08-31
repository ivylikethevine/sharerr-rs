//! Driving the `seed-arr` binary against stand-in *arr databases.
//!
//! `seed-arr` writes the tagged fixture library straight into Sonarr's,
//! Radarr's and Lidarr's SQLite files, because their APIs would trigger
//! metadata lookups the sealed compose network cannot make (see the binary's
//! own module docs). Tier 2 runs it against databases the real apps created;
//! nothing else did, so a typo in one of its fourteen `INSERT`s was invisible
//! until someone brought the whole stack up.
//!
//! # The stand-in schema, and what it does and does not prove
//!
//! [`create_schema`] declares exactly the tables and columns the binary writes,
//! and nothing else. That is deliberately a *stand-in*, not a copy of the real
//! schema — it has no constraints, no foreign keys, and no columns the seeder
//! does not touch.
//!
//! So this catches malformed SQL, a column list that does not match its
//! `VALUES`, a renamed table, and wrong values landing in the right places. It
//! **cannot** catch the real apps having renamed or retyped a column, or a
//! `NOT NULL` this fixture does not impose — only tier 2 catches that, and it
//! stays the authority on whether a real Sonarr can read what this wrote.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::Command;

use sqlx::Row;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool};

/// Every table `seed-arr` writes, with exactly the columns it names.
///
/// Kept in the order the binary writes them so the two can be read side by
/// side. `Id INTEGER PRIMARY KEY` throughout because the seeder pins fixed ids
/// to make re-runs idempotent, and `UNIQUE` where it relies on
/// `INSERT OR IGNORE` being a no-op the second time.
const SCHEMA: &[&str] = &[
    "CREATE TABLE Tags (Id INTEGER PRIMARY KEY, Label TEXT UNIQUE)",
    "CREATE TABLE RootFolders (Id INTEGER PRIMARY KEY, Path TEXT UNIQUE)",
    "CREATE TABLE Series (
         Id INTEGER PRIMARY KEY, TvdbId, TvRageId, TvMazeId, TmdbId, ImdbId, Title, TitleSlug,
         CleanTitle, SortTitle, Status, Overview, Images, Path, Monitored, SeasonFolder,
         Runtime, SeriesType, UseSceneNumbering, Year, Seasons, Ratings, Genres,
         QualityProfileId, Tags, Added, OriginalLanguage, MonitorNewItems, MalIds, AniListIds)",
    "CREATE TABLE EpisodeFiles (
         Id INTEGER PRIMARY KEY, SeriesId, SeasonNumber, RelativePath, Size, DateAdded,
         SceneName, ReleaseGroup, Quality, Languages, MediaInfo, IndexerFlags, ReleaseType)",
    "CREATE TABLE Episodes (
         Id INTEGER PRIMARY KEY, SeriesId, SeasonNumber, EpisodeNumber, Title, EpisodeFileId,
         Monitored, UnverifiedSceneNumbering, Runtime)",
    "CREATE TABLE MovieMetadata (
         Id INTEGER PRIMARY KEY, TmdbId, ImdbId, Images, Genres, Title, SortTitle, CleanTitle,
         OriginalTitle, CleanOriginalTitle, OriginalLanguage, Status, Runtime, Year, Ratings,
         Recommendations, Overview)",
    "CREATE TABLE Movies (
         Id INTEGER PRIMARY KEY, Path, Monitored, QualityProfileId, Added, Tags, MovieFileId,
         MinimumAvailability, MovieMetadataId)",
    "CREATE TABLE MovieFiles (
         Id INTEGER PRIMARY KEY, MovieId, RelativePath, Size, DateAdded, SceneName,
         ReleaseGroup, Quality, Languages, MediaInfo, Edition, IndexerFlags)",
    "CREATE TABLE ArtistMetadata (
         Id INTEGER PRIMARY KEY, ForeignArtistId, Name, Status, Images, Aliases,
         OldForeignArtistIds)",
    "CREATE TABLE Artists (
         Id INTEGER PRIMARY KEY, CleanName, Path, Monitored, SortName, QualityProfileId, Tags,
         Added, MetadataProfileId, ArtistMetadataId, MonitorNewItems)",
    "CREATE TABLE Albums (
         Id INTEGER PRIMARY KEY, ForeignAlbumId, Title, CleanTitle, Images, Monitored, ProfileId,
         Added, AlbumType, ArtistMetadataId, AnyReleaseOk, OldForeignAlbumIds)",
    "CREATE TABLE AlbumReleases (
         Id INTEGER PRIMARY KEY, ForeignReleaseId, AlbumId, Title, Status, Duration, Monitored,
         OldForeignReleaseIds)",
    "CREATE TABLE TrackFiles (
         Id INTEGER PRIMARY KEY, AlbumId, Quality, Size, SceneName, DateAdded, ReleaseGroup,
         MediaInfo, Modified, Path, IndexerFlags)",
    "CREATE TABLE Tracks (
         Id INTEGER PRIMARY KEY, ForeignTrackId, Title, Explicit, TrackFileId, Duration,
         MediumNumber, AbsoluteTrackNumber, ForeignRecordingId, AlbumReleaseId,
         ArtistMetadataId, OldForeignRecordingIds, OldForeignTrackIds)",
];

async fn create_schema(path: &std::path::Path) {
    let pool = SqlitePool::connect_with(
        SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true),
    )
    .await
    .unwrap();
    for statement in SCHEMA {
        sqlx::query(*statement).execute(&pool).await.unwrap();
    }
    pool.close().await;
}

async fn open(path: &std::path::Path) -> SqlitePool {
    SqlitePool::connect_with(SqliteConnectOptions::new().filename(path))
        .await
        .unwrap()
}

async fn count(pool: &SqlitePool, table: &str) -> i64 {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT COUNT(*) AS n FROM {table}"
    )))
    .fetch_one(pool)
    .await
    .unwrap()
    .get::<i64, _>("n")
}

struct Stack {
    _dir: tempfile::TempDir,
    sonarr: std::path::PathBuf,
    radarr: std::path::PathBuf,
    lidarr: std::path::PathBuf,
}

impl Stack {
    async fn prepared() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (sonarr, radarr, lidarr) = (
            dir.path().join("sonarr.db"),
            dir.path().join("radarr.db"),
            dir.path().join("lidarr.db"),
        );
        for db in [&sonarr, &radarr, &lidarr] {
            create_schema(db).await;
        }
        Self {
            _dir: dir,
            sonarr,
            radarr,
            lidarr,
        }
    }

    fn seed(&self, with_lidarr: bool) -> std::process::Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_seed-arr"));
        cmd.arg("--sonarr")
            .arg(&self.sonarr)
            .arg("--radarr")
            .arg(&self.radarr);
        if with_lidarr {
            cmd.arg("--lidarr").arg(&self.lidarr);
        }
        cmd.output().unwrap()
    }
}

#[tokio::test]
async fn seeding_writes_tagged_content_to_all_three_apps() {
    let stack = Stack::prepared().await;
    let out = stack.seed(true);
    assert!(out.status.success(), "{out:?}");

    let sonarr = open(&stack.sonarr).await;
    assert_eq!(count(&sonarr, "Series").await, 1);
    assert!(count(&sonarr, "EpisodeFiles").await > 0);
    assert!(count(&sonarr, "Episodes").await > 0);
    assert_eq!(count(&sonarr, "RootFolders").await, 1);

    // The tag is the whole mechanism — discovery finds nothing without it, and
    // the series has to actually reference its id.
    let tag_id: i64 = sqlx::query("SELECT Id FROM Tags WHERE Label = 'sharerr'")
        .fetch_one(&sonarr)
        .await
        .unwrap()
        .get("Id");
    let tags: String = sqlx::query("SELECT Tags FROM Series")
        .fetch_one(&sonarr)
        .await
        .unwrap()
        .get("Tags");
    assert_eq!(tags, format!("[{tag_id}]"));

    let radarr = open(&stack.radarr).await;
    assert_eq!(count(&radarr, "Movies").await, 1);
    assert_eq!(count(&radarr, "MovieFiles").await, 1);
    assert_eq!(count(&radarr, "MovieMetadata").await, 1);

    let lidarr = open(&stack.lidarr).await;
    assert_eq!(count(&lidarr, "Artists").await, 1);
    assert!(count(&lidarr, "Tracks").await > 0);
    assert!(count(&lidarr, "TrackFiles").await > 0);
}

/// Lidarr is optional — the plain stack does not run it, and asking for it
/// unconditionally would fail there.
#[tokio::test]
async fn lidarr_is_optional() {
    let stack = Stack::prepared().await;
    let out = stack.seed(false);
    assert!(out.status.success(), "{out:?}");

    assert_eq!(count(&open(&stack.sonarr).await, "Series").await, 1);
    assert_eq!(
        count(&open(&stack.lidarr).await, "Artists").await,
        0,
        "an unnamed lidarr must be left alone entirely"
    );
}

/// `run_docker_tests.sh` is documented as safe to re-run, and it invokes this
/// every time. Fixed ids and `INSERT OR REPLACE` are what make that true.
#[tokio::test]
async fn seeding_twice_is_idempotent() {
    let stack = Stack::prepared().await;
    assert!(stack.seed(true).status.success());

    let sonarr = open(&stack.sonarr).await;
    let (series, files, tags) = (
        count(&sonarr, "Series").await,
        count(&sonarr, "EpisodeFiles").await,
        count(&sonarr, "Tags").await,
    );

    assert!(stack.seed(true).status.success());

    let sonarr = open(&stack.sonarr).await;
    assert_eq!(count(&sonarr, "Series").await, series);
    assert_eq!(count(&sonarr, "EpisodeFiles").await, files);
    assert_eq!(
        count(&sonarr, "Tags").await,
        tags,
        "the tag must not be duplicated"
    );
}

/// The paths stored are *relative* to the series or movie folder — the *arr
/// APIs report `path` as folder plus this, so an absolute one here shows up as
/// a doubled path in discovery.
#[tokio::test]
async fn stored_file_paths_are_relative_to_their_folder() {
    let stack = Stack::prepared().await;
    assert!(stack.seed(true).status.success());

    let relative: String = sqlx::query("SELECT RelativePath FROM EpisodeFiles LIMIT 1")
        .fetch_one(&open(&stack.sonarr).await)
        .await
        .unwrap()
        .get("RelativePath");

    assert!(!relative.starts_with('/'), "{relative} is absolute");
    assert!(relative.ends_with(".mkv"), "{relative}");
}

#[tokio::test]
async fn a_database_that_does_not_exist_is_a_named_error() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_seed-arr"))
        .arg("--sonarr")
        .arg(dir.path().join("absent.db"))
        .arg("--radarr")
        .arg(dir.path().join("absent.db"))
        .output()
        .unwrap();

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("does not exist"), "{stderr}");
    assert!(
        stderr.contains("bring the stack up"),
        "the remedy matters: {stderr}"
    );
}

#[tokio::test]
async fn the_required_flags_are_enforced() {
    for args in [
        vec!["--sonarr", "/tmp/x.db"],
        vec![],
        vec!["--nonsense", "x"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_seed-arr"))
            .args(&args)
            .output()
            .unwrap();
        assert!(
            !out.status.success(),
            "{args:?} should not have been accepted"
        );
    }
}

#[tokio::test]
async fn a_flag_without_a_value_is_rejected() {
    let out = Command::new(env!("CARGO_BIN_EXE_seed-arr"))
        .arg("--sonarr")
        .output()
        .unwrap();

    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("needs a path"),
        "{out:?}"
    );
}

/// `--radarr-wanted` is the two-instance stack's own flag, seeding a second
/// Radarr's catalog with an untagged, fileless entry for its automatic search
/// to find — untested until now, unlike the other three flags above.
#[tokio::test]
async fn radarr_wanted_seeds_an_untagged_fileless_catalog_entry() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("radarr_wanted.db");
    create_schema(&db_path).await;

    let out = Command::new(env!("CARGO_BIN_EXE_seed-arr"))
        .arg("--radarr-wanted")
        .arg(&db_path)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("seeded a wanted"), "{stdout}");

    let db = open(&db_path).await;
    assert_eq!(count(&db, "Movies").await, 1);
    assert_eq!(count(&db, "MovieMetadata").await, 1);
    assert_eq!(count(&db, "RootFolders").await, 1);

    let tags: String = sqlx::query("SELECT Tags FROM Movies")
        .fetch_one(&db)
        .await
        .unwrap()
        .get("Tags");
    assert_eq!(tags, "[]", "wanted means untagged");
    let file_id: i64 = sqlx::query("SELECT MovieFileId FROM Movies")
        .fetch_one(&db)
        .await
        .unwrap()
        .get("MovieFileId");
    assert_eq!(file_id, 0, "wanted means fileless");
    assert_eq!(
        count(&db, "MovieFiles").await,
        0,
        "no MovieFiles row must exist either"
    );
}

/// Re-running `--radarr-wanted` must not duplicate its row, the same
/// idempotence guarantee `seeding_twice_is_idempotent` covers for the other
/// three flags.
#[tokio::test]
async fn radarr_wanted_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("radarr_wanted.db");
    create_schema(&db_path).await;
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_seed-arr"))
            .arg("--radarr-wanted")
            .arg(&db_path)
            .status()
            .unwrap()
    };

    assert!(run().success());
    assert!(run().success());

    let db = open(&db_path).await;
    assert_eq!(count(&db, "Movies").await, 1);
    assert_eq!(count(&db, "MovieMetadata").await, 1);
}
