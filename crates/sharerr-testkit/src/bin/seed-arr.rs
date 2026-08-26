//! Write the tagged fixture library straight into Sonarr's, Radarr's, and
//! Lidarr's databases.
//!
//! ```text
//! cargo run -p sharerr-testkit --bin seed-arr -- \
//!     --sonarr docker/state/sonarr/sonarr.db \
//!     --radarr docker/state/radarr/radarr.db \
//!     --lidarr docker/state/lidarr/lidarr.db
//! ```
//!
//! Also seeds a *second, independent* Radarr's own "wanted" catalog entry —
//! untagged, and with no file — for the two-instance stack's requesting
//! friend, via `--radarr-wanted`; see [`seed_radarr_wanted`].
//!
//! # Why not the API
//!
//! `POST /api/v3/series` triggers a metadata lookup against `services.sonarr.tv`,
//! `POST /api/v3/movie` one against `api.radarr.video`, and adding an artist or
//! album to Lidarr one against MusicBrainz. Every fixture title is invented, so
//! the lookup would find nothing even with egress — writing the rows directly
//! is the only way to get tagged content at all, independent of whether the
//! compose network can reach the internet.
//!
//! # Two consequences
//!
//! **Every named app must be stopped.** Each holds its SQLite database open and
//! will not observe an external write while running.
//!
//! **This is coupled to their schemas**, which is why `compose.test.yml` pins
//! image tags rather than using `:latest`. Only the columns sharerr's read
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
    ALBUM_FOREIGN_ID, ALBUM_ID, ALBUM_RELEASE_FOREIGN_ID, ALBUM_TITLE, ARR_MOVIE_PREFIX,
    ARR_MUSIC_PREFIX, ARR_TV_PREFIX, ARTIST_FOLDER, ARTIST_FOREIGN_ID, ARTIST_ID, ARTIST_NAME,
    EPISODES, MOVIE_FOLDER, MOVIE_ID, MOVIE_IMDB_ID, MOVIE_TITLE, MOVIE_TMDB_ID, MOVIE_YEAR,
    MediaFile, SERIES_FOLDER, SERIES_ID, SERIES_IMDB_ID, SERIES_TITLE, SERIES_TVDB_ID,
    SERIES_TVMAZE_ID, TAG_LABEL, TRACKS, movie_files, music_files, tv_files,
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
const MUSIC_ROOT: &str = ARR_MUSIC_PREFIX;

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            eprintln!(
                "usage: seed-arr --sonarr <sonarr.db> --radarr <radarr.db> \
                 [--lidarr <lidarr.db>] [--radarr-wanted <radarr.db>]\n\
                 \n\
                 Every named app must be stopped: it holds its database open and will\n\
                 not see an external write while running. Each flag is independent —\n\
                 give any combination, though the single-instance stack always passes\n\
                 --sonarr/--radarr[/--lidarr] together.\n\
                 \n\
                 --radarr-wanted seeds a *different* Radarr's own catalog entry for\n\
                 the same fixture movie — untagged, and with no file — so its\n\
                 automatic search has something to match another instance's shared\n\
                 release against by TmdbId. Point it at a second Radarr's database,\n\
                 never the same one --radarr already seeded."
            );
            return ExitCode::FAILURE;
        }
    };

    if let Err(err) = run(&args).await {
        eprintln!("seeding failed: {err}");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}

#[derive(Default)]
struct Args {
    sonarr: Option<PathBuf>,
    radarr: Option<PathBuf>,
    lidarr: Option<PathBuf>,
    radarr_wanted: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args::default();
    let mut raw = std::env::args().skip(1);

    while let Some(flag) = raw.next() {
        let target = match flag.as_str() {
            "--sonarr" => &mut args.sonarr,
            "--radarr" => &mut args.radarr,
            "--lidarr" => &mut args.lidarr,
            "--radarr-wanted" => &mut args.radarr_wanted,
            other => return Err(format!("unexpected argument {other:?}")),
        };
        let value = raw.next().ok_or(format!("{flag} needs a path"))?;
        *target = Some(PathBuf::from(value));
    }

    if args.sonarr.is_none()
        && args.radarr.is_none()
        && args.lidarr.is_none()
        && args.radarr_wanted.is_none()
    {
        return Err(
            "nothing to do: give at least one of --sonarr, --radarr, --lidarr, --radarr-wanted"
                .to_owned(),
        );
    }

    Ok(args)
}

