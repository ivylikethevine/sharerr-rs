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
    /// Shared secret embedded in builtin-tracker announce URLs.
    pub const TRACKER_TOKEN: &str = "tracker.token";
    /// The `apikey` a friend's Prowlarr sends to the Torznab endpoint.
    ///
    /// Its absence closes the endpoint rather than opening it: the feed lists
    /// everything this instance shares, so defaulting to unauthenticated would
    /// publish the library to anyone who found the port.
    pub const TORZNAB_API_KEY: &str = "torznab.api_key";

    /// Every key sharerr actually reads.
    ///
    /// One list, because both consumers ask the same question and used to answer
    /// it separately: the CLI warns when `vault set` is given something outside
    /// it, and the web UI offers exactly these as editable fields. A fifth secret
    /// added to only one of those copies is a field the UI silently will not
    /// manage — so the list lives beside the constants that define it.
    pub const ALL: &[&str] = &[
        SONARR_API_KEY,
        RADARR_API_KEY,
        QBITTORRENT_PASSWORD,
        TRACKER_TOKEN,
        TORZNAB_API_KEY,
    ];
}

/// Dotted paths of the settings the web UI can write back to `sharerr.toml`.
///
/// These strings are load-bearing in three independent places: the settings
/// handlers name them when building a `toml_edit` edit, `settings.html` names them
/// again to decide whether a field is pinned by an environment variable, and the
/// environment scan *synthesises* the same strings by lowercasing `SHARERR_*` and
/// turning `__` into `.`. The template's lookup is therefore a string join between
/// a generated key and a hand-typed literal.
///
/// Typed once here so a typo is a compile error on the Rust side rather than a
/// field that silently renders editable while the environment has it pinned — a
/// mismatch nothing else in the build would catch.
pub mod config_paths {
    pub const TAG: &str = "tag";
    pub const DATA_DIR: &str = "data_dir";
    pub const SERVER_BIND: &str = "server.bind";

    pub const SONARR_URL: &str = "sonarr.url";
    pub const RADARR_URL: &str = "radarr.url";

    pub const QBITTORRENT_URL: &str = "qbittorrent.url";
    pub const QBITTORRENT_USERNAME: &str = "qbittorrent.username";
    pub const QBITTORRENT_CATEGORY: &str = "qbittorrent.category";
    pub const QBITTORRENT_TAG: &str = "qbittorrent.tag";
    pub const QBITTORRENT_SKIP_CHECKING: &str = "qbittorrent.skip_checking";

    pub const TRACKER_BACKEND: &str = "tracker.backend";
    pub const TRACKER_ADVERTISED_HOST: &str = "tracker.advertised_host";
    pub const TRACKER_PORT: &str = "tracker.port";

    pub const SYNC_ENABLED: &str = "sync.enabled";
    pub const SYNC_INTERVAL_SECS: &str = "sync.interval_secs";

    /// Every path the UI writes, for the test that proves each one names a real
    /// field. Keep in step with the constants above — a path missing from here is
    /// simply unverified, not broken.
    pub const ALL: &[&str] = &[
        TAG,
        DATA_DIR,
        SERVER_BIND,
        SONARR_URL,
        RADARR_URL,
        QBITTORRENT_URL,
        QBITTORRENT_USERNAME,
        QBITTORRENT_CATEGORY,
        QBITTORRENT_TAG,
        QBITTORRENT_SKIP_CHECKING,
        TRACKER_BACKEND,
        TRACKER_ADVERTISED_HOST,
        TRACKER_PORT,
        SYNC_ENABLED,
        SYNC_INTERVAL_SECS,
    ];
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
    /// qBittorrent's own embedded tracker. The default: sharerr turns it on and
    /// nothing else is required.
    QbittorrentEmbedded,
    /// sharerr's own tracker, served from this process on [`ServerConfig::bind`].
    /// Answers only for torrents sharerr made, and honours `tracker.token` when
    /// one is stored.
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// Every writable path must name a field that actually exists on `Config`.
    ///
    /// This is the check that was missing. The same dotted strings are typed by
    /// hand in the settings handlers and again in `settings.html`, and are
    /// generated a third time from `SHARERR_*` environment variables — so a typo in
    /// any one of them used to compile cleanly and simply stop matching, leaving a
    /// field that looks editable while the environment has it pinned.
    ///
    /// Walking the serialised form rather than naming fields again keeps this
    /// honest: renaming a field in `Config` breaks this test rather than silently
    /// making a constant wrong.
    #[test]
    fn every_writable_path_resolves_to_a_real_config_field() {
        // Sonarr and Radarr are `None` by default and would serialise to null, so
        // populate them — the paths under them are the point.
        let config = Config {
            sonarr: Some(ServiceConfig {
                url: Url::parse("http://sonarr:8989").unwrap(),
            }),
            radarr: Some(ServiceConfig {
                url: Url::parse("http://radarr:7878").unwrap(),
            }),
            ..Config::default()
        };
        let document = serde_json::to_value(&config).unwrap();

        for path in config_paths::ALL {
            let mut cursor = &document;
            for segment in path.split('.') {
                cursor = cursor.get(segment).unwrap_or_else(|| {
                    panic!("config path {path:?} has no field {segment:?} on Config")
                });
            }
        }
    }

    /// The environment scan lowercases `SHARERR_*` and turns `__` into `.`, so a
    /// path containing an uppercase letter could never be matched by an override
    /// and would render as editable no matter what the operator set.
    #[test]
    fn writable_paths_are_lowercase_so_env_overrides_can_match_them() {
        for path in config_paths::ALL {
            assert_eq!(
                **path,
                *path.to_lowercase(),
                "{path:?} must be lowercase to match a SHARERR_* override"
            );
        }
    }

    /// A duplicate would mean two settings sections writing the same key, with the
    /// second silently winning.
    #[test]
    fn writable_paths_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for path in config_paths::ALL {
            assert!(seen.insert(*path), "{path:?} is listed twice");
        }
    }
}
