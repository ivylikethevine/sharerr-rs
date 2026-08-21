//! `sharerr preview` — render every authenticated page with invented data.
//!
//! Exists so the settings/status/peers/items pages can be eyeballed in a real
//! browser without standing up an instance: no vault, no database, no
//! `sharerr.toml`, no master key. Every struct below is fully populated by
//! hand with representative — and deliberately varied — data, so a page with
//! several visual states (an empty vs. a stale token, a revoked peer, a
//! failed sync run) shows all of them at once rather than whichever one a
//! real instance happens to be in right now.
//!
//! This is a development aid, not a feature: nothing in the running app links
//! here, and nothing it writes is ever read back by sharerr itself.

use std::collections::BTreeMap;
use std::net::SocketAddr;

use anyhow::{Context, Result};
use askama::Template;
use axum::response::Html;
use axum::routing::get;
use axum::Router;

use crate::web::templates::{
    ArrSection, EndpointStatus, FilterOption, Glance, ItemRow, ItemsPage, LibraryRow, PathRow,
    PeerEndpointView, PeerRow, PeersPage, RevealedPeer, RunRow, SampleRow, ScopeOption,
    SettingsPage, SortLink, StatusPage, TokenStatus,
};

/// Serve the mock pages on `bind` until the process is killed.
///
/// A real HTTP server rather than static files on disk: the templates pull
/// their CSS and htmx from `/assets/*` with absolute paths (see
/// `layout.html`), same as a real instance, so an `.html` file opened
/// directly with `file://` would render unstyled. Reusing
/// `crate::web::asset` for those paths means the mock pages style themselves
/// with the exact bytes a real instance ships, not a second copy that could
/// drift.
pub async fn run(bind: SocketAddr) -> Result<()> {
    let router = Router::new()
        .route("/", get(|| async { Html(page(status_page())) }))
        .route("/settings", get(|| async { Html(page(settings_page())) }))
        .route("/peers", get(|| async { Html(page(peers_page())) }))
        .route("/items", get(|| async { Html(page(items_page())) }))
        .route("/assets/{file}", get(crate::web::asset));

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;

    println!("mock UI serving on http://{bind}/ — /settings, /peers, /items");
    println!(
        "htmx buttons (Test connection, revoke, delete) have nothing live to call \
         and will not do anything; everything else renders exactly as it would \
         on a real instance. Ctrl+C to stop."
    );

    axum::serve(listener, router.into_make_service())
        .await
        .context("preview server failed")
}

/// Render a template, or fail loudly with the Askama error rather than
/// silently serving a blank page — the same trade `crate::web::templates::render`
/// makes, reproduced here rather than reused because that one builds an axum
/// `Response` this handler shape does not want.
fn page<T: Template>(template: T) -> String {
    template.render().unwrap_or_else(|err| {
        format!("<pre>failed to render: {err}</pre>")
    })
}

fn status_page() -> StatusPage {
    StatusPage {
        signed_in: true,
        glance: Some(Glance {
            items_shared: 128,
            last_sync: Some("4 minutes ago".to_owned()),
            last_sync_note: "2 added, 1 failed".to_owned(),
            last_sync_failed: false,
            friends_recent: 2,
            friends_total: 3,
            swarm_peers: 5,
            swarm_seeders: 3,
        }),
        blocked: None,
        config_error: None,
        recovery_secs: 60,
        master_key_present: true,
        tag: "sharerr".to_owned(),
        client_name: "Transmission",
        client_url: "http://transmission.example:9091".to_owned(),
        sync_enabled: true,
        sync_interval_secs: 900,
        config_path: "/config/sharerr.toml".to_owned(),

        services: vec![
            ServiceLineMock::ok("Sonarr", "reachable, tag present"),
            ServiceLineMock::ok("Radarr", "reachable, tag present"),
            ServiceLineMock::bad("Lidarr", "could not reach it: connection refused"),
        ]
        .into_iter()
        .map(ServiceLineMock::into_line)
        .collect(),
        scanned: true,
        rules: 4,
        checked: 132,
        unmapped: 2,
        missing: vec![
            "/tv/Lanternwick Hollow/S02E04.mkv".to_owned(),
            "/movies/Harborlight (2019)/Harborlight.mkv".to_owned(),
        ],
        more_missing: 0,
        invalid: vec![],
        sample: Some(SampleRow {
            arr: "/data/tv/Lanternwick Hollow/S01E01.mkv".to_owned(),
            sharerr: "/media/tv/Lanternwick Hollow/S01E01.mkv".to_owned(),
            qbit: "/downloads/tv/Lanternwick Hollow/S01E01.mkv".to_owned(),
        }),
        readable: 128,
        healthy: false,
        gluetun: vec![
            EndpointStatus {
                label: "Tracker",
                enabled: true,
                configured: true,
                current: Some("198.51.100.24:51413".to_owned()),
                last_observed: Some("198.51.100.24:51413, 2 minutes ago".to_owned()),
                last_poll: Some("2 minutes ago".to_owned()),
                last_success: Some("2 minutes ago".to_owned()),
                last_error: None,
            },
            EndpointStatus {
                label: "Torrent client",
                enabled: true,
                configured: true,
                current: None,
                last_observed: Some("203.0.113.9:51413, 40 minutes ago".to_owned()),
                last_poll: Some("30 seconds ago".to_owned()),
                last_success: Some("40 minutes ago".to_owned()),
                last_error: Some("could not reach the gluetun control server: timed out".to_owned()),
            },
        ],
        swarm_peers: 5,
        swarm_seeders: 3,
        runs: vec![
            RunRow {
                when: "4 minutes ago".to_owned(),
                summary: "2 added, 1 failed".to_owned(),
                failed: false,
            },
            RunRow {
                when: "19 minutes ago".to_owned(),
                summary: "up to date".to_owned(),
                failed: false,
            },
            RunRow {
                when: "34 minutes ago".to_owned(),
                summary: "could not reach qBittorrent".to_owned(),
                failed: true,
            },
        ],
    }
}

