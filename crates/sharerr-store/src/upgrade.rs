//! The upgrade rehearsal: a database an older release left behind, with rows
//! in it, opened by this one.
//!
//! Every other test here migrates an empty database. That never exercises
//! the thing an upgrade actually does: run the newer migrations over tables
//! that already hold data, then read that data back through today's code.
//! An `ALTER TABLE ... ADD COLUMN` without a default, a table rebuild that
//! drops a column, a `NOT NULL` the old rows cannot satisfy, or a decoder
//! that assumes a column every row has, all pass against an empty database
//! and fail the first operator who upgrades.
//!
//! So for every migration level `k`, this builds a file-backed database at
//! exactly that level, fills it with rows using only the columns that
//! existed then (literal SQL and literal JSON: what an older release wrote,
//! not what today's serializer would write), and opens it with
//! [`Store::open`], the production path, which applies the rest. Then it
//! reads everything back through the store's own API and checks SQLite's
//! own view of the file. Adding a migration means adding its rows to
//! [`seed`] and its expectation to [`check`]; the loop picks the new level
//! up on its own.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::borrow::Cow;
use std::path::Path;

use sharerr_core::model::{MediaSource, MediaSpec, ShareState};
use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{AssertSqlSafe, Row};

use crate::db::{MIGRATOR, Store};
use crate::{EndpointKind, PeerScope};

/// The migrations up to and including `version`, as a migrator of their own.
///
/// `Migrator`'s fields are public for `sqlx::migrate!`'s benefit and marked
/// semver-exempt; this is the one place in the tree that reads them, so a
/// sqlx release that reshapes the struct breaks exactly here and nowhere
/// else.
fn migrator_up_to(version: i64) -> Migrator {
    Migrator {
        migrations: Cow::Owned(
            MIGRATOR
                .iter()
                .filter(|migration| migration.version <= version)
                .cloned()
                .collect(),
        ),
        ignore_missing: MIGRATOR.ignore_missing,
        locking: MIGRATOR.locking,
        no_tx: MIGRATOR.no_tx,
        table_name: MIGRATOR.table_name.clone(),
        create_schemas: MIGRATOR.create_schemas.clone(),
    }
}

/// The literal JSON an older release wrote into `spec_json`, `ids_json`
/// and `media_json`. Literals rather than `serde_json::to_string` of
/// today's types: the model's serde shape has not changed since the first
/// release, and a future change to it must fail here, against what is
/// actually on disk, not quietly re-serialize itself into passing.
const MOVIE_SPEC: &str = r#"{"kind":"movie","title":"The Gilded Ferry","year":2019}"#;
const MOVIE_IDS: &str = r#"{"tvdb":null,"tmdb":555444,"tvmaze":null,"imdb":"tt0000001"}"#;
const EPISODE_SPEC: &str =
    r#"{"kind":"episode","series_title":"Harbour Lights","season":2,"episode":4}"#;
const EPISODE_IDS: &str = r#"{"tvdb":77,"tmdb":null,"tvmaze":null,"imdb":null}"#;
const TRACK_SPEC: &str =
    r#"{"kind":"track","artist":"Quiet Harbour","album":"Low Tide","track":null}"#;
const TRACK_IDS: &str =
    r#"{"tvdb":null,"tmdb":null,"tvmaze":null,"imdb":null,"musicbrainz":"mb-1"}"#;
const MEDIA: &str = r#"{"resolution":"1080p","video_codec":"x264"}"#;

const SEEDING_HASH: &str = "0123456789abcdef0123456789abcdef01234567";

