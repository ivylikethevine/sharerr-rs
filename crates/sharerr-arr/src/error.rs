//! Errors from the Sonarr/Radarr clients.
//!
//! The variants are deliberately fine-grained around *why* a call failed, because
//! `doctor` gives a different remedy for each: an unreachable host is a network or
//! URL problem, a rejected key is a vault problem, and a missing tag is a
//! configuration problem in the *arr app itself.

use sharerr_core::MediaSource;

pub type Result<T> = std::result::Result<T, ArrError>;

#[derive(Debug, thiserror::Error)]
pub enum ArrError {
    #[error("could not reach {service} at {url}: {source}")]
    Unreachable {
        service: MediaSource,
        url: String,
        #[source]
        source: reqwest::Error,
    },

    #[error(
        "{service} rejected the API key (HTTP {status}). Check the {service}.api_key \
         entry in the vault against Settings -> General -> API Key"
    )]
    Unauthorized { service: MediaSource, status: u16 },

    #[error("{service} returned HTTP {status} for {path}: {body}")]
    Status {
        service: MediaSource,
        status: u16,
        path: String,
        body: String,
    },

    #[error("could not decode the {service} response for {path}: {source}")]
    Decode {
        service: MediaSource,
        path: String,
        #[source]
        source: serde_json::Error,
    },

    #[error(
        "{service} has no tag {label:?}. Create it and apply it to the content you \
         want to share (tags present: {})",
        if available.is_empty() { "none".to_owned() } else { available.join(", ") }
    )]
    TagNotFound {
        service: MediaSource,
        label: String,
        available: Vec<String>,
    },

    #[error("invalid {service} URL: {source}")]
    Url {
        service: MediaSource,
        #[source]
        source: url::ParseError,
    },

    #[error("could not build the HTTP client: {0}")]
    Client(#[source] reqwest::Error),

    #[error("{service} is not an *arr app and has no HTTP API to call")]
    NotAnApp { service: MediaSource },
}

impl ArrError {
    /// True when the service could not be contacted at all, as opposed to
    /// contacted and unhappy. `doctor` reports these very differently.
    pub fn is_unreachable(&self) -> bool {
        matches!(self, Self::Unreachable { .. })
    }

    /// Whether the service rejected the API key, as opposed to being unreachable.
    /// The fixes differ, so the caller must be able to tell them apart.
    pub fn is_auth_failure(&self) -> bool {
        matches!(self, Self::Unauthorized { .. })
    }
}
