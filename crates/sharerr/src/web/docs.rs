//! Every documentation URL the web UI links to, in one place.
//!
//! Two kinds live here. **Internal** links point at this project's own `docs/`
//! on GitHub; they are checked by the test at the bottom of this file, which
//! resolves each one to a real file and a real heading anchor in the working
//! tree, so a renamed section fails `cargo test` rather than shipping a link
//! that lands on the top of the page. **External** links point at the upstream
//! documentation for a service sharerr talks to — the *arr wiki, qBittorrent's
//! WebUI API, gluetun's control server — and cannot be checked offline, so they
//! are deliberately pointed at a project's stable landing page rather than at a
//! deep anchor that a docs reshuffle would quietly break.
//!
//! Templates reference these as paths (`{{ crate::web::docs::CONFIGURATION }}`)
//! rather than typing a URL, so the settings page and the status page can never
//! disagree about where the tracker documentation is.

/// The repository itself — the footer's "Source" link and the base every
/// internal link below is built from.
pub const REPO: &str = "https://github.com/ivylikethevine/sharerr-rs";

// ---- This project's own documentation ---------------------------------

pub const README: &str = "https://github.com/ivylikethevine/sharerr-rs#readme";
pub const CONFIGURATION: &str =
    "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md";
pub const CONFIG_LAYERING: &str = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#how-configuration-is-layered";
pub const CONFIG_TOP_LEVEL: &str =
    "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#top-level-settings";
pub const CONFIG_ARR: &str =
    "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#arr-apps";
pub const CONFIG_LIBRARY: &str = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#library--plain-directories";
pub const CONFIG_TORRENT_CLIENT: &str =
    "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#torrent-client";
pub const CONFIG_QBITTORRENT: &str =
    "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#qbittorrent";
pub const CONFIG_TRANSMISSION: &str =
    "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#transmission";
pub const CONFIG_RTORRENT: &str =
    "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#rtorrent";
pub const CONFIG_SEEDING: &str =
    "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#seeding";
pub const CONFIG_FEED: &str =
    "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#feed";
pub const CONFIG_TRACKER: &str =
    "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#tracker";
pub const CONFIG_PATH_MAP: &str =
    "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#path_map";
pub const CONFIG_LIGHTHOUSE: &str =
    "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#lighthouse";
pub const CONFIG_GLUETUN: &str = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#gluetun-and-gluetun_client";
pub const CONFIG_SYNC: &str =
    "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#sync";
pub const CONFIG_CHECKS: &str =
    "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#checks";
pub const CONFIG_NOTIFICATIONS: &str =
    "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#notifications";
pub const CONFIG_METRICS: &str =
    "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#metrics";
pub const CONFIG_VAULT: &str =
    "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#vault-secrets";
pub const CONFIG_ENV: &str = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#environment-variable-overrides";

pub const SUPPORTED: &str =
    "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SUPPORT.md#supported-services";
pub const SUPPORTED_SOURCES: &str = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SUPPORT.md#library-sources-where-tagged-content-comes-from";
pub const SUPPORTED_CLIENTS: &str = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SUPPORT.md#torrent-clients-what-actually-seeds";
pub const SUPPORTED_INDEXERS: &str = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SUPPORT.md#indexers-what-consumes-the-feed";
pub const UNSUPPORTED: &str =
    "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SUPPORT.md#not-supported";

pub const API: &str = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/API.md";
pub const SECURITY: &str =
    "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SECURITY.md";
pub const SECURITY_SCOPE: &str =
    "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SECURITY.md#what-is-in-scope";
pub const ROADMAP: &str =
    "https://github.com/ivylikethevine/sharerr-rs/blob/main/README.md#roadmap";
pub const LIGHTHOUSE: &str =
    "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/LIGHTHOUSE.md";

// ---- Upstream documentation for the services sharerr talks to ---------
//
// Landing pages, not deep anchors: these are other projects' docs, and this
// crate has no way to notice when one of them is reorganised.