/// Rows an instance at migration level `version` could have held, written
/// with that level's columns only. Each block names the migration that made
/// it possible.
async fn seed(pool: &sqlx::SqlitePool, version: i64) {
    // `AssertSqlSafe`: every string below is this file's own literal.
    let run = |sql: String| async move {
        sqlx::raw_sql(AssertSqlSafe(sql))
            .execute(pool)
            .await
            .unwrap();
    };

    // 0001: items and runs. `source` and `state` are inside 0001's CHECK
    // constraints, which 0005 later drops.
    run(format!(
        "INSERT INTO shared_items (source, source_id, file_id, spec_json, release_title, \
         arr_path, size, ids_json, info_hash, state, last_error, created_at, updated_at) VALUES \
         ('radarr', 31, 1001, '{MOVIE_SPEC}', 'The.Gilded.Ferry.2019.1080p.WEB-DL.x264-FAKEGRP', \
         '/movies/The Gilded Ferry (2019)/gilded.ferry.mkv', 8589934592, '{MOVIE_IDS}', \
         '{SEEDING_HASH}', 'seeding', NULL, 1700000000, 1700000000), \
         ('sonarr', 12, 2002, '{EPISODE_SPEC}', 'Harbour.Lights.S02E04.720p.WEB-DL-FAKEGRP', \
         '/tv/Harbour Lights/Season 02/s02e04.mkv', 734003200, '{EPISODE_IDS}', \
         NULL, 'failed', 'client refused', 1700000100, 1700000200)"
    ))
    .await;
    run("INSERT INTO sync_runs (started_at, finished_at, discovered, added, unshared, failed, error) \
         VALUES (1700000000, 1700000030, 2, 1, 0, 1, NULL), (1700003600, NULL, 0, 0, 0, 0, NULL)".to_owned())
        .await;

    if version >= 2 {
        // 0002: a login. Any PHC string will do for the row; nothing here
        // verifies it.
        run(
            "INSERT INTO users (username, password_hash, created_at, updated_at) VALUES \
             ('ivy', '$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$AAAAAAAAAAAAAAAAAAAAAA', \
             1700000000, 1700000000)"
                .to_owned(),
        )
        .await;
    }
    if version >= 3 {
        // 0003: two friends, one of them revoked.
        run(
            "INSERT INTO peers (label, key_hash, created_at, last_seen_at, revoked_at) VALUES \
             ('Sam', 'aa', 1700000000, 1700000500, NULL), \
             ('Alex', 'bb', 1700000001, NULL, 1700000600)"
                .to_owned(),
        )
        .await;
    }
    if version >= 4 {
        // 0004: a scope narrower than the default.
        run("UPDATE peers SET scope = 'tv' WHERE label = 'Sam'".to_owned()).await;
    }
    if version >= 5 {
        // 0005: a source 0001's CHECK would have refused.
        run(format!(
            "INSERT INTO shared_items (source, source_id, file_id, spec_json, release_title, \
             arr_path, size, ids_json, info_hash, state, last_error, created_at, updated_at) VALUES \
             ('lidarr', 5, 3003, '{TRACK_SPEC}', 'Quiet.Harbour-Low.Tide-FLAC-FAKEGRP', \
             '/music/Quiet Harbour/Low Tide', 524288000, '{TRACK_IDS}', NULL, 'pending', NULL, \
             1700000300, 1700000300)"
        )
        )
        .await;
    }
    if version >= 6 {
        // 0006: an observed address, and a gossip identity.
        run(
            "INSERT INTO peer_endpoints (peer_id, kind, addr, observed_at, via) VALUES \
             ((SELECT id FROM peers WHERE label = 'Sam'), 'client', '203.0.113.9:51413', \
             1700000500, 'direct')"
                .to_owned(),
        )
        .await;
        run(
            "UPDATE peers SET pubkey = 'cafe', gossip_url = 'https://sam.example:8477', \
             gossip_record = '{}' WHERE label = 'Sam'"
                .to_owned(),
        )
        .await;
    }
    if version >= 7 {
        run("UPDATE shared_items SET announce_token_fp = 'fp-1' WHERE file_id = 1001".to_owned())
            .await;
    }
    if version >= 8 {
        run("UPDATE shared_items SET created_by_sharerr = 0 WHERE file_id = 1001".to_owned()).await;
    }
    if version >= 9 {
        run(format!(
            "UPDATE shared_items SET media_json = '{MEDIA}' WHERE file_id = 1001"
        ))
        .await;
    }
    if version >= 10 {
        run(
            "UPDATE shared_items SET achieved_ratio = 1.5, ratio_limit_reported = 2.0 \
             WHERE file_id = 1001"
                .to_owned(),
        )
        .await;
    }
    if version >= 11 {
        run(
            "INSERT INTO swarm_samples (sampled_at, swarms, peers, seeders) VALUES \
             (1700003600, 1, 3, 1)"
                .to_owned(),
        )
        .await;
    }
    if version >= 12 {
        run("UPDATE shared_items SET private = 0 WHERE file_id = 1001".to_owned()).await;
    }
    if version >= 13 {
        run("UPDATE peers SET downloaded = 4096, uploaded = 512 WHERE label = 'Sam'".to_owned())
            .await;
        run("UPDATE transfer_totals SET downloaded = 8192, uploaded = 512, updated_at = 1700003600 \
             WHERE id = 1".to_owned())
            .await;
    }
}

