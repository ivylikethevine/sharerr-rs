//! SQLite-backed record of what sharerr has shared.
//!
//! Queries are built at runtime rather than with `sqlx::query!`, so building the
//! project never requires a live database or a checked-in `.sqlx` cache.

use std::path::Path;
use std::str::FromStr;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Row, SqlitePool};

use sharerr_core::model::{ExternalIds, MediaSource, MediaSpec, ShareState, SharedItem};

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sql(#[from] sqlx::Error),

    #[error("failed to apply migrations: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),

    #[error("could not create data directory {path}: {source}")]
    DataDir {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("stored row {id} is malformed: {detail}")]
    Malformed { id: i64, detail: String },

    #[error("password hashing failed: {0}")]
    PasswordHash(String),

    #[error("a user named {username:?} already exists")]
    UserExists { username: String },

    /// Rejected before hashing. `&'static str` rather than `String` because every
    /// one of these is a fixed message safe to render straight back to the form.
    #[error("{0}")]
    InvalidUser(&'static str),
}

type Result<T> = std::result::Result<T, StoreError>;

#[derive(Debug, Clone)]
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    /// Open (creating if needed) the database at `path` and apply migrations.
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| StoreError::DataDir {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            // WAL keeps the periodic sync from blocking reads by the HTTP server.
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(std::time::Duration::from_secs(10));

        Self::from_options(opts, 5).await
    }

    /// An ephemeral database, for tests.
    pub async fn open_in_memory() -> Result<Self> {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")?;
        // A single connection: each new connection to `:memory:` would otherwise
        // get its own empty database.
        Self::from_options(opts, 1).await
    }

    async fn from_options(opts: SqliteConnectOptions, max_connections: u32) -> Result<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(max_connections)
            // Recycling a connection is ordinarily harmless, but for `:memory:` the
            // database lives *in* the connection — retiring it would silently discard
            // everything. Disabled unconditionally; a file-backed pool just reconnects.
            .idle_timeout(None)
            .max_lifetime(None)
            .connect_with(opts)
            .await?;
        MIGRATOR.run(&pool).await?;
        Ok(Self { pool })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Every item sharerr knows about, in any state.
    pub async fn all_items(&self) -> Result<Vec<SharedItem>> {
        let rows = sqlx::query(SELECT_COLUMNS).fetch_all(&self.pool).await?;
        rows.iter().map(row_to_item).collect()
    }

    pub async fn get(&self, source: MediaSource, file_id: i64) -> Result<Option<SharedItem>> {
        let row = sqlx::query(&format!(
            "{SELECT_COLUMNS} WHERE source = ?1 AND file_id = ?2"
        ))
        .bind(source.as_str())
        .bind(file_id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(row_to_item).transpose()
    }

    /// Insert or update by the natural key `(source, file_id)`.
    ///
    /// `created_at` is preserved across updates; re-running discovery must not
    /// make an item look newly shared.
    pub async fn upsert(&self, item: &SharedItem) -> Result<i64> {
        let now = now_epoch();
        let spec_json = serde_json::to_string(&item.spec).map_err(|e| StoreError::Malformed {
            id: 0,
            detail: e.to_string(),
        })?;
        let ids_json = serde_json::to_string(&item.ids).map_err(|e| StoreError::Malformed {
            id: 0,
            detail: e.to_string(),
        })?;

        let row = sqlx::query(
            r#"
            INSERT INTO shared_items (
                source, source_id, file_id, spec_json, release_title, arr_path,
                size, ids_json, info_hash, state, last_error, created_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)
            ON CONFLICT (source, file_id) DO UPDATE SET
                source_id     = excluded.source_id,
                spec_json     = excluded.spec_json,
                release_title = excluded.release_title,
                arr_path      = excluded.arr_path,
                size          = excluded.size,
                ids_json      = excluded.ids_json,
                -- Never clear a known infohash. Discovery rebuilds items with
                -- `info_hash: None`, so a plain assignment would drop the hash of a
                -- torrent qBittorrent is still seeding; if the re-share then failed
                -- and the item were later untagged, nothing would know which torrent
                -- to remove and it would seed forever. Use `set_info_hash` to change
                -- it, and `set_state(Unshared)` to retire it.
                info_hash     = COALESCE(excluded.info_hash, shared_items.info_hash),
                state         = excluded.state,
                last_error    = excluded.last_error,
                updated_at    = excluded.updated_at
            RETURNING id
            "#,
        )
        .bind(item.source.as_str())
        .bind(item.source_id)
        .bind(item.file_id)
        .bind(&spec_json)
        .bind(&item.release_title)
        .bind(item.arr_path.to_string_lossy().as_ref())
        .bind(i64::try_from(item.size).unwrap_or(i64::MAX))
        .bind(&ids_json)
        .bind(item.info_hash.as_deref())
        .bind(item.state.as_str())
        .bind(item.last_error.as_deref())
        .bind(now)
        .fetch_one(&self.pool)
        .await?;

        Ok(row.try_get::<i64, _>("id")?)
    }

    /// Record a state transition, clearing or setting the error message with it.
    pub async fn set_state(
        &self,
        source: MediaSource,
        file_id: i64,
        state: ShareState,
        last_error: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE shared_items SET state = ?1, last_error = ?2, updated_at = ?3 \
             WHERE source = ?4 AND file_id = ?5",
        )
        .bind(state.as_str())
        .bind(last_error)
        .bind(now_epoch())
        .bind(source.as_str())
        .bind(file_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_info_hash(
        &self,
        source: MediaSource,
        file_id: i64,
        info_hash: &str,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE shared_items SET info_hash = ?1, updated_at = ?2 \
             WHERE source = ?3 AND file_id = ?4",
        )
        .bind(info_hash)
        .bind(now_epoch())
        .bind(source.as_str())
        .bind(file_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn begin_run(&self) -> Result<i64> {
        let row = sqlx::query("INSERT INTO sync_runs (started_at) VALUES (?1) RETURNING id")
            .bind(now_epoch())
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get::<i64, _>("id")?)
    }

    pub async fn finish_run(&self, run_id: i64, summary: &RunSummary) -> Result<()> {
        sqlx::query(
            "UPDATE sync_runs SET finished_at = ?1, discovered = ?2, added = ?3, \
             unshared = ?4, failed = ?5, error = ?6 WHERE id = ?7",
        )
        .bind(now_epoch())
        .bind(summary.discovered)
        .bind(summary.added)
        .bind(summary.unshared)
        .bind(summary.failed)
        .bind(summary.error.as_deref())
        .bind(run_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn recent_runs(&self, limit: i64) -> Result<Vec<RunRecord>> {
        let rows = sqlx::query(
            "SELECT id, started_at, finished_at, discovered, added, unshared, failed, error \
             FROM sync_runs ORDER BY id DESC LIMIT ?1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        rows.iter()
            .map(|r| {
                Ok(RunRecord {
                    id: r.try_get("id")?,
                    started_at: r.try_get("started_at")?,
                    finished_at: r.try_get("finished_at")?,
                    summary: RunSummary {
                        discovered: r.try_get("discovered")?,
                        added: r.try_get("added")?,
                        unshared: r.try_get("unshared")?,
                        failed: r.try_get("failed")?,
                        error: r.try_get("error")?,
                    },
                })
            })
            .collect()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunSummary {
    pub discovered: i64,
    pub added: i64,
    pub unshared: i64,
    pub failed: i64,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RunRecord {
    pub id: i64,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub summary: RunSummary,
}

const SELECT_COLUMNS: &str = "SELECT id, source, source_id, file_id, spec_json, release_title, \
     arr_path, size, ids_json, info_hash, state, last_error FROM shared_items";

fn row_to_item(row: &sqlx::sqlite::SqliteRow) -> Result<SharedItem> {
    let id: i64 = row.try_get("id")?;
    let malformed = |detail: String| StoreError::Malformed { id, detail };

    let source = match row.try_get::<String, _>("source")?.as_str() {
        "sonarr" => MediaSource::Sonarr,
        "radarr" => MediaSource::Radarr,
        other => return Err(malformed(format!("unknown source {other:?}"))),
    };

    let state = match row.try_get::<String, _>("state")?.as_str() {
        "pending" => ShareState::Pending,
        "seeding" => ShareState::Seeding,
        "unshared" => ShareState::Unshared,
        "failed" => ShareState::Failed,
        other => return Err(malformed(format!("unknown state {other:?}"))),
    };

    let spec: MediaSpec = serde_json::from_str(&row.try_get::<String, _>("spec_json")?)
        .map_err(|e| malformed(format!("spec_json: {e}")))?;
    let ids: ExternalIds = serde_json::from_str(&row.try_get::<String, _>("ids_json")?)
        .map_err(|e| malformed(format!("ids_json: {e}")))?;

    let size: i64 = row.try_get("size")?;

    Ok(SharedItem {
        id: Some(id),
        source,
        source_id: row.try_get("source_id")?,
        file_id: row.try_get("file_id")?,
        spec,
        release_title: row.try_get("release_title")?,
        arr_path: row.try_get::<String, _>("arr_path")?.into(),
        size: u64::try_from(size).unwrap_or(0),
        ids,
        info_hash: row.try_get("info_hash")?,
        state,
        last_error: row.try_get("last_error")?,
    })
}

pub(crate) fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::path::PathBuf;

    /// Invented titles only — no real show or movie names anywhere in the fixtures.
    fn episode(file_id: i64) -> SharedItem {
        SharedItem {
            id: None,
            source: MediaSource::Sonarr,
            source_id: 7,
            file_id,
            spec: MediaSpec::Episode {
                series_title: "Lanternwick Hollow".to_owned(),
                season: 2,
                episode: 4,
            },
            release_title: "Lanternwick.Hollow.S02E04.1080p.WEB-DL.x264-SHARERR".to_owned(),
            arr_path: PathBuf::from("/tv/Lanternwick Hollow/Season 02/s02e04.mkv"),
            size: 2_147_483_648,
            ids: ExternalIds {
                tvdb: Some(918_273),
                tmdb: None,
                tvmaze: Some(4_242),
                imdb: Some("tt7654321".to_owned()),
            },
            info_hash: None,
            state: ShareState::Pending,
            last_error: None,
        }
    }

    fn movie(file_id: i64) -> SharedItem {
        SharedItem {
            id: None,
            source: MediaSource::Radarr,
            source_id: 31,
            file_id,
            spec: MediaSpec::Movie {
                title: "The Gilded Ferry".to_owned(),
                year: Some(2019),
            },
            release_title: "The.Gilded.Ferry.2019.1080p.WEB-DL.x264-SHARERR".to_owned(),
            arr_path: PathBuf::from("/movies/The Gilded Ferry (2019)/gilded.ferry.mkv"),
            size: 8_589_934_592,
            ids: ExternalIds {
                tmdb: Some(555_444),
                ..ExternalIds::default()
            },
            info_hash: None,
            state: ShareState::Pending,
            last_error: None,
        }
    }

    /// `SharedItem` carries no timestamps, so read them back directly.
    async fn timestamps(store: &Store, file_id: i64) -> (i64, i64) {
        let row = sqlx::query("SELECT created_at, updated_at FROM shared_items WHERE file_id = ?1")
            .bind(file_id)
            .fetch_one(store.pool())
            .await
            .unwrap();
        (
            row.try_get("created_at").unwrap(),
            row.try_get("updated_at").unwrap(),
        )
    }

    #[tokio::test]
    async fn migrations_apply_to_a_fresh_database() {
        let store = Store::open_in_memory().await.unwrap();
        assert!(store.all_items().await.unwrap().is_empty());
        assert!(store.recent_runs(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn upsert_round_trips_every_field() {
        let store = Store::open_in_memory().await.unwrap();
        let item = episode(1001);
        let id = store.upsert(&item).await.unwrap();

        let got = store.get(MediaSource::Sonarr, 1001).await.unwrap().unwrap();
        assert_eq!(got.id, Some(id));
        // Compare on equal footing: `item` has no database id yet.
        assert_eq!(
            SharedItem {
                id: Some(id),
                ..item
            },
            got
        );
    }

    #[tokio::test]
    async fn get_is_scoped_by_source_as_well_as_file_id() {
        let store = Store::open_in_memory().await.unwrap();
        // Same file_id in both apps — only the pair is unique.
        store.upsert(&episode(500)).await.unwrap();
        store.upsert(&movie(500)).await.unwrap();

        let from_sonarr = store.get(MediaSource::Sonarr, 500).await.unwrap().unwrap();
        let from_radarr = store.get(MediaSource::Radarr, 500).await.unwrap().unwrap();
        assert_eq!(from_sonarr.source, MediaSource::Sonarr);
        assert_eq!(from_radarr.source, MediaSource::Radarr);
        assert_eq!(store.all_items().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn get_returns_none_for_an_unknown_key() {
        let store = Store::open_in_memory().await.unwrap();
        assert!(store.get(MediaSource::Sonarr, 999).await.unwrap().is_none());
    }

    /// The property the whole "running sync twice changes nothing" design rests on.
    #[tokio::test]
    async fn upsert_is_idempotent_on_the_natural_key() {
        let store = Store::open_in_memory().await.unwrap();
        let first = store.upsert(&episode(1001)).await.unwrap();
        let second = store.upsert(&episode(1001)).await.unwrap();

        assert_eq!(
            first, second,
            "re-discovery must reuse the row, not insert a second"
        );
        assert_eq!(store.all_items().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn upsert_preserves_created_at_and_advances_updated_at() {
        let store = Store::open_in_memory().await.unwrap();
        store.upsert(&episode(1001)).await.unwrap();

        // Backdate the row rather than sleeping: `now_epoch` has one-second
        // resolution, so a same-second re-upsert would prove nothing either way.
        sqlx::query("UPDATE shared_items SET created_at = 1000, updated_at = 1000")
            .execute(store.pool())
            .await
            .unwrap();

        let mut changed = episode(1001);
        changed.release_title = "Lanternwick.Hollow.S02E04.2160p.WEB-DL.x265-SHARERR".to_owned();
        store.upsert(&changed).await.unwrap();

        let (created, updated) = timestamps(&store, 1001).await;
        assert_eq!(
            created, 1000,
            "an item must not look newly shared after a re-sync"
        );
        assert!(
            updated > created,
            "updated_at should advance, got {updated}"
        );

        let got = store.get(MediaSource::Sonarr, 1001).await.unwrap().unwrap();
        assert_eq!(got.release_title, changed.release_title);
    }

    /// Re-running discovery must not forget which torrent an item is seeding.
    ///
    /// Discovery has no way to know the infohash, so the item it rebuilds carries
    /// `None`. If that overwrote the stored hash, a later withdrawal would have
    /// nothing to remove and the torrent would seed on after the row said
    /// otherwise.
    #[tokio::test]
    async fn upsert_never_clears_a_known_info_hash() {
        let store = Store::open_in_memory().await.unwrap();
        store.upsert(&episode(1001)).await.unwrap();
        store
            .set_info_hash(MediaSource::Sonarr, 1001, "abc123")
            .await
            .unwrap();

        // Exactly what the reconciliation loop re-upserts: no hash on it.
        let rediscovered = episode(1001);
        assert!(rediscovered.info_hash.is_none());
        store.upsert(&rediscovered).await.unwrap();

        let got = store.get(MediaSource::Sonarr, 1001).await.unwrap().unwrap();
        assert_eq!(got.info_hash.as_deref(), Some("abc123"));
    }

    #[tokio::test]
    async fn upsert_still_applies_a_new_info_hash() {
        let store = Store::open_in_memory().await.unwrap();
        store.upsert(&episode(1001)).await.unwrap();
        store
            .set_info_hash(MediaSource::Sonarr, 1001, "old")
            .await
            .unwrap();

        let mut replacement = episode(1001);
        replacement.info_hash = Some("new".to_owned());
        store.upsert(&replacement).await.unwrap();

        let got = store.get(MediaSource::Sonarr, 1001).await.unwrap().unwrap();
        assert_eq!(
            got.info_hash.as_deref(),
            Some("new"),
            "an explicit hash must still win"
        );
    }

    #[tokio::test]
    async fn set_state_and_info_hash_touch_only_their_own_row() {
        let store = Store::open_in_memory().await.unwrap();
        store.upsert(&episode(1001)).await.unwrap();
        store.upsert(&episode(1002)).await.unwrap();

        store
            .set_state(
                MediaSource::Sonarr,
                1001,
                ShareState::Failed,
                Some("qbit refused"),
            )
            .await
            .unwrap();
        store
            .set_info_hash(MediaSource::Sonarr, 1001, "abc123")
            .await
            .unwrap();

        let target = store.get(MediaSource::Sonarr, 1001).await.unwrap().unwrap();
        assert_eq!(target.state, ShareState::Failed);
        assert_eq!(target.last_error.as_deref(), Some("qbit refused"));
        assert_eq!(target.info_hash.as_deref(), Some("abc123"));

        let untouched = store.get(MediaSource::Sonarr, 1002).await.unwrap().unwrap();
        assert_eq!(untouched.state, ShareState::Pending);
        assert!(untouched.last_error.is_none());
        assert!(untouched.info_hash.is_none());
    }

    #[tokio::test]
    async fn set_state_clears_a_previous_error() {
        let store = Store::open_in_memory().await.unwrap();
        store.upsert(&episode(1001)).await.unwrap();

        store
            .set_state(
                MediaSource::Sonarr,
                1001,
                ShareState::Failed,
                Some("transient"),
            )
            .await
            .unwrap();
        store
            .set_state(MediaSource::Sonarr, 1001, ShareState::Seeding, None)
            .await
            .unwrap();

        let got = store.get(MediaSource::Sonarr, 1001).await.unwrap().unwrap();
        assert_eq!(got.state, ShareState::Seeding);
        assert!(
            got.last_error.is_none(),
            "a successful retry must not keep the stale error"
        );
    }

    /// The CHECK constraints pin the column to exactly what `as_str` emits; if the
    /// two ever drift apart this fails rather than writing a row nothing can read.
    #[tokio::test]
    async fn schema_rejects_states_outside_the_enum() {
        let store = Store::open_in_memory().await.unwrap();
        store.upsert(&episode(1001)).await.unwrap();

        let bad = sqlx::query("UPDATE shared_items SET state = 'bogus' WHERE file_id = 1001")
            .execute(store.pool())
            .await;
        assert!(
            bad.is_err(),
            "CHECK (state IN (...)) should have rejected this"
        );

        for state in [
            ShareState::Pending,
            ShareState::Seeding,
            ShareState::Unshared,
            ShareState::Failed,
        ] {
            store
                .set_state(MediaSource::Sonarr, 1001, state, None)
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn a_malformed_row_is_an_error_not_a_panic() {
        let store = Store::open_in_memory().await.unwrap();
        store.upsert(&episode(1001)).await.unwrap();

        sqlx::query("UPDATE shared_items SET spec_json = '{not json' WHERE file_id = 1001")
            .execute(store.pool())
            .await
            .unwrap();

        let err = store.all_items().await.unwrap_err();
        assert!(
            matches!(&err, StoreError::Malformed { detail, .. } if detail.starts_with("spec_json")),
            "expected a Malformed error naming the column, got {err:?}"
        );
    }

    #[tokio::test]
    async fn sync_runs_record_a_summary_newest_first() {
        let store = Store::open_in_memory().await.unwrap();

        let first = store.begin_run().await.unwrap();
        let summary = RunSummary {
            discovered: 12,
            added: 3,
            unshared: 1,
            failed: 0,
            error: None,
        };
        store.finish_run(first, &summary).await.unwrap();

        let second = store.begin_run().await.unwrap();

        let runs = store.recent_runs(10).await.unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].id, second);
        assert!(
            runs[0].finished_at.is_none(),
            "an in-flight run has no finish time"
        );
        assert_eq!(runs[1].id, first);
        assert!(runs[1].finished_at.is_some());
        assert_eq!(runs[1].summary, summary);
    }

    #[tokio::test]
    async fn recent_runs_honours_its_limit() {
        let store = Store::open_in_memory().await.unwrap();
        for _ in 0..5 {
            store.begin_run().await.unwrap();
        }
        assert_eq!(store.recent_runs(2).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn open_creates_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/deeper/sharerr.db");

        let store = Store::open(&path).await.unwrap();
        store.upsert(&movie(4242)).await.unwrap();
        assert!(path.exists());

        // Reopening the same file must find the data, and re-running migrations
        // against an already-migrated database must be a no-op.
        drop(store);
        let reopened = Store::open(&path).await.unwrap();
        assert_eq!(reopened.all_items().await.unwrap().len(), 1);
    }
}
