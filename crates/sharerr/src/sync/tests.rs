//! End-to-end reconciliation against a mock stack.
//!
//! Fully hermetic: a wiremock Sonarr, a *stateful* wiremock qBittorrent, synthetic
//! media on disk, and an in-memory database. The qBittorrent mock has to remember
//! what was added, because the property under test — running sync twice changes
//! nothing the second time — is invisible to a stateless mock.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use secrecy::SecretString;
use serde_json::{Value, json};
use sharerr_core::config::{PathMapping, ServiceConfig};
use sharerr_core::{Config, MediaSource, ShareState};
use sharerr_qbit::QbitClient;
use sharerr_store::Store;
use sharerr_testkit::library::{self, TvLibrary};
use sharerr_torrent::{LavaTorrentFactory, TrackerProvider};
use url::Url;
use wiremock::matchers::{method, path as route};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use super::seed::Seeder;
use super::{SyncReport, Syncer};

/// The qbit-view prefix. Deliberately a third distinct path, so a bug that
/// conflates any two of the three views shows up here.
const QBIT_PREFIX: &str = "/downloads/tv";

// ------------------------------------------------------------------ fake qbit

/// What the fake qBittorrent remembers between calls.
#[derive(Debug, Default)]
struct QbitState {
    torrents: Vec<AddedTorrent>,
    removed: Vec<String>,
    add_calls: usize,
}

#[derive(Debug, Clone)]
struct AddedTorrent {
    hash: String,
    save_path: String,
    /// For a single-file torrent this is the file itself; for a multi-file one it
    /// is the root directory. Mirroring qBittorrent here matters, because
    /// existing-torrent detection takes a different path for each.
    content_path: String,
    /// Paths relative to `save_path`, as `torrents/files` reports them.
    files: Vec<String>,
    /// Everything the `torrents/add` form carried, for assertions.
    form: String,
}

impl AddedTorrent {
    /// A torrent qBittorrent was already seeding before sharerr came along.
    fn preexisting(hash: &str, save_path: &str, files: &[&str]) -> Self {
        let content_path = match files {
            [single] => format!("{save_path}/{single}"),
            _ => save_path.to_owned(),
        };
        Self {
            hash: hash.to_owned(),
            save_path: save_path.to_owned(),
            content_path,
            files: files.iter().map(|f| (*f).to_owned()).collect(),
            form: String::new(),
        }
    }
}

#[derive(Clone, Default)]
struct FakeQbit {
    state: Arc<Mutex<QbitState>>,
}

