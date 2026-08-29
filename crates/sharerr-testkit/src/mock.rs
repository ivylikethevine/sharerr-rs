//! Shared wiremock scaffolding.
//!
//! Only stubs reused across more than one crate's tests live here — e.g.
//! qBittorrent's login handshake and preferences endpoint. A stub that encodes
//! one test's specific choreography (expected call counts, staged responses)
//! stays with that test.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

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

/// Throwaway HTTP Basic credentials for a mock RPC server: a fresh pair per
/// call, deliberately not a constant.
///
/// rTorrent and Transmission both send their password as Basic Auth on every
/// request, so a literal handed to either constructor reaches `basic_auth` and
/// is a `rust/hard-coded-cryptographic-value` finding — CodeQL cannot tell a
/// wiremock stub on loopback from a deployment, and a password literal flowing
/// to that sink looks identical either way. Nothing here checks the value: the
/// mock answers every request regardless of `Authorization`, and the
/// closed-port tests never build a request at all. Generating it costs nothing
/// and leaves no literal for a future reader to have to justify.
///
/// `unwrap_or_default` on a clock read failure, deliberately over `expect`:
/// uniqueness never rested on the clock in the first place — the `AtomicU64`
/// counter below is concatenated into every value regardless — and unlike a
/// literal fallback (which would be the same taint shape CodeQL flags above,
/// a constant reaching `basic_auth`), `unwrap_or_default()` has no literal in
/// the expression for a taint query to anchor on.
pub fn rpc_credentials() -> (String, String) {
    static NEXT: AtomicU64 = AtomicU64::new(0);

    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    (format!("u{n:x}"), format!("{stamp:x}{n:x}"))
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

/// Every request the server has received whose path ends in `suffix`.
///
/// A client that composes its endpoint with a fixed base plus a per-call
/// path — qBittorrent's WebUI, one route per action — narrows an assertion
/// down to the call under test this way rather than assuming the last
/// request received is the right one.
pub async fn requests_to(server: &MockServer, suffix: &str) -> Vec<Request> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.url.path().ends_with(suffix))
        .collect()
}

/// A received request's body as text, lossily. Test bodies are always ASCII
/// or UTF-8 by construction; lossy decoding means a stray byte fails the
/// assertion that reads the text instead of panicking here first.
pub fn body_text(request: &Request) -> String {
    String::from_utf8_lossy(&request.body).into_owned()
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
