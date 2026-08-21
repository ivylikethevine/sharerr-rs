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
    /// The sole qBittorrent credential: a WebUI API key (5.2+), sent as a bearer
    /// token. qBittorrent has no username/password support here — nothing has
    /// shipped against an older build, so there is no legacy setup to preserve.
    pub const QBITTORRENT_API_KEY: &str = "qbittorrent.api_key";
    /// The Transmission RPC password, when Transmission is the selected backend.
    pub const TRANSMISSION_PASSWORD: &str = "transmission.password";
    /// rTorrent's Basic Auth password, when rTorrent is the selected backend.
    /// rTorrent's own XML-RPC has no credential of its own; this authenticates
    /// against whatever reverse proxy fronts it — see `sharerr_rtorrent`.
    pub const RTORRENT_PASSWORD: &str = "rtorrent.password";
    /// Shared secret embedded in builtin-tracker announce URLs.
    pub const TRACKER_TOKEN: &str = "tracker.token";
    /// gluetun's control server API key, sent as `X-Api-Key`. Required since
    /// gluetun v3.40 made `apikey` the default auth type for the control
    /// server; without it every request comes back `401`.
    pub const GLUETUN_API_KEY: &str = "gluetun.api_key";
    /// The API key for the *second* gluetun poller — the torrent client's own
    /// tunnel, when it is a separate one from the tracker's. See
    /// [`super::GluetunConfig`] and `[gluetun_client]`.
    pub const GLUETUN_CLIENT_API_KEY: &str = "gluetun_client.api_key";

    /// Where a sync-failure or peer-quiet notification is POSTed.
    ///
    /// In the vault, not `[notifications]` in `sharerr.toml`, even though it is
    /// not credential-shaped at a glance: a Discord webhook URL embeds its own
    /// bearer token in the path, so this is exactly the kind of value this
    /// project treats as a secret everywhere else.
    pub const NOTIFICATIONS_WEBHOOK_URL: &str = "notifications.webhook_url";

    /// This instance's Ed25519 signing key for gossip records, hex-encoded.
    ///
    /// Deliberately **not** in [`ALL`]: that list is what the web UI offers as
    /// editable fields, and a signing key is not a credential an operator types —
    /// it is generated on first use, and "editing" it would silently break every
    /// friendship whose peers pinned the old public key. Rotation, when it is
    /// ever needed, deserves an explicit re-pair flow rather than a text box.
    pub const IDENTITY_SIGNING_KEY: &str = "identity.signing_key";

    /// The seed the embedded lighthouse derives its fabricated decoy answers
    /// from, hex-encoded.
    ///
    /// Same reasoning as [`IDENTITY_SIGNING_KEY`], deliberately **not** in
    /// [`ALL`]: generated on first use by [`super::LighthouseConfig`]'s
    /// embedding path, not typed by an operator. Only present when
    /// `[lighthouse] enabled = true` has actually been used at least once.
    pub const LIGHTHOUSE_DECOY_SEED: &str = "lighthouse.decoy_seed";

    /// The vault key holding the API key a friend issued *us*, for pulling
    /// gossip from their sharerr. Per-peer and minted by them, so it cannot be a
    /// constant; also not in [`ALL`] — it is managed from the Friends page,
    /// beside the peer it belongs to, not from the settings page.
    pub fn peer_gossip_key(peer_id: i64) -> String {
        format!("peer.gossip.{peer_id}")
    }

    /// Every key sharerr actually reads.
    ///
    /// One list: both consumers ask the same question — the CLI warns when
    /// `vault set` is given something outside it, and the web UI offers exactly
    /// these as editable fields. A fifth secret added to only one of those
    /// copies is a field the UI silently will not manage — so the list lives
    /// beside the constants that define it.
    pub const ALL: &[&str] = &[
        SONARR_API_KEY,
        RADARR_API_KEY,
        LIDARR_API_KEY,
        READARR_API_KEY,
        WHISPARR_API_KEY,
        QBITTORRENT_API_KEY,
        TRANSMISSION_PASSWORD,
        RTORRENT_PASSWORD,
        TRACKER_TOKEN,
        GLUETUN_API_KEY,
        GLUETUN_CLIENT_API_KEY,
        NOTIFICATIONS_WEBHOOK_URL,
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
    pub const QBITTORRENT_CATEGORY: &str = "qbittorrent.category";
    pub const QBITTORRENT_TAG: &str = "qbittorrent.tag";
    pub const QBITTORRENT_SKIP_CHECKING: &str = "qbittorrent.skip_checking";

    pub const TORRENT_BACKEND: &str = "torrent_backend";
    pub const TRANSMISSION_URL: &str = "transmission.url";
    pub const TRANSMISSION_USERNAME: &str = "transmission.username";
    pub const TRANSMISSION_LABEL: &str = "transmission.label";
    pub const RTORRENT_URL: &str = "rtorrent.url";
    pub const RTORRENT_USERNAME: &str = "rtorrent.username";
    pub const RTORRENT_LABEL: &str = "rtorrent.label";

    pub const TRACKER_ADVERTISED_HOST: &str = "tracker.advertised_host";
    pub const TRACKER_ADVERTISED_URL: &str = "tracker.advertised_url";
    pub const TRACKER_PORT: &str = "tracker.port";

    /// Per-torrent upload cap in KiB/s — see [`super::SeedingConfig::upload_limit_kib`].
    pub const SEEDING_UPLOAD_LIMIT_KIB: &str = "seeding.upload_limit_kib";
    /// Seed-ratio goal — see [`super::SeedingConfig::ratio_limit`].
    pub const SEEDING_RATIO_LIMIT: &str = "seeding.ratio_limit";

    /// Whether the lighthouse rendezvous service runs as extra routes on one
    /// of this instance's own listeners — see [`super::LighthouseConfig`].
    pub const LIGHTHOUSE_ENABLED: &str = "lighthouse.enabled";
    /// Which listener: `"frontend"` or `"tracker"` — see
    /// [`super::LighthouseMount`].
    pub const LIGHTHOUSE_MOUNT: &str = "lighthouse.mount";
    /// Lighthouse(s) this instance reports its own endpoint to and queries
    /// for a quiet friend — independent of whether it also hosts one via
    /// `LIGHTHOUSE_ENABLED`. See [`super::LighthouseConfig::urls`].
    pub const LIGHTHOUSE_URLS: &str = "lighthouse.urls";

    pub const GLUETUN_ENABLED: &str = "gluetun.enabled";
    pub const GLUETUN_CONTROL_URL: &str = "gluetun.control_url";
    pub const GLUETUN_POLL_SECS: &str = "gluetun.poll_secs";

    /// The second gluetun poller, for the torrent client's own tunnel — see
    /// `docker/deploy/dual-vpn/`. Independent of the tracker-facing `[gluetun]`
    /// above: separate control server, separate enabled flag, separate poll
    /// interval, because the two tunnels rotate on their own schedules.
    pub const GLUETUN_CLIENT_ENABLED: &str = "gluetun_client.enabled";
    pub const GLUETUN_CLIENT_CONTROL_URL: &str = "gluetun_client.control_url";
    pub const GLUETUN_CLIENT_POLL_SECS: &str = "gluetun_client.poll_secs";

    pub const SYNC_ENABLED: &str = "sync.enabled";
    pub const SYNC_INTERVAL_SECS: &str = "sync.interval_secs";

    /// Which webhook shape to send — see [`super::NotifyKind`]. The URL itself
    /// is a vault secret, [`super::secret_keys::NOTIFICATIONS_WEBHOOK_URL`].
    pub const NOTIFICATIONS_KIND: &str = "notifications.kind";
    /// How long a peer must go unseen before "gone quiet" fires, in seconds.
    pub const NOTIFICATIONS_PEER_QUIET_SECS: &str = "notifications.peer_quiet_secs";

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
        QBITTORRENT_CATEGORY,
        QBITTORRENT_TAG,
        QBITTORRENT_SKIP_CHECKING,
        TORRENT_BACKEND,
        TRANSMISSION_URL,
        TRANSMISSION_USERNAME,
        TRANSMISSION_LABEL,
        RTORRENT_URL,
        RTORRENT_USERNAME,
        RTORRENT_LABEL,
        TRACKER_ADVERTISED_HOST,
        TRACKER_ADVERTISED_URL,
        TRACKER_PORT,
        SEEDING_UPLOAD_LIMIT_KIB,
        SEEDING_RATIO_LIMIT,
        LIGHTHOUSE_ENABLED,
        LIGHTHOUSE_MOUNT,
        LIGHTHOUSE_URLS,
        GLUETUN_ENABLED,
        GLUETUN_CONTROL_URL,
        GLUETUN_POLL_SECS,
        GLUETUN_CLIENT_ENABLED,
        GLUETUN_CLIENT_CONTROL_URL,
        GLUETUN_CLIENT_POLL_SECS,
        SYNC_ENABLED,
        SYNC_INTERVAL_SECS,
        NOTIFICATIONS_KIND,
        NOTIFICATIONS_PEER_QUIET_SECS,
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
    /// Only read when `torrent_backend` selects it.
    pub rtorrent: RtorrentConfig,
    pub tracker: TrackerConfig,
    /// A per-torrent upload/ratio goal, applied once at add time — see
    /// [`SeedingConfig`]. Unset by default: no cap, no goal, matching
    /// today's behaviour exactly until an operator opts in.
    pub seeding: SeedingConfig,
    /// Embedding the lighthouse rendezvous service on one of this instance's
    /// own listeners. Off by default — see [`LighthouseConfig`].
    pub lighthouse: LighthouseConfig,
    /// Resolving the advertised endpoint from a gluetun VPN container's control
    /// server, for deployments with no stable public IP or forwarded port.
    pub gluetun: GluetunConfig,
    /// A second, independent gluetun poller for the torrent client's own
    /// tunnel, when it is not the same one the tracker uses — see
    /// `docker/deploy/dual-vpn/`. Disabled (`control_url: None`) by default:
    /// the ordinary single-tunnel deployment has nothing to point this at, and
    /// `gluetun` above already covers it.
    pub gluetun_client: GluetunConfig,
    pub sync: SyncConfig,
    /// A webhook fired on sync failure or a peer going quiet. The URL itself is
    /// a vault secret — see [`secret_keys::NOTIFICATIONS_WEBHOOK_URL`].
    pub notifications: NotificationsConfig,
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
            rtorrent: RtorrentConfig::default(),
            tracker: TrackerConfig::default(),
            seeding: SeedingConfig::default(),
            lighthouse: LighthouseConfig::default(),
            gluetun: GluetunConfig::default(),
            gluetun_client: GluetunConfig::default(),
            sync: SyncConfig::default(),
            notifications: NotificationsConfig::default(),
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

    /// The HTTP base URL a friend reaches this instance on, rendered for display
    /// and for building feed links, without a trailing slash.
    ///
    /// One resolver decides this — [`crate::endpoint::advertised_base`] — and the
    /// tracker's announce URLs come from the same place, so the two can no longer
    /// drift. An unconfigured or unusable address falls back to `localhost` so
    /// the settings page still has something to show.
    pub fn public_base_url(&self) -> String {
        match crate::endpoint::advertised_base(&self.tracker, self.server.bind.port()) {
            Ok(Some(base)) => crate::endpoint::base_string(&base),
            Ok(None) | Err(_) => {
                format!("http://localhost:{}", self.server.bind.port())
            }
        }
    }

    /// The selected torrent client's connection and labelling settings.
    ///
    /// One accessor instead of a `match self.torrent_backend` at every call site:
    /// the section names in `sharerr.toml` differ per client, so the URL, username
    /// and vault key must be resolved together — plucking fields from the unused
    /// section is how an operator ends up debugging the wrong service.
    pub fn torrent_client(&self) -> TorrentClientConfig<'_> {
        self.torrent_client_for(self.torrent_backend)
    }

    /// The same resolution as [`Self::torrent_client`], for a specific backend
    /// rather than whichever one `torrent_backend` currently selects.
    ///
    /// What the settings page's "Test connection" button needs: an operator
    /// filling in Transmission's fields while qBittorrent is still the active
    /// backend must be able to test *those* credentials, not have the button
    /// silently test qBittorrent instead because that is what is configured to
    /// seed right now.
    pub fn torrent_client_for(&self, backend: TorrentBackend) -> TorrentClientConfig<'_> {
        match backend {
            TorrentBackend::Qbittorrent => TorrentClientConfig {
                url: &self.qbittorrent.url,
                // qBittorrent authenticates by API key alone — see
                // `secret_keys::QBITTORRENT_API_KEY`. There is no username/password
                // fallback to resolve here.
                username: None,
                password_key: None,
                api_key_key: Some(secret_keys::QBITTORRENT_API_KEY),
                category: &self.qbittorrent.category,
                tag: &self.qbittorrent.tag,
                skip_checking: self.qbittorrent.skip_checking,
                upload_limit_kib: self.seeding.upload_limit_kib,
                ratio_limit: self.seeding.ratio_limit,
            },
            TorrentBackend::Transmission => TorrentClientConfig {
                url: &self.transmission.url,
                username: Some(&self.transmission.username),
                password_key: Some(secret_keys::TRANSMISSION_PASSWORD),
                // Transmission's RPC has no key auth — only a username and
                // password — so there is nothing for a caller to prefer.
                api_key_key: None,
                // Transmission has only labels, so the category and tag collapse
                // into one value, and there is no skip-check switch to honour.
                category: &self.transmission.label,
                tag: &self.transmission.label,
                skip_checking: false,
                // Seeding goals are backend-agnostic — see `SeedingConfig` — so
                // the same values apply regardless of which client is selected.
                upload_limit_kib: self.seeding.upload_limit_kib,
                ratio_limit: self.seeding.ratio_limit,
            },
            TorrentBackend::Rtorrent => TorrentClientConfig {
                url: &self.rtorrent.url,
                username: Some(&self.rtorrent.username),
                password_key: Some(secret_keys::RTORRENT_PASSWORD),
                // rTorrent's XML-RPC has no key auth of its own — see
                // `sharerr_rtorrent`'s module docs.
                api_key_key: None,
                // Same collapse as Transmission: one free-text slot
                // (`d.custom1`) stands in for both category and tag.
                category: &self.rtorrent.label,
                tag: &self.rtorrent.label,
                // rTorrent always verifies a torrent's data on start; there is
                // no documented way to skip it.
                skip_checking: false,
                upload_limit_kib: self.seeding.upload_limit_kib,
                ratio_limit: self.seeding.ratio_limit,
            },
        }
    }
}

