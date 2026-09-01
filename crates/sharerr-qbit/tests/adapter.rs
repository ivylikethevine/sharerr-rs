//! Hermetic tests for the [`sharerr_client::TorrentClient`] impl over
//! [`QbitClient`] — `webui.rs` exercises `QbitClient`'s own methods, this file
//! drives the same server through the shared trait to cover the translation
//! layer in `adapter.rs`: the trait-method glue and `translate`'s mapping from
//! [`QbitError`] to [`ClientError`].

#![allow(clippy::unwrap_used, clippy::expect_used)]

use secrecy::SecretString;
use serde_json::json;
use sharerr_client::{AddRequest, ClientError, ClientKind, TorrentClient};
use sharerr_qbit::QbitClient;
use sharerr_testkit::mock::{QBIT_API_KEY as API_KEY, base_url, mount_ok};
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer) -> QbitClient {
    QbitClient::with_api_key(&base_url(server), SecretString::from(API_KEY)).expect("client builds")
}

#[test]
fn kind_reports_qbittorrent() {
    let base = Url::parse("http://localhost:8080").unwrap();
    let qbit = QbitClient::with_api_key(&base, SecretString::from(API_KEY)).unwrap();
    assert_eq!(TorrentClient::kind(&qbit), ClientKind::QBittorrent);
}

#[tokio::test]
async fn login_and_version_succeed_through_the_trait() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/version"))
        .respond_with(ResponseTemplate::new(200).set_body_string("v5.2.3"))
        .mount(&server)
        .await;

    let qbit = client(&server);
    TorrentClient::login(&qbit).await.unwrap();
    assert_eq!(TorrentClient::version(&qbit).await.unwrap(), "v5.2.3");
}

#[tokio::test]
async fn list_maps_torrents_info_into_torrent_summaries() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "hash": "aabbccddeeff00112233445566778899aabbccdd",
                "name": "Lanternwick.Hollow.S02E01.1080p.WEB-DL.x264-FAKEGRP",
                "save_path": "/downloads/tv/Lanternwick Hollow/Season 02",
                "content_path": "/downloads/tv/Lanternwick Hollow/Season 02/lanternwick.s02e01.mkv",
                "state": "stalledUP",
                "progress": 1.0,
                "category": "sharerr",
                "tags": "sharerr, cross-seed",
            },
        ])))
        .mount(&server)
        .await;

    let summaries = TorrentClient::list(&client(&server), None).await.unwrap();
    assert_eq!(summaries.len(), 1);
    let s = &summaries[0];
    assert_eq!(s.hash, "aabbccddeeff00112233445566778899aabbccdd");
    assert_eq!(s.category, "sharerr");
    assert_eq!(s.tags, vec!["sharerr", "cross-seed"]);
    assert!(s.is_seeding);
    assert_eq!(
        s.content_path,
        "/downloads/tv/Lanternwick Hollow/Season 02/lanternwick.s02e01.mkv"
    );
}

#[tokio::test]
async fn files_maps_torrent_files_into_file_entries() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/files"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "index": 0, "name": "Lanternwick Hollow/lanternwick.s02e01.mkv", "size": 2_147_483_648_u64, "progress": 1.0 }
        ])))
        .mount(&server)
        .await;

    let files = TorrentClient::files(&client(&server), "aabbcc")
        .await
        .unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].name, "Lanternwick Hollow/lanternwick.s02e01.mkv");
    assert_eq!(files[0].size, 2_147_483_648);
}

#[tokio::test]
async fn add_remove_and_set_trackers_succeed_through_the_trait() {
    let server = MockServer::start().await;
    mount_ok(&server, "/api/v2/torrents/add").await;
    mount_ok(&server, "/api/v2/torrents/delete").await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/trackers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/addTrackers"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let qbit = client(&server);
    TorrentClient::add(
        &qbit,
        &AddRequest::new(b"data", "abc123", "s.torrent", "/downloads"),
    )
    .await
    .unwrap();
    TorrentClient::remove(&qbit, "aabbcc").await.unwrap();
    TorrentClient::set_trackers(
        &qbit,
        "aabbcc",
        &[Url::parse("http://tracker.example:8477/announce").unwrap()],
    )
    .await
    .unwrap();
}

// ------------------------------------------------------------ translate()

