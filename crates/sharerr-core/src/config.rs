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
    pub const LIDARR_API_KEY: &str = "lidarr.api_key";
    pub const READARR_API_KEY: &str = "readarr.api_key";
    pub const WHISPARR_API_KEY: &str = "whisparr.api_key";

    /// The vault key holding one *arr app's API key.
    ///
    /// A function rather than five match arms at each call site — every consumer
    /// asks the same question, and a sixth app should mean editing one place.
    /// `None` for the directory source, which has no credential at all.
    pub fn api_key_for(source: crate::MediaSource) -> Option<&'static str> {
        use crate::MediaSource::{Directory, Lidarr, Radarr, Readarr, Sonarr, Whisparr};
        match source {
            Sonarr => Some(SONARR_API_KEY),
            Radarr => Some(RADARR_API_KEY),
            Lidarr => Some(LIDARR_API_KEY),
            Readarr => Some(READARR_API_KEY),
            Whisparr => Some(WHISPARR_API_KEY),
            Directory => None,
        }
    }
    pub const QBITTORRENT_PASSWORD: &str = "qbittorrent.password";
    /// The Transmission RPC password, when Transmission is the selected backend.
    pub const TRANSMISSION_PASSWORD: &str = "transmission.password";
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
        LIDARR_API_KEY,
        READARR_API_KEY,
        WHISPARR_API_KEY,
        QBITTORRENT_PASSWORD,
        TRANSMISSION_PASSWORD,
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
    pub const LIDARR_URL: &str = "lidarr.url";
    pub const READARR_URL: &str = "readarr.url";
    pub const WHISPARR_URL: &str = "whisparr.url";

    /// The config path holding one *arr app's URL — the write-side counterpart
    /// of [`super::secret_keys::api_key_for`], for the same reason: every
    /// consumer asks the same question, and a sixth app should mean editing one
    /// function. `None` for the directory source, which has no URL.
    pub fn url_for(source: crate::MediaSource) -> Option<&'static str> {
        use crate::MediaSource::{Directory, Lidarr, Radarr, Readarr, Sonarr, Whisparr};
        match source {
            Sonarr => Some(SONARR_URL),
            Radarr => Some(RADARR_URL),
            Lidarr => Some(LIDARR_URL),
            Readarr => Some(READARR_URL),
            Whisparr => Some(WHISPARR_URL),
            Directory => None,
        }
    }

    pub const QBITTORRENT_URL: &str = "qbittorrent.url";
    pub const QBITTORRENT_USERNAME: &str = "qbittorrent.username";
    pub const QBITTORRENT_CATEGORY: &str = "qbittorrent.category";
    pub const QBITTORRENT_TAG: &str = "qbittorrent.tag";
    pub const QBITTORRENT_SKIP_CHECKING: &str = "qbittorrent.skip_checking";

    pub const TORRENT_BACKEND: &str = "torrent_backend";
    pub const TRANSMISSION_URL: &str = "transmission.url";
    pub const TRANSMISSION_USERNAME: &str = "transmission.username";
    pub const TRANSMISSION_LABEL: &str = "transmission.label";

    pub const TRACKER_BACKEND: &str = "tracker.backend";
    pub const TRACKER_ADVERTISED_HOST: &str = "tracker.advertised_host";
    pub const TRACKER_PORT: &str = "tracker.port";

    pub const SYNC_ENABLED: &str = "sync.enabled";
    pub const SYNC_INTERVAL_SECS: &str = "sync.interval_secs";

    /// The `SHARERR_*` variable that overrides a dotted config path — the inverse
    /// of the env scan's lowercase-and-`__`-to-`.` transform. Kept beside the
    /// paths so a consumer that needs the variable's name derives it from the
    /// same constant everything else uses, instead of hand-typing a third
    /// spelling that breaks silently when a field is renamed.
    pub fn env_var(path: &str) -> String {
        format!("SHARERR_{}", path.to_uppercase().replace('.', "__"))
    }

    /// Every path the UI writes, for the test that proves each one names a real
    /// field. Keep in step with the constants above — a path missing from here is
    /// simply unverified, not broken.
    pub const ALL: &[&str] = &[
        TAG,
        DATA_DIR,
        SERVER_BIND,
        SONARR_URL,
        RADARR_URL,
        LIDARR_URL,
        READARR_URL,
        WHISPARR_URL,
        QBITTORRENT_URL,
        QBITTORRENT_USERNAME,
        QBITTORRENT_CATEGORY,
        QBITTORRENT_TAG,
        QBITTORRENT_SKIP_CHECKING,
        TORRENT_BACKEND,
        TRANSMISSION_URL,
        TRANSMISSION_USERNAME,
        TRANSMISSION_LABEL,
        TRACKER_BACKEND,
        TRACKER_ADVERTISED_HOST,
        TRACKER_PORT,
        SYNC_ENABLED,
        SYNC_INTERVAL_SECS,
    ];
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
/// Everything sharerr needs to know that is not a secret.
///
/// Secrets live in the vault and are looked up by [`secret_keys`]; the paths this
/// type's own fields are written back to are in [`config_paths`].
pub struct Config {
    /// Where the SQLite database, vault, and generated .torrent files live.
    pub data_dir: PathBuf,
    /// The Sonarr/Radarr tag label that marks content for sharing.
    pub tag: String,
    pub server: ServerConfig,
    pub sonarr: Option<ServiceConfig>,
    pub radarr: Option<ServiceConfig>,
    /// Music. Lidarr is on API v1, which the client handles per-source.
    pub lidarr: Option<ServiceConfig>,
    /// Books. Also v1.
    pub readarr: Option<ServiceConfig>,
    /// Adult content. Whisparr is Sonarr's codebase, so it walks identically.
    pub whisparr: Option<ServiceConfig>,
    /// Which torrent client actually seeds. See [`TorrentBackend`].
    pub torrent_backend: TorrentBackend,
    pub qbittorrent: QbitConfig,
    /// Only read when `torrent_backend` selects it.
    pub transmission: TransmissionConfig,
    pub tracker: TrackerConfig,
    pub sync: SyncConfig,
    /// Translations between how the *arr apps, sharerr, and qBittorrent each see
    /// the media library. Empty means all three agree on paths.
    pub path_map: Vec<PathMapping>,
    /// Plain directories shared without any *arr app: everything in each one is
    /// shared, classified by its declared [`LibraryKind`]. The zero-dependency
    /// path — but it loses every external id, so a friend's app can only parse
    /// the release name.
    pub library: Vec<LibraryConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("/data"),
            tag: "sharerr".to_owned(),
            server: ServerConfig::default(),
            sonarr: None,
            radarr: None,
            lidarr: None,
            readarr: None,
            whisparr: None,
            torrent_backend: TorrentBackend::default(),
            qbittorrent: QbitConfig::default(),
            transmission: TransmissionConfig::default(),
            tracker: TrackerConfig::default(),
            sync: SyncConfig::default(),
            path_map: Vec::new(),
            library: Vec::new(),
        }
    }
}

