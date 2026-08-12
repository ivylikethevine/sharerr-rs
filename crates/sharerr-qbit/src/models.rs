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
    pub progress: f64,
    #[serde(default)]
    pub category: String,
    /// Comma-separated in the wire format; use [`TorrentInfo::tag_list`].
    #[serde(default)]
    pub tags: String,
}

impl TorrentInfo {
    pub fn tag_list(&self) -> Vec<&str> {
        self.tags
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .collect()
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
    #[serde(default)]
    pub index: Option<i64>,
    #[serde(default)]
    pub progress: f64,
}

/// The subset of `app/preferences` sharerr reads.
///
/// qBittorrent returns well over a hundred keys; the field names here match the
/// wire format exactly, so no renaming is needed.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Preferences {
    #[serde(default)]
    pub enable_embedded_tracker: bool,
    #[serde(default)]
    pub embedded_tracker_port: u16,
    #[serde(default)]
    pub save_path: String,
}

/// A request to start seeding an existing file.
///
/// Constructed via [`AddTorrent::new`] so the invariants that keep qBittorrent from
/// relocating data are not left to each call site.
#[derive(Debug, Clone)]
pub struct AddTorrent<'a> {
    /// The bencoded `.torrent`.
    pub data: &'a [u8],
    /// Filename for the multipart part. Cosmetic, but qBittorrent logs it.
    pub filename: &'a str,
    /// Directory holding the existing content, **as qBittorrent sees it**. This is
    /// the qbit view from the path resolver, not sharerr's own view.
    pub save_path: &'a str,
    pub category: Option<&'a str>,
    pub tags: Option<&'a str>,
    /// Skip the hash check. Default `false`: qBittorrent verifies the file it
    /// already has, finds it complete, and seeds. `true` is faster on a large
    /// library but will happily seed mismatched data if a path mapping is wrong.
    pub skip_checking: bool,
    /// Add in a stopped state. Used by dry runs and tests.
    pub stopped: bool,
}

impl<'a> AddTorrent<'a> {
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

    pub fn category(mut self, category: &'a str) -> Self {
        self.category = Some(category);
        self
    }

    pub fn tags(mut self, tags: &'a str) -> Self {
        self.tags = Some(tags);
        self
    }

    pub fn skip_checking(mut self, skip: bool) -> Self {
        self.skip_checking = skip;
        self
    }

    pub fn stopped(mut self, stopped: bool) -> Self {
        self.stopped = stopped;
        self
    }
}
