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

/// The repository itself — the footer's "Source" link.
pub const REPO: &str = "https://github.com/ivylikethevine/sharerr-rs";

// ---- This project's own documentation ---------------------------------

pub const README: &str = "https://github.com/ivylikethevine/sharerr-rs#readme";

/// One constant per link, and the list the test below walks, paired with each
/// constant's name so a failure says which one to fix. The URLs stay whole
/// literals so the link checker's sweep of this file can read them.
macro_rules! internal_docs {
    ($($name:ident = $url:literal;)*) => {
        $(pub const $name: &str = $url;)*

        #[cfg(test)]
        const INTERNAL_LINKS: &[(&str, &str)] = &[$((stringify!($name), $name)),*];
    };
}

internal_docs! {
    CONFIGURATION = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md";
    CONFIG_LAYERING = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#how-configuration-is-layered";
    CONFIG_TOP_LEVEL = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#top-level-settings";
    CONFIG_ARR = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#arr-apps";
    CONFIG_LIBRARY = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#library--plain-directories";
    CONFIG_TORRENT_CLIENT = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#torrent-client";
    CONFIG_QBITTORRENT = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#qbittorrent";
    CONFIG_TRANSMISSION = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#transmission";
    CONFIG_RTORRENT = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#rtorrent";
    CONFIG_DELUGE = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#deluge";
    CONFIG_SEEDING = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#seeding";
    CONFIG_FEED = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#feed";
    CONFIG_TRACKER = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#tracker";
    CONFIG_PATH_MAP = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#path_map";
    CONFIG_LIGHTHOUSE = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#lighthouse";
    CONFIG_GLUETUN = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#gluetun-and-gluetun_client";
    CONFIG_SYNC = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#sync";
    CONFIG_CHECKS = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#checks";
    CONFIG_NOTIFICATIONS = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#notifications";
    CONFIG_METRICS = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#metrics";
    CONFIG_API = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#api";
    CONFIG_VAULT = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#vault-secrets";
    CONFIG_ENV = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SETTINGS.md#environment-variable-overrides";

    SUPPORTED = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/COMPATIBILITY.md#supported-services";
    SUPPORTED_SOURCES = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/COMPATIBILITY.md#library-sources-where-tagged-content-comes-from";
    SUPPORTED_CLIENTS = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/COMPATIBILITY.md#torrent-clients-what-actually-seeds";
    SUPPORTED_INDEXERS = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/COMPATIBILITY.md#indexers-what-consumes-the-feed";
    UNSUPPORTED = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/COMPATIBILITY.md#not-supported";

    API = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/API.md";
    SECURITY = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SECURITY.md";
    SECURITY_SCOPE = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/SECURITY.md#what-is-in-scope";
    ROADMAP = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/ROADMAP.md";
    LIGHTHOUSE = "https://github.com/ivylikethevine/sharerr-rs/blob/main/docs/LIGHTHOUSE.md";
}

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
pub const DELUGE_RPC: &str = "https://deluge.readthedocs.io/en/latest/reference/webapi.html";

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

    #[test]
    fn every_internal_link_resolves_to_a_real_file_and_heading() {
        let root = repo_root();
        for (name, url) in super::INTERNAL_LINKS {
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

    #[test]
    fn every_arr_source_has_a_wiki_and_a_plain_directory_has_none() {
        use sharerr_core::MediaSource;
        for source in MediaSource::ARRS {
            assert!(super::for_source(*source).is_some(), "{source:?}");
        }
        assert_eq!(super::for_source(MediaSource::Directory), None);
    }
}