/// A tiny local stand-in so the `status_page` builder above can express
/// "ok"/"bad" without repeating `ServiceLine { .. }` three times.
struct ServiceLineMock {
    name: &'static str,
    message: &'static str,
    ok: bool,
}

impl ServiceLineMock {
    fn ok(name: &'static str, message: &'static str) -> Self {
        Self { name, message, ok: true }
    }

    fn bad(name: &'static str, message: &'static str) -> Self {
        Self { name, message, ok: false }
    }

    fn into_line(self) -> crate::web::templates::ServiceLine {
        crate::web::templates::ServiceLine {
            name: self.name.to_owned(),
            message: self.message.to_owned(),
            ok: self.ok,
        }
    }
}

fn settings_page() -> SettingsPage {
    let mut locks = BTreeMap::new();
    locks.insert("data_dir".to_owned(), "SHARERR_DATA_DIR".to_owned());

    SettingsPage {
        signed_in: true,
        saved: Some("transmission".to_owned()),
        error: None,
        config_error: None,
        config_notice: None,
        master_key_present: true,
        locks,

        tag: "sharerr".to_owned(),

        arrs: vec![
            ArrSection {
                source: "sonarr",
                title: "Sonarr".to_owned(),
                url: "http://sonarr.example:8989".to_owned(),
                key_set: true,
                placeholder: "http://sonarr:8989",
                url_path: "sonarr.url",
                primary: true,
            },
            ArrSection {
                source: "radarr",
                title: "Radarr".to_owned(),
                url: "http://radarr.example:7878".to_owned(),
                key_set: true,
                placeholder: "http://radarr:7878",
                url_path: "radarr.url",
                primary: true,
            },
            ArrSection {
                source: "lidarr",
                title: "Lidarr".to_owned(),
                url: "http://lidarr.example:8686".to_owned(),
                key_set: false,
                placeholder: "http://lidarr:8686",
                url_path: "lidarr.url",
                primary: false,
            },
            ArrSection {
                source: "readarr",
                title: "Readarr".to_owned(),
                url: String::new(),
                key_set: false,
                placeholder: "http://readarr:8787",
                url_path: "readarr.url",
                primary: false,
            },
            ArrSection {
                source: "whisparr",
                title: "Whisparr".to_owned(),
                url: String::new(),
                key_set: false,
                placeholder: "http://whisparr:6969",
                url_path: "whisparr.url",
                primary: false,
            },
        ],
        secondary_arr_configured: true,

        torrent_backend: "transmission",

        qbit_url: "http://qbit.example:8080".to_owned(),
        qbit_api_key_set: true,
        qbit_category: "sharerr".to_owned(),
        qbit_tag: "sharerr".to_owned(),
        qbit_skip_checking: true,

        transmission_url: "http://transmission.example:9091".to_owned(),
        transmission_username: "sharerr".to_owned(),
        transmission_password_set: true,
        transmission_label: "sharerr".to_owned(),

        seeding_upload_limit_kib: "2048".to_owned(),
        seeding_ratio_limit: "2.5".to_owned(),

        tracker_advertised_host: "seed.example.com".to_owned(),
        tracker_port: "51413".to_owned(),
        tracker_advertised_url: String::new(),
        tracker_token_set: true,

        lighthouse_enabled: true,
        lighthouse_mount: "tracker",
        lighthouse_urls: "https://lighthouse.example:9443".to_owned(),

        gluetun_control_url: "http://gluetun.example:8000".to_owned(),
        gluetun_enabled: true,
        gluetun_api_key_set: true,
        gluetun_poll_secs: 30,
        gluetun_last_observed: Some("198.51.100.24:51413, 2 minutes ago".to_owned()),
        gluetun_last_error: None,

        gluetun_client_control_url: String::new(),
        gluetun_client_enabled: false,
        gluetun_client_api_key_set: false,
        gluetun_client_poll_secs: 30,
        gluetun_client_last_observed: None,
        gluetun_client_last_error: None,
        gluetun_client_configured: false,

        revealed: None,

        sync_enabled: true,
        sync_interval_secs: 900,

        notifications_webhook_set: true,
        notifications_kind: "generic",
        notifications_peer_quiet_secs: 86_400,

        libraries: vec![
            LibraryRow {
                path: "/media/home-videos".to_owned(),
                kind: "tv",
            },
            LibraryRow::default(),
        ],

        path_map: vec![
            PathRow {
                arr: "/data/tv".to_owned(),
                sharerr: "/media/tv".to_owned(),
                qbit: "/downloads/tv".to_owned(),
            },
            PathRow {
                arr: "/data/movies".to_owned(),
                sharerr: "/media/movies".to_owned(),
                qbit: "/downloads/movies".to_owned(),
            },
        ],

        min_password_len: 12,

        data_dir: "/data".to_owned(),
        bind: "0.0.0.0:8477".to_owned(),
        config_path: "/config/sharerr.toml".to_owned(),
    }
}

