//! The service checks `doctor` and the web UI both run, decided in one place.
//!
//! # Why this exists
//!
//! `sharerr doctor` and the settings page's "Test connection" button ask the same
//! questions of the same services, and they used to answer them separately. The two
//! implementations had already drifted into describing *different conditions* under
//! similar words: the CLI warned "tag exists but nothing carries it", while the UI
//! failed with "no tag named X exists there yet". Those are not two phrasings of one
//! finding — they are two distinct states, and each tool could only report the one
//! it happened to look for.
//!
//! So the decision lives here and the wording lives with the caller. This module
//! answers *what is true*; [`crate::commands::doctor`] and [`crate::web::probe`]
//! each render that their own way, because a terminal report and an inline badge
//! genuinely want different sentences. What they can no longer do is disagree about
//! the facts.
//!
//! # What is deliberately not here
//!
//! Path-mapping resolution. It needs a library to walk and a `PathResolver`, and
//! only `doctor` currently does it — folding it in would mean this module growing a
//! second shape for one caller.

use std::sync::Arc;

use secrecy::SecretString;
use sharerr_arr::{ArrClient, Discovered};
use sharerr_client::{ClientKind, TorrentClient};
use sharerr_core::config::TorrentBackend;
use sharerr_core::paths::ResolvedPaths;
use sharerr_core::{Config, MediaSource};
use sharerr_qbit::QbitClient;
use sharerr_transmission::TransmissionClient;
use url::Url;

/// Render an error together with its cause chain.
///
/// The distinction matters: reqwest's own `Display` is just "error sending request
/// for url (...)", and the part an operator actually needs — `Connection refused`,
/// `dns error`, `operation timed out` — lives further down the chain.
pub fn chain(err: &dyn std::error::Error) -> String {
    let mut rendered = err.to_string();
    let mut cause = err.source();

    while let Some(next) = cause {
        let text = next.to_string();
        // `#[source]` fields are often interpolated into the parent's message
        // already; only append what is genuinely new.
        if !rendered.contains(&text) {
            rendered.push_str(": ");
            rendered.push_str(&text);
        }
        cause = next.source();
    }

    rendered
}

/// What a Sonarr or Radarr instance turned out to be.
///
/// Ordered roughly as the checks run, and deliberately distinguishes
/// [`Self::TagMissing`] from [`Self::TagUnused`] — conflating them is the exact
/// drift this module was created to end. They have different fixes: create the tag,
/// versus apply the tag you already created.
#[derive(Debug)]
pub enum ArrOutcome {
    /// No URL in the config, so there is nothing to contact.
    NotConfigured,
    /// The API key is not in the vault yet.
    NoCredential,
    /// The vault itself would not open, or the key could not be read.
    CredentialUnreadable(String),
    /// The URL is not usable as an *arr base address.
    BadUrl(String),
    /// Nothing answered.
    Unreachable(String),
    /// Something answered and rejected the key.
    AuthRejected,
    /// Answered, but the request failed for some other reason.
    Failed(String),
    /// Reachable and authenticated, but no tag with that label exists.
    TagMissing { version: String },
    /// The tag exists, and nothing carries it.
    TagUnused { version: String },
    /// Everything is in order.
    Ready {
        version: String,
        app_name: String,
        items: Vec<Discovered>,
    },
}

impl ArrOutcome {
    /// The discovered files, if the check got that far.
    ///
    /// Every other state yields nothing, which is the honest answer: a service that
    /// could not be reached contributed no files, and callers summing across
    /// services want that to be an empty list rather than an error to handle.
    pub fn into_items(self) -> Vec<Discovered> {
        match self {
            Self::Ready { items, .. } => items,
            _ => Vec::new(),
        }
    }
}

