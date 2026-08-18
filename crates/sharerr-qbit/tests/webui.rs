//! Hermetic tests for the qBittorrent WebUI client.
//!
//! Everything runs against a local wiremock server — no real torrents, no real
//! paths, no network.

// The workspace denies casual panics because the production code handles secrets.
// In a test, a panic on an unexpected value *is* the assertion.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use secrecy::SecretString;
use serde_json::json;
use sharerr_qbit::{AddRequest, QbitClient, QbitError};
use url::Url;
use wiremock::matchers::{body_string_contains, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const API_KEY: &str = "qbt_jCGn3V76XutJwQpsXgIm6A9NLB86";

fn client(server: &MockServer) -> QbitClient {
    let base = Url::parse(&server.uri()).expect("wiremock uri is a valid url");
    QbitClient::with_api_key(&base, SecretString::from(API_KEY)).expect("client builds")
}

async fn requests_to(server: &MockServer, suffix: &str) -> Vec<Request> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.url.path().ends_with(suffix))
        .collect()
}

fn body_text(request: &Request) -> String {
    String::from_utf8_lossy(&request.body).into_owned()
}

// --------------------------------------------------------------- auth

/// The whole point of key auth: every call carries the bearer header and there is
/// no `auth/login` round trip at all. qBittorrent rejects a key sent to the auth
/// endpoints, so calling login would be worse than merely wasteful.
#[tokio::test]
async fn an_api_key_client_sends_a_bearer_header_and_never_logs_in() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/auth/login"))
        .respond_with(ResponseTemplate::new(403))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/version"))
        .respond_with(ResponseTemplate::new(200).set_body_string("v5.2.3"))
        .mount(&server)
        .await;

    let client = client(&server);
    // A no-op that must still succeed, so callers do not have to know which mode
    // they are in before probing.
    client.login().await.unwrap();
    assert_eq!(client.version().await.unwrap(), "v5.2.3");

    let sent = requests_to(&server, "/app/version").await;
    let header = sent[0]
        .headers
        .get("authorization")
        .expect("the key rides on every request")
        .to_str()
        .expect("ascii");
    assert_eq!(header, format!("Bearer {API_KEY}"));
}

/// A rejected key must not be retried. There is no session to renew, and
/// repeating the call only walks towards qBittorrent's ban counter.
#[tokio::test]
async fn a_rejected_api_key_is_reported_without_a_retry() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/version"))
        .respond_with(ResponseTemplate::new(403).set_body_string("Forbidden"))
        .expect(1)
        .mount(&server)
        .await;

    let err = client(&server).version().await.unwrap_err();
    assert!(matches!(err, QbitError::ApiKeyRejected), "{err:?}");
    assert!(err.is_auth_failure(), "{err}");
}

/// The other status qBittorrent answers a rejected bearer token with — both must
/// be treated as the key being rejected, not as two different problems.
#[tokio::test]
async fn a_401_is_also_reported_as_a_rejected_key() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/version"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;

    let err = client(&server).version().await.unwrap_err();
    assert!(matches!(err, QbitError::ApiKeyRejected), "{err:?}");
}

/// A password pasted into the key box is caught where it is entered, not hours
/// later as a puzzling rejection from the middle of a sync.
#[tokio::test]
async fn a_value_that_is_not_key_shaped_is_refused_up_front() {
    let base = Url::parse("http://localhost:8080").expect("literal url");
    let err = QbitClient::with_api_key(&base, SecretString::from("hunter2")).unwrap_err();
    assert!(matches!(err, QbitError::MalformedApiKey), "{err:?}");
}

/// qBittorrent's WebUI is localized, and proxies in front of it commonly are too.
/// Byte-truncating such a body panicked the process instead of returning an error.
#[tokio::test]
async fn a_long_non_ascii_body_does_not_panic() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/version"))
        .respond_with(ResponseTemplate::new(500).set_body_string(format!(
            "{}é{}",
            "a".repeat(399),
            "b".repeat(500)
        )))
        .mount(&server)
        .await;

    let err = client(&server).version().await.unwrap_err();
    match &err {
        QbitError::Status { status, body, .. } => {
            assert_eq!(*status, 500);
            assert!(body.chars().count() <= 400, "body should be clamped");
        }
        other => panic!("expected Status, got {other:?}"),
    }
}