/// What [`Config::torrent_client`] resolves: everything about whichever torrent
/// client is configured, independent of which one that is.
#[derive(Debug, Clone, Copy)]
pub struct TorrentClientConfig<'a> {
    pub url: &'a Url,
    /// `None` for a client with no username/password credential — qBittorrent,
    /// which authenticates by API key alone.
    pub username: Option<&'a str>,
    /// Vault key holding this client's password, or `None` for a client with no
    /// password credential.
    pub password_key: Option<&'static str>,
    /// Vault key holding this client's API key, for clients that have one.
    ///
    /// `Some` does not mean a key is stored — only that this client can use one.
    /// When a value is present under it, it takes precedence over the password.
    pub api_key_key: Option<&'static str>,
    /// The grouping applied to torrents sharerr creates: qBittorrent's category,
    /// or Transmission's label.
    pub category: &'a str,
    /// qBittorrent's tag; for Transmission this repeats the label.
    pub tag: &'a str,
    /// Whether to skip hash-checking on add. Always `false` for Transmission,
    /// which has no such switch.
    pub skip_checking: bool,
    /// Per-torrent upload cap in KiB/s, applied at add time — see
    /// [`SeedingConfig::upload_limit_kib`].
    pub upload_limit_kib: Option<u64>,
    /// Seed-ratio goal, applied at add time — see
    /// [`SeedingConfig::ratio_limit`].
    pub ratio_limit: Option<f64>,
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
/// How to reach qBittorrent, and what to label what sharerr puts there. The API
/// key is in the vault, not here — see `secret_keys::QBITTORRENT_API_KEY`.
pub struct QbitConfig {
    pub url: Url,
    /// Category applied to torrents sharerr creates, so they are easy to find.
    pub category: String,
    /// Tag applied alongside the category.
    pub tag: String,
    /// Skip qBittorrent's hash check when adding a torrent.
    ///
    /// Default `true`: faster on large libraries, since sharerr never moves,
    /// renames, or re-links media and expects `qbittorrent.url`/path mapping to
    /// already be correct by the time a sync adds anything. Set this `false` to
    /// have qBittorrent verify the existing file on every add instead — the
    /// safer choice while path mappings are still being worked out, since a wrong
    /// mapping otherwise seeds mismatched data instead of being caught.
    pub skip_checking: bool,
}

