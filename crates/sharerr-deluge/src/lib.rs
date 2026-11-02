//! Deluge Web JSON-RPC client, for Deluge 2.
//!
//! sharerr uses Deluge for exactly one thing: seeding files that already exist,
//! from where they already are. See [`DelugeClient::add`] for the option that
//! keeps a daemon-wide "move completed" setting away from the media.
//!
//! # Two layers, one session
//!
//! This speaks to `deluge-web`, which proxies to a `deluged` it may not be
//! connected to yet. Until it is, every `core.*` call answers "Unknown method",
//! so signing in is three steps: `auth.login` for the session cookie,
//! `web.connected`, and `web.connect` to the first known host when it is not.
//! The session expires and the daemon can restart under a long-lived web UI, so
//! every call redoes all three once when its answer says either happened.
//!
//! # Failures arrive in the body
//!
//! Every answer is HTTP 200 with an `error` object, so a status check alone
//! proves nothing.
//!
//! # Labels are a plugin
//!
//! Deluge has no categories, and labels come from its Label plugin, which is off
//! by default. A torrent seeds identically without one, so a label that cannot be
//! applied is a warning and never fails an add.

use std::collections::BTreeMap;

use async_trait::async_trait;
use base64::Engine as _;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sharerr_client::{
    AddRequest, ClientError, ClientKind, Result, SeedingLimits, SessionSlot, TorrentClient,
    TorrentFileEntry, TorrentSummary, http_client, rpc_endpoint,
};
use url::Url;

const KIND: ClientKind = ClientKind::Deluge;

/// The JSON-RPC endpoint, at a fixed path under the web root.
const RPC_PATH: &str = "json";

/// The cookie `auth.login` hands out, replayed by hand so that this one client
/// does not turn on reqwest's cookie store for the whole workspace.
const SESSION_COOKIE: &str = "_session_id";

/// `error.code` when the session is missing or expired.
const NOT_AUTHENTICATED: i64 = 1;
/// `error.code` for a method nobody registered: the daemon is not connected, or
/// the plugin providing it is off.
const UNKNOWN_METHOD: i64 = 2;

/// A Deluge Web client. Holds the session cookie, so signing in is paid once
/// rather than on every call.
pub struct DelugeClient {
    http: reqwest::Client,
    endpoint: Url,
    password: SecretString,
    session: SessionSlot,
}

impl std::fmt::Debug for DelugeClient {
    /// Hand-written so the password cannot reach a log through a derived `Debug`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        sharerr_client::debug_redacted(
            f,
            "DelugeClient",
            &[("endpoint", &self.endpoint.as_str() as &dyn std::fmt::Debug)],
            &["password"],
        )
    }
}

#[derive(Debug, Deserialize)]
struct Reply {
    #[serde(default)]
    result: Value,
    #[serde(default)]
    error: Option<RpcError>,
}

#[derive(Debug, Deserialize)]
struct RpcError {
    #[serde(default)]
    message: String,
    #[serde(default)]
    code: i64,
}

impl DelugeClient {
    /// Build a client for the Deluge web UI at `base`: the web root,
    /// `http://host:8112`, with any reverse-proxy subpath preserved.
    pub fn new(base: &Url, password: SecretString) -> Result<Self> {
        Ok(Self {
            http: http_client()?,
            endpoint: rpc_endpoint(base, RPC_PATH)?,
            password,
            session: SessionSlot::default(),
        })
    }

    /// One round trip, keeping any session cookie the answer carries.
    async fn post(&self, method: &str, params: &Value) -> Result<Reply> {
        let body = json!({ "method": method, "params": params, "id": 1 });
        let mut request = self.http.post(self.endpoint.clone()).json(&body);
        if let Some(session) = self.session.get() {
            request = request.header(reqwest::header::COOKIE, session);
        }

        let response = request
            .send()
            .await
            .map_err(|e| sharerr_client::unreachable(KIND, self.endpoint.as_str(), &e))?;
        sharerr_client::check_status(KIND, response.status(), method)?;

        let cookie = response
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .filter_map(|value| value.split(';').next())
            .find(|pair| pair.starts_with(&format!("{SESSION_COOKIE}=")))
            .map(str::to_owned);
        if let Some(cookie) = cookie {
            self.session.set(cookie);
        }

        response
            .json()
            .await
            .map_err(|e| sharerr_client::malformed(KIND, method, e))
    }