#[tokio::test]
async fn an_unreachable_host_is_reported_as_such() {
    let base = Url::parse(&format!(
        "http://127.0.0.1:{}",
        sharerr_testkit::net::closed_port()
    ))
    .unwrap();
    let qbit = QbitClient::with_api_key(&base, SecretString::from(API_KEY)).unwrap();

    let err = qbit.version().await.unwrap_err();
    assert!(err.is_unreachable(), "got {err:?}");
    assert!(!err.is_auth_failure());
}

/// Missing this header is the difference between a client that works and one that
/// authenticates and then 403s on everything.
#[tokio::test]
async fn every_request_carries_a_referer_matching_the_webui() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/version"))
        .respond_with(ResponseTemplate::new(200).set_body_string("v5.0.4"))
        .mount(&server)
        .await;

    assert_eq!(client(&server).version().await.unwrap(), "v5.0.4");

    let expected = Url::parse(&server.uri())
        .unwrap()
        .origin()
        .ascii_serialization();
    let sent = server.received_requests().await.unwrap();
    assert!(!sent.is_empty());
    for request in &sent {
        let referer = request
            .headers
            .get("referer")
            .unwrap_or_else(|| panic!("no Referer on {}", request.url.path()));
        assert_eq!(
            referer.to_str().unwrap(),
            expected,
            "on {}",
            request.url.path()
        );
    }
}

// --------------------------------------------------------------- torrents

#[tokio::test]
async fn torrents_info_decodes_state_and_tags() {
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
            {
                "hash": "1111111111111111111111111111111111111111",
                "name": "Something.Downloading",
                "state": "downloading",
                "progress": 0.42,
                "tags": "",
            },
        ])))
        .mount(&server)
        .await;

    let torrents = client(&server).torrents_info(None, None).await.unwrap();
    assert_eq!(torrents.len(), 2);

    assert!(torrents[0].is_seeding());
    assert_eq!(torrents[0].tag_list(), vec!["sharerr", "cross-seed"]);
    assert_eq!(
        torrents[0].save_path,
        "/downloads/tv/Lanternwick Hollow/Season 02"
    );

    assert!(!torrents[1].is_seeding());
    assert!(
        torrents[1].tag_list().is_empty(),
        "an empty tag string is no tags, not one empty tag"
    );
}

#[tokio::test]
async fn torrents_info_narrows_by_category_and_tag_server_side() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/info"))
        .and(query_param("category", "sharerr"))
        .and(query_param("tag", "sharerr"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(1)
        .mount(&server)
        .await;

    let found = client(&server)
        .torrents_info(Some("sharerr"), Some("sharerr"))
        .await
        .unwrap();
    assert!(found.is_empty());
}

#[tokio::test]
async fn torrent_files_decodes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/files"))
        .and(query_param("hash", "aabbcc"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "index": 0, "name": "Lanternwick Hollow/lanternwick.s02e01.mkv", "size": 2147483648_u64, "progress": 1.0 }
        ])))
        .mount(&server)
        .await;

    let files = client(&server).torrent_files("aabbcc").await.unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].size, 2_147_483_648);
    // Relative to the torrent's save_path — that is what makes it joinable.
    assert_eq!(files[0].name, "Lanternwick Hollow/lanternwick.s02e01.mkv");
}

