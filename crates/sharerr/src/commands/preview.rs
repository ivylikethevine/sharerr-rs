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
use axum::Router;
use axum::response::Html;
use axum::routing::get;

use crate::web::templates::{
    ArrSection, ClientCheck, ClientMismatch, DiagnosticsData, EdgeStyle, EndpointStatus,
    FilterOption, Glance, ItemRow, ItemsPage, LibraryRow, LighthouseRow, LighthouseView,
    NodeStatus, PathRow, PeerEndpointView, PeerRow, PeersPage, RevealedPeer, RunRow, SampleRow,
    ScopeOption, SettingsPage, SortLink, StateCount, StatusPage, TokenStatus, TopologyPage,
};
use crate::web::topology::{Channel, FriendNode, SourceNode, layout};

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
        .route("/topology", get(|| async { Html(page(topology_page())) }))
        .route("/debug", get(|| async { Html(page(debug_page())) }))
        // The status page polls this every thirty seconds. Without it here the
        // preview's own status page would 404 on a timer, which contradicts what
        // this command promises: that everything renders as it would live.
        .route(
            "/status/tiles",
            get(|| async {
                Html(
                    crate::web::templates::StatTiles {
                        glance: Some(glance()),
                    }
                    .render()
                    .unwrap_or_else(|err| format!("<p class=\"error\">{err}</p>")),
                )
            }),
        )
        .route("/assets/{file}", get(crate::web::asset));

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;

    println!("mock UI serving on http://{bind}/ — /settings, /peers, /items, /topology, /debug");
    println!(
        "htmx buttons (Test connection, revoke, delete) have nothing live to call \
         and will not do anything; everything else renders exactly as it would \
         on a real instance. Ctrl+C to stop."
    );

    // The same handler `serve` installs, for the same reason plus one: this
    // command tells the operator "Ctrl+C to stop", and without it that is a
    // SIGINT killing the process by default disposition rather than a stop.
    axum::serve(listener, router.into_make_service())
        .with_graceful_shutdown(super::serve::shutdown_signal())
        .await
        .context("preview server failed")
}

/// The four headline numbers, shared by the status page and the fragment it
/// polls — one fixture, so the tiles cannot render differently depending on
/// which of the two produced them.
fn glance() -> Glance {
    Glance {
        items_shared: 128,
        shared_size: "412.6 GiB".to_owned(),
        last_sync: Some("4 minutes ago".to_owned()),
        last_sync_note: "2 added, 1 failed".to_owned(),
        last_sync_failed: false,
        friends_recent: 2,
        friends_total: 3,
        swarm_peers: 5,
        swarm_seeders: 3,
        swarm_torrents: 4,
        next_sync: "in ~11 min".to_owned(),
        cpu_percent: Some("12.3%".to_owned()),
        memory_usage: Some("4.2 GiB of 15.6 GiB".to_owned()),
        disk_usage: Some("120.4 GiB of 500.0 GiB".to_owned()),
    }
}

/// Render a template, or fail loudly with the Askama error rather than
/// silently serving a blank page — the same trade `crate::web::templates::render`
/// makes, reproduced here rather than reused because that one builds an axum
/// `Response` this handler shape does not want.
fn page<T: Template>(template: T) -> String {
    template
        .render()
        .unwrap_or_else(|err| format!("<pre>failed to render: {err}</pre>"))
}