    /// One round trip with no second sign-in, for a call where a refusal is its
    /// own answer rather than a sign the session went.
    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        into_result(method, self.post(method, &params).await?)
    }

    /// Issue one call, signing in again once if the session or the daemon
    /// connection has gone. Once: a second refusal means something other than
    /// expiry (a plugin that is off), and looping on it would be a hang.
    async fn rpc(&self, method: &str, params: Value) -> Result<Value> {
        let reply = self.post(method, &params).await?;
        let reply = match &reply.error {
            Some(error) if matches!(error.code, NOT_AUTHENTICATED | UNKNOWN_METHOD) => {
                tracing::debug!(method, code = error.code, "signing in to Deluge again");
                self.connect().await?;
                self.post(method, &params).await?
            }
            _ => reply,
        };
        into_result(method, reply)
    }

    /// Sign in and make sure the web UI is attached to a daemon.
    async fn connect(&self) -> Result<()> {
        let login = self
            .call("auth.login", json!([self.password.expose_secret()]))
            .await?;
        if !login.as_bool().unwrap_or(false) {
            return Err(ClientError::AuthRejected { kind: KIND });
        }

        let connected = self.call("web.connected", json!([])).await?;
        if connected.as_bool().unwrap_or(false) {
            return Ok(());
        }

        let hosts = self.call("web.get_hosts", json!([])).await?;
        // Each row is `[id, host, port, status]`, an array rather than an object.
        let hosts: Vec<Vec<Value>> = decode("web.get_hosts", hosts)?;
        let Some(host) = hosts.first().and_then(|row| row.first()) else {
            return Err(ClientError::Api {
                kind: KIND,
                detail: "the web UI knows no daemon to connect to — add one in its \
                         Connection Manager"
                    .to_owned(),
            });
        };
        self.call("web.connect", json!([host])).await.map(|_| ())
    }

    /// Read one torrent's status fields. A hash the daemon does not hold comes
    /// back as an empty object, which each row type's defaults read as nothing.
    async fn torrent_status<T: serde::de::DeserializeOwned>(
        &self,
        hash: &str,
        keys: &[&str],
    ) -> Result<T> {
        let call = "core.get_torrent_status";
        decode(call, self.rpc(call, json!([hash, keys])).await?)
    }

    /// Apply a torrent's label, creating it first if this is its first use. Never
    /// fails the add: see the module docs.
    async fn apply_label(&self, hash: &str, label: &str) {
        // Deluge folds labels to lowercase on both calls; doing it here keeps
        // what gets logged the same as what it stored.
        let label = label.to_ascii_lowercase();
        // `call`, not `rpc`: the add just succeeded, so "Unknown method" here means
        // the plugin is off rather than the session gone.
        let applied = match self.call("label.add", json!([label])).await {
            Err(err) if !err.to_string().contains("already exists") => Err(err),
            _ => self.call("label.set_torrent", json!([hash, label])).await,
        };
        if let Err(err) = applied {
            tracing::warn!(
                label,
                %err,
                "could not label the torrent — is Deluge's Label plugin enabled? It seeds regardless"
            );
        }
    }
}

/// Unwrap a reply's `error` object, the only place Deluge reports failure.
fn into_result(method: &str, reply: Reply) -> Result<Value> {
    match reply.error {
        Some(error) => Err(ClientError::Api {
            kind: KIND,
            detail: format!("{method}: {}", error.message),
        }),
        None => Ok(reply.result),
    }
}

/// Decode a result, naming the call on failure. Typed for the reason
/// `sharerr-transmission` gives: a renamed field must be an error, not an empty
/// value that makes reconciliation re-add every torrent.
fn decode<T: serde::de::DeserializeOwned>(call: &str, result: Value) -> Result<T> {
    serde_json::from_value(result).map_err(|err| sharerr_client::malformed(KIND, call, err))
}

/// `core.set_torrent_options`' and `core.add_torrent_file`'s shared names for a
/// seeding goal, carrying only the fields with an opinion.
fn limit_options(limits: &SeedingLimits) -> Map<String, Value> {
    let mut options = Map::new();
    if let Some(kib) = limits.upload_limit_kib {
        options.insert("max_upload_speed".to_owned(), json!(kib));
    }
    if let Some(ratio) = limits.ratio_limit {
        options.insert("stop_at_ratio".to_owned(), json!(true));
        options.insert("stop_ratio".to_owned(), json!(ratio));
    }
    options
}

/// The status fields [`TorrentClient::list`] asks for.
const LIST_KEYS: [&str; 8] = [
    "name",
    "save_path",
    "state",
    "label",
    "ratio",
    "stop_at_ratio",
    "stop_ratio",
    "max_upload_speed",
];