/// The single most important assertion in this crate. Automatic Torrent Management
/// relocates content to a category-derived path the moment a torrent is added, so
/// sending anything other than `autoTMM=false` would move the user's media.
#[tokio::test]
async fn add_torrent_always_disables_automatic_torrent_management() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/add"))
        .and(body_string_contains("autoTMM"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Ok."))
        .expect(1)
        .mount(&server)
        .await;

    let torrent = b"d8:announce4:faked4:infod4:name4:teseee";
    client(&server)
        .add_torrent(
            &AddRequest::new(torrent, "share.torrent", "/downloads/tv/Lanternwick Hollow")
                .category("sharerr")
                .tags("sharerr"),
        )
        .await
        .unwrap();

    let sent = requests_to(&server, "/torrents/add").await;
    let body = body_text(&sent[0]);

    let field = |name: &str| {
        body.split(&format!("name=\"{name}\"")).nth(1).map(|rest| {
            rest.trim_start()
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or_default()
                .trim()
                .to_owned()
        })
    };

    assert_eq!(
        field("autoTMM").as_deref(),
        Some("false"),
        "body was:\n{body}"
    );
    assert_eq!(
        field("savepath").as_deref(),
        Some("/downloads/tv/Lanternwick Hollow"),
        "savepath must be where the content already lives"
    );
    assert_eq!(
        field("skip_checking").as_deref(),
        Some("false"),
        "hash checking is on by default"
    );
    assert_eq!(field("category").as_deref(), Some("sharerr"));
    assert_eq!(field("tags").as_deref(), Some("sharerr"));
    // The part must be typed as a torrent, not octet-stream.
    assert!(
        body.contains("application/x-bittorrent"),
        "body was:\n{body}"
    );
    assert!(body.contains("share.torrent"), "body was:\n{body}");
}

#[tokio::test]
async fn skip_checking_is_opt_in() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/add"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Ok."))
        .mount(&server)
        .await;

    client(&server)
        .add_torrent(&AddRequest::new(b"data", "s.torrent", "/downloads").skip_checking(true))
        .await
        .unwrap();

    let sent = requests_to(&server, "/torrents/add").await;
    assert!(body_text(&sent[0]).contains("true"));
}

#[tokio::test]
async fn a_rejected_torrent_is_an_error_despite_the_200() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/add"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Fails."))
        .mount(&server)
        .await;

    let err = client(&server)
        .add_torrent(&AddRequest::new(
            b"not a torrent",
            "bad.torrent",
            "/downloads",
        ))
        .await
        .unwrap_err();

    assert!(
        matches!(&err, QbitError::InvalidTorrent { name } if name == "bad.torrent"),
        "got {err:?}"
    );
}

/// sharerr shares files it does not own. Unsharing must never delete media.
#[tokio::test]
async fn remove_torrent_never_deletes_files() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/delete"))
        .and(body_string_contains("deleteFiles=false"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Ok."))
        .expect(1)
        .mount(&server)
        .await;

    client(&server).remove_torrent("aabbcc").await.unwrap();
}

// ----------------------------------------------------------------- trackers

/// Replacing the tracker list adds the new URLs before removing the stale ones
/// (never trackerless in between), and leaves qBittorrent's `**`-style DHT/PEX
/// pseudo-entries alone.
#[tokio::test]
async fn set_torrent_trackers_adds_then_removes_and_skips_pseudo_entries() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/trackers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "url": "** [DHT] **", "status": 0 },
            { "url": "http://old.example:9000/announce", "status": 2 },
            { "url": "http://kept.example:8477/announce", "status": 2 },
        ])))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/addTrackers"))
        .and(body_string_contains("new.example"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/removeTrackers"))
        .and(body_string_contains("old.example"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    client(&server)
        .set_torrent_trackers(
            "aabbcc",
            &[
                "http://new.example:41234/announce".to_owned(),
                "http://kept.example:8477/announce".to_owned(),
            ],
        )
        .await
        .unwrap();
}

/// When the list already matches, nothing is written at all — this runs on
/// every sync pass, and a pass that changes nothing must issue no writes.
#[tokio::test]
async fn set_torrent_trackers_is_a_no_op_when_the_list_already_matches() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/trackers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "url": "http://current.example:8477/announce", "status": 2 },
        ])))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/addTrackers"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/removeTrackers"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    client(&server)
        .set_torrent_trackers(
            "aabbcc",
            &["http://current.example:8477/announce".to_owned()],
        )
        .await
        .unwrap();
}
