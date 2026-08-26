//! Transmission RPC client.
//!
//! sharerr uses Transmission for exactly one thing: seeding files that already
//! exist, from where they already are. Everything in this crate is shaped by the
//! requirement that adding a share must never move, re-link, or delete media — see
//! [`TransmissionClient::add`] for the two fields that enforce it.
//!
//! # The session-id handshake
//!
//! Transmission's RPC answers the *first* request of any session with `409
//! Conflict` and an `X-Transmission-Session-Id` header, expecting the client to
//! repeat the request carrying it. This is not an error condition and not
//! optional — it is CSRF protection, and every request thereafter must carry the
//! token until the server rotates it, at which point it 409s again.
//!
//! A client that treats the 409 as a failure appears to work against a freshly
//! restarted daemon and then breaks hours later, which is a miserable thing to
//! debug. `TransmissionClient::rpc` handles it centrally and retries once.
//!
//! # Why not a category
//!
//! Transmission has no categories. It has *labels*, a flat list per torrent, so
//! sharerr's category and tags both land there. That means a category filter is
//! applied by this crate rather than by the server — see [`TransmissionClient::list`].

use std::sync::Mutex;

use async_trait::async_trait;
use base64::Engine as _;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::{Value, json};
use sharerr_client::{
    AddRequest, ClientError, ClientKind, Result, TorrentClient, TorrentFileEntry, TorrentSummary,
    http_client, is_auth_rejection, normalise_base,
};
use url::Url;

const KIND: ClientKind = ClientKind::Transmission;

/// Transmission's RPC lives at a fixed path under the base URL.
const RPC_PATH: &str = "transmission/rpc";

/// The header Transmission both demands and supplies.
const SESSION_HEADER: &str = "X-Transmission-Session-Id";

/// A Transmission RPC client.
///
/// Holds the session id, so the 409 handshake is paid once rather than on every
/// call.
pub struct TransmissionClient {
    http: reqwest::Client,
    endpoint: Url,
    username: String,
    password: SecretString,
    /// Learned from a 409 and replayed on every later request. A plain mutex:
    /// it is only ever held for a clone or a store, never across an await.
    session: Mutex<Option<String>>,
}

impl std::fmt::Debug for TransmissionClient {
    /// Hand-written so the password cannot reach a log through a derived `Debug`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransmissionClient")
            .field("endpoint", &self.endpoint.as_str())
            .field("username", &self.username)
            .field("password", &"<redacted>")
            // `finish_non_exhaustive` rather than `finish`: the omission is
            // deliberate, and rendering `..` says so to whoever reads the log
            // instead of implying this is the whole struct.
            .finish_non_exhaustive()
    }
}

impl TransmissionClient {
    /// Build a client for the Transmission instance at `base`.
    ///
    /// `base` is the web root — `http://host:9091` — not the RPC path; the `/rpc`
    /// suffix is appended here so an operator cannot get it subtly wrong. A base
    /// with a path (a reverse-proxy subpath) is preserved.
    pub fn new(base: &Url, username: &str, password: SecretString) -> Result<Self> {
        let http = http_client()?;
        let base = normalise_base(base);
        let endpoint = base
            .join(RPC_PATH)
            .map_err(|e| ClientError::Config(format!("{base} is not a usable base URL: {e}")))?;

        Ok(Self {
            http,
            endpoint,
            username: username.to_owned(),
            password,
            session: Mutex::new(None),
        })
    }