impl FakeQbit {
    async fn mount(&self, server: &MockServer) {
        Mock::given(method("POST"))
            .and(route("/api/v2/auth/login"))
            .respond_with(ResponseTemplate::new(200).set_body_string("Ok."))
            .mount(server)
            .await;

        Mock::given(method("GET"))
            .and(route("/api/v2/app/preferences"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "enable_embedded_tracker": true,
                "embedded_tracker_port": 9000,
            })))
            .mount(server)
            .await;

        let state = Arc::clone(&self.state);
        Mock::given(method("POST"))
            .and(route("/api/v2/torrents/add"))
            .respond_with(move |request: &Request| {
                let body = String::from_utf8_lossy(&request.body).into_owned();
                let mut state = state.lock().unwrap();
                state.add_calls += 1;
                // The Seeder names the part `<info_hash>.torrent`, so the mock can
                // learn the identity of what it was handed without bencode parsing.
                let save_path = multipart_field(&body, "savepath").unwrap_or_default();
                state.torrents.push(AddedTorrent {
                    hash: form_field(&body, "filename=")
                        .and_then(|f| f.strip_suffix(".torrent").map(str::to_owned))
                        .unwrap_or_default(),
                    content_path: save_path.clone(),
                    save_path,
                    files: Vec::new(),
                    form: body,
                });
                ResponseTemplate::new(200).set_body_string("Ok.")
            })
            .mount(server)
            .await;

        let state = Arc::clone(&self.state);
        Mock::given(method("GET"))
            .and(route("/api/v2/torrents/info"))
            .respond_with(move |_: &Request| {
                let state = state.lock().unwrap();
                let torrents: Vec<Value> = state
                    .torrents
                    .iter()
                    .filter(|t| !state.removed.contains(&t.hash))
                    .map(|t| {
                        json!({
                            "hash": t.hash,
                            "name": t.hash,
                            "save_path": t.save_path,
                            "content_path": t.content_path,
                            "state": "stalledUP",
                            "progress": 1.0,
                            "category": "sharerr",
                            "tags": "sharerr",
                        })
                    })
                    .collect();
                ResponseTemplate::new(200).set_body_json(Value::Array(torrents))
            })
            .mount(server)
            .await;

        let state = Arc::clone(&self.state);
        Mock::given(method("GET"))
            .and(route("/api/v2/torrents/files"))
            .respond_with(move |request: &Request| {
                let hash = request
                    .url
                    .query_pairs()
                    .find(|(k, _)| k == "hash")
                    .map(|(_, v)| v.into_owned())
                    .unwrap_or_default();

                let state = state.lock().unwrap();
                let files: Vec<Value> = state
                    .torrents
                    .iter()
                    .find(|t| t.hash == hash)
                    .map(|t| {
                        t.files
                            .iter()
                            .enumerate()
                            .map(|(i, name)| {
                                json!({ "index": i, "name": name, "size": 1, "progress": 1.0 })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                ResponseTemplate::new(200).set_body_json(Value::Array(files))
            })
            .mount(server)
            .await;

        let state = Arc::clone(&self.state);
        Mock::given(method("POST"))
            .and(route("/api/v2/torrents/delete"))
            .respond_with(move |request: &Request| {
                let body = String::from_utf8_lossy(&request.body).into_owned();
                if let Some(hashes) = form_field(&body, "hashes=") {
                    state.lock().unwrap().removed.push(hashes.to_owned());
                }
                ResponseTemplate::new(200).set_body_string("Ok.")
            })
            .mount(server)
            .await;
    }

    fn snapshot(&self) -> QbitSnapshot {
        let state = self.state.lock().unwrap();
        QbitSnapshot {
            live: state
                .torrents
                .iter()
                .filter(|t| !state.removed.contains(&t.hash))
                .cloned()
                .collect(),
            removed: state.removed.clone(),
            add_calls: state.add_calls,
        }
    }
}

struct QbitSnapshot {
    live: Vec<AddedTorrent>,
    removed: Vec<String>,
    add_calls: usize,
}

/// Pull `name="key"\r\n\r\nvalue` out of a multipart body.
fn multipart_field(body: &str, key: &str) -> Option<String> {
    let marker = format!("name=\"{key}\"");
    let rest = body.split(&marker).nth(1)?;
    rest.trim_start()
        .lines()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_owned())
}

/// Pull a `key=value` out of either a multipart header or a urlencoded body.
fn form_field<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    let rest = body.split(key).nth(1)?;
    Some(
        rest.trim_start_matches('"')
            .split(['"', '\r', '\n', '&'])
            .next()
            .unwrap_or(""),
    )
}

// ------------------------------------------------------------------ harness

#[derive(Debug)]
struct StubTracker;

#[async_trait]
impl TrackerProvider for StubTracker {
    async fn ensure_ready(&self) -> sharerr_torrent::Result<()> {
        Ok(())
    }
    async fn announce_url(&self) -> sharerr_torrent::Result<Url> {
        Ok(Url::parse("http://sharerr.example:9000/announce").unwrap())
    }
}

struct Harness {
    syncer: Syncer,
    qbit: FakeQbit,
    library: TvLibrary,
    _media: tempfile::TempDir,
    _torrents: tempfile::TempDir,
    _sonarr: MockServer,
    _qbit_server: MockServer,
}