/// What today's code must read back from a database seeded at `version`
/// and migrated the rest of the way. A column the seed could not have
/// written reads as the migration's own default, which is the contract an
/// upgrade keeps.
async fn check(store: &Store, version: i64) {
    let items = store.all_items().await.unwrap();
    assert_eq!(
        items.len(),
        if version >= 5 { 3 } else { 2 },
        "v{version}: every seeded item must survive"
    );

    let movie = items.iter().find(|item| item.file_id == 1001).unwrap();
    assert_eq!(movie.source, MediaSource::Radarr);
    assert_eq!(movie.state, ShareState::Seeding);
    assert_eq!(movie.info_hash.as_deref(), Some(SEEDING_HASH));
    assert!(store.is_shared(SEEDING_HASH).await.unwrap());
    assert!(
        matches!(&movie.spec, MediaSpec::Movie { title, year: Some(2019) } if title == "The Gilded Ferry")
    );
    assert_eq!(movie.ids.tmdb, Some(555_444));
    assert_eq!(movie.ids.imdb.as_deref(), Some("tt0000001"));
    assert_eq!(movie.size, 8_589_934_592);
    assert_eq!(
        movie.announce_token_fp.as_deref(),
        (version >= 7).then_some("fp-1")
    );
    assert_eq!(
        movie.created_by_sharerr,
        version < 8,
        "v{version}: 0008 backfills existing rows as sharerr-made"
    );
    assert_eq!(
        movie.media.as_ref().and_then(|m| m.resolution.as_deref()),
        (version >= 9).then_some("1080p")
    );
    assert_eq!(movie.achieved_ratio, (version >= 10).then_some(1.5));
    assert_eq!(movie.ratio_limit_reported, (version >= 10).then_some(2.0));
    assert_eq!(
        movie.private,
        version < 12,
        "v{version}: 0012 backfills existing rows as private"
    );

    let episode = items.iter().find(|item| item.file_id == 2002).unwrap();
    assert_eq!(episode.state, ShareState::Failed);
    assert_eq!(episode.last_error.as_deref(), Some("client refused"));
    assert!(matches!(
        &episode.spec,
        MediaSpec::Episode {
            season: 2,
            episode: 4,
            ..
        }
    ));
    assert_eq!(episode.ids.tvdb, Some(77));
    assert!(episode.media.is_none());

    if version >= 5 {
        let track = items.iter().find(|item| item.file_id == 3003).unwrap();
        assert_eq!(track.source, MediaSource::Lidarr);
        assert!(matches!(&track.spec, MediaSpec::Track { track: None, .. }));
        assert_eq!(track.ids.musicbrainz.as_deref(), Some("mb-1"));
    }

    let runs = store.recent_runs(10).await.unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!(
        runs.iter().filter(|run| run.finished_at.is_some()).count(),
        1
    );
    assert_eq!(runs.iter().map(|run| run.summary.added).sum::<i64>(), 1);

    assert_eq!(store.user_count().await.unwrap(), i64::from(version >= 2));

    let peers = store.list_peers().await.unwrap();
    if version >= 3 {
        assert_eq!(peers.len(), 2);
        let sam = peers.iter().find(|peer| peer.label == "Sam").unwrap();
        let alex = peers.iter().find(|peer| peer.label == "Alex").unwrap();
        assert!(!sam.is_revoked());
        assert!(alex.is_revoked());
        assert_eq!(sam.last_seen_at, Some(1_700_000_500));
        assert_eq!(
            sam.scope,
            if version >= 4 {
                PeerScope::Tv
            } else {
                PeerScope::All
            },
            "v{version}: 0004 backfills the scope as 'all'"
        );
        assert_eq!(sam.pubkey.as_deref(), (version >= 6).then_some("cafe"));
        assert_eq!(
            sam.gossip_url.as_deref(),
            (version >= 6).then_some("https://sam.example:8477")
        );
        assert_eq!(
            store.peer_by_key_hash("aa").await.unwrap().map(|p| p.id),
            Some(sam.id),
            "the issued key must still authenticate"
        );
        assert_eq!(
            store.peer_by_key_hash("bb").await.unwrap(),
            None,
            "the revoked key must still be refused"
        );

        let endpoints = store.peer_endpoints(sam.id).await.unwrap();
        if version >= 6 {
            assert_eq!(endpoints.len(), 1);
            assert_eq!(endpoints[0].kind, EndpointKind::Client);
            assert_eq!(endpoints[0].addr, "203.0.113.9:51413");
            assert_eq!(
                store.peer_gossip_record(sam.id).await.unwrap().as_deref(),
                Some("{}")
            );
        } else {
            assert!(endpoints.is_empty());
        }

        let (downloaded, uploaded) = if version >= 13 { (4096, 512) } else { (0, 0) };
        assert_eq!((sam.downloaded, sam.uploaded), (downloaded, uploaded));
        assert_eq!((alex.downloaded, alex.uploaded), (0, 0));
    } else {
        assert!(peers.is_empty());
    }

    let samples = store.recent_swarm_samples(10).await.unwrap();
    assert_eq!(samples.len(), usize::from(version >= 11));
    assert_eq!(
        store.last_active_swarm_sample_at().await.unwrap(),
        (version >= 11).then_some(1_700_003_600)
    );

    let totals = store.transfer_totals().await.unwrap();
    if version >= 13 {
        assert_eq!((totals.downloaded, totals.uploaded), (8192, 512));
        assert_eq!(totals.updated_at, Some(1_700_003_600));
    } else {
        assert_eq!(totals, crate::TransferTotals::default());
    }
}