    /// The session lock. A poisoned mutex only means another task panicked
    /// mid-store of a `String`; the value is still a valid (if stale) token,
    /// and a stale token is exactly what the 409 handshake already recovers
    /// from.
    fn session(&self) -> std::sync::MutexGuard<'_, Option<String>> {
        self.session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Issue one RPC call, paying the session handshake if the server asks.
    ///
    /// Retries exactly once on 409. A second 409 means something other than a
    /// rotated token — a proxy stripping the header, most likely — and looping on
    /// it would turn a misconfiguration into a hang.
    async fn rpc(&self, method: &str, arguments: Value) -> Result<Value> {
        let body = json!({ "method": method, "arguments": arguments });

        for attempt in 0..2 {
            let session = self.session().clone();
            let mut request = self
                .http
                .post(self.endpoint.clone())
                .basic_auth(&self.username, Some(self.password.expose_secret()))
                .json(&body);
            if let Some(session) = &session {
                request = request.header(SESSION_HEADER, session);
            }

            let response = request
                .send()
                .await
                .map_err(|e| sharerr_client::unreachable(KIND, self.endpoint.as_str(), &e))?;
            let status = response.status();

            if status == reqwest::StatusCode::CONFLICT && attempt == 0 {
                // Not a failure: this is Transmission handing over its CSRF token.
                let token = response
                    .headers()
                    .get(SESSION_HEADER)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned);
                match token {
                    Some(token) => {
                        tracing::debug!("picked up a Transmission session id");
                        *self.session() = Some(token);
                        continue;
                    }
                    None => {
                        return Err(ClientError::Api {
                            kind: KIND,
                            detail: "409 with no session id header — is a proxy stripping it?"
                                .to_owned(),
                        });
                    }
                }
            }

            if is_auth_rejection(status) {
                return Err(ClientError::AuthRejected { kind: KIND });
            }

            if !status.is_success() {
                return Err(ClientError::Api {
                    kind: KIND,
                    detail: format!("HTTP {status} from {method}"),
                });
            }

            let envelope: Envelope = response.json().await.map_err(|e| ClientError::Malformed {
                kind: KIND,
                detail: format!("reading the {method} response: {e}"),
            })?;

            // Transmission reports application-level failure in the body with a 200,
            // so the status code alone is not enough to know the call worked.
            if envelope.result != "success" {
                return Err(ClientError::Api {
                    kind: KIND,
                    detail: format!("{method}: {}", envelope.result),
                });
            }

            return Ok(envelope.arguments.unwrap_or(Value::Null));
        }