fn peers_page() -> PeersPage {
    PeersPage {
        scope_options: vec![
            ScopeOption { value: "all", label: "Everything".to_owned() },
            ScopeOption { value: "tv", label: "TV only".to_owned() },
            ScopeOption { value: "movies", label: "Films only".to_owned() },
        ],
        signed_in: true,
        peers: vec![
            PeerRow {
                id: 1,
                label: "Sam".to_owned(),
                scope: "all",
                scope_label: "everything",
                created: "3 months ago".to_owned(),
                last_seen: "12 minutes ago".to_owned(),
                revoked: false,
                pubkey_short: Some("a1b2c3d4…e5f6".to_owned()),
                gossip_url: "https://sams-sharerr.example:8477".to_owned(),
                gossip_key_set: true,
                endpoints: vec![
                    PeerEndpointView {
                        kind: "client",
                        addr: "203.0.113.44:51413".to_owned(),
                        seen: "12 minutes ago".to_owned(),
                        via: "direct",
                    },
                    PeerEndpointView {
                        kind: "api",
                        addr: "203.0.113.44".to_owned(),
                        seen: "1 hour ago".to_owned(),
                        via: "direct",
                    },
                ],
            },
            PeerRow {
                id: 2,
                label: "Alex".to_owned(),
                scope: "tv",
                scope_label: "TV only",
                created: "1 month ago".to_owned(),
                last_seen: "never".to_owned(),
                revoked: false,
                pubkey_short: None,
                gossip_url: String::new(),
                gossip_key_set: false,
                endpoints: vec![],
            },
            PeerRow {
                id: 3,
                label: "Old Roommate".to_owned(),
                scope: "movies",
                scope_label: "films only",
                created: "8 months ago".to_owned(),
                last_seen: "5 months ago".to_owned(),
                revoked: true,
                pubkey_short: Some("9f8e7d6c…1a2b".to_owned()),
                gossip_url: String::new(),
                gossip_key_set: false,
                endpoints: vec![],
            },
        ],
        error: None,
        revealed: Some(RevealedPeer {
            label: "Sam".to_owned(),
            key: "sam-9f2a7c4e1b6d3f80".to_owned(),
        }),
        feed_url: "http://seed.example.com:8477/api?t=caps&apikey=<their key>".to_owned(),
    }
}

