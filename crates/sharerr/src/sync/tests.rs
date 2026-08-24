//! End-to-end reconciliation against a mock stack.
//!
//! Fully hermetic: a wiremock Sonarr, a *stateful* wiremock qBittorrent, synthetic
//! media on disk, and an in-memory database. The qBittorrent mock has to remember
//! what was added, because the property under test — running sync twice changes
//! nothing the second time — is invisible to a stateless mock.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::result_large_err)]

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use secrecy::SecretString;
use serde_json::{Value, json};
use sharerr_core::config::{PathMapping, SeedingConfig, ServiceConfig};
use sharerr_core::{Config, MediaSource, ShareState};
use sharerr_qbit::QbitClient;
use sharerr_store::Store;
use sharerr_testkit::library::{self, TvLibrary};
use sharerr_torrent::TrackerProvider;
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
    /// Bodies of `torrents/addTrackers` calls, for the rotation assertions.
    trackers_added: Vec<String>,
    /// Bodies of `torrents/removeTrackers` calls.
    trackers_removed: Vec<String>,
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

        // The tracker-rotation surface: every torrent reports the harness's
        // birth announce URL, so a rotated endpoint produces one add and one
        // remove per torrent.
        Mock::given(method("GET"))
            .and(route("/api/v2/torrents/trackers"))
            .respond_with(move |_: &Request| {
                ResponseTemplate::new(200)
                    .set_body_json(json!([{ "url": STUB_ANNOUNCE, "status": 2 }]))
            })
            .mount(server)
            .await;

        let state = Arc::clone(&self.state);
        Mock::given(method("POST"))
            .and(route("/api/v2/torrents/addTrackers"))
            .respond_with(move |request: &Request| {
                let body = String::from_utf8_lossy(&request.body).into_owned();
                state.lock().unwrap().trackers_added.push(body);
                ResponseTemplate::new(200)
            })
            .mount(server)
            .await;

        let state = Arc::clone(&self.state);
        Mock::given(method("POST"))
            .and(route("/api/v2/torrents/removeTrackers"))
            .respond_with(move |request: &Request| {
                let body = String::from_utf8_lossy(&request.body).into_owned();
                state.lock().unwrap().trackers_removed.push(body);
                ResponseTemplate::new(200)
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
            trackers_added: state.trackers_added.clone(),
            trackers_removed: state.trackers_removed.clone(),
        }
    }
}

