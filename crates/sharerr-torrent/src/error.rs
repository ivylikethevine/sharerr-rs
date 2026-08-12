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

    #[error(
        "the builtin tracker is not implemented yet (milestone 2). Set \
         tracker.backend = \"qbittorrent-embedded\" to share content today"
    )]
    BuiltinTrackerUnavailable,

    #[error(
        "tracker.advertised_host is not set — sharerr cannot guess the address \
         friends reach it on"
    )]
    NoAdvertisedHost,

    #[error("qBittorrent reported embedded tracker port 0, which cannot be announced to")]
    NoTrackerPort,

    #[error("could not build an announce URL from host {host:?} port {port}: {source}")]
    AnnounceUrl {
        host: String,
        port: u16,
        #[source]
        source: url::ParseError,
    },

    #[error("qBittorrent: {0}")]
    Qbit(#[from] sharerr_qbit::QbitError),
}
