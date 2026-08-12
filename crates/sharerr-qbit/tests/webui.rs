//! Hermetic tests for the qBittorrent WebUI client.
//!
//! Everything runs against a local wiremock server — no real torrents, no real
//! paths, no network.

// The workspace denies casual panics because the production code handles secrets.
// In a test, a panic on an unexpected value *is* the assertion.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use secrecy::SecretString;
use serde_json::json;
use sharerr_qbit::{AddTorrent, QbitClient, QbitError};
use url::Url;
use wiremock::matchers::{body_string_contains, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const USER: &str = "admin";
const PASSWORD: &str = "correct-horse-battery-staple";

fn client(server: &MockServer) -> QbitClient {
    let base = Url::parse(&server.uri()).expect("wiremock uri is a valid url");
    QbitClient::new(&base, USER, SecretString::from(PASSWORD)).expect("client builds")
}

/// A login endpoint that succeeds, expecting exactly `times` calls.
async fn mount_login(server: &MockServer, times: u64) {
    Mock::given(method("POST"))
        .and(path("/api/v2/auth/login"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Ok."))
        .expect(times)
        .mount(server)
        .await;
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

#[tokio::test]
async fn login_sends_credentials_and_accepts_the_ok_body() {
    let server = MockServer::start().await;
    mount_login(&server, 1).await;

    client(&server).login().await.unwrap();

    let sent = requests_to(&server, "/auth/login").await;
    let body = body_text(&sent[0]);
    assert!(body.contains("username=admin"), "{body}");
    assert!(body.contains("password="), "{body}");
}

/// The gotcha that makes naive clients appear to work: qBittorrent answers a bad
/// password with HTTP 200 and `Fails.` in the body.
#[tokio::test]
async fn a_rejected_password_arrives_as_an_http_200() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/auth/login"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Fails."))
        .mount(&server)
        .await;

    let err = client(&server).login().await.unwrap_err();
    assert!(matches!(err, QbitError::LoginRejected), "got {err:?}");
    assert!(err.is_auth_failure());
    assert!(!err.is_unreachable());
}

#[tokio::test]
async fn a_403_at_login_is_reported_as_a_ban() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/auth/login"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;

    let err = client(&server).login().await.unwrap_err();
    assert!(matches!(err, QbitError::LoginBanned), "got {err:?}");
    assert!(
        err.to_string().contains("ban"),
        "the message should explain the wait: {err}"
    );
}

/// qBittorrent's WebUI is localized, and proxies in front of it commonly are too.
/// Byte-truncating such a body panicked the process instead of returning an error.
#[tokio::test]
async fn a_long_non_ascii_error_body_does_not_panic() {
    let server = MockServer::start().await;
    let body = format!("{}é{}", "a".repeat(399), "b".repeat(500));

    Mock::given(method("POST"))
        .and(path("/api/v2/auth/login"))
        .respond_with(ResponseTemplate::new(500).set_body_string(body.clone()))
        .mount(&server)
        .await;

    let err = client(&server).login().await.unwrap_err();
    assert!(
        matches!(&err, QbitError::Status { status: 500, .. }),
        "got {err:?}"
    );
}

#[tokio::test]
async fn a_long_non_ascii_body_on_a_normal_call_does_not_panic() {
    let server = MockServer::start().await;
    mount_login(&server, 1).await;
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
    let base = Url::parse("http://127.0.0.1:1").unwrap();
    let qbit = QbitClient::new(&base, USER, SecretString::from(PASSWORD)).unwrap();

    let err = qbit.version().await.unwrap_err();
    assert!(err.is_unreachable(), "got {err:?}");
    assert!(!err.is_auth_failure());
}