fn status_page() -> StatusPage {
    // Newest first, as the store hands them over. Varied enough to exercise
    // every state the history strip can draw: a busy pass, several quiet
    // ones, and an outright failure.
    let runs = vec![
        RunRow {
            when: "4 minutes ago".to_owned(),
            when_absolute: "2024-05-06 11:18:04 UTC".to_owned(),
            took: "12s".to_owned(),
            summary: "412 discovered, 2 added".to_owned(),
            failed: false,
            discovered: 412,
            changed: true,
        },
        RunRow {
            when: "19 minutes ago".to_owned(),
            when_absolute: "2024-05-06 11:03:41 UTC".to_owned(),
            took: "under a second".to_owned(),
            summary: "410 discovered".to_owned(),
            failed: false,
            discovered: 410,
            changed: false,
        },
        RunRow {
            when: "34 minutes ago".to_owned(),
            when_absolute: "2024-05-06 10:48:22 UTC".to_owned(),
            took: "2m 5s".to_owned(),
            summary: "could not reach qBittorrent".to_owned(),
            failed: true,
            discovered: 0,
            changed: false,
        },
        RunRow {
            when: "49 minutes ago".to_owned(),
            when_absolute: "2024-05-06 10:33:12 UTC".to_owned(),
            took: "9s".to_owned(),
            summary: "398 discovered, 6 added, 1 unshared".to_owned(),
            failed: false,
            discovered: 398,
            changed: true,
        },
        RunRow {
            when: "about an hour ago".to_owned(),
            when_absolute: "2024-05-06 10:18:47 UTC".to_owned(),
            took: "under a second".to_owned(),
            summary: "393 discovered".to_owned(),
            failed: false,
            discovered: 393,
            changed: false,
        },
        RunRow {
            when: "about an hour ago".to_owned(),
            when_absolute: "2024-05-06 10:03:29 UTC".to_owned(),
            took: "under a second".to_owned(),
            summary: "393 discovered".to_owned(),
            failed: false,
            discovered: 393,
            changed: false,
        },
        RunRow {
            when: "2 hours ago".to_owned(),
            when_absolute: "2024-05-06 09:48:05 UTC".to_owned(),
            took: "41s".to_owned(),
            summary: "393 discovered, 18 added".to_owned(),
            failed: false,
            discovered: 393,
            changed: true,
        },
        RunRow {
            when: "2 hours ago".to_owned(),
            when_absolute: "2024-05-06 09:32:58 UTC".to_owned(),
            took: "under a second".to_owned(),
            summary: "375 discovered".to_owned(),
            failed: false,
            discovered: 375,
            changed: false,
        },
    ];

    StatusPage {
        signed_in: true,
        glance: Some(glance()),
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

        diag: DiagnosticsData {
            services: vec![
                service_line(
                    "Torrent client",
                    "Transmission v4.0.6 — reachable",
                    true,
                    "http://transmission.example:9091/",
                ),
                service_line(
                    "Sonarr",
                    "reachable, tag present",
                    true,
                    "http://sonarr.example:8989/",
                ),
                service_line(
                    "Radarr",
                    "reachable, tag present",
                    true,
                    "http://radarr.example:7878/",
                ),
                service_line(
                    "Lidarr",
                    "could not reach it: connection refused",
                    false,
                    "http://lidarr.example:8686/",
                ),
            ],
            scanned: true,
            rules: 4,
            checked: 132,
            unmapped: 2,
            missing: vec![
                "/tv/Lanternwick Hollow/S02E04.mkv".to_owned(),
                "/movies/Harborlight (2019)/Harborlight.mkv".to_owned(),
            ],
            more_missing: 0,
            missing_total: 2,
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
                    last_error: Some(
                        "could not reach the gluetun control server: timed out".to_owned(),
                    ),
                },
            ],
            run_chart: crate::web::diagnostics::run_chart(&runs),
            runs,
            // One accepting and one refusing, so the preview shows both the row
            // shape and the warning verdict that a partial failure produces.
            lighthouse: Some(LighthouseView {
                configured: 2,
                last_pass: Some("6 minutes ago".to_owned()),
                healthy: false,
                rows: vec![
                    LighthouseRow {
                        url: "https://lighthouse.example".to_owned(),
                        last_success: Some("6 minutes ago".to_owned()),
                        last_error: None,
                    },
                    LighthouseRow {
                        url: "https://beacon.example".to_owned(),
                        last_success: Some("2 days ago".to_owned()),
                        last_error: Some(
                            "answered 403 Forbidden: key hash is pinned to another identity"
                                .to_owned(),
                        ),
                    },
                ],
                last_recovery: Some("3 days ago".to_owned()),
                last_recovery_peer: Some("Riley".to_owned()),
                lookups_attempted: 1,
            }),
        },
    }
}

