//! Errors from the qBittorrent WebUI client.

pub type Result<T> = std::result::Result<T, QbitError>;

#[derive(Debug, thiserror::Error)]
pub enum QbitError {
    #[error("could not reach qBittorrent at {url}: {source}")]
    Unreachable {
        url: String,
        #[source]
        source: reqwest::Error,
    },

    #[error(
        "qBittorrent rejected the credentials. Check the qbittorrent.password entry \
         in the vault and the qbittorrent.username setting"
    )]
    LoginRejected,

    #[error(
        "qBittorrent refused to accept a login (HTTP 403). Repeated failures ban the \
         client IP for a few minutes — wait, or clear the ban in Options -> Web UI"
    )]
    LoginBanned,

    #[error(
        "qBittorrent returned 403 for {path} even after re-authenticating. The most \
         common cause is a Referer that does not match the configured WebUI address"
    )]
    Forbidden { path: String },

    #[error("qBittorrent returned HTTP {status} for {path}: {body}")]
    Status {
        status: u16,
        path: String,
        body: String,
    },

    #[error("could not decode the qBittorrent response for {path}: {source}")]
    Decode {
        path: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("qBittorrent rejected the .torrent file for {name}")]
    InvalidTorrent { name: String },

    #[error("invalid qBittorrent URL: {source}")]
    Url {
        #[source]
        source: url::ParseError,
    },

    #[error("could not build the HTTP client: {0}")]
    Client(#[source] reqwest::Error),
}

impl QbitError {
    /// True when qBittorrent could not be contacted at all, as opposed to
    /// contacted and unhappy. `doctor` gives different advice for each.
    pub fn is_unreachable(&self) -> bool {
        matches!(self, Self::Unreachable { .. })
    }

    pub fn is_auth_failure(&self) -> bool {
        matches!(self, Self::LoginRejected | Self::LoginBanned)
    }
}