fn items_page() -> ItemsPage {
    ItemsPage {
        signed_in: true,
        error: None,
        items: vec![
            ItemRow {
                title: "Lanternwick Hollow S01E01".to_owned(),
                kind: "episode",
                source_label: "Sonarr".to_owned(),
                size: "1.9 GiB".to_owned(),
                state_label: "Seeding".to_owned(),
                state_hint: None,
                visible_to: "Sam, Alex".to_owned(),
                since: "3 months ago".to_owned(),
                info_hash: Some("ab".repeat(20)),
                announce_url: Some(
                    "http://seed.example.com:51413/announce/9f2a7c4e".to_owned(),
                ),
                token_fp: Some("9f2a7c4e".to_owned()),
                token_status: TokenStatus::Valid,
                last_error: None,
            },
            ItemRow {
                title: "Harborlight (2019)".to_owned(),
                kind: "movie",
                source_label: "Radarr".to_owned(),
                size: "8.1 GiB".to_owned(),
                state_label: "Seeding".to_owned(),
                state_hint: None,
                visible_to: "Sam".to_owned(),
                since: "1 month ago".to_owned(),
                info_hash: Some("cd".repeat(20)),
                announce_url: Some(
                    "http://seed.example.com:51413/announce/OLDTOKEN12".to_owned(),
                ),
                token_fp: Some("OLDTOKEN12".to_owned()),
                token_status: TokenStatus::Stale,
                last_error: None,
            },
            ItemRow {
                title: "Lanternwick Hollow S02E04".to_owned(),
                kind: "episode",
                source_label: "Sonarr".to_owned(),
                size: "2.0 GiB".to_owned(),
                state_label: "Pending".to_owned(),
                state_hint: Some("waiting for the next sync"),
                visible_to: String::new(),
                since: "2 minutes ago".to_owned(),
                info_hash: None,
                announce_url: None,
                token_fp: None,
                token_status: TokenStatus::None,
                last_error: None,
            },
            ItemRow {
                title: "Midnight Frequency".to_owned(),
                kind: "track",
                source_label: "Lidarr".to_owned(),
                size: "8.4 MiB".to_owned(),
                state_label: "Failed".to_owned(),
                state_hint: None,
                visible_to: String::new(),
                since: "40 minutes ago".to_owned(),
                info_hash: None,
                announce_url: None,
                token_fp: None,
                token_status: TokenStatus::None,
                last_error: Some("qBittorrent rejected the add: category does not exist".to_owned()),
            },
            ItemRow {
                title: "Seaglass & Static".to_owned(),
                kind: "book",
                source_label: "Readarr".to_owned(),
                size: "1.2 MiB".to_owned(),
                state_label: "Seeding".to_owned(),
                state_hint: None,
                visible_to: "no friend's scope covers it".to_owned(),
                since: "6 days ago".to_owned(),
                info_hash: Some("ef".repeat(20)),
                announce_url: Some(
                    "http://seed.example.com:51413/announce/9f2a7c4e".to_owned(),
                ),
                token_fp: Some("9f2a7c4e".to_owned()),
                token_status: TokenStatus::Valid,
                last_error: None,
            },
        ],
        total: 132,
        shown: 5,
        source_options: vec![
            FilterOption { value: "", label: "All sources".to_owned() },
            FilterOption { value: "sonarr", label: "Sonarr".to_owned() },
            FilterOption { value: "radarr", label: "Radarr".to_owned() },
            FilterOption { value: "lidarr", label: "Lidarr".to_owned() },
            FilterOption { value: "readarr", label: "Readarr".to_owned() },
        ],
        state_options: vec![
            FilterOption { value: "", label: "All states".to_owned() },
            FilterOption { value: "seeding", label: "Seeding".to_owned() },
            FilterOption { value: "pending", label: "Pending".to_owned() },
            FilterOption { value: "failed", label: "Failed".to_owned() },
        ],
        source_filter: String::new(),
        state_filter: String::new(),
        q: String::new(),
        sort_links: vec![
            SortLink { label: "Title", href: "/items?sort=title&dir=asc".to_owned(), active: true, dir: "asc" },
            SortLink { label: "Size", href: "/items?sort=size&dir=asc".to_owned(), active: false, dir: "" },
            SortLink { label: "Since", href: "/items?sort=since&dir=asc".to_owned(), active: false, dir: "" },
        ],
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// The one thing worth asserting about hand-built mock data: it actually
    /// renders. A field added to a template struct without a matching change
    /// here would fail to compile, but a value that compiles and still
    /// produces nonsense — an `askama::Error` from a bad expression, an empty
    /// document — would not, so this is a floor under "the preview command
    /// still works" that a future template edit cannot silently break.
    #[test]
    fn every_mock_page_renders_non_empty_html() {
        for (name, html) in [
            ("status", status_page().render().unwrap()),
            ("settings", settings_page().render().unwrap()),
            ("peers", peers_page().render().unwrap()),
            ("items", items_page().render().unwrap()),
        ] {
            assert!(html.contains("<!DOCTYPE html>"), "{name}: {html}");
            assert!(html.len() > 500, "{name} rendered suspiciously small: {html}");
        }
    }

    /// The new Transmission section this command exists to spot-check must
    /// actually be there, with the mock values it was given — the concrete
    /// case that motivated adding this command in the first place.
    #[test]
    fn the_settings_mock_shows_a_populated_transmission_panel() {
        let html = settings_page().render().unwrap();
        assert!(html.contains("http://transmission.example:9091"), "{html}");
        assert!(html.contains(r#"<option value="transmission" selected>"#), "{html}");
    }
}