/// One service row for the `status_page` fixture above, so it does not
/// repeat `ServiceLine { .. }` with `.to_owned()` on every field three times.
fn service_line(
    name: &str,
    message: &str,
    ok: bool,
    url: &str,
) -> crate::web::templates::ServiceLine {
    crate::web::templates::ServiceLine {
        name: name.to_owned(),
        message: message.to_owned(),
        ok,
        url: url.to_owned(),
    }
}

/// Built from the real `layout()` function rather than hand-copied
/// coordinates, so this preview cannot silently drift from what the diagram
/// actually draws — the one fixture on this page that reuses production
/// logic instead of a fully hand-populated struct, because coordinates are
/// exactly the kind of value that goes stale quietly.
fn topology_page() -> TopologyPage {
    use crate::web::templates::NodeIcon;
    use crate::web::topology::{
        ACCENT_ARR, ACCENT_LIBRARY, address_line, line, peer_color, truncate,
    };

    let sources = vec![
        SourceNode {
            label: "Sonarr".to_owned(),
            icon: NodeIcon::Arr,
            lines: vec![
                address_line("url", "http://sonarr.example:8989"),
                line("version", "v4.0.1"),
                line("tagged", "12 file(s)"),
            ],
            status: NodeStatus::Ok,
            accent: ACCENT_ARR,
        },
        SourceNode {
            label: "Radarr".to_owned(),
            icon: NodeIcon::Arr,
            lines: vec![
                address_line("url", "http://radarr.example:7878"),
                line("", "Unreachable"),
            ],
            status: NodeStatus::Error,
            accent: ACCENT_ARR,
        },
        SourceNode {
            label: "extras".to_owned(),
            icon: NodeIcon::Library,
            lines: vec![
                line("path", "/media/extras"),
                line("files", "8 shareable"),
                line("skipped", "2 unclassified"),
            ],
            status: NodeStatus::Ok,
            accent: ACCENT_LIBRARY,
        },
    ];

    let seen = |addr: &str, style: EdgeStyle, edge_label: &str| Channel {
        addr: Some(addr.to_owned()),
        style,
        edge_label: edge_label.to_owned(),
    };
    let unseen = || Channel {
        addr: None,
        style: EdgeStyle::None,
        edge_label: String::new(),
    };

    let friends = vec![
        FriendNode {
            label: "Sam".to_owned(),
            accent: peer_color(0),
            indexer: seen("203.0.113.9:38412", EdgeStyle::Solid, "direct 4m"),
            client: seen("203.0.113.9:51413", EdgeStyle::Solid, "direct 4m"),
            tracker: seen("203.0.113.9:8477", EdgeStyle::Solid, "direct 4m"),
        },
        FriendNode {
            label: "Alex".to_owned(),
            accent: peer_color(1),
            indexer: seen("198.51.100.7:38412", EdgeStyle::Dashed, "gossip 2h"),
            client: unseen(),
            tracker: seen("198.51.100.7:8477", EdgeStyle::Dashed, "gossip 2h"),
        },
        FriendNode {
            label: "Riley".to_owned(),
            accent: peer_color(2),
            indexer: seen("203.0.113.44:38412", EdgeStyle::Dotted, "lighthouse 1d"),
            client: seen("203.0.113.44:51413", EdgeStyle::Dotted, "lighthouse 1d"),
            tracker: unseen(),
        },
        FriendNode {
            label: truncate("a very long friend name indeed"),
            accent: peer_color(3),
            indexer: unseen(),
            client: unseen(),
            tracker: unseen(),
        },
    ];

    let (nodes, edges, width, height) = layout(
        &sources,
        &[
            address_line("address", "http://seed.example.com:51413/"),
            line("swarm", "3 peer(s), 2 seeding"),
        ],
        NodeStatus::Ok,
        "qBittorrent",
        &[
            address_line("url", "http://qbittorrent.example:8080"),
            line("version", "qBittorrent v5.2.0"),
        ],
        NodeStatus::Ok,
        "2 of 20 missing",
        &friends,
    );

    let address = |full: &str| crate::web::templates::AddressCell {
        masked: crate::web::topology::mask_address(full),
        full: full.to_owned(),
    };
    let swarms = vec![
        crate::web::templates::SwarmRow {
            title: "Lanternwick Hollow S02E04".to_owned(),
            complete: 2,
            incomplete: 1,
            peers: vec![
                address("203.0.113.9:51413"),
                address("198.51.100.7:6881"),
                address("203.0.113.44:51413"),
            ],
            more: 0,
        },
        crate::web::templates::SwarmRow {
            title: "Copper Vale (2019)".to_owned(),
            complete: 1,
            incomplete: 0,
            peers: vec![address("203.0.113.9:38412")],
            more: 5,
        },
    ];

    TopologyPage {
        signed_in: true,
        // One absent and one paused, so the preview exercises both halves of
        // the disagreement and the warning verdict they produce.
        client_check: Some(ClientCheck {
            expected: 20,
            confirmed: 18,
            absent: vec![ClientMismatch {
                title: "Copper Vale (2019)".to_owned(),
                hash: "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd".to_owned(),
            }],
            more_absent: 0,
            idle: vec![ClientMismatch {
                title: "Lanternwick Hollow S01E01".to_owned(),
                hash: "abababababababababababababababababababab".to_owned(),
            }],
            more_idle: 0,
            error: None,
            healthy: false,
        }),
        width,
        height,
        nodes,
        swarms,
        edges,
    }
}