/// Contact one *arr instance and establish everything sharerr needs from it.
///
/// `api_key` is `None` when the vault has no entry, and `Err` when the vault could
/// not be read at all — a distinction worth keeping, because the first is a field to
/// fill in and the second is usually a missing environment variable.
pub async fn check_arr(
    kind: MediaSource,
    url: Option<&Url>,
    api_key: Result<Option<SecretString>, String>,
    tag: &str,
) -> ArrOutcome {
    let Some(url) = url else {
        return ArrOutcome::NotConfigured;
    };

    let api_key = match api_key {
        Ok(Some(api_key)) => api_key,
        Ok(None) => return ArrOutcome::NoCredential,
        Err(reason) => return ArrOutcome::CredentialUnreadable(reason),
    };

    let client = match ArrClient::new(kind, url, api_key) {
        Ok(client) => client,
        Err(err) => return ArrOutcome::BadUrl(chain(&err)),
    };

    // Reachability and authentication first: every later check would report the
    // same underlying problem in a less useful way.
    let status = match client.system_status().await {
        Ok(status) => status,
        Err(err) if err.is_auth_failure() => return ArrOutcome::AuthRejected,
        Err(err) if err.is_unreachable() => return ArrOutcome::Unreachable(chain(&err)),
        Err(err) => return ArrOutcome::Failed(chain(&err)),
    };

    let version = status.version;
    let app_name = if status.app_name.is_empty() {
        kind.as_str().to_owned()
    } else {
        status.app_name
    };

    // Both tag questions are asked, every time. Previously each caller asked only
    // one of them and reported the other's condition in its wording.
    if client.tag_id(tag).await.is_err() {
        return ArrOutcome::TagMissing { version };
    }

    match client.discover(tag).await {
        Ok(items) if items.is_empty() => ArrOutcome::TagUnused { version },
        Ok(items) => ArrOutcome::Ready {
            version,
            app_name,
            items,
        },
        Err(err) => ArrOutcome::Failed(chain(&err)),
    }
}

/// How the tagged files resolve across the three views of the library.
///
/// This is the check most likely to explain "sharerr does nothing": the services
/// can all be reachable, the tag can be right, and every file can still be
/// invisible because the *arr view and sharerr's mount do not line up.
///
/// It lived only in `doctor` and so was only ever available from a shell. The
/// summary is computed here so the web UI can show it too, without either copy
/// deciding for itself what "unmapped" means.
#[derive(Debug, Default)]
pub struct PathReport {
    /// How many `[[path_map]]` rules are configured.
    pub rules: usize,
    /// How many tagged files were examined.
    pub checked: usize,
    /// Files that matched no rule and passed through unchanged. Only meaningful
    /// when at least one rule exists — with none, everything passes through by
    /// design.
    pub unmapped: usize,
    /// Files that do not exist at the path sharerr resolved them to. This is the
    /// finding that matters: sharerr cannot build a torrent for a file it cannot
    /// open.
    pub missing: Vec<std::path::PathBuf>,
    /// Paths that could not be resolved at all, with the reason.
    pub invalid: Vec<String>,
    /// One worked example, for showing what the mapping actually does.
    pub sample: Option<ResolvedPaths>,
}

impl PathReport {
    /// Whether anything here stops sharerr sharing a file.
    pub fn is_failure(&self) -> bool {
        !self.missing.is_empty() || !self.invalid.is_empty()
    }

    /// Files that resolved to something readable.
    pub fn readable(&self) -> usize {
        self.checked
            .saturating_sub(self.missing.len())
            .saturating_sub(self.invalid.len())
    }
}

/// Resolve every discovered file through the configured mapping and summarise it.
///
/// Touches the filesystem — `exists()` per file — which is the entire point: a
/// mapping that looks plausible and resolves to nothing is the failure being
/// hunted. Only sharerr's own view can be checked this way; the qBittorrent view
/// is another container's filesystem and has to be verified against qBittorrent.
pub fn check_paths(config: &Config, discovered: &[Discovered]) -> PathReport {
    let mut report = PathReport {
        rules: config.path_map.len(),
        checked: discovered.len(),
        ..PathReport::default()
    };

    let resolver = config.resolver();
    for item in discovered {
        match resolver.resolve(&item.arr_path) {
            Ok(paths) => {
                if !paths.mapping_applied {
                    report.unmapped += 1;
                }
                if !paths.sharerr.exists() {
                    report.missing.push(paths.sharerr.clone());
                }
                if report.sample.is_none() {
                    report.sample = Some(paths);
                }
            }
            Err(err) => report.invalid.push(chain(&err)),
        }
    }

    report
}