async fn run(args: &Args) -> anyhow::Result<()> {
    // The fixture root is irrelevant here — only `arr_path`, `size` and
    // `scene_name` are used, and those describe how the *arr app sees the file,
    // not where it is on this host.
    let unused_root = std::path::Path::new("/");

    // Every flag is independent — the single-instance stack always passes
    // `--sonarr`/`--radarr`/`--lidarr` together, but the two-instance stack's
    // instance A has no Sonarr at all, so `--radarr` has to work alone.
    if let Some(sonarr_db) = &args.sonarr {
        let tv = tv_files(unused_root);
        let sonarr = open(sonarr_db).await?;
        let tag_id = ensure_tag(&sonarr).await?;
        seed_sonarr(&sonarr, tag_id, &tv).await?;
        sonarr.close().await;
        println!(
            "tagged {TAG_LABEL:?}: {} episode file(s) on {SERIES_TITLE:?}",
            tv.len()
        );
    }

    if let Some(radarr_db) = &args.radarr {
        let movies = movie_files(unused_root);
        let radarr = open(radarr_db).await?;
        let tag_id = ensure_tag(&radarr).await?;
        seed_radarr(&radarr, tag_id, &movies).await?;
        radarr.close().await;
        println!(
            "tagged {TAG_LABEL:?}: {} movie file(s) on {MOVIE_TITLE:?}",
            movies.len()
        );
    }

    if let Some(lidarr_db) = &args.lidarr {
        let music = music_files(unused_root);
        let lidarr = open(lidarr_db).await?;
        let tag_id = ensure_tag(&lidarr).await?;
        seed_lidarr(&lidarr, tag_id, &music).await?;
        lidarr.close().await;
        println!(
            "tagged {TAG_LABEL:?}: {} track file(s) on {ARTIST_NAME:?}",
            music.len()
        );
    }

    if let Some(radarr_wanted_db) = &args.radarr_wanted {
        let db = open(radarr_wanted_db).await?;
        seed_radarr_wanted(&db).await?;
        db.close().await;
        println!(
            "seeded a wanted (untagged, fileless) {MOVIE_TITLE:?} for a second Radarr's own automatic search"
        );
    }

    println!("start the apps just stopped again to pick this up");
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
    insert_movie_metadata(db).await?;

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

/// The descriptive half of the fixture movie, shared by [`seed_radarr`]
/// (tagged, with a file) and [`seed_radarr_wanted`] (untagged, fileless — a
/// second, independent Radarr's own catalog entry, so its automatic search
/// has something to match the first Radarr's shared release against by
/// `TmdbId`). Radarr 5 keeps this in its own table, so it has to exist
/// before either caller's `Movies` row can point at it.
async fn insert_movie_metadata(db: &SqlitePool) -> anyhow::Result<()> {
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
    Ok(())
}

/// A second Radarr's own "wanted" catalog entry for the fixture movie — no
/// tag, and critically no `MovieFiles` row, so this Radarr considers it
/// missing and eligible for its own automatic search. Exists for the
/// two-instance end-to-end test: instance A's Radarr is seeded by
/// [`seed_radarr`] and shares the file; instance B's Radarr is seeded by
/// this function so its real automatic-search-and-grab flow has a matching
/// `TmdbId` to search for, without B's own sharerr ever discovering or
/// tagging the row itself.
async fn seed_radarr_wanted(db: &SqlitePool) -> anyhow::Result<()> {
    let movie_path = format!("{MOVIE_ROOT}/{MOVIE_FOLDER}");

    db.execute(sqlx::query("INSERT OR IGNORE INTO RootFolders (Path) VALUES (?)").bind(MOVIE_ROOT))
        .await?;
    insert_movie_metadata(db).await?;

    sqlx::query(
        "INSERT OR REPLACE INTO Movies (
             Id, Path, Monitored, QualityProfileId, Added, Tags, MovieFileId,
             MinimumAvailability, MovieMetadataId
         ) VALUES (?, ?, 1, 1, ?, '[]', 0, 3, ?)",
    )
    .bind(MOVIE_ID)
    .bind(&movie_path)
    .bind(ADDED)
    .bind(MOVIE_ID)
    .execute(db)
    .await?;

    Ok(())
}

