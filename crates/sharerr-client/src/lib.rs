//! The contract sharerr needs from a torrent client.
//!
//! # Why this is so small
//!
//! sharerr asks a torrent client to do one unusual thing: seed a file that already
//! exists, from where it already is, without moving, renaming, or re-linking it.
//! Everything else — scheduling, ratios, RSS, categories as an organising system —
//! belongs to the client and is none of sharerr's business.
//!
//! So the surface here is deliberately six operations wide. That narrowness is what
//! makes a second client tractable at all: qBittorrent, Transmission, Deluge and
//! rTorrent disagree about almost everything *except* "add this torrent, with the
//! data already at this path".
//!
//! # The one thing that is not universal
//!
//! [`TorrentClient::embedded_tracker_port`] returns an `Option`, because qBittorrent
//! has a built-in tracker and most clients do not. A client without one is not
//! broken — it just means sharerr's own tracker has to be the backend, which is
//! precisely why that tracker exists.

use std::fmt::Debug;
use std::path::Path;

use async_trait::async_trait;
use url::Url;

/// Which client an error or message is about.
///
/// Carried so that a failure can name the thing the operator has to go and look at,
/// rather than saying "the torrent client" and leaving them to work it out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientKind {
    QBittorrent,
    Transmission,
}

impl ClientKind {
    /// The name as an operator would write it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::QBittorrent => "qBittorrent",
            Self::Transmission => "Transmission",
        }
    }
}

impl std::fmt::Display for ClientKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What can go wrong talking to a torrent client.
///
/// The variants exist to keep apart the failures with *different fixes*.
/// "Unreachable" sends an operator to their network or their URL; "auth rejected"
/// sends them to their password. Collapsing the two into one error string is how a
/// diagnostic stops being useful.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("{kind} at {url} could not be reached: {detail}")]
    Unreachable {
        kind: ClientKind,
        url: String,
        detail: String,
    },

    #[error("{kind} rejected the username or password")]
    AuthRejected { kind: ClientKind },

    #[error("{kind} refused the request: {detail}")]
    Api { kind: ClientKind, detail: String },

    #[error("{kind} sent a response this build could not read: {detail}")]
    Malformed { kind: ClientKind, detail: String },

    #[error("{0}")]
    Config(String),
}

impl ClientError {
    /// Whether this is a credential problem rather than a reachability one.
    pub fn is_auth_failure(&self) -> bool {
        matches!(self, Self::AuthRejected { .. })
    }

    /// Whether nothing answered at all.
    pub fn is_unreachable(&self) -> bool {
        matches!(self, Self::Unreachable { .. })
    }
}

/// Result alias for client operations.
pub type Result<T> = std::result::Result<T, ClientError>;

/// A torrent the client already knows about.
///
/// Deliberately not the union of every client's fields: this is what sharerr's
/// reconciliation actually reads, and a field nobody reads is a field two clients
/// have to agree about for no reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorrentSummary {
    /// Lowercase hex info hash. sharerr's join key between its store and the client.
    pub hash: String,
    pub name: String,
    /// Directory the client expects the content in — **the client's view of the
    /// filesystem**, which need not match sharerr's.
    pub save_path: String,
    /// Full path of the content: the file itself for a single-file torrent, the
    /// root directory for a multi-file one.
    ///
    /// qBittorrent reports this directly. Transmission does not, so its client
    /// derives it as `downloadDir` joined with the torrent name — which is the same
    /// thing, and is what makes cross-seed detection work identically on both.
    pub content_path: String,
    /// qBittorrent's category, or Transmission's first label. Empty when unset.
    pub category: String,
    /// qBittorrent's tags, or Transmission's labels.
    pub tags: Vec<String>,
    /// Whether the client considers this complete and uploading.
    pub is_seeding: bool,
}

/// One file inside a torrent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorrentFileEntry {
    /// Path relative to the torrent's save path.
    pub name: String,
    pub size: u64,
}

/// A request to seed content that already exists on disk.
///
/// `save_path` is the single most important field and the easiest to get wrong: it
/// is the directory **as the torrent client sees it**, which on a containerised
/// setup is usually not the path sharerr used to read the file. Getting it wrong
/// does not fail loudly — the client simply re-downloads, or sits at 0%.
#[derive(Debug, Clone)]
pub struct AddRequest<'a> {
    /// The `.torrent` file's bytes.
    pub data: &'a [u8],
    /// Filename for the upload. Cosmetic, but clients log it.
    pub filename: &'a str,
    /// Directory holding the existing content, as the *client* sees it.
    pub save_path: &'a str,
    /// qBittorrent's category; Transmission has no equivalent and folds it into
    /// labels.
    pub category: Option<&'a str>,
    /// Comma-separated for qBittorrent, a label list for Transmission.
    pub tags: Option<&'a str>,
    /// Skip the hash check.
    ///
    /// Default `false`: the client verifies what is already on disk, finds it
    /// complete, and seeds. `true` is faster on a large library and will happily
    /// seed mismatched data if the path is wrong.
    pub skip_checking: bool,
    /// Add without starting. Used by dry runs and tests.
    pub stopped: bool,
}

