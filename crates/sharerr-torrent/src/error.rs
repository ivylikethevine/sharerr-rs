//! Errors from torrent construction and tracker resolution.

use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, TorrentError>;

#[derive(Debug, thiserror::Error)]
pub enum TorrentError {
    #[error("cannot read {path} to build a torrent: {source}")]
    Unreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{path} is not a regular file")]
    NotAFile { path: PathBuf },

    #[error("{path} has no filename component")]
    NoFileName { path: PathBuf },

    #[error("building the torrent for {path} failed: {source}")]
    Build {
        path: PathBuf,
        #[source]
        source: lava_torrent::LavaTorrentError,
    },

    #[error("encoding the torrent for {path} failed: {source}")]
    Encode {
        path: PathBuf,
        #[source]
        source: lava_torrent::LavaTorrentError,
    },

    #[error("could not parse a stored .torrent: {source}")]
    Reparse {
        #[source]
        source: lava_torrent::LavaTorrentError,
    },

    #[error("could not re-encode a stored .torrent: {source}")]
    Reencode {
        #[source]
        source: lava_torrent::LavaTorrentError,
    },

    #[error(
        "neither tracker.advertised_host nor tracker.advertised_url is set — \
         sharerr cannot guess the address friends reach it on"
    )]
    NoAdvertisedHost,

    #[error("could not build an announce URL from {base:?}: {source}")]
    AnnounceUrl {
        base: String,
        #[source]
        source: url::ParseError,
    },
}
