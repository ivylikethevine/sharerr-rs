//! Layered configuration: defaults -> `sharerr.toml` -> `SHARERR_*` env vars.
//!
//! Secrets never appear here. API keys and the qBittorrent password live in the
//! encrypted vault (`sharerr-store`) and are looked up by the keys in [`secret_keys`].

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use url::Url;

/// Vault lookup keys for the credentials that back the configured services.
pub mod secret_keys {
    pub const SONARR_API_KEY: &str = "sonarr.api_key";
    pub const RADARR_API_KEY: &str = "radarr.api_key";
    pub const QBITTORRENT_PASSWORD: &str = "qbittorrent.password";
    /// Shared secret embedded in builtin-tracker announce URLs (milestone 2).
    pub const TRACKER_TOKEN: &str = "tracker.token";
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Where the SQLite database, vault, and generated .torrent files live.
    pub data_dir: PathBuf,
    /// The Sonarr/Radarr tag label that marks content for sharing.
    pub tag: String,
    pub server: ServerConfig,
    pub sonarr: Option<ServiceConfig>,
    pub radarr: Option<ServiceConfig>,
    pub qbittorrent: QbitConfig,
    pub tracker: TrackerConfig,
    pub sync: SyncConfig,
    /// Translations between how the *arr apps, sharerr, and qBittorrent each see
    /// the media library. Empty means all three agree on paths.
    pub path_map: Vec<PathMapping>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("/data"),
            tag: "sharerr".to_owned(),
            server: ServerConfig::default(),
            sonarr: None,
            radarr: None,
            qbittorrent: QbitConfig::default(),
            tracker: TrackerConfig::default(),
            sync: SyncConfig::default(),
            path_map: Vec::new(),
        }
    }
}

impl Config {
    pub fn database_path(&self) -> PathBuf {
        self.data_dir.join("sharerr.db")
    }

    pub fn vault_path(&self) -> PathBuf {
        self.data_dir.join("vault.bin")
    }

    /// Directory for the .torrent files handed to qBittorrent.
    pub fn torrent_dir(&self) -> PathBuf {
        self.data_dir.join("torrents")
    }

    pub fn resolver(&self) -> crate::paths::PathResolver {
        crate::paths::PathResolver::new(self.path_map.clone())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    pub bind: SocketAddr,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 8477),
        }
    }
}

/// A Sonarr or Radarr instance. The API key is stored in the vault, not here.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    pub url: Url,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct QbitConfig {
    pub url: Url,
    pub username: String,
    /// Category applied to torrents sharerr creates, so they are easy to find.
    pub category: String,
    /// Tag applied alongside the category.
    pub tag: String,
    /// Skip qBittorrent's hash check when adding a torrent.
    ///
    /// Default `false`: qBittorrent verifies the existing file, finds it complete,
    /// and seeds immediately. Setting this `true` is faster on large libraries but
    /// will happily seed mismatched data if a path mapping is wrong.
    pub skip_checking: bool,
}

impl Default for QbitConfig {
    // The URL is a compile-time literal; parsing it cannot fail at runtime and
    // `Url` has no const constructor to express that in the type system.
    #[allow(clippy::expect_used)]
    fn default() -> Self {
        Self {
            url: Url::parse("http://localhost:8080").expect("valid literal url"),
            username: "admin".to_owned(),
            category: "sharerr".to_owned(),
            tag: "sharerr".to_owned(),
            skip_checking: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrackerBackend {
    /// qBittorrent's own embedded tracker. Fully supported in milestone 1.
    QbittorrentEmbedded,
    /// sharerr's builtin tracker. Announce URLs are generated, but the tracker
    /// server itself arrives in milestone 2.
    Builtin,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TrackerConfig {
    pub backend: TrackerBackend,
    /// Hostname or IP friends will reach the tracker on. Required — sharerr cannot
    /// guess its own externally reachable address, and a wrong guess silently
    /// produces torrents nobody can announce to.
    pub advertised_host: Option<String>,
    /// Override the announce port. For the qBittorrent backend this defaults to
    /// whatever `embedded_tracker_port` reports; for the builtin backend, to
    /// [`ServerConfig::bind`]'s port.
    pub port: Option<u16>,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        Self {
            backend: TrackerBackend::QbittorrentEmbedded,
            advertised_host: None,
            port: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SyncConfig {
    /// Run the reconciliation loop on a timer while `serve` is running.
    pub enabled: bool,
    pub interval_secs: u64,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_secs: 900,
        }
    }
}

/// One rewrite rule between the three views of the media library.
///
/// Sonarr reports `arr`, sharerr must open `sharerr`, and qBittorrent must be told
/// `qbit`. These differ whenever the containers mount the library at different
/// points, which is the common case.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PathMapping {
    /// Prefix as Sonarr/Radarr report it.
    pub arr: PathBuf,
    /// Prefix as the sharerr process sees it.
    pub sharerr: PathBuf,
    /// Prefix as qBittorrent sees it. Defaults to `sharerr` when omitted.
    #[serde(default)]
    pub qbit: Option<PathBuf>,
}