impl Default for QbitConfig {
    // The URL is a compile-time literal; parsing it cannot fail at runtime and
    // `Url` has no const constructor to express that in the type system.
    #[allow(clippy::expect_used)]
    fn default() -> Self {
        Self {
            url: Url::parse("http://localhost:8080").expect("valid literal url"),
            category: "sharerr".to_owned(),
            tag: "sharerr".to_owned(),
            skip_checking: true,
        }
    }
}

/// Which torrent client sharerr drives.
///
/// qBittorrent is the default because every existing config expects it. The choice
/// is purely which HTTP API is spoken: sharerr's builtin tracker answers announces
/// whichever client seeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TorrentBackend {
    #[default]
    Qbittorrent,
    Transmission,
    Rtorrent,
}

impl TorrentBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Qbittorrent => "qbittorrent",
            Self::Transmission => "transmission",
            Self::Rtorrent => "rtorrent",
        }
    }

    /// The name as an operator would write it. Unlike `as_str`, capitalizes
    /// mid-word where the brand does — `title_case`-ing `as_str` would give
    /// "Qbittorrent", not "qBittorrent".
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Qbittorrent => "qBittorrent",
            Self::Transmission => "Transmission",
            Self::Rtorrent => "rTorrent",
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

/// How to reach rTorrent, and what to label what sharerr puts there. The
/// password is in the vault, not here — see [`secret_keys::RTORRENT_PASSWORD`].
///
/// Unlike [`QbitConfig`] and [`TransmissionConfig`], `url` is the exact
/// XML-RPC endpoint rather than a base a fixed path is appended to — see
/// `sharerr_rtorrent`'s module docs for why rTorrent has no one standard path
/// to assume.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RtorrentConfig {
    pub url: Url,
    pub username: String,
    /// rTorrent has no categories, only a free-text `d.custom1` slot per
    /// download. This one value stands in for qBittorrent's category *and*
    /// its tag, same as [`TransmissionConfig::label`].
    pub label: String,
}