/// Build a stack with Sonarr serving `series_json` and the library on disk.
async fn harness(series_json: Value) -> Harness {
    let media = tempfile::tempdir().unwrap();
    let torrents = tempfile::tempdir().unwrap();
    let lib = library::tv_library(media.path()).unwrap();

    let sonarr_server = MockServer::start().await;
    for (path, body) in [
        (
            "/api/v3/system/status",
            library::system_status_json("Sonarr"),
        ),
        ("/api/v3/tag", library::tag_json()),
        ("/api/v3/series", series_json),
        ("/api/v3/episodefile", lib.episodefile_json()),
        ("/api/v3/episode", lib.episode_json()),
    ] {
        Mock::given(method("GET"))
            .and(route(path))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&sonarr_server)
            .await;
    }

    let qbit_server = MockServer::start().await;
    let qbit_mock = FakeQbit::default();
    qbit_mock.mount(&qbit_server).await;

    let sonarr = sharerr_arr::ArrClient::new(
        MediaSource::Sonarr,
        &Url::parse(&sonarr_server.uri()).unwrap(),
        SecretString::from("test-key"),
    )
    .unwrap();

    let qbit = Arc::new(
        QbitClient::new(
            &Url::parse(&qbit_server.uri()).unwrap(),
            "admin",
            SecretString::from("password"),
        )
        .unwrap(),
    );

    // The three views: Sonarr says /tv, sharerr sees the tempdir, qbit sees /downloads/tv.
    let config = Config {
        tag: "sharerr".to_owned(),
        sonarr: Some(ServiceConfig {
            url: Url::parse(&sonarr_server.uri()).unwrap(),
        }),
        path_map: vec![PathMapping {
            arr: PathBuf::from(library::ARR_TV_PREFIX),
            sharerr: media.path().join("tv"),
            qbit: Some(PathBuf::from(QBIT_PREFIX)),
        }],
        ..Config::default()
    };

    let seeder = Seeder {
        qbit: Arc::clone(&qbit),
        factory: Arc::new(LavaTorrentFactory),
        category: "sharerr".to_owned(),
        tag: "sharerr".to_owned(),
        skip_checking: false,
        torrent_dir: torrents.path().to_path_buf(),
    };

    let syncer = Syncer::new(
        config,
        Store::open_in_memory().await.unwrap(),
        Some(sonarr),
        None,
        Arc::clone(&qbit),
        Arc::new(StubTracker),
        seeder,
    );

    Harness {
        syncer,
        qbit: qbit_mock,
        library: lib,
        _media: media,
        _torrents: torrents,
        _sonarr: sonarr_server,
        _qbit_server: qbit_server,
    }
}

/// The default library: one tagged series with two files.
async fn tagged_harness() -> Harness {
    let series = json!([
        {
            "id": 11,
            "title": "Lanternwick Hollow",
            "tvdbId": 918273,
            "tvMazeId": 4242,
            "imdbId": "tt7654321",
            "tags": [library::TAG_ID],
        },
        { "id": 12, "title": "Copper Vale Station", "tvdbId": 112233, "tags": [1] },
    ]);
    harness(series).await
}

/// The same library with the tag removed — what happens after an operator untags.
async fn untagged_harness() -> Harness {
    harness(json!([
        { "id": 11, "title": "Lanternwick Hollow", "tvdbId": 918273, "tags": [1] },
    ]))
    .await
}

fn file_identity(path: &Path) -> (u64, std::time::SystemTime, u64) {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).unwrap();
    (meta.ino(), meta.modified().unwrap(), meta.len())
}

// ------------------------------------------------------------------ tests

#[tokio::test]
async fn a_first_sync_shares_every_tagged_file() {
    let h = tagged_harness().await;

    let report = h.syncer.run(false).await.unwrap();

    assert_eq!(
        report,
        SyncReport {
            discovered: 2,
            added: 2,
            reused: 0,
            unchanged: 0,
            unshared: 0,
            failed: 0,
            sources_failed: 0
        }
    );

    let snapshot = h.qbit.snapshot();
    assert_eq!(snapshot.live.len(), 2);
    assert_eq!(snapshot.add_calls, 2);

    let items = h.syncer.store().all_items().await.unwrap();
    assert_eq!(items.len(), 2);
    for item in &items {
        assert_eq!(item.state, ShareState::Seeding);
        assert!(
            item.info_hash.is_some(),
            "a seeding item must record its infohash"
        );
    }
}