/// Lidarr tags the artist, not the album — one tagged artist shares their whole
/// discography, the same surprise Sonarr's series-level tags produce.
///
/// Every `TrackFiles` row needs at least one `Tracks` row pointing at it, or it
/// is invisible to Lidarr's own API — confirmed against a live container that
/// its artist/album-filtered `trackfile` endpoint inner-joins through `Tracks`,
/// so a file nothing points at does not merely look track-less, it does not
/// exist as far as the API is concerned. `sharerr_arr::lidarr::track_number`'s
/// "whole album" fallback is therefore two `Tracks` rows sharing one
/// `TrackFileId`, not zero — see [`sharerr_testkit::library::TRACKS`].
///
/// Column names here were confirmed against a live `lscr.io/linuxserver/lidarr`
/// container's schema rather than assumed — Lidarr splits the descriptive half
/// of both an artist and an album into their own tables (`ArtistMetadata`,
/// `AlbumReleases`), the same way Radarr 5 splits `MovieMetadata` from `Movies`.
/// Unlike `EpisodeFiles`/`MovieFiles`, `TrackFiles.Path` holds the *full* path —
/// there is no `RelativePath` column to join against the artist's own `Path`.
async fn seed_lidarr(db: &SqlitePool, tag_id: i64, files: &[MediaFile]) -> anyhow::Result<()> {
    let artist_path = format!("{MUSIC_ROOT}/{ARTIST_FOLDER}");
    let tags = format!("[{tag_id}]");

    db.execute(sqlx::query("INSERT OR IGNORE INTO RootFolders (Path) VALUES (?)").bind(MUSIC_ROOT))
        .await?;

    sqlx::query(
        "INSERT OR REPLACE INTO ArtistMetadata (
             Id, ForeignArtistId, Name, Status, Images, Aliases, OldForeignArtistIds
         ) VALUES (?, ?, ?, 1, '[]', '[]', '[]')",
    )
    .bind(ARTIST_ID)
    .bind(ARTIST_FOREIGN_ID)
    .bind(ARTIST_NAME)
    .execute(db)
    .await?;

    sqlx::query(
        "INSERT OR REPLACE INTO Artists (
             Id, CleanName, Path, Monitored, SortName, QualityProfileId, Tags, Added,
             MetadataProfileId, ArtistMetadataId, MonitorNewItems
         ) VALUES (?, ?, ?, 1, ?, 1, ?, ?, 1, ?, 0)",
    )
    .bind(ARTIST_ID)
    .bind(clean(ARTIST_NAME))
    .bind(&artist_path)
    .bind(ARTIST_NAME.to_lowercase())
    .bind(&tags)
    .bind(ADDED)
    .bind(ARTIST_ID)
    .execute(db)
    .await?;

    sqlx::query(
        "INSERT OR REPLACE INTO Albums (
             Id, ForeignAlbumId, Title, CleanTitle, Images, Monitored, ProfileId, Added,
             AlbumType, ArtistMetadataId, AnyReleaseOk, OldForeignAlbumIds
         ) VALUES (?, ?, ?, ?, '[]', 1, 1, ?, 'Album', ?, 1, '[]')",
    )
    .bind(ALBUM_ID)
    .bind(ALBUM_FOREIGN_ID)
    .bind(ALBUM_TITLE)
    .bind(clean(ALBUM_TITLE))
    .bind(ADDED)
    .bind(ARTIST_ID)
    .execute(db)
    .await?;

    // The specific pressing `Tracks` points at, distinct from the album itself —
    // one album can have several MusicBrainz releases; this fixture has one.
    sqlx::query(
        "INSERT OR REPLACE INTO AlbumReleases (
             Id, ForeignReleaseId, AlbumId, Title, Status, Duration, Monitored, OldForeignReleaseIds
         ) VALUES (?, ?, ?, ?, 'Official', 0, 1, '[]')",
    )
    .bind(ALBUM_ID)
    .bind(ALBUM_RELEASE_FOREIGN_ID)
    .bind(ALBUM_ID)
    .bind(ALBUM_TITLE)
    .execute(db)
    .await?;

    for file in files {
        sqlx::query(
            "INSERT OR REPLACE INTO TrackFiles (
                 Id, AlbumId, Quality, Size, SceneName, DateAdded, ReleaseGroup, MediaInfo,
                 Modified, Path, IndexerFlags
             ) VALUES (?, ?, ?, ?, ?, ?, NULL, NULL, ?, ?, 0)",
        )
        .bind(file.file_id)
        .bind(ALBUM_ID)
        .bind(QUALITY)
        .bind(file.size as i64)
        .bind(file.scene_name.as_deref())
        .bind(ADDED)
        .bind(ADDED)
        .bind(&file.arr_path)
        .execute(db)
        .await?;
    }

    for (track_id, file_id, number, foreign_track_id) in TRACKS {
        sqlx::query(
            "INSERT OR REPLACE INTO Tracks (
                 Id, ForeignTrackId, Title, Explicit, TrackFileId, Duration, MediumNumber,
                 AbsoluteTrackNumber, ForeignRecordingId, AlbumReleaseId, ArtistMetadataId,
                 OldForeignRecordingIds, OldForeignTrackIds
             ) VALUES (?, ?, ?, 0, ?, 180, 1, ?, ?, ?, ?, '[]', '[]')",
        )
        .bind(track_id)
        .bind(foreign_track_id)
        .bind(format!("Track {number}"))
        .bind(file_id)
        .bind(number)
        .bind(foreign_track_id)
        .bind(ALBUM_ID)
        .bind(ARTIST_ID)
        .execute(db)
        .await?;
    }

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