        Err(ClientError::Api {
            kind: KIND,
            detail: "Transmission kept asking for a new session id".to_owned(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct Envelope {
    result: String,
    #[serde(default)]
    arguments: Option<Value>,
}

/// Typed views of the `torrent-get` responses, mirroring the sibling qBittorrent
/// crate's wire structs. Typed on purpose: hand-walking `Value` with
/// `unwrap_or_default` would silently turn a renamed `hashString` into an empty
/// hash, which never matches the live set and would make reconciliation re-add
/// every torrent on every pass. A missing field here is a `Malformed` error that
/// names the call instead.
#[derive(Debug, Deserialize)]
struct TorrentGetResponse {
    torrents: Vec<ListedTorrent>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListedTorrent {
    hash_string: String,
    name: String,
    download_dir: String,
    #[serde(default)]
    labels: Vec<String>,
    status: i64,
    #[serde(default)]
    upload_ratio: f64,
    #[serde(default)]
    seed_ratio_limit: f64,
    /// `0` = use Transmission's global default, `1` = `seed_ratio_limit` applies
    /// to this torrent, `2` = unlimited. Only `1` is a fixed number this specific
    /// torrent is held to — see [`ratio_limit_reported`].
    #[serde(default)]
    seed_ratio_mode: i64,
}

/// The actual per-torrent limit Transmission is enforcing, or `None` when this
/// torrent instead follows the global default or has no limit at all.
fn ratio_limit_reported(torrent: &ListedTorrent) -> Option<f64> {
    (torrent.seed_ratio_mode == 1).then_some(torrent.seed_ratio_limit)
}

/// Transmission uses two negative sentinels for `uploadRatio`: `TR_RATIO_NA`
/// (`-2`, nothing downloaded yet — genuinely unknown) and `TR_RATIO_INF` (`-1`,
/// a completed torrent with nothing ever uploaded is still a real infinite
/// ratio, not a missing one).
fn ratio_reported(upload_ratio: f64) -> Option<f64> {
    if upload_ratio <= -2.0 {
        None
    } else if upload_ratio <= -1.0 {
        Some(f64::INFINITY)
    } else {
        Some(upload_ratio)
    }
}

#[derive(Debug, Deserialize)]
struct FilesResponse {
    torrents: Vec<FilesTorrent>,
}

#[derive(Debug, Deserialize)]
struct FilesTorrent {
    #[serde(default)]
    files: Vec<ListedFile>,
}

#[derive(Debug, Deserialize)]
struct ListedFile {
    name: String,
    length: u64,
}

/// The slice of a `torrent-get` reply that [`TorrentClient::add_trackers`]
/// reads back before rewriting the list.
#[derive(Debug, Deserialize)]
struct TrackerListResponse {
    #[serde(default)]
    torrents: Vec<TrackerListTorrent>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TrackerListTorrent {
    #[serde(default)]
    tracker_list: String,
}

/// Decode one RPC response body, naming the call on failure.
fn decode<T: serde::de::DeserializeOwned>(call: &str, arguments: Value) -> Result<T> {
    serde_json::from_value(arguments).map_err(|err| ClientError::Malformed {
        kind: KIND,
        detail: format!("reading the {call} response: {err}"),
    })
}

/// Transmission's numeric status values that mean "complete and uploading".
///
/// 5 is `TR_STATUS_SEED_WAIT` (queued to seed) and 6 is `TR_STATUS_SEED`. Anything
/// below is still downloading, checking, or stopped.
fn is_seeding_status(status: i64) -> bool {
    matches!(status, 5 | 6)
}

#[async_trait]
impl TorrentClient for TransmissionClient {
    fn kind(&self) -> ClientKind {
        KIND
    }

    async fn login(&self) -> Result<()> {
        // Transmission has no login call; `session-get` is the cheapest request
        // that proves both reachability and credentials, and it also primes the
        // session id so the first real call does not pay the handshake.
        self.rpc("session-get", json!({})).await.map(|_| ())
    }

    async fn version(&self) -> Result<String> {
        let arguments = self.rpc("session-get", json!({})).await?;
        Ok(arguments
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned())
    }

    async fn list(&self, category: Option<&str>) -> Result<Vec<TorrentSummary>> {
        let arguments = self
            .rpc(
                "torrent-get",
                json!({
                    "fields": ["hashString", "name", "downloadDir", "labels", "status",
                               "uploadRatio", "seedRatioLimit", "seedRatioMode"]
                }),
            )
            .await?;
        let listed: TorrentGetResponse = decode("torrent-get", arguments)?;

        let mut out = Vec::with_capacity(listed.torrents.len());
        for torrent in listed.torrents {
            // Transmission has no categories, so sharerr's category is simply one
            // of the labels. Filtering here rather than server-side is why the
            // trait warns callers not to assume the filter was applied remotely.
            if let Some(wanted) = category
                && !torrent.labels.iter().any(|l| l == wanted)
            {
                continue;
            }

            // Transmission has no `content_path`. Joining the download directory
            // with the torrent name reconstructs it, which is what qBittorrent
            // reports directly — so cross-seed detection behaves the same on both
            // clients rather than silently degrading here.
            let content_path = if torrent.download_dir.is_empty() || torrent.name.is_empty() {
                String::new()
            } else {
                format!(
                    "{}/{}",
                    torrent.download_dir.trim_end_matches('/'),
                    torrent.name
                )
            };

            let ratio_limit = ratio_limit_reported(&torrent);
            out.push(TorrentSummary {
                // Lowercased because sharerr joins on this against its own store,
                // which holds lowercase hex.
                hash: torrent.hash_string.to_ascii_lowercase(),
                name: torrent.name,
                save_path: torrent.download_dir,
                content_path,
                category: category.unwrap_or_default().to_owned(),
                is_seeding: is_seeding_status(torrent.status),
                ratio: ratio_reported(torrent.upload_ratio),
                ratio_limit,
                tags: torrent.labels,
            });
        }

        Ok(out)
    }

    async fn files(&self, hash: &str) -> Result<Vec<TorrentFileEntry>> {
        let arguments = self
            .rpc("torrent-get", json!({ "ids": [hash], "fields": ["files"] }))
            .await?;
        let listed: FilesResponse = decode("torrent-get files", arguments)?;

        Ok(listed
            .torrents
            .into_iter()
            .next()
            .map(|torrent| torrent.files)
            .unwrap_or_default()
            .into_iter()
            .map(|file| TorrentFileEntry {
                name: file.name,
                size: file.length,
            })
            .collect())
    }

    async fn add(&self, request: &AddRequest<'_>) -> Result<()> {
        let metainfo = base64::engine::general_purpose::STANDARD.encode(request.data);

        // Category and tags both become labels — Transmission has one flat list and
        // no notion of a category.
        let mut labels: Vec<&str> = Vec::new();
        if let Some(category) = request.category {
            labels.push(category);
        }
        labels.extend(request.tag_list());
        labels.dedup();

        let arguments = json!({
            "metainfo": metainfo,
            "download-dir": request.save_path,
            "paused": request.stopped,
            "labels": labels,
        });

        let result = self.rpc("torrent-add", arguments).await?;

        // A torrent Transmission already had comes back as `torrent-duplicate`
        // rather than an error. That is success for sharerr's purposes: the point
        // was to have it seeding, and it is.
        if result.get("torrent-duplicate").is_some() {
            tracing::debug!(
                file = request.filename,
                "Transmission already had this torrent"
            );
        }

        if request.skip_checking {
            // Transmission has no equivalent of qBittorrent's `skip_checking`, and
            // there is no safe way to fake one: telling it the data is complete
            // without verifying would mean seeding whatever happens to be at the
            // path. Verification is cheap relative to being wrong.
            tracing::warn!("Transmission has no skip-checking; it will verify the existing data");
        }

        // `torrent-add` itself takes no ratio/speed arguments — Transmission
        // only exposes those on `torrent-set`, the same call `set_trackers`
        // below already uses. Skipped entirely when no goal is configured,
        // so an ordinary add costs exactly one RPC call as it always has.
        if request.upload_limit_kib.is_some() || request.ratio_limit.is_some() {
            let hash = result
                .get("torrent-added")
                .or_else(|| result.get("torrent-duplicate"))
                .and_then(|t| t.get("hashString"))
                .and_then(Value::as_str);
            let Some(hash) = hash else {
                // The add itself already succeeded; a missing hash just means
                // there is nothing to attach a limit to.
                tracing::warn!(
                    "could not read the added torrent's hash — seeding goal not applied"
                );
                return Ok(());
            };

            let mut limits = json!({ "ids": [hash] });
            if let Some(kib) = request.upload_limit_kib {
                limits["uploadLimit"] = json!(kib);
                limits["uploadLimited"] = json!(true);
            }
            if let Some(ratio) = request.ratio_limit {
                limits["seedRatioLimit"] = json!(ratio);
                // 1 = this torrent's own limit, overriding the session default.
                limits["seedRatioMode"] = json!(1);
            }
            self.rpc("torrent-set", limits).await?;
        }

        Ok(())
    }

    async fn remove(&self, hash: &str) -> Result<()> {
        // `delete-local-data: false` is the whole point. The media is the
        // operator's and predates sharerr knowing about it; withdrawing a share
        // must never remove it.
        self.rpc(
            "torrent-remove",
            json!({ "ids": [hash], "delete-local-data": false }),
        )
        .await
        .map(|_| ())
    }

    async fn set_trackers(&self, hash: &str, urls: &[Url]) -> Result<()> {
        // `trackerList` (RPC 17, Transmission 4.0+): announce URLs one per line,
        // a blank line between tiers — so one URL per tier is a blank line
        // between every pair. Older daemons reject the argument outright, which
        // surfaces as an API error the caller logs; the alternative
        // (trackerAdd/trackerRemove by id) needs a read-modify-write against
        // per-torrent tracker ids and is not worth carrying for EOL versions.
        let list = urls
            .iter()
            .map(Url::to_string)
            .collect::<Vec<_>>()
            .join("\n\n");
        self.rpc("torrent-set", json!({ "ids": [hash], "trackerList": list }))
            .await
            .map(|_| ())
    }

    async fn add_trackers(&self, hash: &str, urls: &[Url]) -> Result<()> {
        if urls.is_empty() {
            return Ok(());
        }
        // Read-modify-write on the same `trackerList` (RPC 17) `set_trackers`
        // uses, rather than the `trackerAdd` argument it replaced: 4.0
        // deprecated `trackerAdd` in favour of exactly this field, and
        // carrying both spellings would mean two code paths for one call.
        let arguments = self
            .rpc(
                "torrent-get",
                json!({ "ids": [hash], "fields": ["trackerList"] }),
            )
            .await?;
        let response: TrackerListResponse = decode("torrent-get", arguments)?;
        let existing = response
            .torrents
            .into_iter()
            .next()
            .map(|t| t.tracker_list)
            .unwrap_or_default();

        // The new tiers go first, so the endpoint sharerr knows is live is
        // tried before whatever the torrent already carried — and the existing
        // list is appended verbatim, tier boundaries and all, because it is the
        // operator's and this call must not reorder or drop any of it.
        let mut tiers: Vec<String> = urls
            .iter()
            .map(Url::to_string)
            .filter(|url| !existing.lines().any(|line| line.trim() == url))
            .collect();
        if tiers.is_empty() {
            return Ok(());
        }
        if !existing.trim().is_empty() {
            tiers.push(existing);
        }
        let list = tiers.join("\n\n");

        self.rpc("torrent-set", json!({ "ids": [hash], "trackerList": list }))
            .await
            .map(|_| ())
    }

    async fn export(&self, _hash: &str) -> Result<Option<Vec<u8>>> {
        // `torrent-get`'s `torrentFile` field is a *path* on the daemon's own
        // filesystem, and Transmission has no call that returns the bytes. In
        // the deployments this targets the daemon is a separate container, so
        // that path is not one sharerr can open — and guessing when it might
        // be would make this succeed or fail on layout rather than on the API.
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use wiremock::matchers::{header_exists, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> TransmissionClient {
        let base = Url::parse(&server.uri()).unwrap();
        TransmissionClient::new(&base, "admin", SecretString::from("pw")).unwrap()
    }

    fn ok_body(arguments: Value) -> Value {
        json!({ "result": "success", "arguments": arguments })
    }

    /// Mount the 409 handshake followed by a successful answer.
    async fn mount_handshake_then(server: &MockServer, body: Value) {
        // The 409 is mounted first and expected exactly once; wiremock matches the
        // most recently mounted rule first, so the success rule is mounted after.
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .respond_with(
                ResponseTemplate::new(409).insert_header(SESSION_HEADER, "session-token-1"),
            )
            .up_to_n_times(1)
            .mount(server)
            .await;

        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .and(header_exists(SESSION_HEADER))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    /// The defining quirk of this API: the first request is answered with a 409 and
    /// a token, and the client is expected to repeat it. Treating that as a failure
    /// works against a freshly restarted daemon and breaks whenever the token
    /// rotates.
    #[tokio::test]
    async fn the_session_handshake_is_paid_and_the_call_succeeds() {
        let server = MockServer::start().await;
        mount_handshake_then(&server, ok_body(json!({ "version": "4.0.5" }))).await;

        let version = client(&server).version().await.unwrap();
        assert_eq!(version, "4.0.5");
    }

    #[tokio::test]
    async fn a_rejected_password_is_reported_as_such() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let err = client(&server).version().await.unwrap_err();
        assert!(err.is_auth_failure(), "{err}");
        assert!(!err.is_unreachable(), "{err}");
    }

    #[tokio::test]
    async fn nothing_listening_is_reported_as_unreachable() {
        let port = sharerr_testkit::net::closed_port();
        let base = Url::parse(&format!("http://127.0.0.1:{port}")).unwrap();
        let client = TransmissionClient::new(&base, "admin", SecretString::from("pw")).unwrap();

        let err = client.version().await.unwrap_err();
        assert!(err.is_unreachable(), "{err}");
    }

    /// Transmission reports application-level failure in the body with HTTP 200, so
    /// the status code alone never proves a call worked.
    #[tokio::test]
    async fn a_body_level_failure_is_an_error_despite_the_200() {
        let server = MockServer::start().await;
        mount_handshake_then(&server, json!({ "result": "invalid argument" })).await;

        let err = client(&server).version().await.unwrap_err();
        assert!(err.to_string().contains("invalid argument"), "{err}");
    }

    /// The constraint the whole project is built around: withdrawing a share must
    /// never delete the operator's media.
    #[tokio::test]
    async fn removal_never_deletes_the_data() {
        let server = MockServer::start().await;
        mount_handshake_then(&server, ok_body(json!({}))).await;

        client(&server).remove("abc").await.unwrap();

        let requests = server.received_requests().await.unwrap();
        let last = requests.last().expect("a request was made");
        let body: Value = serde_json::from_slice(&last.body).unwrap();
        assert_eq!(body["method"], "torrent-remove");
        assert_eq!(
            body["arguments"]["delete-local-data"], false,
            "sharerr must never ask Transmission to delete media"
        );
    }

    /// The add must point Transmission at the data that is already there, rather
    /// than asking it to fetch anything.
    #[tokio::test]
    async fn adding_points_at_the_existing_data_and_does_not_move_it() {
        let server = MockServer::start().await;
        mount_handshake_then(&server, ok_body(json!({}))).await;

        let data = b"d8:announce0:e";
        let request = AddRequest::new(data, "abc123", "x.torrent", "/downloads/tv")
            .category("sharerr")
            .tags("shared");
        client(&server).add(&request).await.unwrap();

        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests.last().unwrap().body).unwrap();
        assert_eq!(body["method"], "torrent-add");
        assert_eq!(body["arguments"]["download-dir"], "/downloads/tv");
        // The metainfo goes as base64, not as a multipart upload.
        let encoded = body["arguments"]["metainfo"].as_str().unwrap();
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .unwrap(),
            data
        );
        // Category and tags both land in labels, because that is all Transmission
        // has.
        let labels: Vec<&str> = body["arguments"]["labels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l.as_str().unwrap())
            .collect();
        assert_eq!(labels, vec!["sharerr", "shared"]);
        // Nothing in the request may ask Transmission to relocate anything.
        assert!(
            body["arguments"].get("move").is_none(),
            "an add must never move data: {body}"
        );
    }

    #[tokio::test]
    async fn listing_maps_status_and_labels() {
        let server = MockServer::start().await;
        mount_handshake_then(
            &server,
            ok_body(json!({ "torrents": [
                { "hashString": "ABCDEF", "name": "a", "downloadDir": "/downloads",
                  "labels": ["sharerr"], "status": 6 },
                { "hashString": "123456", "name": "b", "downloadDir": "/downloads",
                  "labels": ["other"], "status": 4 }
            ]})),
        )
        .await;

        let all = client(&server).list(None).await.unwrap();
        assert_eq!(all.len(), 2);
        // Lowercased, because sharerr joins on this against its own store.
        assert_eq!(all[0].hash, "abcdef");
        assert!(all[0].is_seeding, "status 6 is seeding");
        assert!(!all[1].is_seeding, "status 4 is still downloading");
    }

    /// `seedRatioMode` decides whether `seedRatioLimit` is a fixed number this
    /// torrent is held to (`1`) or Transmission's own global default / no
    /// limit at all (`0` / `2`) — only the first should surface as `Some`.
    /// `uploadRatio`'s own sentinels (`TR_RATIO_NA` = `-2`, `TR_RATIO_INF` =
    /// `-1`) are distinct: the first is genuinely unknown, the second a real
    /// infinite ratio.
    #[tokio::test]
    async fn listing_maps_ratio_and_resolves_transmissions_sentinels() {
        let server = MockServer::start().await;
        mount_handshake_then(
            &server,
            ok_body(json!({ "torrents": [
                { "hashString": "aa", "name": "a", "downloadDir": "/d", "labels": [], "status": 6,
                  "uploadRatio": 1.85, "seedRatioLimit": 2.0, "seedRatioMode": 1 },
                { "hashString": "bb", "name": "b", "downloadDir": "/d", "labels": [], "status": 6,
                  "uploadRatio": -1.0, "seedRatioLimit": 5.0, "seedRatioMode": 2 },
                { "hashString": "cc", "name": "c", "downloadDir": "/d", "labels": [], "status": 6,
                  "uploadRatio": -2.0, "seedRatioLimit": 5.0, "seedRatioMode": 0 }
            ]})),
        )
        .await;

        let all = client(&server).list(None).await.unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].ratio, Some(1.85));
        assert_eq!(all[0].ratio_limit, Some(2.0), "mode 1 is a fixed limit");
        assert_eq!(
            all[1].ratio,
            Some(f64::INFINITY),
            "TR_RATIO_INF is a real ratio"
        );
        assert_eq!(all[1].ratio_limit, None, "mode 2 is unlimited");
        assert_eq!(all[2].ratio, None, "TR_RATIO_NA is genuinely unknown");
        assert_eq!(
            all[2].ratio_limit, None,
            "mode 0 follows the global default"
        );
    }

    /// Transmission cannot filter by category server-side, so this crate does it —
    /// and the caller must still get the right answer.
    #[tokio::test]
    async fn a_category_filter_is_applied_even_though_transmission_has_no_categories() {
        let server = MockServer::start().await;
        mount_handshake_then(
            &server,
            ok_body(json!({ "torrents": [
                { "hashString": "aa", "name": "a", "downloadDir": "/d",
                  "labels": ["sharerr"], "status": 6 },
                { "hashString": "bb", "name": "b", "downloadDir": "/d",
                  "labels": ["something-else"], "status": 6 }
            ]})),
        )
        .await;

        let filtered = client(&server).list(Some("sharerr")).await.unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].hash, "aa");
    }

    /// Replacing trackers goes through `torrent-set`'s `trackerList`: one URL per
    /// line, a blank line between tiers — so one-URL-per-tier means a blank line
    /// between every pair.
    #[tokio::test]
    async fn set_trackers_sends_a_tiered_tracker_list() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .respond_with(
                ResponseTemplate::new(409).insert_header(SESSION_HEADER, "session-token-1"),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .and(header_exists(SESSION_HEADER))
            .and(wiremock::matchers::body_string_contains("torrent-set"))
            .and(wiremock::matchers::body_string_contains(
                "http://new.example:41234/announce\\n\\nhttp://old.example:8477/announce",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body(json!({}))))
            .expect(1)
            .mount(&server)
            .await;

        client(&server)
            .set_trackers(
                "aabbcc",
                &[
                    Url::parse("http://new.example:41234/announce").unwrap(),
                    Url::parse("http://old.example:8477/announce").unwrap(),
                ],
            )
            .await
            .unwrap();
    }

    /// The additive form reads the current list back and puts sharerr's tiers
    /// in front of it, verbatim. Anything that dropped or reordered what was
    /// already there would be rearranging the operator's own torrent.
    #[tokio::test]
    async fn add_trackers_prepends_without_dropping_the_existing_list() {
        let server = MockServer::start().await;
        mount_handshake_then(
            &server,
            ok_body(json!({ "torrents": [
                { "trackerList": "http://theirs.example/announce\n\nhttp://backup.example/announce" }
            ]})),
        )
        .await;

        client(&server)
            .add_trackers(
                "aabbcc",
                &[Url::parse("http://sharerr.example/announce").unwrap()],
            )
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        let set = requests
            .iter()
            .filter_map(|r| serde_json::from_slice::<Value>(&r.body).ok())
            .find(|b| b["method"] == "torrent-set")
            .expect("a torrent-set was sent");
        assert_eq!(
            set["arguments"]["trackerList"].as_str().unwrap(),
            "http://sharerr.example/announce\n\nhttp://theirs.example/announce\n\nhttp://backup.example/announce"
        );
    }

    /// Already present means nothing to do — and nothing sent, so a repeat
    /// pass over an adopted item cannot slowly rewrite its tracker list.
    #[tokio::test]
    async fn add_trackers_sends_no_torrent_set_when_the_url_is_already_listed() {
        let server = MockServer::start().await;
        mount_handshake_then(
            &server,
            ok_body(json!({ "torrents": [
                { "trackerList": "http://sharerr.example/announce" }
            ]})),
        )
        .await;

        client(&server)
            .add_trackers(
                "aabbcc",
                &[Url::parse("http://sharerr.example/announce").unwrap()],
            )
            .await
            .unwrap();

        let sent_set = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter_map(|r| serde_json::from_slice::<Value>(&r.body).ok())
            .any(|b| b["method"] == "torrent-set");
        assert!(!sent_set, "nothing to add means nothing sent");
    }

    /// Transmission has no call that returns a `.torrent`, and says so as
    /// `Ok(None)` rather than an error — the caller turns that into a message
    /// naming the choice the operator has, which an RPC failure could not.
    #[tokio::test]
    async fn export_reports_that_transmission_cannot_produce_the_file() {
        let server = MockServer::start().await;
        assert_eq!(client(&server).export("aabbcc").await.unwrap(), None);
        assert!(
            server.received_requests().await.unwrap().is_empty(),
            "and answers without a round trip"
        );
    }

    /// `torrent-add` itself carries no ratio/speed arguments, so a
    /// configured seeding goal must land on a follow-up `torrent-set`
    /// naming the hash the add just reported.
    #[tokio::test]
    async fn a_seeding_goal_configured_at_add_time_lands_on_a_follow_up_torrent_set() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .respond_with(
                ResponseTemplate::new(409).insert_header(SESSION_HEADER, "session-token-1"),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .and(header_exists(SESSION_HEADER))
            .and(wiremock::matchers::body_string_contains("torrent-add"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body(
                json!({ "torrent-added": { "id": 7, "hashString": "abc123" } }),
            )))
            .expect(1)
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .and(header_exists(SESSION_HEADER))
            .and(wiremock::matchers::body_string_contains("torrent-set"))
            .and(wiremock::matchers::body_string_contains("uploadLimit"))
            .and(wiremock::matchers::body_string_contains("seedRatioLimit"))
            .and(wiremock::matchers::body_string_contains("abc123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body(json!({}))))
            .expect(1)
            .mount(&server)
            .await;

        let data = b"d8:announce0:e";
        let request = AddRequest::new(data, "abc123", "x.torrent", "/downloads")
            .upload_limit_kib(512)
            .ratio_limit(2.5);
        client(&server).add(&request).await.unwrap();
    }

    /// The common case — no seeding goal configured — must cost exactly the
    /// one `torrent-add` call it always has, not a silent extra round trip.
    #[tokio::test]
    async fn no_seeding_goal_means_no_follow_up_call() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .respond_with(
                ResponseTemplate::new(409).insert_header(SESSION_HEADER, "session-token-1"),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;

        // Exactly one call expected after the handshake — an unwanted
        // torrent-set would push this past 1 and fail verification on drop.
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .and(header_exists(SESSION_HEADER))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body(
                json!({ "torrent-added": { "hashString": "abc123" } }),
            )))
            .expect(1)
            .mount(&server)
            .await;

        let data = b"d8:announce0:e";
        client(&server)
            .add(&AddRequest::new(data, "abc123", "x.torrent", "/downloads"))
            .await
            .unwrap();
    }

    /// A reverse-proxy subpath must survive: `join` would otherwise replace the
    /// last segment rather than appending to it.
    #[test]
    fn a_subpath_base_url_keeps_its_prefix() {
        let base = Url::parse("http://box.lan/transmission-proxy").unwrap();
        let client = TransmissionClient::new(&base, "a", SecretString::from("b")).unwrap();
        assert_eq!(
            client.endpoint.as_str(),
            "http://box.lan/transmission-proxy/transmission/rpc"
        );
    }

    /// The password must not reach a log through `Debug`.
    #[test]
    fn debug_does_not_leak_the_password() {
        let base = Url::parse("http://box.lan").unwrap();
        let client =
            TransmissionClient::new(&base, "admin", SecretString::from("hunter2")).unwrap();
        let rendered = format!("{client:?}");
        assert!(!rendered.contains("hunter2"), "{rendered}");
        assert!(rendered.contains("redacted"), "{rendered}");
    }
}