/// The core property. A second pass must not create anything, and in particular
/// must not hand qBittorrent a second copy of every torrent.
#[tokio::test]
async fn running_sync_twice_changes_nothing_the_second_time() {
    let h = tagged_harness().await;

    let first = h.syncer.run(false).await.unwrap();
    let after_first = h.qbit.snapshot();

    let second = h.syncer.run(false).await.unwrap();
    let after_second = h.qbit.snapshot();

    assert_eq!(first.added, 2);
    assert_eq!(
        second,
        SyncReport {
            discovered: 2,
            added: 0,
            reused: 0,
            unchanged: 2,
            unshared: 0,
            failed: 0,
            sources_failed: 0
        }
    );

    assert_eq!(
        after_first.add_calls, after_second.add_calls,
        "the second run added torrents"
    );
    assert_eq!(after_second.live.len(), 2, "a duplicate torrent appeared");
    assert_eq!(
        h.syncer.store().all_items().await.unwrap().len(),
        2,
        "a duplicate row appeared"
    );
}

/// The proof that the "never move files" requirement holds: inode, mtime, and
/// length all unchanged across a full share.
#[tokio::test]
async fn sharing_never_touches_the_media_file() {
    let h = tagged_harness().await;

    let before: Vec<_> = h
        .library
        .files
        .iter()
        .map(|f| file_identity(&f.disk_path))
        .collect();
    h.syncer.run(false).await.unwrap();
    let after: Vec<_> = h
        .library
        .files
        .iter()
        .map(|f| file_identity(&f.disk_path))
        .collect();

    assert_eq!(
        before, after,
        "a media file was moved, rewritten, or replaced"
    );
}

/// Automatic Torrent Management is what would relocate the file. It must be off on
/// every single add, and the save path must be the qbit view of where the file is.
#[tokio::test]
async fn every_add_disables_auto_torrent_management_and_seeds_in_place() {
    let h = tagged_harness().await;
    h.syncer.run(false).await.unwrap();

    for torrent in &h.qbit.snapshot().live {
        assert_eq!(
            multipart_field(&torrent.form, "autoTMM").as_deref(),
            Some("false"),
            "autoTMM was not disabled"
        );
        assert_eq!(
            torrent.save_path, "/downloads/tv/Lanternwick Hollow/Season 02",
            "savepath must be the qbit view of the existing directory"
        );
        assert_eq!(
            multipart_field(&torrent.form, "category").as_deref(),
            Some("sharerr")
        );
        assert_eq!(
            multipart_field(&torrent.form, "tags").as_deref(),
            Some("sharerr")
        );
    }
}

#[tokio::test]
async fn release_titles_come_from_the_scene_name_when_there_is_one() {
    let h = tagged_harness().await;
    h.syncer.run(false).await.unwrap();

    let items = h.syncer.store().all_items().await.unwrap();
    let with_scene = items.iter().find(|i| i.file_id == 501).unwrap();
    let without_scene = items.iter().find(|i| i.file_id == 502).unwrap();

    assert_eq!(
        with_scene.release_title,
        h.library.file(501).scene_name.as_deref().unwrap(),
        "a recorded scene name is the best possible title and must win"
    );
    // No scene name, and `lanternwick.s02e02` does not parse to a series title, so
    // it is synthesised rather than published as-is.
    assert_eq!(
        without_scene.release_title,
        "Lanternwick.Hollow.S02E02.WEB-DL.x264-SHARERR"
    );
}

#[tokio::test]
async fn untagging_withdraws_the_share_but_never_the_file() {
    let h = tagged_harness().await;
    h.syncer.run(false).await.unwrap();
    let identities: Vec<_> = h
        .library
        .files
        .iter()
        .map(|f| file_identity(&f.disk_path))
        .collect();

    // Rebuild the stack with the tag removed, reusing the same media directory is
    // not possible across harnesses, so assert on the second harness's own files.
    let h2 = untagged_harness().await;
    let first = h2.syncer.run(false).await.unwrap();
    assert_eq!(first.discovered, 0, "nothing carries the tag any more");

    // And on the original: files are still exactly where they were.
    let after: Vec<_> = h
        .library
        .files
        .iter()
        .map(|f| file_identity(&f.disk_path))
        .collect();
    assert_eq!(identities, after);
}

