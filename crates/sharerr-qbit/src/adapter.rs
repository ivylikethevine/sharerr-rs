//! [`TorrentClient`] for qBittorrent.
//!
//! A thin translation rather than a rewrite: this crate's own types predate the
//! trait and stay as they are, because they mirror qBittorrent's wire format and
//! that is what makes them easy to check against its documentation. The mapping
//! lives here so neither side has to compromise.

use async_trait::async_trait;
use sharerr_client::{
    AddRequest, ClientError, ClientKind, Result, TorrentClient, TorrentFileEntry, TorrentSummary,
};

use crate::QbitClient;
use crate::error::QbitError;

/// Translate a qBittorrent error into the shared shape.
///
/// The two predicates this crate already exposes are exactly the distinction the
/// shared error preserves, so nothing is lost — an unreachable service and a
/// rejected password stay apart, and everything else becomes an API error carrying
/// the original text.
impl QbitClient {
    fn translate(&self, err: QbitError) -> ClientError {
        let kind = ClientKind::QBittorrent;
        if err.is_auth_failure() {
            return ClientError::AuthRejected { kind };
        }
        if err.is_unreachable() {
            return ClientError::Unreachable {
                kind,
                url: self.base_url().to_string(),
                detail: err.to_string(),
            };
        }
        ClientError::Api {
            kind,
            detail: err.to_string(),
        }
    }
}

#[async_trait]
impl TorrentClient for QbitClient {
    fn kind(&self) -> ClientKind {
        ClientKind::QBittorrent
    }

    async fn login(&self) -> Result<()> {
        QbitClient::login(self).await.map_err(|e| self.translate(e))
    }

    async fn version(&self) -> Result<String> {
        QbitClient::version(self)
            .await
            .map_err(|e| self.translate(e))
    }

    async fn list(&self, category: Option<&str>) -> Result<Vec<TorrentSummary>> {
        let torrents = self
            .torrents_info(category, None)
            .await
            .map_err(|e| self.translate(e))?;

        Ok(torrents
            .into_iter()
            .map(|t| TorrentSummary {
                is_seeding: t.is_seeding(),
                tags: t.tag_list().into_iter().map(str::to_owned).collect(),
                hash: t.hash,
                name: t.name,
                save_path: t.save_path,
                content_path: t.content_path,
                category: t.category,
            })
            .collect())
    }

    async fn files(&self, hash: &str) -> Result<Vec<TorrentFileEntry>> {
        let files = self
            .torrent_files(hash)
            .await
            .map_err(|e| self.translate(e))?;

        Ok(files
            .into_iter()
            .map(|f| TorrentFileEntry {
                name: f.name,
                size: f.size,
            })
            .collect())
    }

    async fn add(&self, request: &AddRequest<'_>) -> Result<()> {
        self.add_torrent(request)
            .await
            .map_err(|e| self.translate(e))
    }

    async fn remove(&self, hash: &str) -> Result<()> {
        // `remove_torrent` already passes `deleteFiles=false`; the media is the
        // operator's and predates sharerr knowing about it.
        self.remove_torrent(hash)
            .await
            .map_err(|e| self.translate(e))
    }

    async fn set_trackers(&self, hash: &str, urls: &[url::Url]) -> Result<()> {
        let urls: Vec<String> = urls.iter().map(url::Url::to_string).collect();
        self.set_torrent_trackers(hash, &urls)
            .await
            .map_err(|e| self.translate(e))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use secrecy::SecretString;
    use url::Url;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    const API_KEY: &str = "qbt_jCGn3V76XutJwQpsXgIm6A9NLB86";

    fn make_client(base: &str) -> QbitClient {
        let base = Url::parse(base).unwrap();
        QbitClient::with_api_key(&base, SecretString::from(API_KEY)).unwrap()
    }

    async fn mocked_client(server: &MockServer) -> QbitClient {
        make_client(&server.uri())
    }

    // `kind` has no inherent-method collision, so the trait method resolves
    // through ordinary dot-call syntax.
    #[test]
    fn kind_identifies_as_qbittorrent() {
        let client = make_client("http://127.0.0.1:8080");
        assert_eq!(client.kind(), ClientKind::QBittorrent);
    }

    // `login` and `version` share a name with an inherent method on
    // `QbitClient` (see client.rs), which Rust's method resolution prefers
    // over a trait method of the same name. Fully-qualified syntax is the
    // only way to actually exercise the `TorrentClient` impl below rather
    // than silently re-testing the inherent one client.rs already covers.
    #[tokio::test]
    async fn login_always_succeeds_there_is_no_session_to_establish() {
        let client = make_client("http://127.0.0.1:8080");
        TorrentClient::login(&client).await.unwrap();
    }

    #[tokio::test]
    async fn version_is_trimmed_and_passed_through() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/app/version"))
            .respond_with(ResponseTemplate::new(200).set_body_string("v4.6.0\n"))
            .mount(&server)
            .await;

        let client = mocked_client(&server).await;
        let version = TorrentClient::version(&client).await.unwrap();
        assert_eq!(version, "v4.6.0");
    }