pub const SONARR: &str = "https://wiki.servarr.com/sonarr";
pub const RADARR: &str = "https://wiki.servarr.com/radarr";
pub const LIDARR: &str = "https://wiki.servarr.com/lidarr";
pub const READARR: &str = "https://wiki.servarr.com/readarr";
pub const WHISPARR: &str = "https://wiki.servarr.com/whisparr";
/// Where an operator finds the API key to paste into a source's form. The
/// *arr apps share one wiki layout, so Sonarr's page describes all of them.
pub const ARR_API_KEY: &str = "https://wiki.servarr.com/sonarr/settings";
/// Adding the Generic Torznab indexer that consumes a friend's feed.
pub const PROWLARR_INDEXERS: &str = "https://wiki.servarr.com/prowlarr/indexers";

/// The upstream wiki for one library source, for the "documentation" link on
/// its section of the settings page and the wizard. Kept here rather than
/// beside `url_placeholder` in `web::settings` so that every documentation URL
/// in the app is in this one file.
pub fn for_source(source: sharerr_core::MediaSource) -> Option<&'static str> {
    use sharerr_core::MediaSource::{Directory, Lidarr, Radarr, Readarr, Sonarr, Whisparr};
    match source {
        Sonarr => Some(SONARR),
        Radarr => Some(RADARR),
        Lidarr => Some(LIDARR),
        Readarr => Some(READARR),
        Whisparr => Some(WHISPARR),
        // A plain directory is sharerr's own concept, not another project's.
        Directory => None,
    }
}

pub const QBITTORRENT_API: &str =
    "https://github.com/qbittorrent/qBittorrent/wiki/WebUI-API-Documentation";
pub const TRANSMISSION_RPC: &str =
    "https://github.com/transmission/transmission/blob/main/docs/rpc-spec.md";
pub const RTORRENT_RPC: &str = "https://rtorrent-docs.readthedocs.io/en/latest/cmd-ref.html";

pub const GLUETUN_CONTROL_SERVER: &str =
    "https://github.com/qdm12/gluetun-wiki/blob/main/setup/advanced/control-server.md";
pub const GLUETUN_PORT_FORWARDING: &str =
    "https://github.com/qdm12/gluetun-wiki/blob/main/setup/advanced/vpn-port-forwarding.md";