/// Missing this header is the difference between a client that works and one that
/// authenticates and then 403s on everything.
#[tokio::test]
async fn every_request_carries_a_referer_matching_the_webui() {
    let server = MockServer::start().await;
    mount_login(&server, 1).await;
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

#[tokio::test]
async fn an_expired_session_triggers_exactly_one_relogin_and_a_retry() {
    let server = MockServer::start().await;
    // The initial login plus exactly one re-login. `expect` fails the test on drop
    // if the client logged in a different number of times.
    mount_login(&server, 2).await;

    // Mounted first so it wins while it still has matches left; once exhausted the
    // request falls through to the success mock below.
    Mock::given(method("GET"))
        .and(path("/api/v2/app/version"))
        .respond_with(ResponseTemplate::new(403))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/version"))
        .respond_with(ResponseTemplate::new(200).set_body_string("v5.0.4"))
        .mount(&server)
        .await;

    assert_eq!(client(&server).version().await.unwrap(), "v5.0.4");
}

#[tokio::test]
async fn a_persistent_403_is_reported_rather_than_retried_forever() {
    let server = MockServer::start().await;
    // Initial login + one re-login, and no more: looping would earn a real ban.
    mount_login(&server, 2).await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/version"))
        .respond_with(ResponseTemplate::new(403))
        .expect(2)
        .mount(&server)
        .await;

    let err = client(&server).version().await.unwrap_err();
    match &err {
        QbitError::Forbidden { path } => assert_eq!(path, "app/version"),
        other => panic!("expected Forbidden, got {other:?}"),
    }
    assert!(
        err.to_string().contains("Referer"),
        "the message should name the likely cause: {err}"
    );
}

// --------------------------------------------------------------- torrents

#[tokio::test]
async fn torrents_info_decodes_state_and_tags() {
    let server = MockServer::start().await;
    mount_login(&server, 1).await;
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
    mount_login(&server, 1).await;
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
    mount_login(&server, 1).await;
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
    mount_login(&server, 1).await;
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
            &AddTorrent::new(torrent, "share.torrent", "/downloads/tv/Lanternwick Hollow")
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
    mount_login(&server, 1).await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/add"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Ok."))
        .mount(&server)
        .await;

    client(&server)
        .add_torrent(&AddTorrent::new(b"data", "s.torrent", "/downloads").skip_checking(true))
        .await
        .unwrap();

    let sent = requests_to(&server, "/torrents/add").await;
    assert!(body_text(&sent[0]).contains("true"));
}

#[tokio::test]
async fn a_rejected_torrent_is_an_error_despite_the_200() {
    let server = MockServer::start().await;
    mount_login(&server, 1).await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/add"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Fails."))
        .mount(&server)
        .await;

    let err = client(&server)
        .add_torrent(&AddTorrent::new(
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
    mount_login(&server, 1).await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/delete"))
        .and(body_string_contains("deleteFiles=false"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Ok."))
        .expect(1)
        .mount(&server)
        .await;

    client(&server).remove_torrent("aabbcc").await.unwrap();
}

// --------------------------------------------------------------- preferences

#[tokio::test]
async fn ensure_embedded_tracker_is_a_no_op_when_already_enabled() {
    let server = MockServer::start().await;
    mount_login(&server, 1).await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/preferences"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "enable_embedded_tracker": true,
            "embedded_tracker_port": 9000,
            "save_path": "/downloads",
        })))
        .expect(1)
        .mount(&server)
        .await;
    // Writing preferences when nothing needs changing would rewrite settings
    // sharerr does not model.
    Mock::given(method("POST"))
        .and(path("/api/v2/app/preferences"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    assert_eq!(
        client(&server).ensure_embedded_tracker().await.unwrap(),
        9000
    );
}

#[tokio::test]
async fn ensure_embedded_tracker_enables_it_then_rereads_the_port() {
    let server = MockServer::start().await;
    mount_login(&server, 1).await;

    // First read: disabled, and the port qBittorrent reports is not yet meaningful.
    Mock::given(method("GET"))
        .and(path("/api/v2/app/preferences"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "enable_embedded_tracker": false,
            "embedded_tracker_port": 0,
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/v2/app/preferences"))
        .and(body_string_contains("enable_embedded_tracker"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Ok."))
        .expect(1)
        .mount(&server)
        .await;

    // Second read: enabled, with the port qBittorrent chose.
    Mock::given(method("GET"))
        .and(path("/api/v2/app/preferences"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "enable_embedded_tracker": true,
            "embedded_tracker_port": 9000,
        })))
        .mount(&server)
        .await;

    // The re-read is the point: assuming the port would produce announce URLs
    // nobody can reach.
    assert_eq!(
        client(&server).ensure_embedded_tracker().await.unwrap(),
        9000
    );
}

#[tokio::test]
async fn preferences_tolerates_the_hundred_keys_it_does_not_model() {
    let server = MockServer::start().await;
    mount_login(&server, 1).await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/preferences"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "enable_embedded_tracker": true,
            "embedded_tracker_port": 9000,
            "save_path": "/downloads",
            "locale": "en",
            "dht": true,
            "some_future_key": { "nested": [1, 2, 3] },
        })))
        .mount(&server)
        .await;

    let prefs = client(&server).preferences().await.unwrap();
    assert!(prefs.enable_embedded_tracker);
    assert_eq!(prefs.embedded_tracker_port, 9000);
    assert_eq!(prefs.save_path, "/downloads");
}