impl Default for RtorrentConfig {
    // A compile-time literal; parsing cannot fail at runtime and `Url` has no const
    // constructor to say so in the type system.
    #[allow(clippy::expect_used)]
    fn default() -> Self {
        Self {
            url: Url::parse("http://localhost/RPC2").expect("valid literal url"),
            username: "rtorrent".to_owned(),
            label: "sharerr".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
/// The address peers reach sharerr's tracker on.
///
/// The tracker itself is always sharerr's own, served by this process — one
/// backend, not two independently built announce URLs to keep in sync on every
/// dynamic-endpoint change. A config that still names a `backend` is rejected
/// with the fix — see [`removed_tracker_backend`].
pub struct TrackerConfig {
    /// **Removed.** Present only so a `sharerr.toml` still setting it fails to
    /// load with an error naming this exact change, rather than a generic
    /// "unknown field". Never holds a value.
    #[serde(skip_serializing, deserialize_with = "removed_tracker_backend")]
    pub backend: (),
    /// Hostname or IP friends will reach the tracker on. Required (unless
    /// `advertised_url` is set) — sharerr cannot guess its own externally
    /// reachable address, and a wrong guess silently produces torrents nobody can
    /// announce to.
    pub advertised_host: Option<String>,
    /// Override the announce port, for the common case where a published docker
    /// port differs from the internal one. Defaults to [`ServerConfig::bind`]'s
    /// port.
    pub port: Option<u16>,
    /// The expressive form of the advertised address: a full base URL carrying
    /// scheme, port, and any reverse-proxy path prefix — `https`, `/sharerr`,
    /// a bracketed IPv6 literal. Wins over `advertised_host` and `port` when
    /// set. See [`crate::endpoint::advertised_base`].
    pub advertised_url: Option<Url>,
    /// An extra listener carrying only the tracker (and the `.torrent`
    /// downloads), for the deployment where exactly one port is forwarded —
    /// gluetun grants one, and the port that has to be reachable is the
    /// tracker's, not the web UI's. The default single-listener layout stays the
    /// default; `serve` merges everything onto [`ServerConfig::bind`] either
    /// way, and this adds a second listener rather than moving anything.
    pub bind: Option<SocketAddr>,
}

/// Which of sharerr's own listeners carries the embedded lighthouse, when
/// [`LighthouseConfig::enabled`] is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LighthouseMount {
    /// `server.bind` — the same port as the web UI and the Torznab feed.
    #[default]
    Frontend,
    /// `tracker.bind` when it is set, otherwise `server.bind` — the port a
    /// friend's torrent client already reaches for announces.
    Tracker,
}

impl LighthouseMount {
    pub const ALL: &'static [Self] = &[Self::Frontend, Self::Tracker];

    /// The value stored in `lighthouse.mount` and rendered by the settings
    /// page's `<select>` — the same spelling serde's `rename_all = "snake_case"`
    /// already produces, named explicitly so the web UI does not have to
    /// round-trip through serde to render a `<select>` option.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Frontend => "frontend",
            Self::Tracker => "tracker",
        }
    }