#[derive(Debug, Deserialize)]
struct ListedTorrent {
    name: String,
    save_path: String,
    state: String,
    /// Absent, not empty, with the Label plugin off.
    #[serde(default)]
    label: String,
    /// `-1` until the torrent has data to compute one against.
    #[serde(default)]
    ratio: f64,
    #[serde(default)]
    stop_at_ratio: bool,
    #[serde(default)]
    stop_ratio: f64,
    /// KiB/s, the unit sharerr writes; `-1` means no per-torrent cap.
    #[serde(default)]
    max_upload_speed: f64,
}

impl ListedTorrent {
    fn into_summary(self, hash: &str) -> TorrentSummary {
        let content_path = sharerr_client::content_path(&self.save_path, &self.name);
        // A float on the wire, holding the whole number `max_upload_speed` was
        // given; the saturating cast cannot wrap, and a negative is ruled out.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "see the comment above"
        )]
        let upload_limit_kib =
            (self.max_upload_speed >= 0.0).then_some(self.max_upload_speed.round() as u64);
        TorrentSummary {
            // Lowercased because sharerr joins on this against its own store.
            hash: hash.to_ascii_lowercase(),
            save_path: self.save_path,
            content_path,
            category: self.label.clone(),
            tags: if self.label.is_empty() {
                Vec::new()
            } else {
                vec![self.label]
            },
            is_seeding: self.state == "Seeding",
            is_stopped: self.state == "Paused",
            ratio: (self.ratio >= 0.0).then_some(self.ratio),
            ratio_limit: self.stop_at_ratio.then_some(self.stop_ratio),
            upload_limit_kib,
            name: self.name,
        }
    }
}

#[derive(Debug, Deserialize)]
struct FilesTorrent {
    #[serde(default)]
    files: Vec<ListedFile>,
}

#[derive(Debug, Deserialize)]
struct ListedFile {
    path: String,
    size: u64,
}

#[derive(Debug, Deserialize)]
struct TrackersTorrent {
    #[serde(default)]
    trackers: Vec<Tracker>,
}

#[derive(Debug, Deserialize)]
struct Tracker {
    url: String,
    #[serde(default)]
    tier: u64,
}

/// One entry per tier, in the shape `core.set_torrent_trackers` takes.
fn tiered<'a>(urls: impl Iterator<Item = &'a str>) -> Vec<Value> {
    urls.enumerate()
        .map(|(tier, url)| json!({ "url": url, "tier": tier }))
        .collect()
}

#[async_trait]
impl TorrentClient for DelugeClient {
    fn kind(&self) -> ClientKind {
        KIND
    }

    async fn login(&self) -> Result<()> {
        self.connect().await
    }

    async fn version(&self) -> Result<String> {
        let version = self.rpc("daemon.get_version", json!([])).await?;
        Ok(version.as_str().unwrap_or("unknown").to_owned())
    }

    async fn list(&self, category: Option<&str>) -> Result<Vec<TorrentSummary>> {
        let call = "core.get_torrents_status";
        let listed: BTreeMap<String, ListedTorrent> =
            decode(call, self.rpc(call, json!([{}, LIST_KEYS])).await?)?;

        // Applied here: the server-side filter is the Label plugin's, which may
        // be off, and Deluge has folded the label to lowercase.
        Ok(listed
            .into_iter()
            .filter(|(_, torrent)| category.is_none_or(|c| torrent.label.eq_ignore_ascii_case(c)))
            .map(|(hash, torrent)| torrent.into_summary(&hash))
            .collect())
    }

    async fn files(&self, hash: &str) -> Result<Vec<TorrentFileEntry>> {
        let torrent: FilesTorrent = self.torrent_status(hash, &["files"]).await?;
        Ok(torrent
            .files
            .into_iter()
            .map(|file| TorrentFileEntry {
                name: file.path,
                size: file.size,
            })
            .collect())
    }

    /// `move_completed: false` is the field that matters: Deluge's own default
    /// comes from the daemon's preferences, and an operator with "move completed"
    /// on would otherwise have their media relocated the moment its check ended.
    async fn add(&self, request: &AddRequest<'_>) -> Result<()> {
        let mut options = limit_options(&request.limits);
        options.insert("download_location".to_owned(), json!(request.save_path));
        options.insert("move_completed".to_owned(), json!(false));
        options.insert("add_paused".to_owned(), json!(request.stopped));
        options.insert("seed_mode".to_owned(), json!(request.skip_checking));

        let metainfo = base64::engine::general_purpose::STANDARD.encode(request.data);
        let added = self
            .rpc(
                "core.add_torrent_file",
                json!([request.filename, metainfo, options]),
            )
            .await;
        match added {
            Ok(_) => {}
            // Success for sharerr's purposes: the point was to have it seeding. The
            // goal in the add's options never reached it, so it is restated.
            Err(err) if err.to_string().contains("already in session") => {
                tracing::debug!(file = request.filename, "Deluge already had this torrent");
                self.set_limits(request.info_hash, &request.limits).await?;
            }
            Err(err) => return Err(err),
        }

        if let Some(label) = request.category {
            self.apply_label(request.info_hash, label).await;
        }
        Ok(())
    }