#[tokio::test]
async fn a_rejected_key_translates_to_auth_rejected() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/version"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let err = TorrentClient::version(&client(&server)).await.unwrap_err();
    assert!(matches!(err, ClientError::AuthRejected { kind } if kind == ClientKind::QBittorrent));
    assert!(err.is_auth_failure());
}

#[tokio::test]
async fn an_unreachable_host_translates_to_unreachable_with_the_base_url() {
    let base = Url::parse(&format!(
        "http://127.0.0.1:{}",
        sharerr_testkit::net::closed_port()
    ))
    .unwrap();
    let qbit = QbitClient::with_api_key(&base, SecretString::from(API_KEY)).unwrap();

    let err = TorrentClient::version(&qbit).await.unwrap_err();
    match &err {
        ClientError::Unreachable { kind, url, .. } => {
            assert_eq!(*kind, ClientKind::QBittorrent);
            assert!(url.starts_with("http://127.0.0.1:"), "{url}");
        }
        other => panic!("expected Unreachable, got {other:?}"),
    }
    assert!(err.is_unreachable());
}

#[tokio::test]
async fn any_other_failure_translates_to_a_generic_api_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/version"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;

    let err = TorrentClient::version(&client(&server)).await.unwrap_err();
    match &err {
        ClientError::Api { kind, detail } => {
            assert_eq!(*kind, ClientKind::QBittorrent);
            assert!(detail.contains("boom"), "{detail}");
        }
        other => panic!("expected Api, got {other:?}"),
    }
    assert!(!err.is_auth_failure());
    assert!(!err.is_unreachable());
}

// Nothing below is private to `adapter.rs`, so these tests live here against
// the public API and share the one `client` helper rather than a second
// mocked-server harness in-crate.

fn make_client(base: &str) -> QbitClient {
    QbitClient::with_api_key(&Url::parse(base).unwrap(), SecretString::from(API_KEY)).unwrap()
}

/// `login` costs zero requests — proven by never starting a mock server at
/// all, not just by asserting `Ok`.
#[tokio::test]
async fn login_always_succeeds_there_is_no_session_to_establish() {
    let client = make_client("http://127.0.0.1:8080");
    TorrentClient::login(&client).await.unwrap();
}

#[tokio::test]
async fn version_is_trimmed_and_passed_through() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/version"))
        .respond_with(ResponseTemplate::new(200).set_body_string("v4.6.0\n"))
        .mount(&server)
        .await;

    let version = TorrentClient::version(&client(&server)).await.unwrap();
    assert_eq!(version, "v4.6.0");
}

#[tokio::test]
async fn list_maps_hash_tags_paths_and_seeding_state() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "hash": "abc123",
                "name": "one",
                "save_path": "/downloads",
                "content_path": "/downloads/one",
                "state": "uploading",
                "category": "sharerr",
                "tags": "a,b",
            }
        ])))
        .mount(&server)
        .await;

    let list = TorrentClient::list(&client(&server), None).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].hash, "abc123");
    assert_eq!(list[0].save_path, "/downloads");
    assert_eq!(list[0].content_path, "/downloads/one");
    assert_eq!(list[0].category, "sharerr");
    assert_eq!(list[0].tags, vec!["a".to_owned(), "b".to_owned()]);
    assert!(list[0].is_seeding, "state=uploading is seeding");
}

/// qBittorrent's `-2` ("use the global default") and `-1` ("unlimited")
/// ratio_limit sentinels both resolve to `None` — neither is a fixed
/// number this specific torrent is held to. A genuine positive value
/// passes through unchanged.
#[tokio::test]
async fn list_maps_ratio_and_resolves_qbittorrents_sentinels() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "hash": "fixed", "ratio": 1.85, "ratio_limit": 2.0 },
            { "hash": "unlimited", "ratio": 0.5, "ratio_limit": -1.0 },
            { "hash": "global", "ratio": 0.1, "ratio_limit": -2.0 },
        ])))
        .mount(&server)
        .await;

    let list = TorrentClient::list(&client(&server), None).await.unwrap();
    assert_eq!(list.len(), 3);
    assert_eq!(list[0].ratio, Some(1.85));
    assert_eq!(list[0].ratio_limit, Some(2.0));
    assert_eq!(list[1].ratio, Some(0.5));
    assert_eq!(list[1].ratio_limit, None, "unlimited is not a fixed number");
    assert_eq!(list[2].ratio, Some(0.1));
    assert_eq!(
        list[2].ratio_limit, None,
        "using the global default is not a fixed number"
    );
}

