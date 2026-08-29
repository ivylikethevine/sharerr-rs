//! The service checks `doctor` and the web UI both run, decided in one place.
//!
//! # Why this exists
//!
//! `sharerr doctor` and the settings page's "Test connection" button ask the same
//! questions of the same services. "Tag exists but nothing carries it" and "no tag
//! named X exists there yet" are not two phrasings of one finding — they are two
//! distinct states with different fixes, and a single implementation must report
//! the one that actually applies.
//!
//! So the decision lives here and the wording lives with the caller. This module
//! answers *what is true*; [`crate::commands::doctor`] and [`crate::web::probe`]
//! each render that their own way, because a terminal report and an inline badge
//! genuinely want different sentences. What they can no longer do is disagree about
//! the facts.
//!
//! # What is deliberately not here
//!
//! The checks only `doctor` runs — the vault's key inventory, the database, the
//! tracker endpoint, the gluetun control server — stay in
//! [`crate::commands::doctor`]; the web UI has no equivalent to keep in
//! agreement with.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use secrecy::SecretString;
use sharerr_arr::{ArrClient, Discovered};
use sharerr_client::{ClientKind, TorrentClient};
use sharerr_core::config::{TorrentBackend, TorrentClientConfig};
use sharerr_core::paths::{PathResolver, ResolvedPaths};
use sharerr_core::{Config, MediaSource};
use sharerr_qbit::QbitClient;
use sharerr_rtorrent::RtorrentClient;
use sharerr_transmission::TransmissionClient;
use url::Url;