impl Config {
    /// Where the SQLite database lives, derived from `data_dir`.
    pub fn database_path(&self) -> PathBuf {
        self.data_dir.join("sharerr.db")
    }

    /// Where the encrypted credential vault lives, derived from `data_dir`.
    pub fn vault_path(&self) -> PathBuf {
        self.data_dir.join("vault.bin")
    }

    /// Directory for the .torrent files handed to qBittorrent.
    pub fn torrent_dir(&self) -> PathBuf {
        self.data_dir.join("torrents")
    }

    /// The configuration for one *arr app, or `None` if it is not set up.
    ///
    /// Every consumer wants this and none of them should carry a five-way match to
    /// get it — adding a sixth app should mean editing one function.
    pub fn service(&self, source: crate::MediaSource) -> Option<&ServiceConfig> {
        use crate::MediaSource::{Directory, Lidarr, Radarr, Readarr, Sonarr, Whisparr};
        match source {
            Sonarr => self.sonarr.as_ref(),
            Radarr => self.radarr.as_ref(),
            Lidarr => self.lidarr.as_ref(),
            Readarr => self.readarr.as_ref(),
            Whisparr => self.whisparr.as_ref(),
            // A directory is configured through `library`, not a service section.
            Directory => None,
        }
    }

    /// Every *arr app that is actually configured. Directory libraries are not
    /// listed here — they have no service section; see [`Config::library`].
    pub fn configured_sources(&self) -> Vec<crate::MediaSource> {
        crate::MediaSource::ARRS
            .iter()
            .copied()
            .filter(|s| self.service(*s).is_some())
            .collect()
    }