    /// Inverse of [`Self::as_str`], derived from it so the two cannot drift.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|m| m.as_str() == value)
    }
}

/// Running the lighthouse rendezvous service ([`sharerr_lighthouse`] in the
/// workspace) as extra routes on one of sharerr's own listeners, instead of
/// its own separate image and port.
///
/// The design brief in `docs/ROADMAP.md` wants the lighthouse to be a
/// deliberately separate deployment — no shared process, no shared port — so
/// that it can be self-hosted by anyone on neutral ground away from any
/// particular library. This is the exception to that: a single operator
/// running the lighthouse for their own circle of friends, who would rather
/// not run a second container for it. Off by default; enabling it changes
/// nothing about the standalone binary, which keeps working the same way for
/// anyone who wants the separation.
/// A seed-ratio/bandwidth goal applied once, at the moment sharerr hands a
/// torrent to the client — never enforced by sharerr itself afterward. Both
/// fields are backend-agnostic: qBittorrent and Transmission each honour
/// them through their own already-running seeding engine, via whatever
/// native mechanism that client offers (see [`Config::torrent_client`]'s
/// callers, `sharerr-qbit` and `sharerr-transmission`). `None` in either
/// field leaves that client's own default — uncapped, or whatever its own
/// global setting already does — untouched.
///
/// Deliberately no time-based goal: qBittorrent's equivalent is total time
/// seeded since completion, but Transmission's only related knob is *idle*
/// time with no upload/download activity — a materially different
/// condition. A single field that means two different things depending on
/// which client is configured would be a footgun, so it is left out rather
/// than faked.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SeedingConfig {
    /// Per-torrent upload cap in KiB/s, applied at add time.
    pub upload_limit_kib: Option<u64>,
    /// Stop seeding once this ratio is reached, applied at add time.
    pub ratio_limit: Option<f64>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LighthouseConfig {
    pub enabled: bool,
    pub mount: LighthouseMount,
    /// Lighthouse(s) this instance uses as a *client*: reports its own
    /// endpoint to each, and queries each for a friend gossip cannot
    /// currently reach. Independent of `enabled` — that only controls
    /// whether this instance also *hosts* one; an operator can consume a
    /// friend's lighthouse without running one, or run one without using it
    /// themselves. Empty means this half of the feature is off.
    pub urls: Vec<Url>,
}

