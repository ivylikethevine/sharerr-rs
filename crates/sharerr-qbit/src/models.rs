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
    #[serde(default)]
    pub ratio: f64,
    /// `-2` means "use qBittorrent's global default", `-1` means "unlimited" —
    /// see [`TorrentInfo::ratio_limit_reported`], which resolves both to `None`.
    #[serde(default)]
    pub ratio_limit: f64,
    /// Per-torrent upload cap in **bytes/s**; `-1` (older builds) or `0`
    /// means none — see [`TorrentInfo::upload_limit_kib_reported`].
    #[serde(default = "no_limit")]
    pub up_limit: i64,
}

fn no_limit() -> i64 {
    -1
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

    /// The actual per-torrent limit, resolving qBittorrent's `-2`
    /// (use-global-default) and `-1` (unlimited) sentinels to `None` — neither is
    /// a fixed number this specific torrent is held to.
    pub fn ratio_limit_reported(&self) -> Option<f64> {
        (self.ratio_limit >= 0.0).then_some(self.ratio_limit)
    }

    /// The per-torrent upload cap in KiB/s, or `None` for qBittorrent's two
    /// "no limit" spellings (`-1` and `0`). Rounded up so a cap sharerr set
    /// in KiB/s reads back as the same number.
    pub fn upload_limit_kib_reported(&self) -> Option<u64> {
        (self.up_limit > 0).then(|| u64::try_from(self.up_limit).unwrap_or(0).div_ceil(1024))
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