    /// A resolver over the configured `path_map`, for translating between the
    /// three views of the library.
    pub fn resolver(&self) -> crate::paths::PathResolver {
        crate::paths::PathResolver::new(self.path_map.clone())
    }

    /// The HTTP base URL a friend reaches this instance on.
    ///
    /// Built from `tracker.advertised_host`, the only address sharerr is told
    /// that is known to work from outside — the bind address is usually
    /// `0.0.0.0`, which is not a URL anyone can fetch. The port is this server's
    /// own: `tracker.port` names where friends *announce*, which under the
    /// qbittorrent-embedded backend is **qBittorrent's** port — using it here
    /// once pointed every feed link at a BitTorrent announce endpoint. Only the
    /// builtin backend, where the tracker is this same HTTP server, honours the
    /// override.
    pub fn public_base_url(&self) -> String {
        let host = self
            .tracker
            .advertised_host
            .as_deref()
            .unwrap_or("localhost");
        let port = match self.tracker.backend {
            TrackerBackend::Builtin => self.tracker.port.unwrap_or_else(|| self.server.bind.port()),
            TrackerBackend::QbittorrentEmbedded => self.server.bind.port(),
        };
        format!("http://{host}:{port}")
    }

    /// The selected torrent client's connection and labelling settings.
    ///
    /// One accessor instead of a `match self.torrent_backend` at every call site:
    /// the section names in `sharerr.toml` differ per client, so the URL, username
    /// and vault key must be resolved together — plucking fields from the unused
    /// section is how an operator ends up debugging the wrong service.
    pub fn torrent_client(&self) -> TorrentClientConfig<'_> {
        match self.torrent_backend {
            TorrentBackend::Qbittorrent => TorrentClientConfig {
                url: &self.qbittorrent.url,
                username: &self.qbittorrent.username,
                password_key: secret_keys::QBITTORRENT_PASSWORD,
                category: &self.qbittorrent.category,
                tag: &self.qbittorrent.tag,
                skip_checking: self.qbittorrent.skip_checking,
            },
            TorrentBackend::Transmission => TorrentClientConfig {
                url: &self.transmission.url,
                username: &self.transmission.username,
                password_key: secret_keys::TRANSMISSION_PASSWORD,
                // Transmission has only labels, so the category and tag collapse
                // into one value, and there is no skip-check switch to honour.
                category: &self.transmission.label,
                tag: &self.transmission.label,
                skip_checking: false,
            },
        }
    }
}

/// What [`Config::torrent_client`] resolves: everything about whichever torrent
/// client is configured, independent of which one that is.
#[derive(Debug, Clone, Copy)]
pub struct TorrentClientConfig<'a> {
    pub url: &'a Url,
    pub username: &'a str,
    /// Vault key holding this client's password.
    pub password_key: &'static str,
    /// The grouping applied to torrents sharerr creates: qBittorrent's category,
    /// or Transmission's label.
    pub category: &'a str,
    /// qBittorrent's tag; for Transmission this repeats the label.
    pub tag: &'a str,
    /// Whether to skip hash-checking on add. Always `false` for Transmission,
    /// which has no such switch.
    pub skip_checking: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
/// Where the HTTP server listens. One port carries the web UI, the tracker, and
/// the Torznab feed.
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
/// How to reach qBittorrent, and what to label what sharerr puts there. The
/// password is in the vault, not here.
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

/// Which torrent client sharerr drives.
///
/// qBittorrent remains the default because it was the only option for the whole of
/// M1–M5 and every existing config expects it. The choice matters beyond which HTTP
/// API is spoken: qBittorrent has an embedded tracker and Transmission does not, so
/// selecting Transmission means announce URLs have to point at sharerr's own
/// tracker. `doctor` says so rather than leaving it to be discovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TorrentBackend {
    #[default]
    Qbittorrent,
    Transmission,
}

impl TorrentBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Qbittorrent => "qbittorrent",
            Self::Transmission => "transmission",
        }
    }
}

/// How to reach Transmission. The password is in the vault, not here.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TransmissionConfig {
    pub url: Url,
    pub username: String,
    /// Transmission has no categories, only a flat list of labels. This one stands
    /// in for qBittorrent's category *and* its tag, because there is nothing else
    /// to distinguish them with.
    pub label: String,
}