/// The tracker protocol this instance implements for its own swarms.
pub const BITTORRENT_SPEC: &str = "https://www.bittorrent.org/beps/bep_0003.html";
/// The feed format the indexer endpoint speaks.
pub const TORZNAB_SPEC: &str = "https://torznab.github.io/spec-1.3-draft/";

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    const INTERNAL_PREFIX: &str = "https://github.com/ivylikethevine/sharerr-rs/blob/main/";

    fn repo_root() -> PathBuf {
        // `CARGO_MANIFEST_DIR` is `crates/sharerr`.
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("the workspace root is two levels above this crate")
    }

    /// GitHub's heading-anchor rule, near enough for the headings this project
    /// writes: lowercase, drop everything that is not alphanumeric, a space, a
    /// hyphen or an underscore, then turn each remaining space into a hyphen.
    fn slug(heading: &str) -> String {
        heading
            .to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
            .map(|c| if c == ' ' { '-' } else { c })
            .collect()
    }

    fn anchors_in(markdown: &str) -> HashSet<String> {
        markdown
            .lines()
            .filter_map(|line| line.strip_prefix('#'))
            .map(|rest| slug(rest.trim_start_matches('#').trim()))
            .collect()
    }

    /// Every internal link, paired with the constant's name so a failure says
    /// which one to fix rather than only printing a URL.
    fn internal_links() -> Vec<(&'static str, &'static str)> {
        vec![
            ("CONFIGURATION", super::CONFIGURATION),
            ("CONFIG_LAYERING", super::CONFIG_LAYERING),
            ("CONFIG_TOP_LEVEL", super::CONFIG_TOP_LEVEL),
            ("CONFIG_ARR", super::CONFIG_ARR),
            ("CONFIG_LIBRARY", super::CONFIG_LIBRARY),
            ("CONFIG_TORRENT_CLIENT", super::CONFIG_TORRENT_CLIENT),
            ("CONFIG_QBITTORRENT", super::CONFIG_QBITTORRENT),
            ("CONFIG_TRANSMISSION", super::CONFIG_TRANSMISSION),
            ("CONFIG_RTORRENT", super::CONFIG_RTORRENT),
            ("CONFIG_SEEDING", super::CONFIG_SEEDING),
            ("CONFIG_FEED", super::CONFIG_FEED),
            ("CONFIG_TRACKER", super::CONFIG_TRACKER),
            ("CONFIG_PATH_MAP", super::CONFIG_PATH_MAP),
            ("CONFIG_LIGHTHOUSE", super::CONFIG_LIGHTHOUSE),
            ("CONFIG_GLUETUN", super::CONFIG_GLUETUN),
            ("CONFIG_SYNC", super::CONFIG_SYNC),
            ("CONFIG_CHECKS", super::CONFIG_CHECKS),
            ("CONFIG_NOTIFICATIONS", super::CONFIG_NOTIFICATIONS),
            ("CONFIG_METRICS", super::CONFIG_METRICS),
            ("CONFIG_VAULT", super::CONFIG_VAULT),
            ("CONFIG_ENV", super::CONFIG_ENV),
            ("SUPPORTED", super::SUPPORTED),
            ("SUPPORTED_SOURCES", super::SUPPORTED_SOURCES),
            ("SUPPORTED_CLIENTS", super::SUPPORTED_CLIENTS),
            ("SUPPORTED_INDEXERS", super::SUPPORTED_INDEXERS),
            ("UNSUPPORTED", super::UNSUPPORTED),
            ("API", super::API),
            ("SECURITY", super::SECURITY),
            ("SECURITY_SCOPE", super::SECURITY_SCOPE),
            ("ROADMAP", super::ROADMAP),
            ("LIGHTHOUSE", super::LIGHTHOUSE),
        ]
    }

    #[test]
    fn every_internal_link_resolves_to_a_real_file_and_heading() {
        let root = repo_root();
        for (name, url) in internal_links() {
            let tail = url
                .strip_prefix(INTERNAL_PREFIX)
                .unwrap_or_else(|| panic!("{name} is not a repo-relative link: {url}"));
            let (file, anchor) = match tail.split_once('#') {
                Some((file, anchor)) => (file, Some(anchor)),
                None => (tail, None),
            };

            let path = root.join(file);
            assert!(path.is_file(), "{name} points at a missing file: {file}");

            if let Some(anchor) = anchor {
                let body = std::fs::read_to_string(&path).unwrap();
                assert!(
                    anchors_in(&body).contains(anchor),
                    "{name} points at {file}#{anchor}, which has no such heading"
                );
            }
        }
    }

    /// The templates are where these actually get used, and a hand-typed URL
    /// there is exactly what this module exists to prevent — it would not be
    /// covered by the check above.
    #[test]
    fn no_template_hand_types_a_docs_url() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/web/templates");
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|ext| ext != "html") {
                continue;
            }
            let body = std::fs::read_to_string(&path).unwrap();
            assert!(
                !body.contains(INTERNAL_PREFIX),
                "{} hand-types a docs URL; add a constant in web::docs and reference that instead",
                path.display()
            );
        }
    }

    #[test]
    fn slugs_match_githubs_rules_for_the_headings_this_project_writes() {
        assert_eq!(slug("\\*arr apps"), "arr-apps");
        assert_eq!(
            slug("`[[library]]` — plain directories"),
            "library--plain-directories"
        );
        assert_eq!(slug("`[[path_map]]`"), "path_map");
        assert_eq!(
            slug("`[gluetun]` and `[gluetun_client]`"),
            "gluetun-and-gluetun_client"
        );
        assert_eq!(slug("What's left"), "whats-left");
    }
}