    async fn remove(&self, hash: &str) -> Result<()> {
        // `false` is `remove_data`, and the whole point: the media is the
        // operator's and predates sharerr knowing about it. A torrent already
        // gone is what was asked for, so it is not an error.
        match self.rpc("core.remove_torrent", json!([hash, false])).await {
            Err(err) if !err.to_string().contains("not in session") => Err(err),
            _ => Ok(()),
        }
    }

    async fn set_trackers(&self, hash: &str, urls: &[Url]) -> Result<()> {
        let trackers = tiered(urls.iter().map(Url::as_str));
        self.rpc("core.set_torrent_trackers", json!([hash, trackers]))
            .await
            .map(|_| ())
    }

    async fn add_trackers(&self, hash: &str, urls: &[Url]) -> Result<()> {
        if urls.is_empty() {
            return Ok(());
        }
        let existing: TrackersTorrent = self.torrent_status(hash, &["trackers"]).await?;
        let new = sharerr_client::missing_urls(urls, |url| {
            existing.trackers.iter().any(|t| t.url == url)
        });
        if new.is_empty() {
            return Ok(());
        }

        // New tiers first, so the endpoint sharerr knows is live is tried before
        // whatever the torrent carried. What was there keeps its relative tiers,
        // shifted down: it is the operator's, and this call must not drop any.
        let shift = new.len() as u64;
        let mut trackers = tiered(new.into_iter());
        trackers.extend(
            existing
                .trackers
                .iter()
                .map(|t| json!({ "url": t.url, "tier": t.tier + shift })),
        );
        self.rpc("core.set_torrent_trackers", json!([hash, trackers]))
            .await
            .map(|_| ())
    }