pub use sharerr_client::error_chain as chain;

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
    pub fn items(&self) -> &[Discovered] {
        match self {
            Self::Ready { items, .. } => items,
            _ => &[],
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

    // Both tag questions are asked, every time — the id from the first answer
    // feeds the walk, so the `/tag` list is fetched once.
    let Ok(tag_id) = client.tag_id(tag).await else {
        return ArrOutcome::TagMissing { version };
    };

    match client.discover_with_tag_id(tag_id).await {
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
    check_paths_of(
        config.path_map.len(),
        &config.resolver(),
        discovered
            .iter()
            .map(|item| (item.source, item.arr_path.as_path())),
    )
}

/// [`check_paths`] over just the two things it needs per file — where the
/// path came from and the path itself — so a caller that already holds the
/// discovered items borrowed elsewhere (see [`snapshot`]) does not have to
/// clone every one of them, and the whole `Config`, to run the walk on a
/// blocking thread.
pub fn check_paths_of<'a>(
    rules: usize,
    resolver: &PathResolver,
    items: impl IntoIterator<Item = (MediaSource, &'a Path)>,
) -> PathReport {
    let mut report = PathReport {
        rules,
        ..PathReport::default()
    };

    for (source, arr_path) in items {
        report.checked += 1;
        match resolver.resolve_for(source, arr_path) {
            Ok(paths) => {
                // "Matched no rule" is the normal case for a directory item,
                // not the warning sign it is for a path another container
                // reported.
                if !paths.mapping_applied && source != MediaSource::Directory {
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

/// What `[[library]]` scanning produced — distinguishing a completed scan
/// (however many libraries turned out empty, missing, or unreadable) from
/// the blocking scan task itself panicking partway through. See
/// [`snapshot`]'s docs for why that distinction has to survive out of this
/// function rather than being flattened away.
#[derive(Debug)]
pub enum LibraryScan {
    Scanned(Vec<(sharerr_core::config::LibraryConfig, DirOutcome)>),
    /// The blocking scan task panicked. Carries the panic's message.
    Panicked(String),
}

/// Everything [`snapshot`] gathers, unrendered.
#[derive(Debug)]
pub struct Snapshot {
    pub sources: Vec<(MediaSource, ArrOutcome)>,
    pub libraries: LibraryScan,
    pub paths: PathReport,
}

/// Probe every configured *arr source and scan every `[[library]]`
/// directory, then resolve everything either found through the path
/// mapping — the full gather both `web::diagnostics::gather` and
/// `web::topology::gather` need before they can render their own view of
/// "is this instance healthy".
///
/// Before this existed, each page ran its own copy of this sequence, and the
/// copies had already drifted: a panicked library scan produced a synthetic
/// "did not complete" line on the diagnostics page and silently vanished
/// from the topology page. One function used by both closes that gap the
/// same way [`resolve_torrent_credential`] closed the credential-resolution
/// one.
///
/// The arr probes (network) and the library scan (filesystem) touch
/// disjoint state, so they run concurrently rather than one after the
/// other — wall time is the slower of the two, not their sum.
/// Path-checking still runs after both: it needs everything either phase
/// discovered.
pub async fn snapshot(
    config: &Config,
    secret: &impl Fn(&'static str) -> Result<Option<SecretString>, String>,
) -> Snapshot {
    let sources_fut = async {
        futures::future::join_all(
            config
                .configured_sources()
                .into_iter()
                // `configured_sources` yields only *arr apps, each of which has a key.
                .filter_map(|kind| {
                    sharerr_core::config::secret_keys::api_key_for(kind).map(|key| (kind, key))
                })
                .map(|(kind, key)| {
                    let api_key = secret(key);
                    async move {
                        let url = config.service(kind).map(|s| &s.url);
                        (kind, check_arr(kind, url, api_key, &config.tag).await)
                    }
                }),
        )
        .await
    };

    // Filesystem-bound, off the async loop: a container pinned to one CPU has
    // exactly one runtime worker, and a slow mount must not stall /health and
    // every other request for the duration of the walk.
    let libraries_fut = async {
        let libraries = config.library.clone();
        tokio::task::spawn_blocking(move || {
            libraries
                .into_iter()
                .map(|library| {
                    let outcome = check_library(&library);
                    (library, outcome)
                })
                .collect::<Vec<_>>()
        })
        .await
    };

    let (sources, libraries) = tokio::join!(sources_fut, libraries_fut);

    let libraries = match libraries {
        Ok(scanned) => LibraryScan::Scanned(scanned),
        // A panicked scan must not make a configured [[library]] install look
        // identical to one with no libraries at all — see `LibraryScan`.
        Err(err) => LibraryScan::Panicked(err.to_string()),
    };

    // Only the source and path of each item cross to the blocking thread —
    // the items themselves stay borrowed by the outcomes returned below.
    let scanned_items = match &libraries {
        LibraryScan::Scanned(scanned) => scanned.as_slice(),
        LibraryScan::Panicked(_) => &[],
    };
    let discovered: Vec<(MediaSource, PathBuf)> = sources
        .iter()
        .flat_map(|(_, outcome)| outcome.items())
        .chain(
            scanned_items
                .iter()
                .flat_map(|(_, outcome)| outcome.items()),
        )
        .map(|item| (item.source, item.arr_path.clone()))
        .collect();

    // Filesystem-bound too, and depends on everything either phase above
    // discovered, so it cannot start until both are done.
    let paths = {
        let rules = config.path_map.len();
        let resolver = config.resolver();
        tokio::task::spawn_blocking(move || {
            check_paths_of(
                rules,
                &resolver,
                discovered
                    .iter()
                    .map(|(source, path)| (*source, path.as_path())),
            )
        })
        .await
        // A panicked walk renders as an empty report rather than a 500;
        // the source/library lines still carry the useful half of the page.
        .unwrap_or_default()
    };

    Snapshot {
        sources,
        libraries,
        paths,
    }
}

/// Whether one advertised address actually accepts a connection.
///
/// Deliberately a *TCP* connect and nothing more: an HTTP request would need
/// a credential for the feed and would confuse "the port is closed" with "the
/// port is open and answered 401", which have completely different fixes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReachOutcome {
    /// Nothing to dial — no advertised address is configured yet.
    NotConfigured,
    /// The address is set but could not be parsed into a host and port.
    Unusable(String),
    Reachable,
    Refused(String),
    TimedOut,
}

impl ReachOutcome {
    /// Whether this is a clean pass. A refusal is deliberately *not* a
    /// failure worth alarming over on its own — see [`check_reachable`].
    pub fn is_reachable(&self) -> bool {
        matches!(self, Self::Reachable)
    }
}

/// How long to wait for the connect before calling it a timeout.
const REACH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Dial `base`'s host and port and report what happened.
///
/// The caveat that shapes every message built from this: an instance dialling
/// its *own* public address from inside its own network is exercising NAT
/// hairpinning, which plenty of routers simply do not do — so a failure here
/// means "could not confirm", never "your port is closed". That is also why
/// the check is opt-in (`[checks] reachability`) rather than always-on.
pub async fn check_reachable(base: Option<&Url>) -> ReachOutcome {
    let Some(base) = base else {
        return ReachOutcome::NotConfigured;
    };
    let Some(host) = base.host_str() else {
        return ReachOutcome::Unusable(format!("{base} has no host"));
    };
    let Some(port) = base.port_or_known_default() else {
        return ReachOutcome::Unusable(format!("{base} has no port"));
    };
    // An IPv6 literal arrives bracketed from the URL and unbracketed from the
    // resolver, so the brackets come off before either is dialled.
    let target = format!("{}:{port}", host.trim_matches(['[', ']']));

    match tokio::time::timeout(REACH_TIMEOUT, tokio::net::TcpStream::connect(&target)).await {
        Ok(Ok(_)) => ReachOutcome::Reachable,
        Ok(Err(err)) => ReachOutcome::Refused(err.to_string()),
        Err(_) => ReachOutcome::TimedOut,
    }
}

/// What a `[[library]]` directory turned out to be.
///
/// The same decide-once contract as [`ArrOutcome`]: `doctor`, the settings
/// probe, and the diagnostics page each word these their own way, but cannot
/// disagree about which condition they found.
#[derive(Debug)]
pub enum DirOutcome {
    /// The path does not exist as sharerr sees it.
    Missing,
    /// The path exists but is not a directory.
    NotADirectory,
    /// The walk failed partway — permissions, usually. Carries the reason.
    Unreadable(String),
    /// A perfectly good directory with nothing shareable in it.
    Empty,
    /// Scanned. `skipped` counts media files whose names could not be
    /// classified; they are reported rather than shared.
    Ready {
        skipped: usize,
        items: Vec<Discovered>,
    },
}

impl DirOutcome {
    /// The discovered files, if the scan got that far — same honest-empty
    /// contract as [`ArrOutcome::items`].
    pub fn items(&self) -> &[Discovered] {
        match self {
            Self::Ready { items, .. } => items,
            _ => &[],
        }
    }
}

/// Scan one `[[library]]` directory and establish what sharerr would share.
pub fn check_library(library: &sharerr_core::config::LibraryConfig) -> DirOutcome {
    use crate::library::ScanError;

    match crate::library::scan(library) {
        Ok(outcome) if outcome.items.is_empty() && outcome.skipped == 0 => DirOutcome::Empty,
        Ok(outcome) => DirOutcome::Ready {
            skipped: outcome.skipped,
            items: outcome.items,
        },
        Err(ScanError::Missing(_)) => DirOutcome::Missing,
        Err(ScanError::NotADirectory(_)) => DirOutcome::NotADirectory,
        // `scan` refuses an empty root — it is what an unmounted bind mount
        // looks like, and scanning it to nothing would withdraw everything.
        // The probes report it as "empty" rather than as a scan failure.
        Err(ScanError::Empty(_)) => DirOutcome::Empty,
        Err(err @ ScanError::NotAbsolute(_)) => DirOutcome::Unreadable(err.to_string()),
        Err(ScanError::Unreadable { source, .. }) => DirOutcome::Unreadable(source.to_string()),
    }
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

/// Construct whichever torrent client `backend` selects.
///
/// The one place the backend→constructor decision lives: the reconciliation
/// loop and every health probe build their client through here, so a change to
/// how one is constructed cannot leave `doctor` testing a different thing from
/// what actually seeds.
pub fn build_torrent_client(
    backend: TorrentBackend,
    url: &Url,
    username: Option<&str>,
    credential: TorrentCredential,
) -> Result<Arc<dyn TorrentClient>, String> {
    Ok(match (backend, credential) {
        // Unreachable in practice: qBittorrent's `password_key` is `None`, so
        // nothing ever resolves a `TorrentCredential::Password` for it. Reported
        // rather than matched away, in case that ever changes.
        (TorrentBackend::Qbittorrent, TorrentCredential::Password(_)) => {
            return Err(
                "qBittorrent no longer authenticates with a username and password — set an \
                 API key instead (Options -> Web UI -> API key, qBittorrent 5.2+)."
                    .to_owned(),
            );
        }
        (TorrentBackend::Qbittorrent, TorrentCredential::ApiKey(key)) => {
            Arc::new(QbitClient::with_api_key(url, key).map_err(|e| chain(&e))?)
        }
        (TorrentBackend::Transmission, TorrentCredential::Password(password)) => {
            let username = username.unwrap_or_default();
            Arc::new(TransmissionClient::new(url, username, password).map_err(|e| chain(&e))?)
        }
        // Reported rather than silently ignored: an operator who stored a key for
        // Transmission believes it is in use, and falling back to the password
        // would leave them debugging why rotating the key changed nothing.
        (TorrentBackend::Transmission, TorrentCredential::ApiKey(_)) => {
            return Err(
                "Transmission has no API key — its RPC authenticates with a username and \
                 password. Clear transmission's API key, or select qBittorrent."
                    .to_owned(),
            );
        }
        (TorrentBackend::Rtorrent, TorrentCredential::Password(password)) => {
            let username = username.unwrap_or_default();
            Arc::new(RtorrentClient::new(url, username, password).map_err(|e| chain(&e))?)
        }
        // Same reasoning as Transmission above: rTorrent's XML-RPC has no key
        // auth of its own, only the username/password sent as Basic Auth.
        (TorrentBackend::Rtorrent, TorrentCredential::ApiKey(_)) => {
            return Err(
                "rTorrent has no API key — this authenticates with a username and password \
                 sent as HTTP Basic Auth. Clear rtorrent's API key, or select qBittorrent."
                    .to_owned(),
            );
        }
    })
}

/// How sharerr proves itself to the torrent client.
///
/// Kept as one value rather than two optional arguments so that "which credential
/// is in play" is decided once, by [`TorrentCredential::choose`], instead of at
/// every call site — see [`resolve_torrent_credential`], which is that one place.
#[derive(Debug)]
pub enum TorrentCredential {
    Password(SecretString),
    /// A qBittorrent 5.2+ WebUI API key.
    ApiKey(SecretString),
}

impl TorrentCredential {
    /// Pick the credential to use, preferring an API key when one is stored.
    ///
    /// The key wins because storing one is a deliberate act: an operator who
    /// generated a key and saved it expects it to be what authenticates, even
    /// though the password they set up first is still sitting in the vault.
    pub fn choose(api_key: Option<SecretString>, password: Option<SecretString>) -> Option<Self> {
        api_key
            .map(Self::ApiKey)
            .or_else(|| password.map(Self::Password))
    }

    /// The word to use for this credential in a message aimed at an operator.
    pub fn noun(&self) -> &'static str {
        match self {
            Self::Password(_) => "username or password",
            Self::ApiKey(_) => "API key",
        }
    }
}

/// Read whichever of `client`'s vault keys are configured and resolve them to a
/// credential, via `secret` — the one place this decision is made.
///
/// Before this existed, `sync::build_client`, `web::probe::torrent_client_badge`,
/// and `web::topology::client_node` each read both keys and called
/// [`TorrentCredential::choose`] themselves, and `commands::doctor::check_qbit`
/// picked a variant by hand without calling `choose` at all — four places that
/// could each drift on what "resolve the configured credential" means. `secret`
/// stays generic over the error type each caller already reports failures as
/// (an owned `String` describing what went wrong), rather than forcing every
/// caller through one concrete vault or reporting type.
pub fn resolve_torrent_credential(
    client: &TorrentClientConfig<'_>,
    secret: &impl Fn(&'static str) -> Result<Option<SecretString>, String>,
) -> Result<Option<TorrentCredential>, String> {
    let api_key = match client.api_key_key {
        Some(key) => secret(key)?,
        None => None,
    };
    let password = match client.password_key {
        Some(key) => secret(key)?,
        None => None,
    };
    Ok(TorrentCredential::choose(api_key, password))
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
    username: Option<&str>,
    credential: Result<Option<TorrentCredential>, String>,
) -> QbitOutcome {
    let credential = match credential {
        Ok(Some(credential)) => credential,
        Ok(None) => return QbitOutcome::NoCredential,
        Err(reason) => return QbitOutcome::CredentialUnreadable(reason),
    };

    let client = match build_torrent_client(backend, url, username, credential) {
        Ok(client) => client,
        Err(reason) => return QbitOutcome::BadUrl(reason),
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

    /// Shaped to `check_arr`'s `api_key` parameter, which is itself a
    /// `Result` because resolving a secret from the vault can fail. Unwrapping
    /// it here would mean re-wrapping at every call site below.
    #[allow(clippy::unnecessary_wraps, reason = "matches check_arr's parameter")]
    fn key() -> Result<Option<SecretString>, String> {
        Ok(Some(SecretString::from(API_KEY)))
    }

    use sharerr_testkit::mock::mount_json_status as mount;

    async fn mount_status(server: &MockServer) {
        sharerr_testkit::mock::mount_json(
            server,
            "/api/v3/system/status",
            sharerr_testkit::library::system_status_json("Sonarr"),
        )
        .await;
    }

    fn base(server: &MockServer) -> Url {
        Url::parse(&server.uri()).unwrap()
    }

    /// The distinction this module was created to preserve: a tag that does not
    /// exist and a tag nobody has applied are different findings with different
    /// fixes.
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
        let port = sharerr_testkit::net::closed_port();
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

    /// Each directory condition maps to its own outcome — the wording differs
    /// per caller, the finding must not.
    #[test]
    fn library_conditions_map_to_distinct_outcomes() {
        use sharerr_core::config::{LibraryConfig, LibraryKind};

        let dir = tempfile::tempdir().unwrap();
        let library = |path: std::path::PathBuf| LibraryConfig {
            path,
            kind: LibraryKind::Movie,
        };

        let missing = check_library(&library(dir.path().join("nope")));
        assert!(matches!(missing, DirOutcome::Missing), "got {missing:?}");

        let file_path = dir.path().join("plain.mkv");
        std::fs::write(&file_path, b"x").unwrap();
        let not_dir = check_library(&library(file_path));
        assert!(
            matches!(not_dir, DirOutcome::NotADirectory),
            "got {not_dir:?}"
        );

        let empty_dir = dir.path().join("empty");
        std::fs::create_dir(&empty_dir).unwrap();
        let empty = check_library(&library(empty_dir));
        assert!(matches!(empty, DirOutcome::Empty), "got {empty:?}");

        let full_dir = dir.path().join("full");
        std::fs::create_dir(&full_dir).unwrap();
        std::fs::write(full_dir.join("Gilded.Ferry.2019.mkv"), b"xx").unwrap();
        let ready = check_library(&library(full_dir));
        match ready {
            DirOutcome::Ready { skipped, items } => {
                assert_eq!(skipped, 0);
                assert_eq!(items.len(), 1);
            }
            other => panic!("got {other:?}"),
        }
    }

    /// The point of `snapshot`: what `diagnostics` and `topology` used to
    /// gather separately — an arr probe and a library scan — merges into one
    /// path check, and a library that scanned cleanly comes back as
    /// `LibraryScan::Scanned` rather than lost or misreported.
    #[tokio::test]
    async fn snapshot_merges_arr_and_library_discoveries_into_the_path_check() {
        use sharerr_core::config::{LibraryConfig, LibraryKind};

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Gilded.Ferry.2019.mkv"), b"xx").unwrap();

        let config = Config {
            library: vec![LibraryConfig {
                path: dir.path().to_path_buf(),
                kind: LibraryKind::Movie,
            }],
            ..Config::default()
        };
        // No *arr apps configured, so `configured_sources()` is empty and
        // this closure is never actually called — present only to satisfy
        // `snapshot`'s signature.
        let secret = |_: &'static str| -> Result<Option<SecretString>, String> { Ok(None) };

        let snap = snapshot(&config, &secret).await;

        assert!(snap.sources.is_empty(), "no *arr apps were configured");
        match &snap.libraries {
            LibraryScan::Scanned(scanned) => {
                assert_eq!(scanned.len(), 1);
                match &scanned[0].1 {
                    DirOutcome::Ready { items, .. } => assert_eq!(items.len(), 1),
                    other => panic!("got {other:?}"),
                }
            }
            other => panic!("got {other:?}"),
        }
        assert_eq!(
            snap.paths.checked, 1,
            "the library's one file must have reached the path check"
        );
    }

    /// A directory item resolving through no rule is by design, not a
    /// misconfiguration — the warning is reserved for paths another container
    /// reported.
    #[test]
    fn directory_items_are_not_counted_as_unmapped() {
        use sharerr_core::config::PathMapping;
        use sharerr_core::{ExternalIds, MediaSpec};

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Gilded.Ferry.2019.mkv");
        std::fs::write(&file, b"xx").unwrap();

        let config = Config {
            path_map: vec![PathMapping {
                arr: "/tv".into(),
                sharerr: dir.path().to_path_buf(),
                qbit: None,
            }],
            ..Config::default()
        };
        let item = sharerr_core::Discovered {
            source: MediaSource::Directory,
            source_id: 1,
            file_id: 2,
            spec: MediaSpec::Movie {
                title: "Gilded Ferry".to_owned(),
                year: Some(2019),
            },
            arr_path: file,
            size: 2,
            ids: ExternalIds::default(),
            media: None,
            scene_name: None,
            original_path: None,
        };

        let report = check_paths(&config, &[item]);
        assert_eq!(report.unmapped, 0);
        assert!(report.missing.is_empty());
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

    /// Every other state's `items` must be an honest empty, not a panic —
    /// callers summing across services rely on that.
    #[test]
    fn arr_outcome_items_is_empty_unless_ready() {
        assert!(ArrOutcome::NotConfigured.items().is_empty());
        assert!(ArrOutcome::AuthRejected.items().is_empty());

        let item = sharerr_core::Discovered {
            source: MediaSource::Sonarr,
            source_id: 1,
            file_id: 2,
            spec: sharerr_core::MediaSpec::Movie {
                title: "Gilded Ferry".to_owned(),
                year: Some(2019),
            },
            arr_path: "/tv/Gilded.Ferry.2019.mkv".into(),
            size: 2,
            ids: sharerr_core::ExternalIds::default(),
            media: None,
            original_path: None,
            scene_name: None,
        };
        let ready = ArrOutcome::Ready {
            version: "4.0".to_owned(),
            app_name: "Sonarr".to_owned(),
            items: vec![item],
        };
        assert_eq!(ready.items().len(), 1);
    }

    #[test]
    fn dir_outcome_items_is_empty_unless_ready() {
        assert!(DirOutcome::Missing.items().is_empty());
        assert!(DirOutcome::Empty.items().is_empty());

        let item = sharerr_core::Discovered {
            source: MediaSource::Directory,
            source_id: 0,
            file_id: 0,
            spec: sharerr_core::MediaSpec::Movie {
                title: "Gilded Ferry".to_owned(),
                year: Some(2019),
            },
            arr_path: "/movies/Gilded.Ferry.2019.mkv".into(),
            size: 2,
            original_path: None,
            ids: sharerr_core::ExternalIds::default(),
            media: None,
            scene_name: None,
        };
        let ready = DirOutcome::Ready {
            skipped: 3,
            items: vec![item],
        };
        assert_eq!(ready.items().len(), 1);
    }

    #[test]
    fn path_report_is_failure_and_readable_reflect_missing_and_invalid() {
        let clean = PathReport {
            checked: 5,
            ..PathReport::default()
        };
        assert!(!clean.is_failure());
        assert_eq!(clean.readable(), 5);

        let broken = PathReport {
            checked: 5,
            missing: vec!["/tv/gone.mkv".into()],
            invalid: vec!["not absolute".to_owned()],
            ..PathReport::default()
        };
        assert!(broken.is_failure());
        assert_eq!(broken.readable(), 3);
    }

    /// The finding `check_paths` exists to surface: a mapped path that does not
    /// exist on disk, counted as unmapped only when nothing matched — here a
    /// rule matches, so it must not be.
    #[test]
    fn check_paths_flags_a_missing_file_after_a_rule_applies() {
        use sharerr_core::config::PathMapping;

        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            path_map: vec![PathMapping {
                arr: "/tv".into(),
                sharerr: dir.path().to_path_buf(),
                qbit: None,
            }],
            ..Config::default()
        };
        let item = sharerr_core::Discovered {
            source: MediaSource::Sonarr,
            source_id: 1,
            file_id: 2,
            spec: sharerr_core::MediaSpec::Movie {
                title: "Gilded Ferry".to_owned(),
                year: Some(2019),
            },
            arr_path: "/tv/Gilded.Ferry.2019.mkv".into(),
            original_path: None,
            size: 2,
            ids: sharerr_core::ExternalIds::default(),
            media: None,
            scene_name: None,
        };

        let report = check_paths(&config, &[item]);
        assert_eq!(report.rules, 1);
        assert_eq!(report.checked, 1);
        assert_eq!(report.unmapped, 0, "a rule did match");
        assert_eq!(report.missing.len(), 1);
        assert!(report.is_failure());
    }

    /// A source path that never made it through the *arr's own absolute-path
    /// contract must be reported as invalid, not silently passed through.
    #[test]
    fn check_paths_reports_a_relative_arr_path_as_invalid() {
        let item = sharerr_core::Discovered {
            source: MediaSource::Sonarr,
            source_id: 1,
            file_id: 2,
            spec: sharerr_core::MediaSpec::Movie {
                title: "Gilded Ferry".to_owned(),
                year: Some(2019),
            },
            original_path: None,
            arr_path: "relative/Gilded.Ferry.2019.mkv".into(),
            size: 2,
            ids: sharerr_core::ExternalIds::default(),
            media: None,
            scene_name: None,
        };

        let report = check_paths(&Config::default(), &[item]);
        assert_eq!(report.invalid.len(), 1);
        assert!(report.missing.is_empty());
        assert!(report.is_failure());
    }

    #[test]
    fn build_torrent_client_rejects_a_credential_the_backend_cannot_use() {
        let url = Url::parse("http://localhost:8080").unwrap();

        let err = build_torrent_client(
            TorrentBackend::Qbittorrent,
            &url,
            None,
            TorrentCredential::Password(SecretString::from(
                sharerr_testkit::secrets::fresh_password(),
            )),
        )
        .unwrap_err();
        assert!(err.contains("API key"), "{err}");

        let err = build_torrent_client(
            TorrentBackend::Transmission,
            &url,
            None,
            TorrentCredential::ApiKey(SecretString::from(API_KEY)),
        )
        .unwrap_err();
        assert!(err.contains("username and password"), "{err}");

        let err = build_torrent_client(
            TorrentBackend::Rtorrent,
            &url,
            None,
            TorrentCredential::ApiKey(SecretString::from(API_KEY)),
        )
        .unwrap_err();
        assert!(err.contains("username and password"), "{err}");
    }

    #[test]
    fn build_torrent_client_builds_a_qbit_client_from_a_valid_api_key() {
        let url = Url::parse("http://localhost:8080").unwrap();
        let client = build_torrent_client(
            TorrentBackend::Qbittorrent,
            &url,
            None,
            TorrentCredential::ApiKey(SecretString::from("qbt_jCGn3V76XutJwQpsXgIm6A9NLB86")),
        )
        .unwrap();
        assert_eq!(client.kind(), ClientKind::QBittorrent);
    }

    #[test]
    fn torrent_credential_prefers_an_api_key_over_a_password() {
        let key = SecretString::from(API_KEY);
        let password = SecretString::from("hunter2");

        let chosen = TorrentCredential::choose(Some(key), Some(password.clone())).unwrap();
        assert!(matches!(chosen, TorrentCredential::ApiKey(_)));
        assert_eq!(chosen.noun(), "API key");

        let chosen = TorrentCredential::choose(None, Some(password)).unwrap();
        assert!(matches!(chosen, TorrentCredential::Password(_)));
        assert_eq!(chosen.noun(), "username or password");

        assert!(TorrentCredential::choose(None, None).is_none());
    }

    #[tokio::test]
    async fn check_qbit_distinguishes_a_missing_from_an_unreadable_credential() {
        let url = Url::parse("http://localhost:8080").unwrap();

        let missing = check_qbit(TorrentBackend::Qbittorrent, &url, None, Ok(None)).await;
        assert!(matches!(missing, QbitOutcome::NoCredential), "{missing:?}");

        let unreadable = check_qbit(
            TorrentBackend::Qbittorrent,
            &url,
            None,
            Err("no master key".to_owned()),
        )
        .await;
        assert!(
            matches!(unreadable, QbitOutcome::CredentialUnreadable(_)),
            "{unreadable:?}"
        );
    }

    #[tokio::test]
    async fn check_qbit_signs_in_and_reports_the_version() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/app/version"))
            .respond_with(ResponseTemplate::new(200).set_body_string("v5.2.3"))
            .mount(&server)
            .await;

        let credential = Ok(Some(TorrentCredential::ApiKey(SecretString::from(
            "qbt_jCGn3V76XutJwQpsXgIm6A9NLB86",
        ))));
        let outcome = check_qbit(
            TorrentBackend::Qbittorrent,
            &base(&server),
            None,
            credential,
        )
        .await;

        match outcome {
            QbitOutcome::Ready { version, kind, .. } => {
                assert_eq!(version, "v5.2.3");
                assert_eq!(kind, ClientKind::QBittorrent);
            }
            other => panic!("got {other:?}"),
        }
    }

    /// BUG (pre-existing, not fixed here): `check_qbit` only classifies
    /// `login()`'s error into `AuthRejected`/`Unreachable`/`Failed` — the
    /// `version()` call right after it maps *any* error to `Failed`
    /// unconditionally. For qBittorrent, whose `login()` is a no-op that always
    /// returns `Ok(())` (auth is a per-request bearer token, no handshake),
    /// that means a rejected key here can never come back as `AuthRejected`
    /// despite the enum existing specifically to distinguish that from a
    /// generic failure. This test pins the actual (degraded) behavior; if
    /// `check_qbit` starts classifying the `version()` error the same way as
    /// `login()`'s, this should be updated to expect `AuthRejected`.
    #[tokio::test]
    async fn check_qbit_reports_a_rejected_key_as_a_generic_failure_not_auth_rejected() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/app/version"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let credential = Ok(Some(TorrentCredential::ApiKey(SecretString::from(
            "qbt_jCGn3V76XutJwQpsXgIm6A9NLB86",
        ))));
        let outcome = check_qbit(
            TorrentBackend::Qbittorrent,
            &base(&server),
            None,
            credential,
        )
        .await;

        assert!(matches!(outcome, QbitOutcome::Failed(_)), "{outcome:?}");
    }

    /// Same gap as above, for an unreachable host: see the BUG note on
    /// `check_qbit_reports_a_rejected_key_as_a_generic_failure_not_auth_rejected`.
    #[tokio::test]
    async fn check_qbit_reports_an_unreachable_client_as_a_generic_failure_not_unreachable() {
        let port = sharerr_testkit::net::closed_port();
        let url = Url::parse(&format!("http://127.0.0.1:{port}")).unwrap();

        let credential = Ok(Some(TorrentCredential::ApiKey(SecretString::from(
            "qbt_jCGn3V76XutJwQpsXgIm6A9NLB86",
        ))));
        let outcome = check_qbit(TorrentBackend::Qbittorrent, &url, None, credential).await;

        assert!(matches!(outcome, QbitOutcome::Failed(_)), "{outcome:?}");
    }
}