#[tokio::test]
async fn add_forwards_the_request_to_the_underlying_client() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/add"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let data = b"d8:announce0:e";
    let request = AddRequest::new(data, "abc123", "x.torrent", "/downloads");
    let qbit = client(&server);
    TorrentClient::add(&qbit, &request).await.unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
}

#[tokio::test]
async fn add_translates_a_rejected_torrent_into_an_api_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/add"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Fails."))
        .mount(&server)
        .await;

    let data = b"not really a torrent";
    let request = AddRequest::new(data, "abc123", "x.torrent", "/downloads");
    let err = TorrentClient::add(&client(&server), &request)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            ClientError::Api {
                kind: ClientKind::QBittorrent,
                ..
            }
        ),
        "{err:?}"
    );
}

#[tokio::test]
async fn remove_never_asks_to_delete_files() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/delete"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let qbit = client(&server);
    TorrentClient::remove(&qbit, "abc123").await.unwrap();

    let requests = server.received_requests().await.unwrap();
    let body = sharerr_testkit::mock::body_text(requests.last().unwrap());
    assert!(body.contains("deleteFiles=false"), "{body}");
    assert!(body.contains("hashes=abc123"), "{body}");
}

#[tokio::test]
async fn set_trackers_stringifies_urls_before_forwarding_them() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/trackers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/addTrackers"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let urls = [Url::parse("http://tracker.example/announce").unwrap()];
    let qbit = client(&server);
    TorrentClient::set_trackers(&qbit, "abc123", &urls)
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    let add = requests
        .iter()
        .find(|r| r.url.path() == "/api/v2/torrents/addTrackers")
        .expect("an addTrackers call was made");
    let body = sharerr_testkit::mock::body_text(add);
    assert!(body.contains("tracker.example"), "{body}");
}

/// The additive form must never reach `removeTrackers`. It is pointed at
/// torrents sharerr did not create, whose tracker list is the operator's.
#[tokio::test]
async fn add_trackers_adds_without_removing_what_is_already_there() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/trackers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "url": "http://theirs.example/announce", "status": 2 },
            { "url": "** [DHT] **", "status": 0 },
        ])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/addTrackers"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let urls = [Url::parse("http://sharerr.example/announce").unwrap()];
    let qbit = client(&server);
    TorrentClient::add_trackers(&qbit, "abc123", &urls)
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    let add = requests
        .iter()
        .find(|r| r.url.path() == "/api/v2/torrents/addTrackers")
        .expect("an addTrackers call was made");
    assert!(sharerr_testkit::mock::body_text(add).contains("sharerr.example"));
    assert!(
        !requests
            .iter()
            .any(|r| r.url.path() == "/api/v2/torrents/removeTrackers"),
        "add_trackers must not remove anything"
    );
}

/// A URL the torrent already carries costs no request at all — this runs
/// again every time an adopted item is re-seeded.
#[tokio::test]
async fn add_trackers_is_silent_when_the_url_is_already_present() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/trackers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "url": "http://sharerr.example/announce", "status": 2 },
        ])))
        .mount(&server)
        .await;

    let urls = [Url::parse("http://sharerr.example/announce").unwrap()];
    let qbit = client(&server);
    TorrentClient::add_trackers(&qbit, "abc123", &urls)
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    assert!(
        !requests
            .iter()
            .any(|r| r.url.path() == "/api/v2/torrents/addTrackers"),
        "nothing to add means nothing sent"
    );
}

/// `torrents/export` is bencode. Read as bytes and returned untouched — a
/// `String` round trip would mangle the binary `pieces` field, and with it
/// the infohash of every torrent sharerr adopts.
#[tokio::test]
async fn export_returns_the_torrent_bytes_verbatim() {
    // Not valid UTF-8, deliberately: 0x80..0x9f is what a `pieces` field
    // looks like and what a lossy decode would replace.
    let bytes: Vec<u8> = (0u8..=255).collect();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/export"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes.clone()))
        .mount(&server)
        .await;

    let result = TorrentClient::export(&client(&server), "abc123")
        .await
        .unwrap();
    assert_eq!(result, Some(bytes));
}

/// An unknown hash is a real error, not `Ok(None)` — `None` is reserved for
/// a client that has no export call at all, which is a different fix.
#[tokio::test]
async fn export_of_an_unknown_torrent_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/export"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    assert!(
        TorrentClient::export(&client(&server), "abc123")
            .await
            .is_err()
    );
}