    async fn apply_limits(&self, hash: &str, limits: &SeedingLimits) -> Result<()> {
        let options = limit_options(limits);
        self.rpc("core.set_torrent_options", json!([[hash], options]))
            .await?;
        tracing::info!(hash, ?limits, "applied the configured seeding limits");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use wiremock::matchers::{body_partial_json, header, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const PASSWORD: &str = "deluge-test-password";
    const COOKIE: &str = "_session_id=abc123";

    fn client(server: &MockServer) -> DelugeClient {
        let base = sharerr_testkit::mock::base_url(server);
        DelugeClient::new(&base, SecretString::from(PASSWORD)).unwrap()
    }

    fn ok(result: Value) -> ResponseTemplate {
        ResponseTemplate::new(200)
            .set_body_json(json!({ "id": 1, "result": result, "error": null }))
    }

    fn failed(code: i64, message: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({
            "id": 1, "result": null, "error": { "message": message, "code": code }
        }))
    }

    async fn answer(server: &MockServer, rpc: &str, response: ResponseTemplate) {
        Mock::given(method("POST"))
            .and(body_partial_json(json!({ "method": rpc })))
            .respond_with(response)
            .mount(server)
            .await;
    }

    /// A web UI that accepts the password and is already attached to a daemon.
    async fn mount_session(server: &MockServer) {
        answer(
            server,
            "auth.login",
            ok(json!(true)).insert_header("Set-Cookie", format!("{COOKIE}; Path=/json").as_str()),
        )
        .await;
        answer(server, "web.connected", ok(json!(true))).await;
    }

    /// The body of each request, in order, for the methods the session setup
    /// does not account for.
    async fn calls(server: &MockServer, rpc: &str) -> Vec<Value> {
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter_map(|r| serde_json::from_slice::<Value>(&r.body).ok())
            .filter(|b| b["method"] == rpc)
            .map(|b| b["params"].clone())
            .collect()
    }

    #[tokio::test]
    async fn logging_in_sends_the_password_and_replays_the_session_cookie() {
        let server = MockServer::start().await;
        mount_session(&server).await;
        Mock::given(method("POST"))
            .and(body_partial_json(json!({ "method": "daemon.get_version" })))
            .and(header("cookie", COOKIE))
            .respond_with(ok(json!("2.1.1")))
            .expect(1)
            .mount(&server)
            .await;

        let client = client(&server);
        client.login().await.unwrap();
        assert_eq!(client.version().await.unwrap(), "2.1.1");
        assert_eq!(calls(&server, "auth.login").await, vec![json!([PASSWORD])]);
    }

    /// `auth.login` says no with a 200 and `false`, not with a status code.
    #[tokio::test]
    async fn a_rejected_password_is_reported_as_such() {
        let server = MockServer::start().await;
        answer(&server, "auth.login", ok(json!(false))).await;

        let err = client(&server).login().await.unwrap_err();
        assert!(err.is_auth_failure(), "{err}");
    }

    #[tokio::test]
    async fn nothing_listening_is_reported_as_unreachable() {
        let port = sharerr_testkit::net::closed_port();
        let base = Url::parse(&format!("http://127.0.0.1:{port}")).unwrap();
        let client = DelugeClient::new(&base, SecretString::from(PASSWORD)).unwrap();

        let err = client.login().await.unwrap_err();
        assert!(err.is_unreachable(), "{err}");
    }

    /// Until the web UI is attached to a daemon every `core.*` call is an
    /// "Unknown method", so signing in has to attach it.
    #[tokio::test]
    async fn logging_in_attaches_a_web_ui_with_no_daemon_to_its_first_host() {
        let server = MockServer::start().await;
        answer(&server, "auth.login", ok(json!(true))).await;
        answer(&server, "web.connected", ok(json!(false))).await;
        answer(
            &server,
            "web.get_hosts",
            ok(json!([
                ["host-one", "127.0.0.1", 58846, "Online"],
                ["host-two", "10.0.0.2", 58846, "Offline"]
            ])),
        )
        .await;
        answer(&server, "web.connect", ok(json!([]))).await;

        client(&server).login().await.unwrap();
        assert_eq!(
            calls(&server, "web.connect").await,
            vec![json!(["host-one"])]
        );
    }

    #[tokio::test]
    async fn a_web_ui_that_knows_no_daemon_says_where_to_add_one() {
        let server = MockServer::start().await;
        answer(&server, "auth.login", ok(json!(true))).await;
        answer(&server, "web.connected", ok(json!(false))).await;
        answer(&server, "web.get_hosts", ok(json!([]))).await;

        let err = client(&server).login().await.unwrap_err();
        assert!(err.to_string().contains("Connection Manager"), "{err}");
    }

    /// A long-lived client outlasts its session: the call signs in again and
    /// succeeds rather than surfacing the expiry.
    #[tokio::test]
    async fn an_expired_session_is_renewed_once_and_the_call_retried() {
        let server = MockServer::start().await;
        mount_session(&server).await;
        Mock::given(method("POST"))
            .and(body_partial_json(json!({ "method": "daemon.get_version" })))
            .respond_with(failed(NOT_AUTHENTICATED, "Not authenticated"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        answer(&server, "daemon.get_version", ok(json!("2.1.1"))).await;

        assert_eq!(client(&server).version().await.unwrap(), "2.1.1");
        assert_eq!(calls(&server, "auth.login").await.len(), 1);
    }

    /// A second refusal is not expiry; retrying forever would be a hang.
    #[tokio::test]
    async fn a_call_refused_twice_is_reported_rather_than_retried() {
        let server = MockServer::start().await;
        mount_session(&server).await;
        answer(
            &server,
            "daemon.get_version",
            failed(NOT_AUTHENTICATED, "Not authenticated"),
        )
        .await;

        let err = client(&server).version().await.unwrap_err();
        assert!(err.to_string().contains("Not authenticated"), "{err}");
        assert_eq!(calls(&server, "daemon.get_version").await.len(), 2);
    }

    /// The constraint the whole project is built around: withdrawing a share must
    /// never delete the operator's media.
    #[tokio::test]
    async fn removal_never_deletes_the_data() {
        let server = MockServer::start().await;
        answer(&server, "core.remove_torrent", ok(json!(true))).await;

        client(&server).remove("abc").await.unwrap();
        assert_eq!(
            calls(&server, "core.remove_torrent").await,
            vec![json!(["abc", false])]
        );
    }

    #[tokio::test]
    async fn removing_a_torrent_already_gone_is_not_an_error() {
        let server = MockServer::start().await;
        answer(
            &server,
            "core.remove_torrent",
            failed(3, "torrent_id abc not in session."),
        )
        .await;
        client(&server).remove("abc").await.unwrap();
    }

    #[tokio::test]
    async fn any_other_removal_failure_is_reported() {
        let server = MockServer::start().await;
        answer(&server, "core.remove_torrent", failed(3, "disk on fire")).await;
        let err = client(&server).remove("abc").await.unwrap_err();
        assert!(err.to_string().contains("disk on fire"), "{err}");
    }

    /// The add must point Deluge at the data that is already there and switch
    /// off the daemon-wide move, whatever the operator's preferences say.
    #[tokio::test]
    async fn adding_points_at_the_existing_data_and_never_moves_it() {
        let server = MockServer::start().await;
        answer(&server, "core.add_torrent_file", ok(json!("abc123"))).await;

        let data = b"d8:announce0:e";
        let request = AddRequest::new(data, "abc123", "x.torrent", "/downloads/tv");
        client(&server).add(&request).await.unwrap();

        let params = &calls(&server, "core.add_torrent_file").await[0];
        assert_eq!(params[0], "x.torrent");
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(params[1].as_str().unwrap())
                .unwrap(),
            data
        );
        let options = &params[2];
        assert_eq!(options["download_location"], "/downloads/tv");
        assert_eq!(options["move_completed"], false);
        assert_eq!(options["add_paused"], false);
        assert_eq!(options["seed_mode"], false, "verifying is the default");
        assert!(options.get("max_upload_speed").is_none());
        assert!(options.get("stop_at_ratio").is_none());
    }

    #[tokio::test]
    async fn a_seeding_goal_and_skip_checking_land_in_the_add_options() {
        let server = MockServer::start().await;
        answer(&server, "core.add_torrent_file", ok(json!("abc123"))).await;

        let request = AddRequest::new(b"x", "abc123", "x.torrent", "/downloads")
            .upload_limit_kib(512)
            .ratio_limit(2.5)
            .skip_checking(true)
            .stopped(true);
        client(&server).add(&request).await.unwrap();

        let options = calls(&server, "core.add_torrent_file").await[0][2].clone();
        assert_eq!(options["max_upload_speed"], 512);
        assert_eq!(options["stop_at_ratio"], true);
        assert_eq!(options["stop_ratio"], 2.5);
        assert_eq!(options["seed_mode"], true);
        assert_eq!(options["add_paused"], true);
    }

    /// A torrent Deluge already had is what sharerr wanted, but the goal in the
    /// add's options never reached it, so it has to be restated.
    #[tokio::test]
    async fn a_duplicate_add_succeeds_and_still_gets_its_seeding_goal() {
        let server = MockServer::start().await;
        answer(
            &server,
            "core.add_torrent_file",
            failed(3, "Torrent already in session (abc123)."),
        )
        .await;
        answer(&server, "core.set_torrent_options", ok(Value::Null)).await;

        let request = AddRequest::new(b"x", "abc123", "x.torrent", "/d").ratio_limit(1.5);
        client(&server).add(&request).await.unwrap();

        let params = &calls(&server, "core.set_torrent_options").await[0];
        assert_eq!(params[0], json!(["abc123"]));
        assert_eq!(params[1]["stop_ratio"], 1.5);
    }

    #[tokio::test]
    async fn any_other_add_failure_is_reported() {
        let server = MockServer::start().await;
        answer(&server, "core.add_torrent_file", failed(3, "bad metainfo")).await;
        let request = AddRequest::new(b"x", "abc123", "x.torrent", "/d");
        let err = client(&server).add(&request).await.unwrap_err();
        assert!(err.to_string().contains("bad metainfo"), "{err}");
    }

    #[tokio::test]
    async fn the_category_becomes_a_lowercase_label_created_on_first_use() {
        let server = MockServer::start().await;
        answer(&server, "core.add_torrent_file", ok(json!("abc123"))).await;
        answer(
            &server,
            "label.add",
            failed(3, "Unknown Label: Label already exists"),
        )
        .await;
        answer(&server, "label.set_torrent", ok(Value::Null)).await;

        let request = AddRequest::new(b"x", "abc123", "x.torrent", "/d").category("Sharerr");
        client(&server).add(&request).await.unwrap();

        assert_eq!(
            calls(&server, "label.set_torrent").await,
            vec![json!(["abc123", "sharerr"])],
            "an existing label is not a reason to skip applying it"
        );
    }

    /// The Label plugin is off by default, and the torrent seeds regardless.
    #[tokio::test]
    async fn a_missing_label_plugin_does_not_fail_the_add() {
        let server = MockServer::start().await;
        mount_session(&server).await;
        answer(&server, "core.add_torrent_file", ok(json!("abc123"))).await;
        answer(
            &server,
            "label.add",
            failed(UNKNOWN_METHOD, "Unknown method"),
        )
        .await;

        let request = AddRequest::new(b"x", "abc123", "x.torrent", "/d").category("sharerr");
        client(&server).add(&request).await.unwrap();
        assert!(calls(&server, "label.set_torrent").await.is_empty());
    }

    #[tokio::test]
    async fn listing_maps_state_paths_and_limits() {
        let server = MockServer::start().await;
        answer(
            &server,
            "core.get_torrents_status",
            ok(json!({
                "AAAA": { "name": "a.mkv", "save_path": "/downloads/", "state": "Seeding",
                          "label": "sharerr", "ratio": 1.5, "stop_at_ratio": true,
                          "stop_ratio": 2.0, "max_upload_speed": 512.0 },
                "bbbb": { "name": "b.mkv", "save_path": "/downloads", "state": "Paused",
                          "ratio": -1.0, "stop_at_ratio": false, "stop_ratio": 2.0,
                          "max_upload_speed": -1.0 },
                "cccc": { "name": "c.mkv", "save_path": "/downloads", "state": "Checking" }
            })),
        )
        .await;

        let all = client(&server).list(None).await.unwrap();
        assert_eq!(all.len(), 3);

        let a = &all[0];
        assert_eq!(a.hash, "aaaa", "lowercased to match sharerr's own store");
        assert_eq!(a.content_path, "/downloads/a.mkv");
        assert!(a.is_seeding && !a.is_stopped);
        assert_eq!(a.ratio, Some(1.5));
        assert_eq!(a.ratio_limit, Some(2.0));
        assert_eq!(a.upload_limit_kib, Some(512));
        assert_eq!(a.tags, vec!["sharerr"]);
        assert_eq!(a.category, "sharerr");

        let b = &all[1];
        assert!(b.is_stopped && !b.is_seeding);
        assert_eq!(b.ratio, None, "-1 is no ratio yet, not a real one");
        assert_eq!(
            b.ratio_limit, None,
            "a limit not switched on is not in force"
        );
        assert_eq!(b.upload_limit_kib, None, "-1 is no cap");
        assert!(b.tags.is_empty(), "no Label plugin, no label");

        assert!(!all[2].is_seeding && !all[2].is_stopped);
    }

    #[tokio::test]
    async fn a_category_filter_is_applied_here_and_ignores_case() {
        let server = MockServer::start().await;
        answer(
            &server,
            "core.get_torrents_status",
            ok(json!({
                "aa": { "name": "a", "save_path": "/d", "state": "Seeding", "label": "sharerr" },
                "bb": { "name": "b", "save_path": "/d", "state": "Seeding", "label": "other" },
                "cc": { "name": "c", "save_path": "/d", "state": "Seeding" }
            })),
        )
        .await;

        let filtered = client(&server).list(Some("Sharerr")).await.unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].hash, "aa");
    }

