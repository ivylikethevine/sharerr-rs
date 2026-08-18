//! Wire types for the qBittorrent WebUI API v2.

use serde::Deserialize;

/// A torrent as `torrents/info` reports it.
#[derive(Debug, Clone, Deserialize)]
pub struct TorrentInfo {
    pub hash: String,
    #[serde(default)]
    pub name: String,
    /// Directory qBittorrent expects the content in — **qBittorrent's view of the
    /// filesystem**, which need not match sharerr's.
    #[serde(default)]
    pub save_path: String,
    /// Full path of the content: the file itself for a single-file torrent, the
    /// root directory for a multi-file one.
    #[serde(default)]
    pub content_path: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub category: String,
    /// Comma-separated in the wire format; use [`TorrentInfo::tag_list`].
    #[serde(default)]
    pub tags: String,
}

impl TorrentInfo {
    /// The torrent's tags, split out of qBittorrent's comma-joined string.
    pub fn tag_list(&self) -> Vec<&str> {
        sharerr_client::split_tags(&self.tags)
    }

    /// Whether qBittorrent considers this torrent fully downloaded and seeding.
    pub fn is_seeding(&self) -> bool {
        matches!(
            self.state.as_str(),
            "uploading" | "stalledUP" | "queuedUP" | "forcedUP" | "checkingUP" | "pausedUP"
        )
    }
}

/// One file inside a torrent, from `torrents/files`.
#[derive(Debug, Clone, Deserialize)]
pub struct TorrentFile {
    /// Path relative to the torrent's `save_path`.
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub size: u64,
}

/// One entry from `torrents/trackers`.
///
/// qBittorrent lists its DHT/PEX/LSD sources here too, as pseudo-entries whose
/// `url` starts with `**` — callers that mutate the list must skip those.
#[derive(Debug, Clone, Deserialize)]
pub struct TrackerEntry {
    #[serde(default)]
    pub url: String,
}