/// Writes after the upgrade must work too, not just reads: the rebuilt or
/// altered tables keep their constraints, their defaults and their
/// autoincrement. Run once, after every read-only check, since it changes
/// what those checks expect.
async fn writes_still_work(store: &Store, version: i64) {
    store.record_transfer(None, 1, 0).await.unwrap();
    let totals = store.transfer_totals().await.unwrap();
    assert_eq!(totals.downloaded, if version >= 13 { 8193 } else { 1 });

    let sam = store
        .create_peer(
            "Robin",
            &secrecy::SecretString::from("robin-key"),
            PeerScope::Movies,
        )
        .await
        .unwrap();
    assert!(store.touch_peer(sam.id).await.unwrap().updated());
    store
        .record_peer_endpoint(
            sam.id,
            EndpointKind::Api,
            "203.0.113.10",
            Some(1_700_010_000),
            crate::ObservedVia::Direct,
        )
        .await
        .unwrap();
    assert_eq!(store.peer_endpoints(sam.id).await.unwrap().len(), 1);
    assert!(store.delete_peer(sam.id).await.unwrap());
    assert!(
        store.peer_endpoints(sam.id).await.unwrap().is_empty(),
        "the endpoint cascade must hold on the upgraded file"
    );
    assert_eq!(
        store.list_peers().await.unwrap().len(),
        if version >= 3 { 2 } else { 0 }
    );
}

/// SQLite's own view of the migrated file, independent of what the store
/// decodes: a consistent file and no dangling foreign key.
async fn file_is_sound(store: &Store) {
    let verdict: String = sqlx::query("PRAGMA integrity_check")
        .fetch_one(store.pool())
        .await
        .unwrap()
        .try_get(0)
        .unwrap();
    assert_eq!(verdict, "ok");
    let violations = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(store.pool())
        .await
        .unwrap();
    assert!(
        violations.is_empty(),
        "{} foreign key violation(s)",
        violations.len()
    );
}

/// A database at exactly migration level `version`, seeded, on disk, with
/// its pool closed so [`Store::open`] is the next thing to touch it.
async fn database_at(path: &Path, version: i64) {
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .unwrap();
    migrator_up_to(version).run(&pool).await.unwrap();
    seed(&pool, version).await;
    pool.close().await;
}

fn latest() -> i64 {
    MIGRATOR.iter().map(|m| m.version).max().unwrap()
}

/// sqlx applies migrations in version order and refuses a gap only once a
/// later version is already applied; a skipped number is otherwise legal
/// and would make the rehearsal's level loop silently miss a step.
#[test]
fn migrations_are_numbered_contiguously_from_one() {
    let versions: Vec<i64> = MIGRATOR.iter().map(|m| m.version).collect();
    let expected: Vec<i64> = (1..=latest()).collect();
    assert_eq!(versions, expected);
}

/// The rehearsal itself: one upgrade from every level that has ever existed.
#[tokio::test]
async fn a_database_from_every_earlier_level_upgrades_with_its_rows_intact() {
    for version in 1..=latest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sharerr.db");
        database_at(&path, version).await;

        let store = Store::open(&path)
            .await
            .unwrap_or_else(|err| panic!("upgrading from v{version} failed: {err}"));
        check(&store, version).await;
        file_is_sound(&store).await;

        // The upgraded file reopens as a current one: nothing left pending,
        // nothing re-applied.
        drop(store);
        let again = Store::open(&path).await.unwrap();
        check(&again, version).await;
        writes_still_work(&again, version).await;
        file_is_sound(&again).await;
    }
}

/// A database already at the current level is left alone: the same file
/// must open twice with every migration recorded exactly once.
#[tokio::test]
async fn a_current_database_records_each_migration_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sharerr.db");
    let store = Store::open(&path).await.unwrap();
    drop(store);
    let store = Store::open(&path).await.unwrap();

    let applied: i64 = sqlx::query("SELECT COUNT(*) AS n FROM _sqlx_migrations")
        .fetch_one(store.pool())
        .await
        .unwrap()
        .try_get("n")
        .unwrap();
    assert_eq!(applied, latest());
}