fn debug_page() -> crate::web::templates::DebugPage {
    let tracker = "http://seed.example.com:51413/";
    let feed = "http://seed.example.com:8477";
    crate::web::templates::DebugPage {
        signed_in: true,
        tracker_base: Some(tracker.to_owned()),
        client_base: Some("http://203.0.113.9:51413/".to_owned()),
        feed_base: feed.to_owned(),
        bind: "0.0.0.0:8477".to_owned(),
        tracker_bind: Some("0.0.0.0:51413".to_owned()),
        script: crate::web::debug::script_for(Some(tracker), feed),
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
                docs_url: crate::web::docs::SONARR,
                url_path: "sonarr.url",
                primary: true,
            },
            ArrSection {
                source: "radarr",
                title: "Radarr".to_owned(),
                url: "http://radarr.example:7878".to_owned(),
                key_set: true,
                placeholder: "http://radarr:7878",
                docs_url: crate::web::docs::RADARR,
                url_path: "radarr.url",
                primary: true,
            },
            ArrSection {
                source: "lidarr",
                title: "Lidarr".to_owned(),
                url: "http://lidarr.example:8686".to_owned(),
                key_set: false,
                placeholder: "http://lidarr:8686",
                docs_url: crate::web::docs::LIDARR,
                url_path: "lidarr.url",
                primary: false,
            },
            ArrSection {
                source: "readarr",
                title: "Readarr".to_owned(),
                url: String::new(),
                key_set: false,
                placeholder: "http://readarr:8787",
                docs_url: crate::web::docs::READARR,
                url_path: "readarr.url",
                primary: false,
            },
            ArrSection {
                source: "whisparr",
                title: "Whisparr".to_owned(),
                url: String::new(),
                key_set: false,
                placeholder: "http://whisparr:6969",
                docs_url: crate::web::docs::WHISPARR,
                url_path: "whisparr.url",
                primary: false,
            },
        ],
        secondary_arr_configured: true,
        library_sources_configured: 4,

        torrent_backend: "transmission",
        // qBittorrent's key is set below while Transmission is selected, so
        // the "other clients" fold renders in its opened state here.
        unselected_client_configured: true,

        qbit_url: "http://qbit.example:8080".to_owned(),
        qbit_api_key_set: true,
        qbit_category: "sharerr".to_owned(),
        qbit_tag: "sharerr".to_owned(),
        qbit_skip_checking: true,

        transmission_url: "http://transmission.example:9091".to_owned(),
        transmission_username: "sharerr".to_owned(),
        transmission_password_set: true,
        transmission_label: "sharerr".to_owned(),

        rtorrent_url: "http://seedbox.example/RPC2".to_owned(),
        rtorrent_username: "sharerr".to_owned(),
        rtorrent_password_set: false,
        rtorrent_label: "sharerr".to_owned(),

        seeding_upload_limit_kib: "2048".to_owned(),
        seeding_ratio_limit: "2.5".to_owned(),

        tracker_advertised_host: "seed.example.com".to_owned(),
        tracker_port: "51413".to_owned(),
        tracker_advertised_url: String::new(),
        tracker_token_set: true,
        // Exercises the rotation-in-progress UI: a previous token still
        // being accepted, seen recently.
        tracker_token_previous_set: true,
        tracker_token_previous_last_used: Some("14 minutes ago".to_owned()),

        lighthouse_enabled: true,
        lighthouse_mount: "tracker",
        lighthouse_urls: "https://lighthouse.example:9443".to_owned(),
        lighthouse_url_count: 1,

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
        checks_reachability: true,
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
            PathRow::default(),
        ],
        path_map_count: 2,

        min_password_len: 12,

        data_dir: "/data".to_owned(),
        bind: "0.0.0.0:8477".to_owned(),
        config_path: "/config/sharerr.toml".to_owned(),
    }
}