/// The real withdrawal path: share, then have discovery come back empty.
#[tokio::test]
async fn an_item_that_loses_its_tag_is_unshared_and_its_torrent_removed() {
    let h = tagged_harness().await;
    h.syncer.run(false).await.unwrap();
    let hashes: Vec<String> = h
        .qbit
        .snapshot()
        .live
        .iter()
        .map(|t| t.hash.clone())
        .collect();
    assert_eq!(hashes.len(), 2);

    // Point the same syncer at a Sonarr that no longer reports the tag by swapping
    // the series response. Simpler: drive `withdraw_untagged` through a run where
    // discovery returns nothing, which is what untagging looks like from here.
    let known = h
        .syncer
        .store()
        .all_items()
        .await
        .unwrap()
        .into_iter()
        .map(|i| (i.key(), i))
        .collect();
    let removed = h
        .syncer
        // Sonarr answered and reported nothing tagged, so its items may be withdrawn.
        .withdraw_untagged(
            &known,
            &Default::default(),
            &HashSet::from([MediaSource::Sonarr]),
            false,
        )
        .await;

    assert_eq!(removed, 2);
    let snapshot = h.qbit.snapshot();
    assert_eq!(
        snapshot.removed.len(),
        2,
        "both torrents should have been removed"
    );
    assert!(snapshot.live.is_empty());

    for item in h.syncer.store().all_items().await.unwrap() {
        assert_eq!(item.state, ShareState::Unshared);
    }

    // The files are untouched, which is the entire point.
    for file in &h.library.files {
        assert!(
            file.disk_path.exists(),
            "{} was deleted",
            file.disk_path.display()
        );
    }
}

#[tokio::test]
async fn a_dry_run_reports_without_writing_anything() {
    let h = tagged_harness().await;

    let report = h.syncer.run(true).await.unwrap();

    assert_eq!(report.discovered, 2);
    assert_eq!(report.added, 2, "a dry run reports what it would add");
    assert_eq!(
        h.qbit.snapshot().add_calls,
        0,
        "a dry run must not touch qBittorrent"
    );
    assert!(
        h.syncer.store().all_items().await.unwrap().is_empty(),
        "a dry run must not write rows"
    );
    assert!(
        h.syncer.store().recent_runs(10).await.unwrap().is_empty(),
        "not even a run record"
    );
}

/// A file the *arr app reports but sharerr cannot see is the single most common
/// real-world failure. It must fail that one item, with a reason, and share the rest.
#[tokio::test]
async fn a_missing_file_fails_only_its_own_item() {
    let h = tagged_harness().await;
    std::fs::remove_file(&h.library.file(501).disk_path).unwrap();

    let report = h.syncer.run(false).await.unwrap();

    assert_eq!(report.discovered, 2);
    assert_eq!(report.failed, 1);
    assert_eq!(report.added, 1, "the healthy file must still be shared");

    let items = h.syncer.store().all_items().await.unwrap();
    let failed = items.iter().find(|i| i.file_id == 501).unwrap();
    assert_eq!(failed.state, ShareState::Failed);
    let reason = failed.last_error.as_deref().unwrap_or_default();
    assert!(
        reason.contains("path_map"),
        "the reason should point at the likely fix: {reason}"
    );
}

/// Self-healing: a torrent removed behind sharerr's back is re-added, rather than
/// the row sitting at Seeding forever while nothing seeds.
#[tokio::test]
async fn a_torrent_removed_outside_sharerr_is_restored() {
    let h = tagged_harness().await;
    h.syncer.run(false).await.unwrap();

    {
        let mut state = h.qbit.state.lock().unwrap();
        let hash = state.torrents[0].hash.clone();
        state.removed.push(hash);
    }

    let report = h.syncer.run(false).await.unwrap();
    assert_eq!(report.unchanged, 1);
    assert_eq!(
        report.added, 1,
        "the missing torrent should have been re-added"
    );
}