impl<'a> AddRequest<'a> {
    pub fn new(data: &'a [u8], filename: &'a str, save_path: &'a str) -> Self {
        Self {
            data,
            filename,
            save_path,
            category: None,
            tags: None,
            skip_checking: false,
            stopped: false,
        }
    }

    /// Set the category the client files this torrent under.
    pub fn category(mut self, category: &'a str) -> Self {
        self.category = Some(category);
        self
    }

    /// Set the tags applied alongside the category.
    pub fn tags(mut self, tags: &'a str) -> Self {
        self.tags = Some(tags);
        self
    }

    /// Skip the hash check on add. See the field docs before setting this.
    pub fn skip_checking(mut self, skip: bool) -> Self {
        self.skip_checking = skip;
        self
    }

    /// Add the torrent without starting it.
    pub fn stopped(mut self, stopped: bool) -> Self {
        self.stopped = stopped;
        self
    }

    /// The tags as a list, however the caller joined them.
    pub fn tag_list(&self) -> Vec<&str> {
        self.tags
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .collect()
    }
}

/// Everything sharerr asks of a torrent client.
///
/// `Send + Sync + Debug` because it is held in an `Arc` inside the reconciliation
/// loop and logged when things go wrong.
#[async_trait]
pub trait TorrentClient: Send + Sync + Debug {
    /// Which client this is, for messages that need to name it.
    fn kind(&self) -> ClientKind;

    /// The instance being talked to, for errors that name it.
    fn base_url(&self) -> &Url;

    /// Establish a session, if the protocol has one.
    ///
    /// Called explicitly rather than lazily so that "reached it but the password is
    /// wrong" and "could not reach it" can be reported separately — an implicit
    /// login reports them identically, and they have different fixes.
    async fn login(&self) -> Result<()>;

    /// The client's version string. Proof of both reachability and authentication.
    async fn version(&self) -> Result<String>;

    /// Torrents the client knows about, optionally narrowed to one category.
    ///
    /// A client with no notion of categories may filter on labels instead, or
    /// ignore the filter and let the caller do it — the caller must not assume the
    /// filter was applied.
    async fn list(&self, category: Option<&str>) -> Result<Vec<TorrentSummary>>;

    /// The files inside one torrent.
    async fn files(&self, hash: &str) -> Result<Vec<TorrentFileEntry>>;

    /// Seed content that already exists, from where it already is.
    ///
    /// The whole point of this trait. An implementation that moves, copies, or
    /// renames the data is wrong, however convenient its API makes it.
    async fn add(&self, request: &AddRequest<'_>) -> Result<()>;

    /// Stop seeding a torrent **without deleting its data**.
    ///
    /// Every client can delete the content on removal and most make it easy;
    /// sharerr never wants that. The media belongs to the operator and predates
    /// sharerr knowing about it.
    async fn remove(&self, hash: &str) -> Result<()>;

    /// Turn on the client's embedded tracker and report its port, if it has one.
    ///
    /// `None` means the client has no embedded tracker — the normal case outside
    /// qBittorrent. That is not an error: it means announce URLs have to point at
    /// sharerr's own tracker instead.
    async fn embedded_tracker_port(&self) -> Result<Option<u16>>;
}

/// Whether a path looks like something a client could be handed as a save path.
///
/// Cheap sanity, not validation: only the client can say whether a path exists in
/// *its* filesystem, and that is exactly the mismatch path mapping exists to
/// resolve.
pub fn looks_like_absolute(path: &Path) -> bool {
    path.is_absolute()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn the_error_variants_that_have_different_fixes_stay_apart() {
        let unreachable = ClientError::Unreachable {
            kind: ClientKind::Transmission,
            url: "http://box:9091".to_owned(),
            detail: "connection refused".to_owned(),
        };
        let rejected = ClientError::AuthRejected {
            kind: ClientKind::Transmission,
        };

        assert!(unreachable.is_unreachable() && !unreachable.is_auth_failure());
        assert!(rejected.is_auth_failure() && !rejected.is_unreachable());
    }

    /// An error must name the client, or an operator running two of them cannot
    /// tell which one to go and look at.
    #[test]
    fn an_error_names_the_client_it_is_about() {
        let err = ClientError::AuthRejected {
            kind: ClientKind::Transmission,
        };
        assert!(err.to_string().contains("Transmission"), "{err}");
    }

    #[test]
    fn tags_split_the_way_every_client_expects_to_receive_them() {
        let data = b"x";
        let request = AddRequest::new(data, "a.torrent", "/downloads").tags("sharerr, shared ,");
        assert_eq!(request.tag_list(), vec!["sharerr", "shared"]);
    }

    #[test]
    fn a_request_with_no_tags_has_no_tags_rather_than_one_empty_one() {
        let data = b"x";
        assert!(
            AddRequest::new(data, "a.torrent", "/downloads")
                .tag_list()
                .is_empty()
        );
    }

    /// The defaults are the safe ones: verify what is on disk, and start seeding.
    #[test]
    fn the_defaults_verify_and_start() {
        let data = b"x";
        let request = AddRequest::new(data, "a.torrent", "/downloads");
        assert!(!request.skip_checking, "skipping the check must be opt-in");
        assert!(!request.stopped);
    }
}