/// Reject any `tracker.backend` value with the migration story.
///
/// The builtin tracker is the only tracker, so `backend` is no longer a field.
/// `deny_unknown_fields` alone would say "unknown field `backend`", which reads
/// like a typo; an operator whose working config just stopped loading deserves the
/// sentence that says what changed and what to do.
fn removed_tracker_backend<'de, D>(deserializer: D) -> Result<(), D::Error>
where
    D: serde::Deserializer<'de>,
{
    // Read the old value so the error can quote it; a non-string value still
    // produces the same rejection.
    let was = Option::<String>::deserialize(deserializer)
        .ok()
        .flatten()
        .map(|value| format!(" (was {value:?})"))
        .unwrap_or_default();
    Err(serde::de::Error::custom(format!(
        "tracker.backend has been removed{was}: sharerr's builtin tracker is now the \
         only tracker, and qBittorrent's embedded tracker is no longer used. Delete \
         the `backend` line from [tracker] and re-run sync so torrents announce to \
         sharerr"
    )))
}

/// Resolving the advertised endpoint from gluetun's control server.
///
/// A VPN provider that forwards a port grants a *different* port on every
/// reconnect, on an exit address that also rotates — so the deployment this is
/// for cannot type its endpoint into `tracker.advertised_host` at all. When
/// `control_url` is set, `serve` polls `/v1/publicip/ip` and
/// `/v1/openvpn/portforwarded` as the source of truth, and two small endpoints
/// accept a nudge from `VPN_PORT_FORWARDING_UP_COMMAND` and
/// `VPN_PORT_FORWARDING_DOWN_COMMAND` so a reconnect (or a port going away) is
/// reacted to in seconds rather than at the next poll. The poll is the floor
/// that recovers a missed push; neither alone is enough.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct GluetunConfig {
    /// Whether this poller runs at all, independent of whether `control_url`
    /// is set. Split from `control_url` so pausing polling is a checkbox
    /// rather than blanking (and losing) a saved address.
    pub enabled: bool,
    /// gluetun's control server, `http://localhost:8000` in the intended
    /// topology (sharerr inside gluetun's network namespace). `None` disables
    /// endpoint resolution entirely, same as `enabled = false`.
    pub control_url: Option<Url>,
    /// How often to poll the control server, in seconds.
    pub poll_secs: u64,
}