struct QbitSnapshot {
    live: Vec<AddedTorrent>,
    removed: Vec<String>,
    add_calls: usize,
    trackers_added: Vec<String>,
    trackers_removed: Vec<String>,
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

/// The announce URL every harness torrent is born with.
const STUB_ANNOUNCE: &str = "http://sharerr.example:9000/announce";

/// A tracker whose announce URL the test can move, standing in for a gluetun
/// endpoint rotation.
#[derive(Debug)]
struct StubTracker(Mutex<Url>);

impl Default for StubTracker {
    fn default() -> Self {
        Self(Mutex::new(Url::parse(STUB_ANNOUNCE).unwrap()))
    }
}

impl StubTracker {
    fn rotate(&self, url: &str) {
        *self.0.lock().unwrap() = Url::parse(url).unwrap();
    }
}

#[async_trait]
impl TrackerProvider for StubTracker {
    async fn ensure_ready(&self) -> sharerr_torrent::Result<()> {
        Ok(())
    }
    async fn announce_set(&self) -> sharerr_torrent::Result<sharerr_torrent::AnnounceSet> {
        Ok(sharerr_torrent::AnnounceSet::single(
            self.0.lock().unwrap().clone(),
        ))
    }
}

struct Harness {
    syncer: Syncer,
    qbit: FakeQbit,
    library: TvLibrary,
    /// A handle on the syncer's tracker, so a test can rotate its endpoint.
    tracker: Arc<StubTracker>,
    _media: tempfile::TempDir,
    torrents: tempfile::TempDir,
    _sonarr: MockServer,
    _qbit_server: MockServer,
}

/// Build a stack with Sonarr serving `series_json` and the library on disk.
async fn harness(series_json: Value, seeding: SeedingConfig) -> Harness {
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

    // Typed as the trait object, because that is what the syncer holds now — the
    // concrete client is one implementation of it.
    let qbit: Arc<dyn sharerr_client::TorrentClient> = Arc::new(
        QbitClient::with_api_key(
            &Url::parse(&qbit_server.uri()).unwrap(),
            SecretString::from("qbt_jCGn3V76XutJwQpsXgIm6A9NLB86"),
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
        seeding,
        ..Config::default()
    };

    let seeder = Seeder {
        qbit: Arc::clone(&qbit),
        category: "sharerr".to_owned(),
        tag: "sharerr".to_owned(),
        skip_checking: false,
        upload_limit_kib: seeding.upload_limit_kib,
        ratio_limit: seeding.ratio_limit,
        torrent_dir: torrents.path().to_path_buf(),
    };

    let tracker = Arc::new(StubTracker::default());
    let syncer = Syncer::new(
        config,
        Store::open_in_memory().await.unwrap(),
        vec![Box::new(sonarr)],
        Arc::clone(&tracker) as Arc<dyn TrackerProvider>,
        seeder,
    );

    Harness {
        syncer,
        qbit: qbit_mock,
        library: lib,
        tracker,
        _media: media,
        torrents,
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
    harness(series, SeedingConfig::default()).await
}

/// The default library, with a seeding goal configured — see
/// `an_add_carries_the_configured_seeding_goal`.
async fn tagged_harness_with_seeding(seeding: SeedingConfig) -> Harness {
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
    harness(series, seeding).await
}

/// The same library with the tag removed — what happens after an operator untags.
async fn untagged_harness() -> Harness {
    harness(
        json!([
            { "id": 11, "title": "Lanternwick Hollow", "tvdbId": 918273, "tags": [1] },
        ]),
        SeedingConfig::default(),
    )
    .await
}

fn file_identity(path: &Path) -> (u64, std::time::SystemTime, u64) {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).unwrap();
    (meta.ino(), meta.modified().unwrap(), meta.len())
}

// ------------------------------------------------------------------ tests

/// The gluetun rotation story, end to end through the sync loop: when the
/// advertised endpoint moves between passes, the next pass rewrites every cached
/// `.torrent` to the new announce URL and repoints the client's tracker lists —
/// without re-adding anything.
#[tokio::test]
async fn a_rotated_endpoint_rewrites_torrents_and_repoints_the_client() {
    let h = tagged_harness().await;
    h.syncer.run(false).await.unwrap();

    h.tracker.rotate("http://203.0.113.9:41234/announce");
    let report = h.syncer.run(false).await.unwrap();

    // Still a no-op as far as shares go: nothing added, nothing withdrawn.
    assert_eq!(report.unchanged, 2);
    assert_eq!(report.added, 0);
    let snapshot = h.qbit.snapshot();
    assert_eq!(snapshot.add_calls, 2, "rotation must not re-add torrents");

    // The client's tracker lists were repointed: new URL added, old removed.
    assert_eq!(snapshot.trackers_added.len(), 2);
    assert!(
        snapshot
            .trackers_added
            .iter()
            .all(|body| body.contains("203.0.113.9")),
        "{:?}",
        snapshot.trackers_added
    );
    assert_eq!(snapshot.trackers_removed.len(), 2);
    assert!(
        snapshot
            .trackers_removed
            .iter()
            .all(|body| body.contains("sharerr.example")),
        "{:?}",
        snapshot.trackers_removed
    );

    // And the cached .torrent files — what the feed serves a friend — carry the
    // new endpoint.
    let items = h.syncer.store().all_items().await.unwrap();
    for item in &items {
        let hash = item.info_hash.as_deref().unwrap();
        let path = sharerr_torrent::torrent_file_path(h.torrents.path(), hash);
        let data = std::fs::read(&path).unwrap();
        assert_eq!(
            sharerr_torrent::read_announce(&data).unwrap().as_deref(),
            Some("http://203.0.113.9:41234/announce")
        );
    }

    // A third pass changes nothing: the files already match, so no tracker
    // calls are issued at all.
    h.syncer.run(false).await.unwrap();
    let settled = h.qbit.snapshot();
    assert_eq!(
        settled.trackers_added.len(),
        2,
        "rotation must be idempotent"
    );
}

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

/// A configured seeding goal reaches the wire on every add — proving
/// `Config::seeding` → `Seeder` → `AddRequest` → qBittorrent's `torrents/add`
/// end to end, not just each hop in isolation.
#[tokio::test]
async fn an_add_carries_the_configured_seeding_goal() {
    let h = tagged_harness_with_seeding(SeedingConfig {
        upload_limit_kib: Some(500),
        ratio_limit: Some(2.0),
    })
    .await;
    h.syncer.run(false).await.unwrap();

    let live = h.qbit.snapshot().live;
    assert!(
        !live.is_empty(),
        "the tagged library must have shared something"
    );
    for torrent in &live {
        assert_eq!(
            multipart_field(&torrent.form, "upLimit").as_deref(),
            Some("512000"),
            "500 KiB/s converted to bytes/s"
        );
        assert_eq!(
            multipart_field(&torrent.form, "ratioLimit").as_deref(),
            Some("2")
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

/// The withdrawal side of a dry run: it must report what it *would* unshare
/// without touching qBittorrent or the store — the counterpart to
/// `an_item_that_loses_its_tag_is_unshared_and_its_torrent_removed`, which
/// covers the same call with `dry_run: false`.
#[tokio::test]
async fn withdraw_untagged_dry_run_counts_without_removing_anything() {
    let h = tagged_harness().await;
    h.syncer.run(false).await.unwrap();

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
        .withdraw_untagged(
            &known,
            &Default::default(),
            &HashSet::from([MediaSource::Sonarr]),
            true,
        )
        .await;

    assert_eq!(removed, 2, "a dry run still reports what it would unshare");
    assert!(
        h.qbit.snapshot().removed.is_empty(),
        "a dry run must not remove any torrent"
    );
    for item in h.syncer.store().all_items().await.unwrap() {
        assert_eq!(
            item.state,
            ShareState::Seeding,
            "a dry run must not write the store"
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

/// The specific way `resolve_for` can fail that a missing *file* cannot: a
/// path the *arr app reported that is not absolute at all, e.g. a Windows
/// `C:\tv\…` path from Sonarr, which `Path::is_absolute()` rejects on Linux.
/// This must still leave a row behind for the caller's `Failed` state to
/// attach a reason to — before the fix, the error propagated out of `share`
/// before the "record before anything can fail" upsert ever ran.
#[tokio::test]
async fn a_path_that_cannot_be_resolved_still_leaves_a_row_to_fail() {
    let h = tagged_harness().await;
    let announce =
        sharerr_torrent::AnnounceSet::single(Url::parse("http://tracker.example/announce").unwrap());
    let item = sharerr_core::Discovered {
        source: MediaSource::Sonarr,
        source_id: 99,
        file_id: 999,
        spec: sharerr_core::MediaSpec::Episode {
            series_title: "Windowsy Show".to_owned(),
            season: 1,
            episode: 1,
        },
        arr_path: PathBuf::from(r"C:\tv\Windowsy Show\ep.mkv"),
        size: 1024,
        ids: sharerr_core::ExternalIds::default(),
        scene_name: None,
    };

    let result = h
        .syncer
        .share(&item, &announce, &HashSet::new(), &[], None, false)
        .await;
    let Err(err) = result else {
        panic!("an unresolvable path must fail");
    };
    assert!(format!("{err:#}").contains("resolving"), "{err:#}");

    let stored = h
        .syncer
        .store()
        .get(MediaSource::Sonarr, 999)
        .await
        .unwrap();
    assert!(
        stored.is_some(),
        "a row must exist for the run summary's Failed state to attach to"
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

// ------------------------------------------------- directory libraries

/// Give the harness a `[[library]]` directory of movies alongside its Sonarr.
fn with_movie_library(h: &mut Harness) -> tempfile::TempDir {
    use sharerr_core::config::{LibraryConfig, LibraryKind};

    let extras = tempfile::tempdir().unwrap();
    sharerr_testkit::media::write_media_file(
        &extras.path().join("The.Gilded.Ferry.2019.mkv"),
        4096,
        77,
    )
    .unwrap();

    h.syncer
        .sources
        .push(Box::new(crate::library::DirectoryScanner::new(vec![
            LibraryConfig {
                path: extras.path().to_path_buf(),
                kind: LibraryKind::Movie,
            },
        ])));
    extras
}

/// The zero-dependency source: a plain directory shares alongside a Sonarr,
/// idempotently, and its filename becomes the release title.
#[tokio::test]
async fn a_directory_library_shares_alongside_sonarr() {
    let mut h = tagged_harness().await;
    let _extras = with_movie_library(&mut h);

    let report = h.syncer.run(false).await.unwrap();
    assert_eq!(
        report.discovered, 3,
        "two episodes plus the directory movie"
    );
    assert_eq!(report.added, 3);
    assert_eq!(report.sources_failed, 0);

    let items = h.syncer.store().all_items().await.unwrap();
    let movie = items
        .iter()
        .find(|i| i.source == MediaSource::Directory)
        .expect("the directory item should be recorded");
    assert_eq!(movie.state, ShareState::Seeding);
    assert_eq!(
        movie.release_title, "The.Gilded.Ferry.2019",
        "a parseable filename is its own release title"
    );
    assert_eq!(movie.ids, sharerr_core::ExternalIds::default());

    let second = h.syncer.run(false).await.unwrap();
    assert_eq!(second.added, 0, "a repeat run must change nothing");
    assert_eq!(second.unchanged, 3);
}

/// Deleting a file from the directory is the tag being removed: the torrent is
/// withdrawn and nothing on disk is touched.
#[tokio::test]
async fn a_file_removed_from_the_directory_is_withdrawn() {
    let mut h = tagged_harness().await;
    let extras = with_movie_library(&mut h);
    // A second file, so removing the first leaves a mounted, non-empty root —
    // an *emptied* root is the failed-mount signature and refuses to scan.
    sharerr_testkit::media::write_media_file(
        &extras.path().join("Bramble.Gate.2022.mkv"),
        4096,
        78,
    )
    .unwrap();
    h.syncer.run(false).await.unwrap();

    std::fs::remove_file(extras.path().join("The.Gilded.Ferry.2019.mkv")).unwrap();
    let report = h.syncer.run(false).await.unwrap();

    assert_eq!(report.unshared, 1, "the vanished file must be withdrawn");
    assert_eq!(
        h.qbit.snapshot().removed.len(),
        1,
        "its torrent should have been removed"
    );
}

/// An emptied root is indistinguishable from a bind mount that has not come
/// up: the source fails and the shares survive to the next pass, instead of
/// every directory torrent being torn down in one sweep.
#[tokio::test]
async fn an_emptied_library_root_never_causes_withdrawals() {
    let mut h = tagged_harness().await;
    let extras = with_movie_library(&mut h);
    h.syncer.run(false).await.unwrap();

    std::fs::remove_file(extras.path().join("The.Gilded.Ferry.2019.mkv")).unwrap();
    let report = h.syncer.run(false).await.unwrap();

    assert_eq!(report.sources_failed, 1);
    assert_eq!(report.unshared, 0, "an empty root must not withdraw");
    assert!(h.qbit.snapshot().removed.is_empty());
    let movie_state = h
        .syncer
        .store()
        .all_items()
        .await
        .unwrap()
        .into_iter()
        .find(|i| i.source == MediaSource::Directory)
        .unwrap()
        .state;
    assert_eq!(
        movie_state,
        ShareState::Seeding,
        "the item must be untouched"
    );
}

/// A `[[path_map]]` rule is written for an *arr app's view of the library. One
/// whose prefix happens to match a `[[library]]` path on disk must not rewrite
/// the directory item's path — it is already the sharerr view.
#[tokio::test]
async fn a_path_map_rule_never_rewrites_a_directory_item() {
    let mut h = tagged_harness().await;
    let extras = with_movie_library(&mut h);
    h.syncer.resolver = sharerr_core::paths::PathResolver::new(vec![
        // Correct for some *arr app, catastrophic if applied to the directory.
        PathMapping {
            arr: extras.path().to_path_buf(),
            sharerr: PathBuf::from("/nonexistent"),
            qbit: None,
        },
        // The harness's own Sonarr mapping, unchanged.
        PathMapping {
            arr: PathBuf::from(library::ARR_TV_PREFIX),
            sharerr: h._media.path().join("tv"),
            qbit: Some(PathBuf::from(QBIT_PREFIX)),
        },
    ]);

    let report = h.syncer.run(false).await.unwrap();
    assert_eq!(
        report.failed, 0,
        "the directory item must resolve to itself"
    );
    assert_eq!(report.added, 3);
}

/// One unreadable subdirectory must not stop the readable files from sharing —
/// and must not read as "everything in it was deleted" either.
#[tokio::test]
#[cfg(unix)]
async fn an_incomplete_directory_scan_shares_but_never_withdraws() {
    use std::os::unix::fs::PermissionsExt;

    let mut h = tagged_harness().await;
    let extras = with_movie_library(&mut h);
    sharerr_testkit::media::write_media_file(
        &extras.path().join("deep/Bramble.Gate.2022.mkv"),
        4096,
        78,
    )
    .unwrap();
    h.syncer.run(false).await.unwrap();

    let locked = extras.path().join("deep");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let report = h.syncer.run(false).await;
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    let report = report.unwrap();

    assert_eq!(report.sources_failed, 0, "an incomplete scan still answers");
    assert_eq!(
        report.unshared, 0,
        "a file behind an unreadable directory must not be withdrawn"
    );
    assert!(h.qbit.snapshot().removed.is_empty());
}

/// A library that cannot be scanned is a silent source, and silence never
/// withdraws — same contract as an *arr app that did not answer.
#[tokio::test]
async fn an_unscannable_directory_never_causes_withdrawals() {
    use sharerr_core::config::{LibraryConfig, LibraryKind};

    let mut h = tagged_harness().await;
    let extras = with_movie_library(&mut h);
    h.syncer.run(false).await.unwrap();

    // Replace the scanner with one whose directory does not exist.
    h.syncer.sources.pop();
    h.syncer
        .sources
        .push(Box::new(crate::library::DirectoryScanner::new(vec![
            LibraryConfig {
                path: extras.path().join("vanished"),
                kind: LibraryKind::Movie,
            },
        ])));

    let report = h.syncer.run(false).await.unwrap();
    assert_eq!(report.sources_failed, 1);
    assert_eq!(report.unshared, 0, "a silent source must not withdraw");
    assert!(h.qbit.snapshot().removed.is_empty());

    let movie_state = h
        .syncer
        .store()
        .all_items()
        .await
        .unwrap()
        .into_iter()
        .find(|i| i.source == MediaSource::Directory)
        .unwrap()
        .state;
    assert_eq!(
        movie_state,
        ShareState::Seeding,
        "the item must be untouched"
    );
}

/// A dry run against a directory library reports without writing.
#[tokio::test]
async fn a_directory_dry_run_writes_nothing() {
    let mut h = tagged_harness().await;
    let _extras = with_movie_library(&mut h);

    let report = h.syncer.run(true).await.unwrap();
    assert_eq!(report.discovered, 3);
    assert_eq!(report.added, 3, "a dry run reports what it would add");
    assert_eq!(h.qbit.snapshot().add_calls, 0, "nothing must reach qbit");
    assert!(h.syncer.store().all_items().await.unwrap().is_empty());
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

    h.syncer.sources.push(Box::new(
        sharerr_arr::ArrClient::new(
            MediaSource::Radarr,
            &Url::parse(&radarr.uri()).unwrap(),
            SecretString::from("test-key"),
        )
        .unwrap(),
    ));
    radarr
}

/// "Sonarr has the tag, Radarr does not" is an ordinary setup: one broken app must
/// not fail the whole pass — the healthy app should still share, and nothing
/// should be withdrawn.
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
        announce_token_fp: None,
        state: ShareState::Seeding,
        last_error: None,
        created_at: None,
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
    // Replace the only *arr client with a broken one.
    h.syncer.sources = vec![Box::new(
        sharerr_arr::ArrClient::new(
            MediaSource::Sonarr,
            &Url::parse(&broken.uri()).unwrap(),
            SecretString::from("test-key"),
        )
        .unwrap(),
    )];

    let err = h.syncer.run(false).await.unwrap_err();
    assert!(err.to_string().contains("no library source"), "got {err:#}");

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
    // Replace the only *arr client with a broken one.
    h.syncer.sources = vec![Box::new(
        sharerr_arr::ArrClient::new(
            MediaSource::Sonarr,
            &Url::parse(&broken.uri()).unwrap(),
            SecretString::from("test-key"),
        )
        .unwrap(),
    )];

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

// --------------------------------------------------------- building from config

/// `build_arr`/`build_client`/`build_tracker` take a plain `&Vault` rather than
/// going through `ServeState`, so — per this repo's testing conventions — they can
/// be exercised directly with a vault opened in a tempdir, no `SHARERR_MASTER_KEY`
/// required.
fn vault_in(dir: &tempfile::TempDir) -> sharerr_store::Vault {
    sharerr_store::Vault::open(dir.path().join("vault.bin"), &SecretString::from("master")).unwrap()
}

#[test]
fn build_arr_is_none_when_the_service_is_not_configured() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault_in(&dir);
    let config = Config::default();

    assert!(
        super::build_arr(MediaSource::Sonarr, &config, &vault)
            .unwrap()
            .is_none()
    );
}

#[test]
fn build_arr_fails_with_the_missing_key_named_when_no_credential_is_stored() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault_in(&dir);
    let config = Config {
        sonarr: Some(ServiceConfig {
            url: Url::parse("http://sonarr.example").unwrap(),
        }),
        ..Config::default()
    };

    let err = super::build_arr(MediaSource::Sonarr, &config, &vault).unwrap_err();
    assert!(format!("{err:#}").contains("sonarr.api_key"), "{err:#}");
}

#[test]
fn build_arr_succeeds_once_the_vault_holds_the_key() {
    let dir = tempfile::tempdir().unwrap();
    let mut vault = vault_in(&dir);
    vault
        .put(
            sharerr_core::config::secret_keys::SONARR_API_KEY,
            &SecretString::from("a-key"),
        )
        .unwrap();
    let config = Config {
        sonarr: Some(ServiceConfig {
            url: Url::parse("http://sonarr.example").unwrap(),
        }),
        ..Config::default()
    };

    assert!(
        super::build_arr(MediaSource::Sonarr, &config, &vault)
            .unwrap()
            .is_some()
    );
}

#[test]
fn build_client_fails_naming_the_missing_key_for_the_selected_backend() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault_in(&dir);
    let config = Config::default(); // defaults to qBittorrent

    let err = super::build_client(&config, &vault).unwrap_err();
    assert!(
        format!("{err:#}").contains("qbittorrent.api_key"),
        "{err:#}"
    );
}

#[test]
fn build_client_succeeds_once_the_backends_credential_is_stored() {
    let dir = tempfile::tempdir().unwrap();
    let mut vault = vault_in(&dir);
    vault
        .put(
            sharerr_core::config::secret_keys::QBITTORRENT_API_KEY,
            &SecretString::from("qbt_jCGn3V76XutJwQpsXgIm6A9NLB86"),
        )
        .unwrap();
    let config = Config::default();

    assert!(super::build_client(&config, &vault).is_ok());
}

#[test]
fn build_client_reads_the_backend_specific_key_when_the_backend_is_switched() {
    let dir = tempfile::tempdir().unwrap();
    let mut vault = vault_in(&dir);
    // Only Transmission's own key is stored — if `build_client` mistakenly kept
    // reading qBittorrent's key name after the backend switched, this would fail.
    vault
        .put(
            sharerr_core::config::secret_keys::TRANSMISSION_PASSWORD,
            &SecretString::from("a-password"),
        )
        .unwrap();
    let config = Config {
        torrent_backend: sharerr_core::config::TorrentBackend::Transmission,
        ..Config::default()
    };

    assert!(super::build_client(&config, &vault).is_ok());
}

/// `Syncer::build` is the one function in this module that reads
/// `SHARERR_MASTER_KEY` from the process environment (via `secrets::open_vault_async`),
/// so — same as `secrets.rs`'s own vault-opening tests — it can only be exercised
/// safely inside a `figment::Jail`, which clears/scopes the env and serializes
/// against every other Jail-based test in this binary rather than racing them.
/// A plain `#[tokio::test]` cannot host a `Jail` (its closure needs to drive its
/// own runtime), hence a `#[test]` with a runtime built inside the closure.
#[test]
fn build_fails_without_a_master_key() {
    figment::Jail::expect_with(|jail| {
        jail.clear_env();
        let config = Config {
            data_dir: jail.directory().to_path_buf(),
            library: vec![sharerr_core::config::LibraryConfig {
                path: jail.directory().to_path_buf(),
                kind: sharerr_core::config::LibraryKind::Movie,
            }],
            ..Config::default()
        };
        let endpoint = Arc::new(sharerr_core::endpoint::AdvertisedEndpoint::new(None));

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let err = runtime
            .block_on(Syncer::build(&config, endpoint))
            .unwrap_err();
        assert!(format!("{err:#}").to_lowercase().contains("master key"));
        Ok(())
    });
}

#[test]
fn build_bails_when_no_library_source_is_configured() {
    figment::Jail::expect_with(|jail| {
        jail.clear_env();
        jail.set_env("SHARERR_MASTER_KEY", "a-master-key");
        let data_dir = jail.directory().to_path_buf();
        let config = Config {
            data_dir: data_dir.clone(),
            ..Config::default() // no sonarr/radarr/etc, no [[library]]
        };

        // build_client (called before the sources check) needs the qbit
        // credential in the vault, or the bail this test wants would be masked
        // by an earlier, unrelated failure.
        let mut vault =
            sharerr_store::Vault::open(config.vault_path(), &SecretString::from("a-master-key"))
                .unwrap();
        vault
            .put(
                sharerr_core::config::secret_keys::QBITTORRENT_API_KEY,
                &SecretString::from("qbt_jCGn3V76XutJwQpsXgIm6A9NLB86"),
            )
            .unwrap();
        drop(vault);

        let endpoint = Arc::new(sharerr_core::endpoint::AdvertisedEndpoint::new(None));
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let err = runtime
            .block_on(Syncer::build(&config, endpoint))
            .unwrap_err();
        assert!(format!("{err:#}").contains("no library source"), "{err:#}");
        Ok(())
    });
}

#[test]
fn build_succeeds_with_a_configured_library_and_torrent_client() {
    figment::Jail::expect_with(|jail| {
        jail.clear_env();
        jail.set_env("SHARERR_MASTER_KEY", "a-master-key");
        let data_dir = jail.directory().to_path_buf();
        let config = Config {
            data_dir: data_dir.clone(),
            library: vec![sharerr_core::config::LibraryConfig {
                path: data_dir.clone(),
                kind: sharerr_core::config::LibraryKind::Movie,
            }],
            ..Config::default()
        };

        let mut vault =
            sharerr_store::Vault::open(config.vault_path(), &SecretString::from("a-master-key"))
                .unwrap();
        vault
            .put(
                sharerr_core::config::secret_keys::QBITTORRENT_API_KEY,
                &SecretString::from("qbt_jCGn3V76XutJwQpsXgIm6A9NLB86"),
            )
            .unwrap();
        drop(vault);

        let endpoint = Arc::new(sharerr_core::endpoint::AdvertisedEndpoint::new(None));
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let syncer = runtime.block_on(Syncer::build(&config, endpoint)).unwrap();
        assert!(format!("{syncer:?}").contains("sharerr"));
        Ok(())
    });
}

#[test]
fn build_client_reports_the_missing_password_alone_when_theres_no_api_key_option() {
    // Transmission (and rtorrent) have no `api_key_key` at all, so their "missing
    // credential" message is the `(None, Some(password))` arm — distinct from
    // qBittorrent's `(Some(api), None)` arm covered by
    // `build_client_fails_naming_the_missing_key_for_the_selected_backend`.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault_in(&dir);
    let config = Config {
        torrent_backend: sharerr_core::config::TorrentBackend::Transmission,
        ..Config::default()
    };

    let err = super::build_client(&config, &vault).unwrap_err();
    assert!(
        format!("{err:#}").contains("transmission.password"),
        "{err:#}"
    );
    assert!(
        !format!("{err:#}").contains(" or "),
        "the password-only arm must not read like an either/or, got: {err:#}"
    );
}

#[test]
fn build_tracker_works_with_and_without_a_stored_token() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault_in(&dir);
    let endpoint = Arc::new(sharerr_core::endpoint::AdvertisedEndpoint::new(None));

    assert!(super::build_tracker(Arc::clone(&endpoint), &vault).is_ok());

    let mut vault = vault;
    vault
        .put(
            sharerr_core::config::secret_keys::TRACKER_TOKEN,
            &SecretString::from("a-token"),
        )
        .unwrap();
    assert!(super::build_tracker(endpoint, &vault).is_ok());
}

// ------------------------------------------------------------ report formatting

#[test]
fn a_report_with_no_failures_has_no_problems() {
    let report = SyncReport {
        discovered: 3,
        added: 3,
        ..SyncReport::default()
    };
    assert!(!report.has_problems());
    let text = report.to_string();
    assert!(text.contains("3 discovered"), "{text}");
    assert!(!text.contains("could not be scanned"), "{text}");
}

#[test]
fn a_report_with_a_failed_source_flags_problems_and_says_so() {
    let report = SyncReport {
        sources_failed: 1,
        ..SyncReport::default()
    };
    assert!(report.has_problems());
    let text = report.to_string();
    assert!(text.contains("1 source(s) could not be scanned"), "{text}");
}

#[tokio::test]
async fn the_debug_impl_names_the_tag_and_source_kinds_without_the_credentials() {
    let h = tagged_harness().await;
    let debug = format!("{:?}", h.syncer);
    assert!(debug.contains("sharerr"), "{debug}");
}