fn peers_page() -> PeersPage {
    PeersPage {
        scope_options: vec![
            ScopeOption {
                value: "all",
                label: "Everything".to_owned(),
            },
            ScopeOption {
                value: "tv",
                label: "TV only".to_owned(),
            },
            ScopeOption {
                value: "movies",
                label: "Films only".to_owned(),
            },
        ],
        signed_in: true,
        peers: vec![
            PeerRow {
                id: 1,
                label: "Sam".to_owned(),
                scope: "all",
                scope_label: "everything",
                created: "3 months ago".to_owned(),
                created_absolute: "2024-02-08 14:02:11 UTC".to_owned(),
                last_seen: "12 minutes ago".to_owned(),
                last_seen_absolute: "2024-05-06 11:10:52 UTC".to_owned(),
                revoked: false,
                sharing: Some(41),
                sharing_size: "412.6 GiB".to_owned(),
                revoked_when: String::new(),
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
                created_absolute: "2024-04-05 09:37:20 UTC".to_owned(),
                last_seen: "never".to_owned(),
                last_seen_absolute: String::new(),
                revoked: false,
                sharing: Some(128),
                sharing_size: "298.1 GiB".to_owned(),
                revoked_when: String::new(),
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
                created_absolute: "2023-09-12 20:14:03 UTC".to_owned(),
                last_seen: "5 months ago".to_owned(),
                last_seen_absolute: "2023-12-19 07:55:38 UTC".to_owned(),
                revoked: true,
                sharing: None,
                sharing_size: String::new(),
                revoked_when: "5 months ago".to_owned(),
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

/// A synthetic library for the composition panel.
///
/// Built as real `SharedItem`s and run through the real `compose`, rather than
/// hand-writing the bars: the geometry is the part of that panel most likely to
/// be wrong, and a hand-written fixture would render a picture no operator will
/// ever see. Sizes are chosen so the roll-up is lopsided — a preview where every
/// slice is the same width proves nothing about a stacked bar.
fn composition_fixture() -> Option<crate::web::templates::Composition> {
    use sharerr_core::model::{MediaMeta, MediaSource, MediaSpec, ShareState, SharedItem};

    fn item(
        file_id: i64,
        source: MediaSource,
        state: ShareState,
        size: u64,
        media: Option<MediaMeta>,
    ) -> SharedItem {
        SharedItem {
            id: Some(file_id),
            source,
            source_id: 1,
            file_id,
            spec: MediaSpec::Movie {
                title: "Harborlight".to_owned(),
                year: Some(2019),
            },
            release_title: "Harborlight.2019.1080p.WEB-DL.x264-SYNTH".to_owned(),
            arr_path: std::path::PathBuf::from("/data/movies/Harborlight (2019).mkv"),
            size,
            ids: sharerr_core::ExternalIds::default(),
            info_hash: None,
            announce_token_fp: None,
            created_by_sharerr: false,
            state,
            last_error: None,
            created_at: None,
            media,
            achieved_ratio: None,
            ratio_limit_reported: None,
        }
    }

    fn video(resolution: &str, codec: &str) -> MediaMeta {
        MediaMeta {
            resolution: Some(resolution.to_owned()),
            video_codec: Some(codec.to_owned()),
            ..MediaMeta::default()
        }
    }

    fn audio(codec: &str) -> MediaMeta {
        MediaMeta {
            audio_codec: Some(codec.to_owned()),
            audio_sample_rate: Some("44100".to_owned()),
            audio_bit_depth: Some("16".to_owned()),
            ..MediaMeta::default()
        }
    }

    const GIB: u64 = 1024 * 1024 * 1024;
    let mut items = Vec::new();
    let mut next = 1;
    let mut push = |count: usize, source, state, size, media: Option<MediaMeta>| {
        for _ in 0..count {
            items.push(item(next, source, state, size, media.clone()));
            next += 1;
        }
    };

    push(
        48,
        MediaSource::Sonarr,
        ShareState::Seeding,
        2 * GIB,
        Some(video("1920x1080", "x264")),
    );
    push(
        14,
        MediaSource::Radarr,
        ShareState::Seeding,
        9 * GIB,
        Some(video("3840x2160", "x265")),
    );
    push(
        22,
        MediaSource::Sonarr,
        ShareState::Seeding,
        900 * 1024 * 1024,
        Some(video("1280x720", "x264")),
    );
    push(
        61,
        MediaSource::Lidarr,
        ShareState::Seeding,
        280 * 1024 * 1024,
        Some(audio("FLAC")),
    );
    push(
        9,
        MediaSource::Readarr,
        ShareState::Seeding,
        4 * 1024 * 1024,
        None,
    );
    push(2, MediaSource::Directory, ShareState::Pending, GIB, None);
    push(2, MediaSource::Sonarr, ShareState::Failed, 2 * GIB, None);

    crate::web::composition::compose(&items)
}

fn items_page() -> ItemsPage {
    ItemsPage {
        signed_in: true,
        error: None,
        state_counts: vec![
            StateCount {
                label: "Seeding".to_owned(),
                count: 128,
            },
            StateCount {
                label: "Pending".to_owned(),
                count: 2,
            },
            StateCount {
                label: "Failed".to_owned(),
                count: 2,
            },
        ],
        items: vec![
            ItemRow {
                title: "Lanternwick Hollow S01E01".to_owned(),
                release_title: "Lanternwick.Hollow.S01E01.1080p.WEB-DL.DD5.1.H.264-SYNTH"
                    .to_owned(),
                arr_path: "/data/tv/Lanternwick Hollow/Season 01/Lanternwick Hollow S01E01.mkv"
                    .to_owned(),
                kind: "episode",
                source_label: "Sonarr".to_owned(),
                size: "1.9 GiB".to_owned(),
                state_label: "Seeding".to_owned(),
                state_hint: None,
                ratio: "1.85".to_owned(),
                ratio_hint: "Per-torrent limit the client is enforcing: 2.00".to_owned(),
                visible_to: "Sam, Alex".to_owned(),
                since: "3 months ago".to_owned(),
                info_hash: Some("ab".repeat(20)),
                info_hash_short: Some("abababababab".to_owned()),
                peers: "2↑ 1↓".to_owned(),
                peers_hint: "2 seeding · 1 downloading".to_owned(),
                source_hint: "Sonarr series 42, file 1337".to_owned(),
                announce_url: Some("http://seed.example.com:51413/announce/9f2a7c4e".to_owned()),
                token_fp: Some("9f2a7c4e".to_owned()),
                token_status: TokenStatus::Valid,
                ids: "tvdb 361753 · imdb tt1000001".to_owned(),
                last_error: None,
                created_by_sharerr: true,
                since_absolute: "2024-02-14 09:21:07 UTC".to_owned(),
            },
            ItemRow {
                title: "Harborlight (2019)".to_owned(),
                release_title: "Harborlight.2019.2160p.UHD.BluRay.x265-SYNTH".to_owned(),
                arr_path: "/data/movies/Harborlight (2019)/Harborlight (2019).mkv".to_owned(),
                kind: "movie",
                source_label: "Radarr".to_owned(),
                size: "8.1 GiB".to_owned(),
                state_label: "Seeding".to_owned(),
                state_hint: None,
                ratio: "0.42".to_owned(),
                ratio_hint: "The client is not holding this torrent to a fixed per-torrent limit \
                             — its own global default, unlimited, or (on some backends) not \
                             something it can report"
                    .to_owned(),
                visible_to: "Sam".to_owned(),
                since: "1 month ago".to_owned(),
                info_hash: Some("cd".repeat(20)),
                info_hash_short: Some("cdcdcdcdcdcd".to_owned()),
                peers: "1↑ 0↓".to_owned(),
                peers_hint: "1 seeding · 0 downloading".to_owned(),
                source_hint: "Radarr movie 7, file 91".to_owned(),
                announce_url: Some("http://seed.example.com:51413/announce/OLDTOKEN12".to_owned()),
                token_fp: Some("OLDTOKEN12".to_owned()),
                token_status: TokenStatus::Stale,
                ids: String::new(),
                last_error: None,
                created_by_sharerr: false,
                since_absolute: "2024-04-02 17:44:55 UTC".to_owned(),
            },
            ItemRow {
                title: "Lanternwick Hollow S02E04".to_owned(),
                release_title: "Lanternwick.Hollow.S02E04.1080p.WEB-DL.DD5.1.H.264-SYNTH"
                    .to_owned(),
                arr_path: "/data/tv/Lanternwick Hollow/Season 02/Lanternwick Hollow S02E04.mkv"
                    .to_owned(),
                kind: "episode",
                source_label: "Sonarr".to_owned(),
                size: "2.0 GiB".to_owned(),
                state_label: "Pending".to_owned(),
                state_hint: Some("waiting for the next sync"),
                ratio: String::new(),
                ratio_hint: String::new(),
                visible_to: String::new(),
                since: "2 minutes ago".to_owned(),
                info_hash: None,
                info_hash_short: None,
                peers: String::new(),
                peers_hint: String::new(),
                source_hint: "Sonarr series 42, file 2051".to_owned(),
                announce_url: None,
                token_fp: None,
                token_status: TokenStatus::None,
                ids: String::new(),
                last_error: None,
                created_by_sharerr: false,
                since_absolute: "2024-05-06 11:20:31 UTC".to_owned(),
            },
            ItemRow {
                title: "Midnight Frequency".to_owned(),
                release_title: "Midnight.Frequency-2023-FLAC-SYNTH".to_owned(),
                arr_path: "/data/music/Static Meridian/Midnight Frequency.flac".to_owned(),
                kind: "track",
                source_label: "Lidarr".to_owned(),
                size: "8.4 MiB".to_owned(),
                state_label: "Failed".to_owned(),
                state_hint: None,
                ratio: String::new(),
                ratio_hint: String::new(),
                visible_to: String::new(),
                since: "40 minutes ago".to_owned(),
                info_hash: None,
                info_hash_short: None,
                peers: String::new(),
                peers_hint: String::new(),
                source_hint: "Lidarr artist 5, file 610".to_owned(),
                announce_url: None,
                token_fp: None,
                token_status: TokenStatus::None,
                ids: String::new(),
                last_error: Some(
                    "qBittorrent rejected the add: category does not exist".to_owned(),
                ),
                created_by_sharerr: false,
                since_absolute: "2024-05-06 10:42:12 UTC".to_owned(),
            },
            ItemRow {
                title: "Seaglass & Static".to_owned(),
                release_title: "Seaglass.and.Static.2021.RETAIL.EPUB-SYNTH".to_owned(),
                arr_path: "/data/books/Seaglass & Static/Seaglass & Static.epub".to_owned(),
                kind: "book",
                source_label: "Readarr".to_owned(),
                size: "1.2 MiB".to_owned(),
                state_label: "Seeding".to_owned(),
                state_hint: None,
                ratio: "∞".to_owned(),
                ratio_hint: "Per-torrent limit the client is enforcing: 0.50".to_owned(),
                visible_to: "no friend's scope covers it".to_owned(),
                since: "6 days ago".to_owned(),
                info_hash: Some("ef".repeat(20)),
                info_hash_short: Some("efefefefefef".to_owned()),
                peers: "".to_owned(),
                peers_hint: "".to_owned(),
                source_hint: "Readarr author 3, file 12".to_owned(),
                announce_url: Some("http://seed.example.com:51413/announce/9f2a7c4e".to_owned()),
                token_fp: Some("9f2a7c4e".to_owned()),
                token_status: TokenStatus::Valid,
                ids: String::new(),
                last_error: None,
                created_by_sharerr: false,
                since_absolute: "2024-04-30 08:05:44 UTC".to_owned(),
            },
        ],
        total: 132,
        shown: 5,
        seeding_size: "412.6 GiB".to_owned(),
        shown_size: "12.0 GiB".to_owned(),
        composition: composition_fixture(),
        source_options: vec![
            FilterOption {
                value: "",
                label: "All sources".to_owned(),
            },
            FilterOption {
                value: "sonarr",
                label: "Sonarr".to_owned(),
            },
            FilterOption {
                value: "radarr",
                label: "Radarr".to_owned(),
            },
            FilterOption {
                value: "lidarr",
                label: "Lidarr".to_owned(),
            },
            FilterOption {
                value: "readarr",
                label: "Readarr".to_owned(),
            },
        ],
        state_options: vec![
            FilterOption {
                value: "",
                label: "All states".to_owned(),
            },
            FilterOption {
                value: "seeding",
                label: "Seeding".to_owned(),
            },
            FilterOption {
                value: "pending",
                label: "Pending".to_owned(),
            },
            FilterOption {
                value: "failed",
                label: "Failed".to_owned(),
            },
        ],
        kind_options: crate::web::items::KINDS
            .iter()
            .map(|k| FilterOption {
                value: k,
                label: format!("{}{}", k[..1].to_uppercase(), &k[1..]),
            })
            .collect(),
        source_filter: String::new(),
        state_filter: String::new(),
        kind_filter: String::new(),
        q: String::new(),
        // Built from the real column list rather than a hand-written subset:
        // the table's header count has to match its body's cell count, and a
        // fixture listing three of the five columns rendered every header
        // one place left of the data it belonged to.
        sort_links: crate::web::items::SORT_COLUMNS
            .iter()
            .map(|(field, label)| SortLink {
                label,
                hint: crate::web::items::column_hint(field),
                href: format!("/items?sort={field}&dir=asc"),
                active: *field == "since",
                dir: if *field == "since" { "desc" } else { "" },
            })
            .collect(),
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
            assert!(
                html.len() > 500,
                "{name} rendered suspiciously small: {html}"
            );
        }
    }

    /// The new Transmission section this command exists to spot-check must
    /// actually be there, with the mock values it was given — the concrete
    /// case that motivated adding this command in the first place.
    #[test]
    fn the_settings_mock_shows_a_populated_transmission_panel() {
        let html = settings_page().render().unwrap();
        assert!(html.contains("http://transmission.example:9091"), "{html}");
        assert!(
            html.contains(r#"<option value="transmission" selected>"#),
            "{html}"
        );
    }
}