impl GluetunConfig {
    /// The floor for `poll_secs` — polling a loopback HTTP endpoint faster than
    /// this buys nothing, and the push path covers the fast case.
    pub const MIN_POLL_SECS: u64 = 10;

    /// The configured interval with the floor applied.
    pub fn effective_poll_secs(&self) -> u64 {
        self.poll_secs.max(Self::MIN_POLL_SECS)
    }

    /// Whether this poller should actually run: turned on, and pointed
    /// somewhere.
    pub fn is_active(&self) -> bool {
        self.enabled && self.control_url.is_some()
    }
}

impl Default for GluetunConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            control_url: None,
            poll_secs: 60,
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

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
/// A webhook fired on sync failure or a peer going quiet — standard for the
/// *arr ecosystem this lives in. The URL itself is not here: see
/// [`secret_keys::NOTIFICATIONS_WEBHOOK_URL`] for why it is a vault secret
/// rather than a plain config field. Whether notifications are active at all
/// is therefore decided by whether that secret is set, the same way gluetun
/// polling is decided by whether `control_url` is set.
pub struct NotificationsConfig {
    /// Which webhook shape to send.
    pub kind: NotifyKind,
    /// How long a peer must go unseen before "gone quiet" fires, in seconds.
    /// `0` turns the peer-quiet check off without touching sync-failure
    /// notifications, which are unconditional once a webhook is configured.
    pub peer_quiet_secs: u64,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            kind: NotifyKind::default(),
            // A week. Long enough that a friend's ordinary quiet week (they are
            // travelling, their box is off) is not a false alarm; short enough
            // that "did Sam's instance die" is answered before it has been true
            // for a month.
            peer_quiet_secs: 7 * 24 * 3600,
        }
    }
}

