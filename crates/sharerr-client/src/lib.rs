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
//! data already at this path". Announces always go to sharerr's own tracker, so a
//! client needs no tracker of its own.
//!
//! The one deliberate exception is [`AddRequest::upload_limit_kib`] and
//! [`AddRequest::ratio_limit`]: an operator-configured seeding goal, stated
//! once at add time through whichever native mechanism the client already
//! offers for it. sharerr still runs no scheduling of its own — the client's
//! own already-running seeding engine does the continuous enforcement, the
//! same as it would for a torrent added by hand.

use std::fmt::Debug;

use async_trait::async_trait;
use std::time::Duration;
use url::Url;

/// Enough of an error body to identify the problem, bounded so a stray HTML error
/// page from a misconfigured reverse proxy does not flood the logs.
pub const MAX_ERROR_BODY: usize = 400;

/// Clamp an error body for inclusion in a message.
///
/// Bounded by **characters, not bytes**: `String::truncate` panics outright if the
/// byte index lands inside a multi-byte character, and error bodies are exactly
/// where non-ASCII shows up — a localized error page from a reverse proxy, or a
/// title quoted back in the message.
pub fn clamp_body(body: &str) -> String {
    body.chars().take(MAX_ERROR_BODY).collect()
}

/// Render an error together with its cause chain.
///
/// The distinction matters: reqwest's own `Display` is just "error sending request
/// for url (...)", and the part an operator actually needs — `Connection refused`,
/// `dns error`, `operation timed out` — lives further down the chain.
pub fn error_chain(err: &dyn std::error::Error) -> String {
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

/// Split a comma-joined tag string, however the sender spaced it.
///
/// The one place the comma-joined-tag convention is decoded — qBittorrent reports
/// tags this way and [`AddRequest::tags`] carries them this way.
pub fn split_tags(raw: &str) -> Vec<&str> {
    raw.split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect()
}

/// Whether a status means "the credential was not accepted".
///
/// Both, and not one: a rejected key or password can come back as either 401 or
/// 403 depending on the server, and treating only one as rejection would silently
/// ignore the other.
pub fn is_auth_rejection(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN
}

/// The request timeout every torrent-client HTTP client is built with.
///
/// A host that accepts the TCP connection and then stalls — a VPN namespace
/// half-up, a reverse proxy whose SCGI backend has wedged — would otherwise
/// block the sequential sync loop forever, with `/ready` still reporting
/// ready. Sixty seconds is generous for an RPC call and short enough that
/// the loop reports the stall on the same pass.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// Build the `reqwest::Client` a torrent client speaks through: the shared
/// [`DEFAULT_TIMEOUT`], nothing else. One place so a client cannot forget the
/// timeout — two of the three already had.
pub fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(DEFAULT_TIMEOUT)
        .build()
        .map_err(|e| ClientError::Config(format!("building the HTTP client: {e}")))
}

/// Wrap a transport-level failure as [`ClientError::Unreachable`], with the
/// cause chain rendered via [`error_chain`]. Lifted out of
/// `sharerr-transmission` and `sharerr-rtorrent`, which built this identically
/// apart from which field held the URL a client speaks to.
pub fn unreachable(kind: ClientKind, url: &str, err: &reqwest::Error) -> ClientError {
    ClientError::Unreachable {
        kind,
        url: url.to_owned(),
        detail: error_chain(err),
    }
}

/// A copy of `base` whose path ends in `/`, so `Url::join` appends rather than
/// replacing the last segment. This is what makes reverse-proxy subpaths
/// (`http://host/sonarr/`) work.
pub fn normalise_base(base: &Url) -> Url {
    let mut base = base.clone();
    if !base.path().ends_with('/') {
        let with_slash = format!("{}/", base.path());
        base.set_path(&with_slash);
    }
    base
}

/// Which client an error or message is about.
///
/// Carried so that a failure can name the thing the operator has to go and look at,
/// rather than saying "the torrent client" and leaving them to work it out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientKind {
    QBittorrent,
    Transmission,
    Rtorrent,
}

impl ClientKind {
    /// The name as an operator would write it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::QBittorrent => "qBittorrent",
            Self::Transmission => "Transmission",
            Self::Rtorrent => "rTorrent",
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
    /// Per-torrent upload cap in KiB/s, applied once at add time. The one
    /// exception to this trait's "ratios and scheduling belong to the
    /// client" rule at the module level: sharerr states the goal once here,
    /// and the client's own seeding engine enforces it from then on —
    /// sharerr never polls or re-applies it.
    pub upload_limit_kib: Option<u64>,
    /// Seed-ratio goal, applied once at add time — same caveat as
    /// [`Self::upload_limit_kib`].
    pub ratio_limit: Option<f64>,
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
            upload_limit_kib: None,
            ratio_limit: None,
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

    /// Cap this torrent's upload speed at `kib` KiB/s, from the moment it is
    /// added.
    pub fn upload_limit_kib(mut self, kib: u64) -> Self {
        self.upload_limit_kib = Some(kib);
        self
    }

    /// Set this torrent's seed-ratio goal, from the moment it is added.
    pub fn ratio_limit(mut self, ratio: f64) -> Self {
        self.ratio_limit = Some(ratio);
        self
    }

    /// The tags as a list, however the caller joined them.
    pub fn tag_list(&self) -> Vec<&str> {
        split_tags(self.tags.unwrap_or_default())
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

    /// Replace an existing torrent's tracker list with `urls`, one tier each,
    /// most-preferred first.
    ///
    /// This is what keeps an already-added torrent announcing somewhere alive
    /// after the advertised endpoint rotates — without it, every torrent added
    /// before a VPN reconnect keeps announcing to the dead address until it is
    /// removed and re-added.
    async fn set_trackers(&self, hash: &str, urls: &[Url]) -> Result<()>;
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

        assert_eq!(
            error_chain(&Outer),
            "error sending request: connection refused"
        );
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

        assert_eq!(error_chain(&Outer), "sending request: refused");
    }

    /// Both statuses that mean a credential was rejected must be treated alike.
    #[test]
    fn both_statuses_that_mean_the_credential_was_rejected_are_treated_alike() {
        assert!(is_auth_rejection(reqwest::StatusCode::UNAUTHORIZED));
        assert!(is_auth_rejection(reqwest::StatusCode::FORBIDDEN));
        assert!(!is_auth_rejection(reqwest::StatusCode::OK));
        assert!(!is_auth_rejection(reqwest::StatusCode::NOT_FOUND));
    }

    /// A subpath base must keep its prefix once normalised, or a reverse-proxied
    /// client would have its last segment replaced by `Url::join`.
    #[test]
    fn a_normalised_subpath_base_keeps_its_prefix() {
        let base = Url::parse("http://host/qbit").unwrap();
        let normalised = normalise_base(&base);
        assert_eq!(normalised.as_str(), "http://host/qbit/");
        assert_eq!(
            normalise_base(&normalised).as_str(),
            "http://host/qbit/",
            "an already-normalised base must pass through unchanged"
        );
    }
}