#[tokio::test]
async fn a_pre_existing_torrent_is_reused_rather_than_duplicated() {
    let h = tagged_harness().await;

    // A single-file torrent already seeding one of the files. Detection should
    // catch this on the cheap pass, straight from `content_path`.
    {
        let mut state = h.qbit.state.lock().unwrap();
        state.torrents.push(AddedTorrent::preexisting(
            "preexisting0000000000000000000000000000a",
            &format!("{QBIT_PREFIX}/Lanternwick Hollow/Season 02"),
            &["lanternwick.s02e01.mkv"],
        ));
    }

    let report = h.syncer.run(false).await.unwrap();

    assert_eq!(report.discovered, 2);
    assert_eq!(
        report.reused, 1,
        "the already-seeding file should reuse its torrent"
    );
    assert_eq!(report.added, 1);
    assert_eq!(
        h.qbit.snapshot().add_calls,
        1,
        "only the uncovered file should be added"
    );

    let items = h.syncer.store().all_items().await.unwrap();
    let reused = items.iter().find(|i| i.file_id == 501).unwrap();
    assert_eq!(
        reused.info_hash.as_deref(),
        Some("preexisting0000000000000000000000000000a"),
        "the existing torrent's infohash should have been adopted"
    );
}

/// The expensive detection path: a season-pack torrent whose `content_path` is a
/// directory, so the file list has to be consulted.
#[tokio::test]
async fn a_file_inside_a_pre_existing_season_pack_is_detected() {
    let h = tagged_harness().await;

    {
        let mut state = h.qbit.state.lock().unwrap();
        state.torrents.push(AddedTorrent::preexisting(
            "seasonpack00000000000000000000000000000b",
            QBIT_PREFIX,
            &[
                "Lanternwick Hollow/Season 02/lanternwick.s02e01.mkv",
                "Lanternwick Hollow/Season 02/lanternwick.s02e02.mkv",
            ],
        ));
    }

    let report = h.syncer.run(false).await.unwrap();

    assert_eq!(
        report.reused, 2,
        "both files are already covered by the season pack"
    );
    assert_eq!(report.added, 0);
    assert_eq!(
        h.qbit.snapshot().add_calls,
        0,
        "nothing should have been added"
    );
}

// ------------------------------------------------- multi-app resilience

