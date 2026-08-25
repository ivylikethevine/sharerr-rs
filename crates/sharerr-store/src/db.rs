//! SQLite-backed record of what sharerr has shared.
//!
//! Queries are built at runtime rather than with `sqlx::query!`, so building the
//! project never requires a live database or a checked-in `.sqlx` cache.

use std::path::Path;
use std::str::FromStr;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Row, SqlitePool};

use sharerr_core::endpoint::now_epoch;
use sharerr_core::model::{ExternalIds, MediaMeta, MediaSource, MediaSpec, ShareState, SharedItem};

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

/// Serialises [`Store::from_options`] within this process.
///
/// `ServeState::store()` and `Syncer::build` each open their own pool
/// against the same file independently, so on a fresh start (or an upgrade
/// with a pending migration) they can race each other in two ways: the very
/// first connection's WAL/pragma setup against a brand new file can itself
/// return `SQLITE_BUSY` immediately rather than waiting out `busy_timeout`,
/// and sqlx-sqlite's own migration `lock`/`unlock` are no-ops over
/// migrations that are plain `CREATE TABLE`/`ALTER TABLE ADD COLUMN` with no
/// `IF NOT EXISTS`, so a migration loser fails with "already exists". Either
/// way the feed answers 503, or the syncer is marked blocked until the next
/// retry. This only needs to be process-wide, not cross-process: two `serve`
/// processes pointed at the same database file is already a
/// misconfiguration this project does not support.
static MIGRATE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
/// sharerr's SQLite database: what is shared, and every reconciliation run.
///
/// Cheap to clone — it is a connection pool handle, not a connection.
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
        // Foreign keys on, matching the file-backed options — the peer-endpoint
        // cascade is a property tests must exercise the way production runs it.
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")?.foreign_keys(true);
        // A single connection: each new connection to `:memory:` would otherwise
        // get its own empty database.
        Self::from_options(opts, 1).await
    }

    async fn from_options(opts: SqliteConnectOptions, max_connections: u32) -> Result<Self> {
        // Held across both connecting and migrating: on a brand new file the
        // very first connection's WAL/pragma setup can itself race a second
        // pool doing the same (`SQLITE_BUSY`, immediate rather than retried —
        // `busy_timeout` does not cover it), before either reaches
        // `MIGRATOR.run`. See `MIGRATE_LOCK`.
        let _guard = MIGRATE_LOCK.lock().await;
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

    /// The underlying pool, for queries that live in sibling modules.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Every item sharerr knows about, in any state.
    pub async fn all_items(&self) -> Result<Vec<SharedItem>> {
        let rows = sqlx::query(SELECT_COLUMNS).fetch_all(&self.pool).await?;
        rows.iter().map(row_to_item).collect()
    }

    /// Whether this instance is currently seeding the torrent with this hash.
    ///
    /// The builtin tracker's admission check: it answers only for torrents sharerr
    /// made, so it never becomes an open tracker that strangers can register
    /// swarms on. Hits `idx_shared_items_info_hash`, because it runs on every
    /// announce from every peer.
    ///
    /// `Unshared` and `Failed` items are excluded deliberately — withdrawing a
    /// share has to stop the tracker introducing peers for it, or the swarm
    /// outlives the decision to leave it.
    pub async fn is_shared(&self, info_hash: &str) -> Result<bool> {
        let row = sqlx::query(
            "SELECT 1 AS present FROM shared_items \
             WHERE info_hash = ?1 AND state IN (?2, ?3) LIMIT 1",
        )
        .bind(info_hash)
        .bind(ShareState::Seeding.as_str())
        .bind(ShareState::Pending.as_str())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.is_some())
    }

    /// Every item currently being shared that `scope` may see, newest first.
    ///
    /// What the Torznab feed publishes. Filtered in SQL rather than by the caller
    /// for two reasons: an item without a torrent yet can never reach the feed —
    /// a release the friend's Sonarr can find but not download is worse than one
    /// it cannot see — and rows outside the caller's scope are skipped before
    /// their two JSON columns are decoded, which is most of a feed request's work
    /// for a narrowly-scoped friend.
    pub async fn seeding_items(&self, scope: crate::PeerScope) -> Result<Vec<SharedItem>> {
        let filter = ScopeFilter::new(scope);
        let sql = format!(
            "{SELECT_COLUMNS} WHERE {} ORDER BY created_at DESC, id DESC",
            filter.clause
        );
        let mut query = sqlx::query(&sql);
        for value in filter.binds() {
            query = query.bind(value);
        }
        let rows = query.fetch_all(&self.pool).await?;
        rows.iter().map(row_to_item).collect()
    }

    /// How many items `scope` may see right now, and their total size in bytes
    /// — the same rows [`Self::seeding_items`] returns, aggregated in SQL
    /// rather than decoded. The friends page asks this once per distinct
    /// scope on every load, and the status page asks it for
    /// [`crate::PeerScope::All`]; neither needs the rows themselves.
    pub async fn seeding_summary(&self, scope: crate::PeerScope) -> Result<SeedingSummary> {
        let filter = ScopeFilter::new(scope);
        let sql = format!(
            "SELECT COUNT(*), COALESCE(SUM(size), 0) FROM shared_items WHERE {}",
            filter.clause
        );
        let mut query = sqlx::query_as(&sql);
        for value in filter.binds() {
            query = query.bind(value);
        }
        let (count, size): (i64, i64) = query.fetch_one(&self.pool).await?;
        Ok(SeedingSummary { count, size })
    }

    /// How many items are currently seeding — the "n items shared" number, as a
    /// COUNT rather than a full decode of every row, because the status page
    /// asks on every load.
    pub async fn count_seeding(&self) -> Result<i64> {
        let count = sqlx::query_scalar(
            "SELECT COUNT(*) FROM shared_items WHERE state = ?1 AND info_hash IS NOT NULL",
        )
        .bind(ShareState::Seeding.as_str())
        .fetch_one(&self.pool)
        .await?;
        Ok(count)
    }

    /// One item by its stable identity. The pair is the key — `file_id` alone is not
    /// unique across the two *arr apps.
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

    /// One item by its info hash — the tracker's live swarms are keyed by
    /// hash, not `(source, file_id)`, so a topology view naming which torrent
    /// a swarm belongs to needs this instead of [`Self::get`].
    pub async fn item_by_info_hash(&self, info_hash: &str) -> Result<Option<SharedItem>> {
        let row = sqlx::query(&format!("{SELECT_COLUMNS} WHERE info_hash = ?1"))
            .bind(info_hash)
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
        let spec_json = encode_json(&item.spec)?;
        let ids_json = encode_json(&item.ids)?;
        // `None` stays SQL NULL rather than the string "null": the COALESCE below
        // distinguishes the two, and a JSON "null" would satisfy it forever.
        let media_json = item.media.as_ref().map(encode_json).transpose()?;

        let row = sqlx::query(
            r#"
            INSERT INTO shared_items (
                source, source_id, file_id, spec_json, release_title, arr_path,
                size, ids_json, info_hash, announce_token_fp, created_by_sharerr,
                state, last_error, created_at, updated_at, media_json
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14, ?15)
            ON CONFLICT (source, file_id) DO UPDATE SET
                source_id     = excluded.source_id,
                spec_json     = excluded.spec_json,
                release_title = excluded.release_title,
                arr_path      = excluded.arr_path,
                size          = excluded.size,
                ids_json      = excluded.ids_json,
                -- Never clear a known infohash: discovery rebuilds items with
                -- `info_hash: None`, and a plain assignment would drop the hash
                -- of a torrent qBittorrent is still seeding, leaving nothing to
                -- remove if the item is later untagged. `set_seeding` is what
                -- records it, and `set_state(Unshared)` retires it.
                info_hash     = COALESCE(excluded.info_hash, shared_items.info_hash),
                -- Same reasoning as info_hash: a rediscovery must not blank out a
                -- fingerprint that only `set_seeding`/`set_announce_token_fp` know.
                announce_token_fp = COALESCE(excluded.announce_token_fp, shared_items.announce_token_fp),
                -- Deliberately absent from this clause. A rediscovery describes
                -- a file, not a torrent, so it always arrives with `false` and
                -- assigning that would strip the claim `set_seeding` recorded —
                -- and with it the client's licence to remove the torrent on
                -- withdrawal. Only `set_seeding` writes this column.
                -- COALESCE for the same reason as info_hash, one step removed: a
                -- rediscovery whose *arr has not analysed the file yet, or whose
                -- probe failed this pass, arrives with NULL. Assigning it would
                -- throw away metadata an earlier pass established, and the feed
                -- would silently lose attributes it had been publishing. A source
                -- that later reports better metadata still overwrites.
                media_json    = COALESCE(excluded.media_json, shared_items.media_json),
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
        .bind(item.announce_token_fp.as_deref())
        .bind(i64::from(item.created_by_sharerr))
        .bind(item.state.as_str())
        .bind(item.last_error.as_deref())
        .bind(now)
        .bind(media_json.as_deref())
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

    /// Mark an item seeding under `info_hash`, in one write.
    ///
    /// The hash and the state always change together when a share lands — a
    /// seeding item must have one, since it is what the tracker admits announces
    /// against and what the feed publishes — and two UPDATEs here were two write
    /// transactions per newly-shared item, hundreds back-to-back on a first
    /// sync. A cleared `last_error` rides along: the share just succeeded.
    ///
    /// `announce_token_fp` is the fingerprint of the token embedded in the
    /// torrent just built, so the items page can show whether it is still the
    /// currently configured one — see [`Self::set_announce_token_fp`] for the
    /// case where the torrent already existed and only this needed confirming.
    ///
    /// `created_by_sharerr` says whether this call is recording a torrent
    /// sharerr added or one it adopted, and is the only thing that writes that
    /// column — see [`SharedItem::created_by_sharerr`], and the withdrawal path
    /// that reads it.
    pub async fn set_seeding(
        &self,
        source: MediaSource,
        file_id: i64,
        info_hash: &str,
        announce_token_fp: Option<&str>,
        created_by_sharerr: bool,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE shared_items SET info_hash = ?1, announce_token_fp = ?2, \
             created_by_sharerr = ?3, state = ?4, last_error = NULL, updated_at = ?5 \
             WHERE source = ?6 AND file_id = ?7",
        )
        .bind(info_hash)
        .bind(announce_token_fp)
        .bind(i64::from(created_by_sharerr))
        .bind(ShareState::Seeding.as_str())
        .bind(now_epoch())
        .bind(source.as_str())
        .bind(file_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Record which token an already-seeding item's torrent was just confirmed
    /// (or fixed) to be announcing with. Called from the sync pass's fast path,
    /// which only touches the tracker list — never [`Self::set_seeding`], which
    /// would also mean re-deriving the info hash for a torrent that has not
    /// changed.
    pub async fn set_announce_token_fp(
        &self,
        source: MediaSource,
        file_id: i64,
        announce_token_fp: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE shared_items SET announce_token_fp = ?1, updated_at = ?2 \
             WHERE source = ?3 AND file_id = ?4",
        )
        .bind(announce_token_fp)
        .bind(now_epoch())
        .bind(source.as_str())
        .bind(file_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Record the info hash of the torrent built for an item.
    ///
    /// A seeding item must have one: it is what the tracker admits announces against
    /// and what the feed publishes.
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

    /// Open a reconciliation run, returning its id for [`Self::finish_run`].
    pub async fn begin_run(&self) -> Result<i64> {
        let row = sqlx::query("INSERT INTO sync_runs (started_at) VALUES (?1) RETURNING id")
            .bind(now_epoch())
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get::<i64, _>("id")?)
    }

    /// Close a run and record what it did.
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

    /// The most recent runs, newest first. Also the readiness probe's cheap proof
    /// that the database is answering.
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

/// What is seeding within one scope, aggregated — see [`Store::seeding_summary`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SeedingSummary {
    pub count: i64,
    /// Sum of the items' `size` in bytes.
    pub size: i64,
}

/// The `WHERE` fragment selecting the seeding rows a [`crate::PeerScope`] may
/// see, and the binder that fills its placeholders in the same order.
///
/// One definition for [`Store::seeding_items`] and [`Store::seeding_summary`]:
/// the feed and the count shown next to a friend's name must agree about
/// what "TV only" admits, and two hand-written copies of this clause would
/// not stay agreeing.
struct ScopeFilter {
    clause: String,
    allowed: Vec<&'static str>,
    directory_kind: Option<&'static str>,
}

impl ScopeFilter {
    fn new(scope: crate::PeerScope) -> Self {
        // Derived from `PeerScope::allows` over the fixed source list — the SQL
        // fragment interpolates only the placeholder count, never input.
        let allowed: Vec<&'static str> = MediaSource::ALL
            .iter()
            .copied()
            .filter(|source| scope.allows(*source))
            .map(MediaSource::as_str)
            .collect();
        let placeholders = vec!["?"; allowed.len()].join(", ");

        // Items from the kind-scoped directory source carry no single app
        // identity, so a narrow scope admits them by the declared kind in
        // their spec instead — see `PeerScope::directory_kind`. Under `All`
        // the source list already includes it and this clause is absent.
        let kind_placeholders = vec!["?"; MediaSource::KIND_SCOPED.len()].join(", ");
        let directory_kind = scope.directory_kind();
        let kind_arm = match directory_kind {
            Some(_) => format!(
                " OR (source IN ({kind_placeholders}) AND json_extract(spec_json, '$.kind') = ?)"
            ),
            None => String::new(),
        };

        Self {
            clause: format!(
                "state = ? AND info_hash IS NOT NULL AND (source IN ({placeholders}){kind_arm})"
            ),
            allowed,
            directory_kind,
        }
    }

    /// Every placeholder value in `clause`, in the order it names them. All
    /// are static strings, so one list serves `query` and `query_as` alike.
    fn binds(&self) -> Vec<&'static str> {
        let mut binds = vec![ShareState::Seeding.as_str()];
        binds.extend(&self.allowed);
        if let Some(kind) = self.directory_kind {
            binds.extend(
                MediaSource::KIND_SCOPED
                    .iter()
                    .map(|source| source.as_str()),
            );
            binds.push(kind);
        }
        binds
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
/// What one reconciliation pass changed.
pub struct RunSummary {
    pub discovered: i64,
    pub added: i64,
    pub unshared: i64,
    pub failed: i64,
    pub error: Option<String>,
}

impl RunSummary {
    /// How this pass reads to an operator, and whether it should be marked as a
    /// failure.
    ///
    /// An outright error replaces the counts — there is nothing useful to say
    /// about a pass that did not finish. Otherwise only the counts worth acting
    /// on appear, so a quiet pass renders as an empty string rather than three
    /// zeroes.
    ///
    /// Lives beside the data because two pages render it: the status page's
    /// one-glance line and the run history on Diagnostics. They differ only in
    /// whether the discovered count leads, which is what `with_discovered` picks
    /// — when they were two copies they could disagree about the same run.
    pub fn describe(&self, with_discovered: bool) -> (String, bool) {
        if let Some(error) = &self.error {
            return (error.clone(), true);
        }

        let mut parts = Vec::new();
        if with_discovered {
            parts.push(format!("{} discovered", self.discovered));
        }
        if self.added > 0 {
            parts.push(format!("{} added", self.added));
        }
        if self.unshared > 0 {
            parts.push(format!("{} unshared", self.unshared));
        }
        if self.failed > 0 {
            parts.push(format!("{} failed", self.failed));
        }
        (parts.join(", "), self.failed > 0)
    }
}

#[derive(Debug, Clone)]
/// A completed run as stored, with its timing.
pub struct RunRecord {
    pub id: i64,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub summary: RunSummary,
}

const SELECT_COLUMNS: &str = "SELECT id, source, source_id, file_id, spec_json, release_title, \
     arr_path, size, ids_json, info_hash, announce_token_fp, created_by_sharerr, state, \
     last_error, created_at, media_json \
     FROM shared_items";

fn row_to_item(row: &sqlx::sqlite::SqliteRow) -> Result<SharedItem> {
    let id: i64 = row.try_get("id")?;
    let malformed = |detail: String| StoreError::Malformed { id, detail };

    // Decoded as `&str`, borrowing the row's own buffer instead of allocating:
    // all four of these are either parsed into an enum or handed straight to
    // serde, so a `String` would only be built to be dropped. This runs per row
    // of every feed request and every sync pass.
    let raw_source = row.try_get::<&str, _>("source")?;
    let source = MediaSource::parse(raw_source)
        .ok_or_else(|| malformed(format!("unknown source {raw_source:?}")))?;

    let raw_state = row.try_get::<&str, _>("state")?;
    let state = ShareState::parse(raw_state)
        .ok_or_else(|| malformed(format!("unknown state {raw_state:?}")))?;

    let spec: MediaSpec = serde_json::from_str(row.try_get::<&str, _>("spec_json")?)
        .map_err(|e| malformed(format!("spec_json: {e}")))?;
    let ids: ExternalIds = serde_json::from_str(row.try_get::<&str, _>("ids_json")?)
        .map_err(|e| malformed(format!("ids_json: {e}")))?;
    // Nullable, unlike the two above: rows predating the column have none, and so
    // does anything neither the *arr apps nor a probe could describe.
    let media: Option<MediaMeta> = row
        .try_get::<Option<&str>, _>("media_json")?
        .map(serde_json::from_str)
        .transpose()
        .map_err(|e| malformed(format!("media_json: {e}")))?;

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
        announce_token_fp: row.try_get("announce_token_fp")?,
        created_by_sharerr: row.try_get::<i64, _>("created_by_sharerr")? != 0,
        state,
        last_error: row.try_get("last_error")?,
        created_at: row.try_get("created_at").ok(),
        media,
    })
}

/// Serialise a column value to JSON, wrapping a failure as [`StoreError::Malformed`].
fn encode_json(value: &impl serde::Serialize) -> Result<String> {
    serde_json::to_string(value).map_err(|e| StoreError::Malformed {
        id: 0,
        detail: e.to_string(),
    })
}

/// Trim and bound a human-entered name, with the caller's own messages.
///
/// Usernames and peer labels share the same rule — non-blank, at most 64
/// characters — but the messages stay per-caller because they render straight
/// back to different forms.
pub(crate) fn validate_name(
    raw: &str,
    blank_msg: &'static str,
    long_msg: &'static str,
) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(StoreError::InvalidUser(blank_msg));
    }
    if trimmed.chars().count() > 64 {
        return Err(StoreError::InvalidUser(long_msg));
    }
    Ok(trimmed.to_owned())
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
                ..ExternalIds::default()
            },
            media: None,
            info_hash: None,
            announce_token_fp: None,
            created_by_sharerr: true,
            state: ShareState::Pending,
            last_error: None,
            created_at: None,
        }
    }

    fn sample_media() -> MediaMeta {
        MediaMeta {
            resolution: Some("1920x1080".to_owned()),
            video_codec: Some("x265".to_owned()),
            audio_channels: Some("5.1".to_owned()),
            ..MediaMeta::default()
        }
    }

    #[tokio::test]
    async fn media_metadata_survives_an_insert_and_a_read_back() {
        let store = Store::open_in_memory().await.unwrap();
        let item = SharedItem {
            media: Some(sample_media()),
            ..episode(1)
        };
        store.upsert(&item).await.unwrap();

        let back = store.all_items().await.unwrap();
        assert_eq!(back[0].media, Some(sample_media()));
    }

    /// A row from before the column existed, and one whose source could say
    /// nothing, are both "unknown" — and must load rather than fail the read.
    #[tokio::test]
    async fn an_item_with_no_metadata_round_trips_as_none() {
        let store = Store::open_in_memory().await.unwrap();
        store.upsert(&episode(1)).await.unwrap();

        let back = store.all_items().await.unwrap();
        assert_eq!(back[0].media, None);
    }

    /// The COALESCE in `upsert`. A rediscovery arrives with whatever the source
    /// says *this pass* — and an *arr that has not re-analysed the file, or a
    /// probe that failed, says nothing. Assigning that would silently strip
    /// attributes the feed had been publishing.
    #[tokio::test]
    async fn a_rediscovery_with_no_metadata_does_not_erase_what_is_known() {
        let store = Store::open_in_memory().await.unwrap();
        store
            .upsert(&SharedItem {
                media: Some(sample_media()),
                ..episode(1)
            })
            .await
            .unwrap();

        // The same file, rediscovered by a pass that learned nothing about it.
        store.upsert(&episode(1)).await.unwrap();

        let back = store.all_items().await.unwrap();
        assert_eq!(
            back[0].media,
            Some(sample_media()),
            "a silent pass must not unpublish what an earlier one established"
        );
    }

    /// The other half of that clause: a source that *does* report metadata still
    /// overwrites, or a first bad reading would be permanent.
    #[tokio::test]
    async fn a_rediscovery_with_better_metadata_replaces_the_old() {
        let store = Store::open_in_memory().await.unwrap();
        store
            .upsert(&SharedItem {
                media: Some(MediaMeta {
                    resolution: Some("1280x720".to_owned()),
                    ..MediaMeta::default()
                }),
                ..episode(1)
            })
            .await
            .unwrap();

        store
            .upsert(&SharedItem {
                media: Some(sample_media()),
                ..episode(1)
            })
            .await
            .unwrap();

        let back = store.all_items().await.unwrap();
        assert_eq!(back[0].media, Some(sample_media()));
    }

    /// Every source must survive an insert and a read-back.
    ///
    /// Guards against a schema `CHECK (source IN (...))` silently excluding a
    /// newly added source: such a row would fail at INSERT while the Rust side
    /// compiles clean, and nothing else would notice.
    #[tokio::test]
    async fn every_media_source_round_trips_through_the_store() {
        let store = Store::open_in_memory().await.unwrap();

        for (n, source) in MediaSource::ALL.iter().copied().enumerate() {
            let item = SharedItem {
                source,
                ..episode(n as i64 + 1)
            };
            store
                .upsert(&item)
                .await
                .unwrap_or_else(|err| panic!("could not store a {source} item: {err}"));
        }

        let stored = store.all_items().await.unwrap();
        let sources: std::collections::HashSet<MediaSource> =
            stored.iter().map(|item| item.source).collect();
        assert_eq!(sources.len(), MediaSource::ALL.len());
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
            media: None,
            info_hash: None,
            announce_token_fp: None,
            created_by_sharerr: true,
            state: ShareState::Pending,
            last_error: None,
            created_at: None,
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

        // `created_at` is assigned by the store, so a freshly discovered item does
        // not carry one and this is the only place it can be checked. It is not
        // cosmetic: it becomes the feed's `pubDate`, and Sonarr rejects an entire
        // feed whose items lack one.
        assert!(
            got.created_at.is_some(),
            "the store must stamp created_at; the Torznab feed depends on it"
        );

        // Compare the rest on equal footing: `item` has neither a database id nor a
        // timestamp yet.
        assert_eq!(
            SharedItem {
                id: Some(id),
                created_at: got.created_at,
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
    async fn set_seeding_records_the_token_fingerprint_alongside_the_hash() {
        let store = Store::open_in_memory().await.unwrap();
        store.upsert(&episode(1001)).await.unwrap();

        store
            .set_seeding(MediaSource::Sonarr, 1001, "abc123", Some("fp1"), true)
            .await
            .unwrap();

        let got = store.get(MediaSource::Sonarr, 1001).await.unwrap().unwrap();
        assert_eq!(got.state, ShareState::Seeding);
        assert_eq!(got.info_hash.as_deref(), Some("abc123"));
        assert_eq!(got.announce_token_fp.as_deref(), Some("fp1"));
    }

    /// The fast path for an item that already exists: only the fingerprint
    /// moves, nothing else — proving `set_announce_token_fp` cannot be
    /// confused with `set_seeding` at the SQL layer.
    #[tokio::test]
    async fn set_announce_token_fp_touches_only_the_fingerprint() {
        let store = Store::open_in_memory().await.unwrap();
        store.upsert(&episode(1001)).await.unwrap();
        store
            .set_seeding(MediaSource::Sonarr, 1001, "abc123", Some("fp1"), true)
            .await
            .unwrap();

        store
            .set_announce_token_fp(MediaSource::Sonarr, 1001, Some("fp2"))
            .await
            .unwrap();

        let got = store.get(MediaSource::Sonarr, 1001).await.unwrap().unwrap();
        assert_eq!(got.info_hash.as_deref(), Some("abc123"), "must not change");
        assert_eq!(got.state, ShareState::Seeding, "must not change");
        assert_eq!(got.announce_token_fp.as_deref(), Some("fp2"));
    }

    /// A rediscovery (`upsert` with `announce_token_fp: None`) must not blank
    /// out a fingerprint only `set_seeding`/`set_announce_token_fp` know —
    /// same COALESCE reasoning as `info_hash`.
    #[tokio::test]
    async fn upsert_never_blanks_an_existing_fingerprint() {
        let store = Store::open_in_memory().await.unwrap();
        store.upsert(&episode(1001)).await.unwrap();
        store
            .set_seeding(MediaSource::Sonarr, 1001, "abc123", Some("fp1"), true)
            .await
            .unwrap();

        // A plain rediscovery, as `Discovered::into_shared_item` produces one.
        store.upsert(&episode(1001)).await.unwrap();

        let got = store.get(MediaSource::Sonarr, 1001).await.unwrap().unwrap();
        assert_eq!(got.announce_token_fp.as_deref(), Some("fp1"));
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

    /// The closed set of states lives in `ShareState`, not a SQL CHECK
    /// constraint, which would silently reject any state a future version adds.
    /// Writes only ever bind `as_str`, and a row something else scribbled a bad
    /// state into must surface as `Malformed` on read, never decode to a wrong
    /// state.
    #[tokio::test]
    async fn a_state_outside_the_enum_is_malformed_on_read() {
        let store = Store::open_in_memory().await.unwrap();
        store.upsert(&episode(1001)).await.unwrap();

        sqlx::query("UPDATE shared_items SET state = 'bogus' WHERE file_id = 1001")
            .execute(store.pool())
            .await
            .unwrap();

        let err = store.get(MediaSource::Sonarr, 1001).await.unwrap_err();
        assert!(matches!(err, StoreError::Malformed { .. }), "got {err:?}");

        for state in ShareState::ALL.iter().copied() {
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
    async fn is_shared_admits_only_live_torrents() {
        // The tracker's admission check. A withdrawn share must stop being served,
        // or the swarm outlives the decision to leave it.
        let store = Store::open_in_memory().await.unwrap();
        let mut item = episode(1);
        item.info_hash = Some("aa".repeat(20));
        item.state = ShareState::Seeding;
        store.upsert(&item).await.unwrap();

        assert!(store.is_shared(&"aa".repeat(20)).await.unwrap());
        assert!(
            !store.is_shared(&"bb".repeat(20)).await.unwrap(),
            "unknown hash"
        );

        store
            .set_state(MediaSource::Sonarr, 1, ShareState::Unshared, None)
            .await
            .unwrap();
        assert!(
            !store.is_shared(&"aa".repeat(20)).await.unwrap(),
            "an unshared item must stop being tracked"
        );
    }

    #[tokio::test]
    async fn seeding_items_excludes_anything_without_a_torrent() {
        let store = Store::open_in_memory().await.unwrap();

        // Pending, no info_hash: discovered but not built yet.
        store.upsert(&episode(1)).await.unwrap();

        let mut ready = episode(2);
        ready.info_hash = Some("cc".repeat(20));
        ready.state = ShareState::Seeding;
        store.upsert(&ready).await.unwrap();

        let feed = store.seeding_items(crate::PeerScope::All).await.unwrap();
        assert_eq!(feed.len(), 1, "only the seeding item belongs in the feed");
        assert_eq!(feed[0].file_id, 2);
    }

    /// The aggregate must count and size exactly the rows the feed returns:
    /// same filter, same scope semantics — a pending item and an out-of-scope
    /// one contribute to neither.
    #[tokio::test]
    async fn seeding_summary_aggregates_the_same_rows_the_feed_returns() {
        let store = Store::open_in_memory().await.unwrap();

        let seeding = |mut item: SharedItem, hash: &str, size: u64| {
            item.info_hash = Some(hash.repeat(20));
            item.state = ShareState::Seeding;
            item.size = size;
            item
        };
        store.upsert(&episode(1)).await.unwrap(); // pending: excluded
        store
            .upsert(&seeding(episode(2), "aa", 1_000))
            .await
            .unwrap();
        store
            .upsert(&seeding(episode(3), "bb", 2_000))
            .await
            .unwrap();
        store.upsert(&seeding(movie(4), "cc", 4_000)).await.unwrap();

        let all = store.seeding_summary(crate::PeerScope::All).await.unwrap();
        assert_eq!(
            all,
            SeedingSummary {
                count: 3,
                size: 7_000
            }
        );

        let tv = store.seeding_summary(crate::PeerScope::Tv).await.unwrap();
        let feed = store.seeding_items(crate::PeerScope::Tv).await.unwrap();
        assert_eq!(tv.count as usize, feed.len());
        assert_eq!(
            tv.size.unsigned_abs(),
            feed.iter().map(|i| i.size).sum::<u64>()
        );
        assert_eq!(
            tv,
            SeedingSummary {
                count: 2,
                size: 3_000
            }
        );

        let empty = Store::open_in_memory().await.unwrap();
        assert_eq!(
            empty.seeding_summary(crate::PeerScope::All).await.unwrap(),
            SeedingSummary::default(),
            "an empty table must sum to zero, not NULL"
        );
    }

    /// Directory items have no app identity, so narrow scopes admit them by
    /// the declared kind in their spec — a tv-scoped friend sees a directory
    /// *episode* but not a directory *movie*, and Whisparr stays excluded.
    #[tokio::test]
    async fn narrow_scopes_admit_directory_items_by_spec_kind() {
        let store = Store::open_in_memory().await.unwrap();

        let seeding = |mut item: SharedItem, hash: &str| {
            item.info_hash = Some(hash.repeat(20));
            item.state = ShareState::Seeding;
            item
        };

        store.upsert(&seeding(episode(1), "aa")).await.unwrap();
        let directory_episode = SharedItem {
            source: MediaSource::Directory,
            ..seeding(episode(2), "bb")
        };
        store.upsert(&directory_episode).await.unwrap();
        let directory_movie = SharedItem {
            source: MediaSource::Directory,
            ..seeding(movie(3), "cc")
        };
        store.upsert(&directory_movie).await.unwrap();
        let whisparr_episode = SharedItem {
            source: MediaSource::Whisparr,
            ..seeding(episode(4), "dd")
        };
        store.upsert(&whisparr_episode).await.unwrap();

        let ids = |items: Vec<SharedItem>| {
            let mut ids: Vec<i64> = items.iter().map(|i| i.file_id).collect();
            ids.sort_unstable();
            ids
        };

        let tv = store.seeding_items(crate::PeerScope::Tv).await.unwrap();
        assert_eq!(ids(tv), vec![1, 2]);

        let movies = store.seeding_items(crate::PeerScope::Movies).await.unwrap();
        assert_eq!(ids(movies), vec![3]);

        let music = store.seeding_items(crate::PeerScope::Music).await.unwrap();
        assert_eq!(ids(music), Vec::<i64>::new());

        let all = store.seeding_items(crate::PeerScope::All).await.unwrap();
        assert_eq!(ids(all), vec![1, 2, 3, 4]);
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

    /// `ServeState::store()` and `Syncer::build` each open their own pool
    /// against the same file independently, so on a fresh start they can
    /// both reach `MIGRATOR.run` at once — see `MIGRATE_LOCK`. Without it
    /// this races on sqlx-sqlite's own migration lock being a no-op and the
    /// migrations having no `IF NOT EXISTS`: the loser fails with "already
    /// exists".
    #[tokio::test]
    async fn concurrent_first_opens_against_a_fresh_file_both_succeed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sharerr.db");

        let (a, b) = tokio::join!(Store::open(&path), Store::open(&path));
        a.unwrap();
        b.unwrap();
    }
}
