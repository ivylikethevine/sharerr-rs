//! Shared wiremock scaffolding.
//!
//! Only stubs reused across more than one crate's tests live here — e.g.
//! qBittorrent's login handshake and preferences endpoint. A stub that encodes
//! one test's specific choreography (expected call counts, staged responses)
//! stays with that test.

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