/// What the torrent client turned out to be.
#[derive(Debug)]
pub enum QbitOutcome {
    NoCredential,
    CredentialUnreadable(String),
    BadUrl(String),
    Unreachable(String),
    AuthRejected,
    Failed(String),
    /// Signed in. Carries the client so a caller that has more to ask — `doctor`
    /// goes on to list torrents and read preferences — does not have to build and
    /// authenticate a second one.
    Ready {
        version: String,
        /// Which client answered, so a message can name it.
        kind: ClientKind,
        client: Arc<dyn TorrentClient>,
    },
}

/// Sign in to the configured torrent client and read its version.
///
/// The login is explicit rather than left to the first real call, because "reached
/// it but the password is wrong" and "could not reach it" have different fixes and
/// an implicit login reports them identically.
///
/// Which client this talks to is a configuration choice, and the caller passes the
/// already-resolved backend rather than guessing from the URL — two clients can
/// perfectly well live on the same host.
pub async fn check_qbit(
    backend: TorrentBackend,
    url: &Url,
    username: &str,
    password: Result<Option<SecretString>, String>,
) -> QbitOutcome {
    let password = match password {
        Ok(Some(password)) => password,
        Ok(None) => return QbitOutcome::NoCredential,
        Err(reason) => return QbitOutcome::CredentialUnreadable(reason),
    };

    let client: Arc<dyn TorrentClient> = match backend {
        TorrentBackend::Qbittorrent => match QbitClient::new(url, username, password) {
            Ok(client) => Arc::new(client),
            Err(err) => return QbitOutcome::BadUrl(chain(&err)),
        },
        TorrentBackend::Transmission => match TransmissionClient::new(url, username, password) {
            Ok(client) => Arc::new(client),
            Err(err) => return QbitOutcome::BadUrl(chain(&err)),
        },
    };

    if let Err(err) = client.login().await {
        return if err.is_auth_failure() {
            QbitOutcome::AuthRejected
        } else if err.is_unreachable() {
            QbitOutcome::Unreachable(chain(&err))
        } else {
            QbitOutcome::Failed(chain(&err))
        };
    }

    match client.version().await {
        Ok(version) => QbitOutcome::Ready {
            version,
            kind: client.kind(),
            client,
        },
        Err(err) => QbitOutcome::Failed(chain(&err)),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const API_KEY: &str = "0123456789abcdef0123456789abcdef";

    fn key() -> Result<Option<SecretString>, String> {
        Ok(Some(SecretString::from(API_KEY)))
    }

    async fn mount(server: &MockServer, route: &str, status: u16, body: serde_json::Value) {
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .mount(server)
            .await;
    }

    async fn mount_status(server: &MockServer) {
        mount(
            server,
            "/api/v3/system/status",
            200,
            json!({ "version": "4.0.15", "appName": "Sonarr" }),
        )
        .await;
    }

    fn base(server: &MockServer) -> Url {
        Url::parse(&server.uri()).unwrap()
    }

    /// The distinction this module was created to preserve: a tag that does not
    /// exist and a tag nobody has applied are different findings with different
    /// fixes. `doctor` used to see only the second and the web UI only the first, so
    /// each described the other's condition in its own wording.
    #[tokio::test]
    async fn a_tag_that_does_not_exist_is_reported_as_missing() {
        let server = MockServer::start().await;
        mount_status(&server).await;
        // Only a decoy tag, so `sharerr` genuinely is not there.
        mount(
            &server,
            "/api/v3/tag",
            200,
            json!([{ "id": 1, "label": "anime" }]),
        )
        .await;

        let outcome = check_arr(MediaSource::Sonarr, Some(&base(&server)), key(), "sharerr").await;

        assert!(
            matches!(outcome, ArrOutcome::TagMissing { .. }),
            "got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn a_tag_that_exists_but_carries_nothing_is_reported_as_unused() {
        let server = MockServer::start().await;
        mount_status(&server).await;
        mount(
            &server,
            "/api/v3/tag",
            200,
            json!([{ "id": 3, "label": "sharerr" }]),
        )
        .await;
        // The tag resolves, but no series carries it.
        mount(&server, "/api/v3/series", 200, json!([])).await;

        let outcome = check_arr(MediaSource::Sonarr, Some(&base(&server)), key(), "sharerr").await;

        assert!(
            matches!(outcome, ArrOutcome::TagUnused { .. }),
            "got {outcome:?}"
        );
    }

    /// A rejected key must not be reported as anything else — "the tag is missing"
    /// would send the operator to the wrong screen entirely.
    #[tokio::test]
    async fn a_rejected_api_key_is_reported_as_an_auth_failure() {
        let server = MockServer::start().await;
        mount(&server, "/api/v3/system/status", 401, json!({})).await;

        let outcome = check_arr(MediaSource::Sonarr, Some(&base(&server)), key(), "sharerr").await;

        assert!(
            matches!(outcome, ArrOutcome::AuthRejected),
            "got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn nothing_listening_is_reported_as_unreachable() {
        // Bind a port, learn its number, then drop the listener — that leaves an
        // address where the connection is refused outright, which is what a service
        // being down actually looks like. A dropped `MockServer` is not equivalent:
        // its port gets reused and answers 404, which is a *reachable* service.
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let url = Url::parse(&format!("http://127.0.0.1:{port}")).unwrap();

        let outcome = check_arr(MediaSource::Sonarr, Some(&url), key(), "sharerr").await;

        assert!(
            matches!(outcome, ArrOutcome::Unreachable(_)),
            "got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn an_absent_url_is_not_configured_rather_than_unreachable() {
        let outcome = check_arr(MediaSource::Sonarr, None, key(), "sharerr").await;
        assert!(
            matches!(outcome, ArrOutcome::NotConfigured),
            "got {outcome:?}"
        );
    }

    /// "No key stored yet" and "the vault would not open" have different fixes —
    /// a field to fill in versus a missing environment variable.
    #[tokio::test]
    async fn a_missing_and_an_unreadable_credential_are_distinguished() {
        let server = MockServer::start().await;
        let url = base(&server);

        let missing = check_arr(MediaSource::Sonarr, Some(&url), Ok(None), "sharerr").await;
        assert!(
            matches!(missing, ArrOutcome::NoCredential),
            "got {missing:?}"
        );

        let unreadable = check_arr(
            MediaSource::Sonarr,
            Some(&url),
            Err("no master key".to_owned()),
            "sharerr",
        )
        .await;
        assert!(
            matches!(unreadable, ArrOutcome::CredentialUnreadable(_)),
            "got {unreadable:?}"
        );
    }

    /// The cause chain is the whole reason this helper exists: reqwest's own
    /// `Display` stops before the part that names what went wrong.
    #[test]
    fn the_cause_chain_is_appended_without_repeating_itself() {
        #[derive(Debug)]
        struct Inner;
        impl std::fmt::Display for Inner {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "connection refused")
            }
        }
        impl std::error::Error for Inner {}

        #[derive(Debug)]
        struct Outer;
        impl std::fmt::Display for Outer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "error sending request")
            }
        }
        impl std::error::Error for Outer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&Inner)
            }
        }

        assert_eq!(chain(&Outer), "error sending request: connection refused");
    }

    /// A parent that already interpolates its source must not say it twice.
    #[test]
    fn an_already_interpolated_cause_is_not_repeated() {
        #[derive(Debug)]
        struct Inner;
        impl std::fmt::Display for Inner {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "refused")
            }
        }
        impl std::error::Error for Inner {}

        #[derive(Debug)]
        struct Outer;
        impl std::fmt::Display for Outer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "sending request: refused")
            }
        }
        impl std::error::Error for Outer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&Inner)
            }
        }

        assert_eq!(chain(&Outer), "sending request: refused");
    }
}