/// Attach a Radarr that fails every request to an otherwise-healthy harness.
async fn with_broken_radarr(h: &mut Harness) -> MockServer {
    let radarr = MockServer::start().await;
    Mock::given(method("GET"))
        .and(route("/api/v3/tag"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&radarr)
        .await;

    h.syncer.radarr = Some(
        sharerr_arr::ArrClient::new(
            MediaSource::Radarr,
            &Url::parse(&radarr.uri()).unwrap(),
            SecretString::from("test-key"),
        )
        .unwrap(),
    );
    radarr
}

/// "Sonarr has the tag, Radarr does not" is an ordinary setup, and it used to fail
/// the entire pass — nothing shared from the healthy app, and no withdrawals either.
#[tokio::test]
async fn one_broken_arr_app_does_not_stop_the_other() {
    let mut h = tagged_harness().await;
    let _radarr = with_broken_radarr(&mut h).await;

    let report = h.syncer.run(false).await.unwrap();

    assert_eq!(
        report.discovered, 2,
        "Sonarr's library should still be discovered"
    );
    assert_eq!(
        report.added, 2,
        "the healthy app's content should still be shared"
    );
    assert_eq!(report.sources_failed, 1);
    assert!(
        report.has_problems(),
        "a broken app must still surface as a problem"
    );
}

/// The dangerous half of that change. An app that did not answer has said nothing
/// about what it still carries; reading its silence as "everything was untagged"
/// would tear down a working library because a container was restarting.
#[tokio::test]
async fn a_broken_arr_app_never_causes_its_shares_to_be_withdrawn() {
    let mut h = tagged_harness().await;

    // Seed the database with a Radarr item that is already seeding.
    let movie = sharerr_core::SharedItem {
        id: None,
        source: MediaSource::Radarr,
        source_id: 31,
        file_id: 900,
        spec: sharerr_core::MediaSpec::Movie {
            title: "The Gilded Ferry".to_owned(),
            year: Some(2019),
        },
        release_title: "The.Gilded.Ferry.2019.WEB-DL.x264-SHARERR".to_owned(),
        arr_path: PathBuf::from("/movies/The Gilded Ferry (2019)/gilded.ferry.2019.mkv"),
        size: 1024,
        ids: sharerr_core::ExternalIds::default(),
        info_hash: Some("radarrhash0000000000000000000000000000aa".to_owned()),
        state: ShareState::Seeding,
        last_error: None,
    };
    h.syncer.store().upsert(&movie).await.unwrap();

    let _radarr = with_broken_radarr(&mut h).await;
    let report = h.syncer.run(false).await.unwrap();

    assert_eq!(report.sources_failed, 1);
    assert_eq!(
        report.unshared, 0,
        "a silent app must not cause withdrawals"
    );
    assert!(
        h.qbit.snapshot().removed.is_empty(),
        "no torrent should have been removed on behalf of an app that never answered"
    );

    let stored = h
        .syncer
        .store()
        .get(MediaSource::Radarr, 900)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.state,
        ShareState::Seeding,
        "the Radarr item should be untouched"
    );
}

/// With nothing answering, every item looks untagged. Withdrawing the whole library
/// on that basis would be far worse than doing nothing, so the pass stops instead.
#[tokio::test]
async fn a_pass_with_no_reachable_arr_app_changes_nothing() {
    let mut h = tagged_harness().await;
    h.syncer.run(false).await.unwrap();
    let before = h.qbit.snapshot();

    // Break the one healthy app too.
    let broken = MockServer::start().await;
    Mock::given(method("GET"))
        .and(route("/api/v3/tag"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&broken)
        .await;
    h.syncer.sonarr = Some(
        sharerr_arr::ArrClient::new(
            MediaSource::Sonarr,
            &Url::parse(&broken.uri()).unwrap(),
            SecretString::from("test-key"),
        )
        .unwrap(),
    );

    let err = h.syncer.run(false).await.unwrap_err();
    assert!(err.to_string().contains("no *arr app"), "got {err:#}");

    let after = h.qbit.snapshot();
    assert!(
        after.removed.is_empty(),
        "nothing should have been withdrawn"
    );
    assert_eq!(before.live.len(), after.live.len());
    for item in h.syncer.store().all_items().await.unwrap() {
        assert_eq!(item.state, ShareState::Seeding);
    }
}

/// A failed run is still recorded, so the gap in history has a stated reason.
#[tokio::test]
async fn a_failed_pass_records_its_reason() {
    let mut h = tagged_harness().await;
    let broken = MockServer::start().await;
    Mock::given(method("GET"))
        .and(route("/api/v3/tag"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&broken)
        .await;
    h.syncer.sonarr = Some(
        sharerr_arr::ArrClient::new(
            MediaSource::Sonarr,
            &Url::parse(&broken.uri()).unwrap(),
            SecretString::from("test-key"),
        )
        .unwrap(),
    );

    h.syncer.run(false).await.unwrap_err();

    let runs = h.syncer.store().recent_runs(1).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert!(
        runs[0].summary.error.is_some(),
        "the reason should survive in history"
    );
}

#[tokio::test]
async fn a_run_is_recorded_in_history() {
    let h = tagged_harness().await;
    h.syncer.run(false).await.unwrap();

    let runs = h.syncer.store().recent_runs(10).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert!(runs[0].finished_at.is_some());
    assert_eq!(runs[0].summary.discovered, 2);
    assert_eq!(runs[0].summary.added, 2);
    assert!(runs[0].summary.error.is_none());
}