/// The payload shape a notification is sent in — one per service this project
/// names explicitly, because each expects a different JSON body at the same
/// "POST a webhook URL" mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum NotifyKind {
    /// `{"event": ..., "message": ...}` — for anything that takes a plain
    /// JSON webhook, including a custom receiver.
    #[default]
    Generic,
    /// `{"content": ...}` — a Discord webhook URL.
    Discord,
    /// `{"title": ..., "body": ...}` — an Apprise API server's `/notify`
    /// endpoint, which fans a single call out to whatever Apprise itself is
    /// configured to reach.
    Apprise,
}

impl NotifyKind {
    pub const ALL: &'static [Self] = &[Self::Generic, Self::Discord, Self::Apprise];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Generic => "generic",
            Self::Discord => "discord",
            Self::Apprise => "apprise",
        }
    }

    /// Inverse of [`Self::as_str`], derived from it so the two cannot drift.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|k| k.as_str() == value)
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
    /// The same dotted strings are typed by hand in the settings handlers and
    /// again in `settings.html`, and generated a third time from `SHARERR_*`
    /// environment variables — a typo in any one compiles cleanly and simply
    /// stops matching, leaving a field that looks editable while the environment
    /// has it pinned.
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

    /// `torrent_client_for` must resolve each backend's own settings
    /// regardless of which one `torrent_backend` currently selects — the
    /// settings page's "Test connection" button for the *other* client
    /// depends on this: an operator filling in Transmission's fields while
    /// qBittorrent is still active must be able to test what they just typed.
    #[test]
    fn torrent_client_for_ignores_the_currently_selected_backend() {
        let config = Config {
            torrent_backend: TorrentBackend::Qbittorrent,
            qbittorrent: QbitConfig {
                url: Url::parse("http://qbit.example:8080").unwrap(),
                ..QbitConfig::default()
            },
            transmission: TransmissionConfig {
                url: Url::parse("http://trans.example:9091").unwrap(),
                username: "sam".to_owned(),
                ..TransmissionConfig::default()
            },
            rtorrent: RtorrentConfig {
                url: Url::parse("http://seedbox.example/RPC2").unwrap(),
                username: "alex".to_owned(),
                ..RtorrentConfig::default()
            },
            ..Config::default()
        };

        let qbit = config.torrent_client_for(TorrentBackend::Qbittorrent);
        assert_eq!(qbit.url.as_str(), "http://qbit.example:8080/");
        assert_eq!(qbit.api_key_key, Some(secret_keys::QBITTORRENT_API_KEY));

        let transmission = config.torrent_client_for(TorrentBackend::Transmission);
        assert_eq!(transmission.url.as_str(), "http://trans.example:9091/");
        assert_eq!(transmission.username, Some("sam"));
        assert_eq!(
            transmission.password_key,
            Some(secret_keys::TRANSMISSION_PASSWORD)
        );

        let rtorrent = config.torrent_client_for(TorrentBackend::Rtorrent);
        // No trailing slash appended, unlike qBittorrent/Transmission above —
        // this is the exact RPC endpoint, not a base to join a path onto.
        assert_eq!(rtorrent.url.as_str(), "http://seedbox.example/RPC2");
        assert_eq!(rtorrent.username, Some("alex"));
        assert_eq!(rtorrent.api_key_key, None);
        assert_eq!(rtorrent.password_key, Some(secret_keys::RTORRENT_PASSWORD));
        assert!(
            !rtorrent.skip_checking,
            "rTorrent cannot skip its hash check"
        );

        // `torrent_client()` is the same resolution, applied to whichever
        // backend is actually selected.
        assert_eq!(config.torrent_client().url, qbit.url);
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
