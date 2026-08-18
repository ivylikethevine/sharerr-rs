//! Errors from the qBittorrent WebUI client.

pub type Result<T> = std::result::Result<T, QbitError>;

#[derive(Debug, thiserror::Error)]
pub enum QbitError {
    #[error("could not reach qBittorrent at {url}: {detail}")]
    Unreachable { url: String, detail: String },

    #[error(
        "qBittorrent rejected the API key. Check the qbittorrent.api_key entry in the \
         vault matches the key in Options -> Web UI -> API key — rotating the key \
         there invalidates the old one immediately. This could also be a Host-header \
         port mismatch: qBittorrent validates the Host header's port against its own \
         WebUI port, so reaching it through a remapped docker port or a reverse proxy \
         on a different port is rejected the same way. API keys need qBittorrent 5.2 \
         or newer"
    )]
    ApiKeyRejected,

    #[error(
        "that does not look like a qBittorrent API key. Keys are 32 characters: \
         `qbt_` followed by 28 letters and digits, generated under Options -> Web UI \
         -> API key"
    )]
    MalformedApiKey,

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

    /// Whether this is a rejected credential rather than an unreachable service —
    /// the two have different fixes and must be reported differently.
    pub fn is_auth_failure(&self) -> bool {
        matches!(self, Self::ApiKeyRejected | Self::MalformedApiKey)
    }
}
