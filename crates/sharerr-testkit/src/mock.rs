//! Shared wiremock scaffolding.
//!
//! The stubs here are the ones more than one crate's tests were writing out by
//! hand — qBittorrent's login handshake existed in three copies and its
//! preferences endpoint in two before this module. A stub that encodes one
//! test's specific choreography (expected call counts, staged responses) stays
//! with that test; only the plain building blocks live here.

use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Mount `GET route` returning `200` with a JSON body.
pub async fn mount_json(server: &MockServer, route: &str, body: serde_json::Value) {
    mount_json_status(server, route, 200, body).await;
}

/// Mount `GET route` returning `status` with a JSON body.
pub async fn mount_json_status(
    server: &MockServer,
    route: &str,
    status: u16,
    body: serde_json::Value,
) {
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(server)
        .await;
}

/// A qBittorrent login endpoint that always succeeds.
///
/// Answers in qBittorrent 5.2's shape — `204 No Content`, empty body — because
/// that is what current releases send and what the client used to mistake for a
/// rejected password. The older `200 Ok.` / `200 Fails.` protocol is covered by
/// `sharerr-qbit`'s own tests, so both stay exercised.
pub async fn mount_qbit_login(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/api/v2/auth/login"))
        .respond_with(ResponseTemplate::new(204))
        .mount(server)
        .await;
}

/// qBittorrent's preferences endpoint, reporting the embedded tracker's state.
pub async fn mount_qbit_prefs(server: &MockServer, tracker_enabled: bool, tracker_port: u16) {
    mount_json(
        server,
        "/api/v2/app/preferences",
        json!({
            "enable_embedded_tracker": tracker_enabled,
            "embedded_tracker_port": tracker_port,
        }),
    )
    .await;
}