impl Default for TransmissionConfig {
    // A compile-time literal; parsing cannot fail at runtime and `Url` has no const
    // constructor to say so in the type system.
    #[allow(clippy::expect_used)]
    fn default() -> Self {
        Self {
            url: Url::parse("http://localhost:9091").expect("valid literal url"),
            username: "transmission".to_owned(),
            label: "sharerr".to_owned(),
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

impl TrackerBackend {
    /// Both backends, in the order the UI offers them.
    pub const ALL: &'static [Self] = &[Self::QbittorrentEmbedded, Self::Builtin];

    /// The kebab-case name, matching both the serde form and `sharerr.toml`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::QbittorrentEmbedded => "qbittorrent-embedded",
            Self::Builtin => "builtin",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
/// Which tracker announce URLs point at, and the address peers should use.
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
/// Whether and how often `serve` reconciles in the background.
pub struct SyncConfig {
    /// Run the reconciliation loop on a timer while `serve` is running.
    pub enabled: bool,
    pub interval_secs: u64,
}

impl SyncConfig {
    /// The shortest interval the loop will run at. One constant, because the
    /// settings form rejects values below it and the loop clamps to it — two
    /// hand-typed 60s would drift into a form that stores what the loop ignores.
    pub const MIN_INTERVAL_SECS: u64 = 60;

    /// The configured interval with the floor applied.
    pub fn effective_interval_secs(&self) -> u64 {
        self.interval_secs.max(Self::MIN_INTERVAL_SECS)
    }
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_secs: 900,
        }
    }
}

/// What a `[[library]]` directory holds, declared by the operator.
///
/// Declared rather than sniffed: SxxEyy makes television detectable, but music
/// and books are not, and a misclassified file lands in a Torznab category no
/// friend's app will search. The kind decides the [`crate::MediaSpec`] variant
/// each file becomes, and with it the feed category and the peer-scope bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LibraryKind {
    Tv,
    Movie,
    Music,
    Book,
}

impl LibraryKind {
    /// Every kind, in the order the UI offers them.
    pub const ALL: &'static [Self] = &[Self::Tv, Self::Movie, Self::Music, Self::Book];

    /// The lowercase name, matching both the serde form and `sharerr.toml`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tv => "tv",
            Self::Movie => "movie",
            Self::Music => "music",
            Self::Book => "book",
        }
    }

    /// Inverse of [`Self::as_str`], derived from it so the two cannot drift.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|k| k.as_str() == value)
    }
}

/// One plain directory shared without an *arr app.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LibraryConfig {
    /// The directory as the sharerr process sees it. Scanned recursively.
    pub path: PathBuf,
    /// What every media file under `path` is treated as.
    pub kind: LibraryKind,
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
        // Every *arr app is `None` by default and would serialise to null, so
        // populate them all — the paths under them are the point, and a newly added
        // app that nobody populated here would be silently unverified.
        let service = |url: &str| {
            Some(ServiceConfig {
                url: Url::parse(url).unwrap(),
            })
        };
        let config = Config {
            sonarr: service("http://sonarr:8989"),
            radarr: service("http://radarr:7878"),
            lidarr: service("http://lidarr:8686"),
            readarr: service("http://readarr:8787"),
            whisparr: service("http://whisparr:6969"),
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

    /// `library` entries round-trip through serde, and a kind outside the four
    /// known ones is a startup error rather than a silently empty library.
    #[test]
    fn library_sections_round_trip_and_reject_unknown_kinds() {
        let config: Config = serde_json::from_value(serde_json::json!({
            "library": [
                { "path": "/media/extras", "kind": "movie" },
                { "path": "/media/tapes", "kind": "tv" },
            ],
        }))
        .unwrap();
        assert_eq!(
            config.library,
            vec![
                LibraryConfig {
                    path: PathBuf::from("/media/extras"),
                    kind: LibraryKind::Movie,
                },
                LibraryConfig {
                    path: PathBuf::from("/media/tapes"),
                    kind: LibraryKind::Tv,
                },
            ]
        );

        let err = serde_json::from_value::<Config>(serde_json::json!({
            "library": [{ "path": "/media/x", "kind": "anime" }],
        }));
        assert!(err.is_err(), "an unknown kind must fail to parse");

        for kind in LibraryKind::ALL {
            assert_eq!(LibraryKind::parse(kind.as_str()), Some(*kind));
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
