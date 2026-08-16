//! The embedded tracker has to actually be *running*, not just addressable.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use secrecy::SecretString;
use serde_json::json;
use sharerr_qbit::QbitClient;
use sharerr_torrent::{QbitEmbeddedTracker, TorrentError, TrackerProvider};
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A qBittorrent whose embedded tracker starts **off**, so enabling it is
/// observable: the `expect` on the POST is the assertion.
async fn qbit_with_tracker_off(server: &MockServer, enable_expected: u64) {
    sharerr_testkit::mock::mount_qbit_login(server).await;

    Mock::given(method("GET"))
        .and(path("/api/v2/app/preferences"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "enable_embedded_tracker": false,
            "embedded_tracker_port": 0,
        })))
        .up_to_n_times(1)
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/v2/app/preferences"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Ok."))
        .expect(enable_expected)
        .mount(server)
        .await;

    sharerr_testkit::mock::mount_qbit_prefs(server, true, 9000).await;
}

fn client(server: &MockServer) -> Arc<QbitClient> {
    Arc::new(
        QbitClient::new(
            &Url::parse(&server.uri()).unwrap(),
            "admin",
            SecretString::from("password"),
        )
        .unwrap(),
    )
}

#[tokio::test]
async fn ensure_ready_enables_the_tracker() {
    let server = MockServer::start().await;
    qbit_with_tracker_off(&server, 1).await;

    let tracker = QbitEmbeddedTracker::new(client(&server), Some("sharerr.example"), None).unwrap();

    tracker.ensure_ready().await.unwrap();
    assert_eq!(
        tracker.announce_url().await.unwrap().as_str(),
        "http://sharerr.example:9000/announce"
    );
}

/// The regression that matters: a published port differing from the internal one
/// is the *documented common case* (and what `docker/config/sharerr.toml` uses).
/// Treating it as a reason to skip enabling the tracker meant nothing ever seeded,
/// while `doctor` cheerfully reported the tracker would be enabled on first sync.
#[tokio::test]
async fn a_port_override_still_enables_the_tracker() {
    let server = MockServer::start().await;
    // `expect(1)` fails the test on drop if the enable call never happened.
    qbit_with_tracker_off(&server, 1).await;

    let tracker =
        QbitEmbeddedTracker::new(client(&server), Some("sharerr.example"), Some(19000)).unwrap();

    tracker.ensure_ready().await.unwrap();

    // The override changes only what is advertised, never whether the tracker runs.
    assert_eq!(
        tracker.announce_url().await.unwrap().as_str(),
        "http://sharerr.example:19000/announce"
    );
}

/// Each pass re-verifies, so a qBittorrent restart that lost the preference is
/// repaired rather than papered over by a process-lifetime cache.
#[tokio::test]
async fn every_ensure_ready_rechecks_qbittorrent() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/auth/login"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Ok."))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/preferences"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "enable_embedded_tracker": true,
            "embedded_tracker_port": 9000,
        })))
        // Three passes, three checks — not one cached answer.
        .expect(3)
        .mount(&server)
        .await;

    let tracker = QbitEmbeddedTracker::new(client(&server), Some("sharerr.example"), None).unwrap();

    for _ in 0..3 {
        tracker.ensure_ready().await.unwrap();
    }
}

/// Within one pass the port is cached, so building hundreds of torrents does not
/// mean hundreds of preferences calls.
#[tokio::test]
async fn announce_urls_within_a_pass_reuse_one_lookup() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/auth/login"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Ok."))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/preferences"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "enable_embedded_tracker": true,
            "embedded_tracker_port": 9000,
        })))
        .expect(1)
        .mount(&server)
        .await;

    let tracker = QbitEmbeddedTracker::new(client(&server), Some("sharerr.example"), None).unwrap();

    tracker.ensure_ready().await.unwrap();
    for _ in 0..50 {
        tracker.announce_url().await.unwrap();
    }
}

/// Without an override there is nothing to announce to, so a port of 0 is fatal
/// rather than silently producing `http://host:0/announce`.
#[tokio::test]
async fn a_zero_port_with_no_override_is_refused() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/auth/login"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Ok."))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/preferences"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "enable_embedded_tracker": true,
            "embedded_tracker_port": 0,
        })))
        .mount(&server)
        .await;

    let tracker = QbitEmbeddedTracker::new(client(&server), Some("sharerr.example"), None).unwrap();

    let err = tracker.ensure_ready().await.unwrap_err();
    assert!(matches!(err, TorrentError::NoTrackerPort), "got {err:?}");
}

/// ...but with an override, the operator's answer wins over qBittorrent's zero.
#[tokio::test]
async fn a_zero_port_is_tolerated_when_the_operator_published_one() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/auth/login"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Ok."))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/preferences"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "enable_embedded_tracker": true,
            "embedded_tracker_port": 0,
        })))
        .mount(&server)
        .await;

    let tracker =
        QbitEmbeddedTracker::new(client(&server), Some("sharerr.example"), Some(19000)).unwrap();

    tracker.ensure_ready().await.unwrap();
    assert_eq!(
        tracker.announce_url().await.unwrap().as_str(),
        "http://sharerr.example:19000/announce"
    );
}