    #[tokio::test]
    async fn a_rejected_key_translates_to_auth_rejected() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let client = mocked_client(&server).await;
        let err = TorrentClient::version(&client).await.unwrap_err();
        assert!(
            matches!(
                err,
                ClientError::AuthRejected {
                    kind: ClientKind::QBittorrent
                }
            ),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn nothing_listening_translates_to_unreachable() {
        let port = sharerr_testkit::net::closed_port();
        let client = make_client(&format!("http://127.0.0.1:{port}"));

        let err = TorrentClient::version(&client).await.unwrap_err();
        assert!(
            matches!(
                err,
                ClientError::Unreachable {
                    kind: ClientKind::QBittorrent,
                    ..
                }
            ),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn an_ordinary_failure_status_translates_to_an_api_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        let client = mocked_client(&server).await;
        let err = TorrentClient::version(&client).await.unwrap_err();
        assert!(
            matches!(
                err,
                ClientError::Api {
                    kind: ClientKind::QBittorrent,
                    ..
                }
            ),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn list_maps_hash_tags_paths_and_seeding_state() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/torrents/info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "hash": "abc123",
                    "name": "one",
                    "save_path": "/downloads",
                    "content_path": "/downloads/one",
                    "state": "uploading",
                    "category": "sharerr",
                    "tags": "a,b",
                }
            ])))
            .mount(&server)
            .await;

        let client = mocked_client(&server).await;
        let list = client.list(None).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].hash, "abc123");
        assert_eq!(list[0].save_path, "/downloads");
        assert_eq!(list[0].content_path, "/downloads/one");
        assert_eq!(list[0].category, "sharerr");
        assert_eq!(list[0].tags, vec!["a".to_owned(), "b".to_owned()]);
        assert!(list[0].is_seeding, "state=uploading is seeding");
    }

    #[tokio::test]
    async fn files_maps_name_and_size() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/torrents/files"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "name": "movie.mkv", "size": 1234 }
            ])))
            .mount(&server)
            .await;

        let client = mocked_client(&server).await;
        let files = client.files("abc123").await.unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "movie.mkv");
        assert_eq!(files[0].size, 1234);
    }

    #[tokio::test]
    async fn add_forwards_the_request_to_the_underlying_client() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/torrents/add"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let data = b"d8:announce0:e";
        let request = AddRequest::new(data, "x.torrent", "/downloads");
        let client = mocked_client(&server).await;
        client.add(&request).await.unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
    }

    #[tokio::test]
    async fn add_translates_a_rejected_torrent_into_an_api_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/torrents/add"))
            .respond_with(ResponseTemplate::new(200).set_body_string("Fails."))
            .mount(&server)
            .await;

        let data = b"not really a torrent";
        let request = AddRequest::new(data, "x.torrent", "/downloads");
        let client = mocked_client(&server).await;
        let err = client.add(&request).await.unwrap_err();
        assert!(
            matches!(
                err,
                ClientError::Api {
                    kind: ClientKind::QBittorrent,
                    ..
                }
            ),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn remove_never_asks_to_delete_files() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/torrents/delete"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = mocked_client(&server).await;
        client.remove("abc123").await.unwrap();

        let requests = server.received_requests().await.unwrap();
        let body = String::from_utf8(requests.last().unwrap().body.clone()).unwrap();
        assert!(body.contains("deleteFiles=false"), "{body}");
        assert!(body.contains("hashes=abc123"), "{body}");
    }

    #[tokio::test]
    async fn set_trackers_stringifies_urls_before_forwarding_them() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/torrents/trackers"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v2/torrents/addTrackers"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let urls = [Url::parse("http://tracker.example/announce").unwrap()];
        let client = mocked_client(&server).await;
        client.set_trackers("abc123", &urls).await.unwrap();

        let requests = server.received_requests().await.unwrap();
        let add = requests
            .iter()
            .find(|r| r.url.path() == "/api/v2/torrents/addTrackers")
            .expect("an addTrackers call was made");
        let body = String::from_utf8(add.body.clone()).unwrap();
        assert!(body.contains("tracker.example"), "{body}");
    }
}