    /// A renamed field must be an error: an empty hash or path would make
    /// reconciliation re-add every torrent on every pass.
    #[tokio::test]
    async fn a_listing_of_the_wrong_shape_is_malformed_and_names_the_call() {
        let server = MockServer::start().await;
        answer(
            &server,
            "core.get_torrents_status",
            ok(json!({ "aa": { "title": "renamed" } })),
        )
        .await;

        let err = client(&server).list(None).await.unwrap_err();
        assert!(matches!(err, ClientError::Malformed { .. }), "{err:?}");
        assert!(
            err.to_string().contains("core.get_torrents_status"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn a_body_that_is_not_json_is_reported_as_malformed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html>nope</html>"))
            .mount(&server)
            .await;

        let err = client(&server).version().await.unwrap_err();
        assert!(matches!(err, ClientError::Malformed { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn files_lists_each_entry_with_its_size() {
        let server = MockServer::start().await;
        answer(
            &server,
            "core.get_torrent_status",
            ok(json!({ "files": [
                { "index": 0, "path": "Show/S01E01.mkv", "size": 1024, "offset": 0 },
                { "index": 1, "path": "Show/S01E01.srt", "size": 12, "offset": 1024 }
            ]})),
        )
        .await;

        let files = client(&server).files("abc").await.unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].name, "Show/S01E01.mkv");
        assert_eq!(files[1].size, 12);
        assert_eq!(
            calls(&server, "core.get_torrent_status").await,
            vec![json!(["abc", ["files"]])]
        );
    }

    /// An unknown hash answers an empty object, which is no files, not an error.
    #[tokio::test]
    async fn files_of_a_torrent_the_daemon_does_not_hold_is_empty() {
        let server = MockServer::start().await;
        answer(&server, "core.get_torrent_status", ok(json!({}))).await;
        assert!(client(&server).files("gone").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn set_trackers_replaces_the_list_with_one_tier_per_url() {
        let server = MockServer::start().await;
        answer(&server, "core.set_torrent_trackers", ok(Value::Null)).await;

        client(&server)
            .set_trackers(
                "abc",
                &[
                    Url::parse("http://new.example:41234/announce").unwrap(),
                    Url::parse("http://old.example:8477/announce").unwrap(),
                ],
            )
            .await
            .unwrap();

        assert_eq!(
            calls(&server, "core.set_torrent_trackers").await,
            vec![json!(["abc", [
                { "url": "http://new.example:41234/announce", "tier": 0 },
                { "url": "http://old.example:8477/announce", "tier": 1 }
            ]])]
        );
    }

    /// The additive form keeps every tracker the operator's torrent came with, in
    /// its relative order, behind sharerr's own.
    #[tokio::test]
    async fn add_trackers_prepends_without_dropping_the_existing_list() {
        let server = MockServer::start().await;
        answer(
            &server,
            "core.get_torrent_status",
            ok(json!({ "trackers": [
                { "url": "http://theirs.example/announce", "tier": 0 },
                { "url": "http://backup.example/announce", "tier": 1 }
            ]})),
        )
        .await;
        answer(&server, "core.set_torrent_trackers", ok(Value::Null)).await;

        client(&server)
            .add_trackers(
                "abc",
                &[Url::parse("http://sharerr.example/announce").unwrap()],
            )
            .await
            .unwrap();

        assert_eq!(
            calls(&server, "core.set_torrent_trackers").await,
            vec![json!(["abc", [
                { "url": "http://sharerr.example/announce", "tier": 0 },
                { "url": "http://theirs.example/announce", "tier": 1 },
                { "url": "http://backup.example/announce", "tier": 2 }
            ]])]
        );
    }

    #[tokio::test]
    async fn add_trackers_sends_nothing_when_the_url_is_already_listed() {
        let server = MockServer::start().await;
        answer(
            &server,
            "core.get_torrent_status",
            ok(json!({ "trackers": [{ "url": "http://sharerr.example/announce", "tier": 0 }] })),
        )
        .await;

        client(&server)
            .add_trackers(
                "abc",
                &[Url::parse("http://sharerr.example/announce").unwrap()],
            )
            .await
            .unwrap();
        assert!(calls(&server, "core.set_torrent_trackers").await.is_empty());
    }

    #[tokio::test]
    async fn add_trackers_with_no_urls_sends_nothing() {
        let server = MockServer::start().await;
        client(&server).add_trackers("abc", &[]).await.unwrap();
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    /// A ratio sharerr has no opinion on is not mentioned, so the operator's stays.
    #[tokio::test]
    async fn set_limits_carries_only_the_stated_fields() {
        let server = MockServer::start().await;
        answer(&server, "core.set_torrent_options", ok(Value::Null)).await;

        let limits = SeedingLimits {
            upload_limit_kib: Some(512),
            ratio_limit: None,
        };
        client(&server).set_limits("abc", &limits).await.unwrap();

        assert_eq!(
            calls(&server, "core.set_torrent_options").await,
            vec![json!([["abc"], { "max_upload_speed": 512 }])]
        );
    }

    #[tokio::test]
    async fn set_limits_with_nothing_configured_sends_nothing() {
        let server = MockServer::start().await;
        client(&server)
            .set_limits("abc", &SeedingLimits::default())
            .await
            .unwrap();
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn export_reports_that_deluge_cannot_produce_the_file() {
        let server = MockServer::start().await;
        assert_eq!(client(&server).export("abc").await.unwrap(), None);
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[test]
    fn a_subpath_base_url_keeps_its_prefix() {
        let base = Url::parse("http://box.lan/deluge").unwrap();
        let client = DelugeClient::new(&base, SecretString::from(PASSWORD)).unwrap();
        assert_eq!(client.endpoint.as_str(), "http://box.lan/deluge/json");
    }

    #[test]
    fn debug_does_not_leak_the_password() {
        let base = Url::parse("http://box.lan").unwrap();
        let client = DelugeClient::new(&base, SecretString::from(PASSWORD)).unwrap();
        let rendered = format!("{client:?}");
        assert!(!rendered.contains(PASSWORD), "{rendered}");
        assert!(rendered.contains("redacted"), "{rendered}");
    }
}
