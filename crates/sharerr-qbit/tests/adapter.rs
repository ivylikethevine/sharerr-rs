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
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const API_KEY: &str = "qbt_jCGn3V76XutJwQpsXgIm6A9NLB86";

fn client(server: &MockServer) -> QbitClient {
    let base = Url::parse(&server.uri()).expect("wiremock uri is a valid url");
    QbitClient::with_api_key(&base, SecretString::from(API_KEY)).expect("client builds")
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
            { "index": 0, "name": "Lanternwick Hollow/lanternwick.s02e01.mkv", "size": 2147483648_u64, "progress": 1.0 }
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
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/add"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Ok."))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/delete"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Ok."))
        .mount(&server)
        .await;
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
