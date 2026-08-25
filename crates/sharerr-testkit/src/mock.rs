//! Shared wiremock scaffolding.
//!
//! Only stubs reused across more than one crate's tests live here — e.g.
//! qBittorrent's login handshake and preferences endpoint. A stub that encodes
//! one test's specific choreography (expected call counts, staged responses)
//! stays with that test.

use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The qBittorrent API key every hermetic test hands its client. Shaped like a
/// real `qbt_` key so the client's up-front format check accepts it.
pub const QBIT_API_KEY: &str = "qbt_jCGn3V76XutJwQpsXgIm6A9NLB86";

/// The *arr API key every hermetic test hands its client.
pub const ARR_API_KEY: &str = "0123456789abcdef0123456789abcdef";

/// The server's URI as a [`Url`], for handing to a client constructor.
pub fn base_url(server: &MockServer) -> Url {
    #[allow(clippy::expect_used)] // wiremock always reports a well-formed loopback URI
    Url::parse(&server.uri()).expect("wiremock uri is a valid url")
}

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

/// Mount `verb route` returning `200` with a plain-text body.
pub async fn mount_text(server: &MockServer, verb: &str, route: &str, body: &str) {
    Mock::given(method(verb))
        .and(path(route))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(server)
        .await;
}

/// Mount `POST route` answering `Ok.`, qBittorrent's success body for every
/// mutating WebUI call.
pub async fn mount_ok(server: &MockServer, route: &str) {
    mount_text(server, "POST", route, "Ok.").await;
}

/// Pull `name="key"\r\n\r\nvalue` out of a multipart body.
pub fn multipart_field(body: &str, key: &str) -> Option<String> {
    let marker = format!("name=\"{key}\"");
    let rest = body.split(&marker).nth(1)?;
    rest.trim_start()
        .lines()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_owned())
}
