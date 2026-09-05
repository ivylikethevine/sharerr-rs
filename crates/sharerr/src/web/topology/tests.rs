#![allow(clippy::unwrap_used, clippy::expect_used)]

use sharerr_core::Config;

use super::*;

fn summary(hash: &str, seeding: bool) -> sharerr_client::TorrentSummary {
    sharerr_client::TorrentSummary {
        hash: hash.to_owned(),
        name: "whatever".to_owned(),
        save_path: "/downloads".to_owned(),
        content_path: "/downloads/whatever".to_owned(),
        category: String::new(),
        tags: Vec::new(),
        is_seeding: seeding,
        ratio: None,
        ratio_limit: None,
        upload_limit_kib: None,
    }
}

fn expect(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(h, t)| ((*h).to_owned(), (*t).to_owned()))
        .collect()
}

/// The whole point of asking the client: the store still says `Seeding`
/// for a torrent somebody removed there, and nothing else contradicts it.
#[test]
fn a_torrent_missing_from_the_client_is_reported_absent() {
    let check = reconcile(
        &expect(&[("aabb", "Kept"), ("ccdd", "Removed")]),
        &[summary("aabb", true)],
    );

    assert_eq!(check.expected, 2);
    assert_eq!(check.confirmed, 1);
    assert_eq!(check.absent.len(), 1);
    assert_eq!(check.absent[0].title, "Removed");
    assert!(check.idle.is_empty());
    assert!(!check.healthy);
}

/// Held but paused is a different problem from not held at all, and has a
/// different fix — so the two must not be collapsed into one count.
#[test]
fn a_paused_torrent_is_idle_rather_than_absent() {
    let check = reconcile(&expect(&[("aabb", "Paused")]), &[summary("aabb", false)]);

    assert_eq!(check.confirmed, 0);
    assert!(check.absent.is_empty());
    assert_eq!(check.idle.len(), 1);
    assert_eq!(check.idle[0].title, "Paused");
    assert!(!check.healthy);
}

#[test]
fn everything_seeding_is_healthy() {
    let check = reconcile(
        &expect(&[("aabb", "One"), ("ccdd", "Two")]),
        &[summary("aabb", true), summary("ccdd", true)],
    );

    assert_eq!(check.confirmed, 2);
    assert!(check.healthy);
    assert!(check.error.is_none());
}

/// `TorrentSummary::hash` documents itself as lowercase, but a hash from a
/// torrent sharerr did not build carries no such guarantee — and comparing
/// case-sensitively would report every one of them as absent.
#[test]
fn hashes_compare_without_regard_to_case() {
    let check = reconcile(
        &expect(&[("AABBCCDD", "Shouty")]),
        &[summary("aabbccdd", true)],
    );

    assert_eq!(check.confirmed, 1);
    assert!(check.healthy);
}

/// A wiped client would otherwise render ten thousand rows and bury the
/// one line that explains it.
#[test]
fn a_long_list_of_mismatches_is_capped_and_counted() {
    let pairs: Vec<(String, String)> = (0..MAX_MISMATCHES + 5)
        .map(|i| (format!("{i:040x}"), format!("Item {i}")))
        .collect();

    let check = reconcile(&pairs, &[]);

    assert_eq!(check.absent.len(), MAX_MISMATCHES);
    assert_eq!(check.more_absent, 5);
    assert!(!check.healthy);
}

/// A client that answered the version probe but not the listing is not the
/// same as a healthy one — the counts are meaningless, and saying "0 of 0
/// seeding" would be a lie rather than an absence of information.
#[test]
fn a_failed_listing_is_not_healthy() {
    let check = client_check_failed("connection reset".to_owned());

    assert!(!check.healthy);
    assert_eq!(check.error.as_deref(), Some("connection reset"));
    assert_eq!(check.expected, 0);
}

use super::super::web_state;

fn node(label: &str, status: NodeStatus) -> SourceNode {
    SourceNode {
        label: label.to_owned(),
        icon: NodeIcon::Arr,
        lines: vec![line("", "configured")],
        status,
        accent: ACCENT_ARR,
    }
}

fn unseen_friend(label: &str) -> FriendNode {
    FriendNode {
        label: label.to_owned(),
        accent: peer_color(0),
        tracker: Channel::unseen(),
        indexer: Channel::unseen(),
        client: Channel::unseen(),
    }
}

// ------------------------------------------------------------- layout

/// Nothing configured at all: still exactly two nodes (sharerr, the
/// client) and the one edge between them — a fresh instance must render
/// a diagram, not an empty page.
#[test]
fn layout_with_nothing_configured_still_draws_the_instance_and_client() {
    let Layout {
        nodes,
        edges,
        width,
        height,
    } = layout(LayoutInput {
        sources: &[],
        instance_lines: &[line("address", "not advertised yet")],
        instance_status: NodeStatus::Error,
        client_label: "qBittorrent",
        client_lines: &[line("version", "no credential stored")],
        client_status: NodeStatus::Error,
        path_edge_label: "",
        friends: &[],
    });

    assert_eq!(nodes.len(), 2);
    assert_eq!(edges.len(), 1, "just the sharerr-client edge");
    assert!(width > 0 && height > 0);
}

#[test]
fn layout_adds_one_node_and_one_edge_per_source() {
    let sources = vec![
        node("Sonarr", NodeStatus::Ok),
        node("Radarr", NodeStatus::Error),
    ];
    let Layout { nodes, edges, .. } = layout(LayoutInput {
        sources: &sources,
        instance_lines: &[line("address", "")],
        instance_status: NodeStatus::Unknown,
        client_label: "qBittorrent",
        client_lines: &[line("version", "")],
        client_status: NodeStatus::Unknown,
        path_edge_label: "",
        friends: &[],
    });

    // 2 sources + sharerr + client.
    assert_eq!(nodes.len(), 4);
    // 2 source->sharerr edges + 1 sharerr->client edge.
    assert_eq!(edges.len(), 3);
}

/// A friend never observed on either channel gets a node but no edges —
/// the diagram must not draw a connection that has not actually
/// happened yet.
#[test]
fn a_friend_with_no_endpoints_gets_no_edges() {
    let friends = vec![unseen_friend("Sam")];
    let Layout { nodes, edges, .. } = layout(LayoutInput {
        sources: &[],
        instance_lines: &[line("address", "")],
        instance_status: NodeStatus::Unknown,
        client_label: "qBittorrent",
        client_lines: &[line("version", "")],
        client_status: NodeStatus::Unknown,
        path_edge_label: "",
        friends: &friends,
    });

    assert_eq!(nodes.len(), 3, "sharerr + client + the one friend");
    assert_eq!(edges.len(), 1, "only the sharerr-client edge");
}

/// A friend observed on only one of the two channels gets exactly one
/// edge, not two — the diagram must not invent a connection for the
/// channel that has never actually been seen.
#[test]
fn a_friend_observed_on_only_one_channel_gets_one_edge() {
    let friends = vec![FriendNode {
        label: "Sam".to_owned(),
        accent: peer_color(0),
        tracker: Channel::unseen(),
        indexer: Channel {
            addr: Some("203.0.113.5:1".to_owned()),
            style: EdgeStyle::Solid,
            edge_label: "direct now".to_owned(),
        },
        client: Channel::unseen(),
    }];
    let Layout { edges, .. } = layout(LayoutInput {
        sources: &[],
        instance_lines: &[line("address", "")],
        instance_status: NodeStatus::Unknown,
        client_label: "qBittorrent",
        client_lines: &[line("version", "")],
        client_status: NodeStatus::Unknown,
        path_edge_label: "",
        friends: &friends,
    });

    assert_eq!(edges.len(), 2, "the sharerr-client edge plus one channel");
}

/// Both of a peer's edges converge on the same point at the sharerr end,
/// so two identical labels render as doubled, unreadable text. One
/// gossip exchange carries both channels, which makes identical labels
/// the common case rather than the exception.
#[test]
fn two_channels_learned_the_same_way_are_labelled_once() {
    let same = || Channel {
        addr: Some("203.0.113.5:1".to_owned()),
        style: EdgeStyle::Dashed,
        edge_label: "gossip 2h".to_owned(),
    };
    let friends = vec![FriendNode {
        label: "Sam".to_owned(),
        accent: peer_color(0),
        tracker: Channel::unseen(),
        indexer: same(),
        client: same(),
    }];

    let Layout { edges, .. } = layout(LayoutInput {
        sources: &[],
        instance_lines: &[line("address", "")],
        instance_status: NodeStatus::Unknown,
        client_label: "qBittorrent",
        client_lines: &[line("version", "")],
        client_status: NodeStatus::Unknown,
        path_edge_label: "",
        friends: &friends,
    });

    let labelled = edges.iter().filter(|e| e.label == "gossip 2h").count();
    assert_eq!(labelled, 1, "the duplicate label must be drawn once");
    // Both edges are still drawn — only the second one's text is dropped.
    assert_eq!(
        edges
            .iter()
            .filter(|e| e.style == EdgeStyle::Dashed)
            .count(),
        2
    );
}

/// The tallest lane decides the diagram's height; a lane with fewer rows
/// does not shrink it.
#[test]
fn height_scales_with_the_tallest_lane() {
    let few_sources = vec![node("Sonarr", NodeStatus::Ok)];
    let many_friends: Vec<_> = (0..5)
        .map(|i| unseen_friend(&format!("friend{i}")))
        .collect();

    let Layout { height: short, .. } = layout(LayoutInput {
        sources: &few_sources,
        instance_lines: &[line("address", "")],
        instance_status: NodeStatus::Unknown,
        client_label: "qBittorrent",
        client_lines: &[line("version", "")],
        client_status: NodeStatus::Unknown,
        path_edge_label: "",
        friends: &[],
    });
    let Layout { height: tall, .. } = layout(LayoutInput {
        sources: &few_sources,
        instance_lines: &[line("address", "")],
        instance_status: NodeStatus::Unknown,
        client_label: "qBittorrent",
        client_lines: &[line("version", "")],
        client_status: NodeStatus::Unknown,
        path_edge_label: "",
        friends: &many_friends,
    });

    assert!(tall > short, "5 friends must need more height than 0");
}

// -------------------------------------------------------------- nodes

/// A box sizes itself to its rows, which is what lets the three lanes be
/// stacked from real heights rather than one fixed row pitch.
#[test]
fn node_height_grows_one_row_at_a_time() {
    let none = node_height(0);
    let one = node_height(1);

    assert_eq!(one - none, NODE_LINE_H);
    assert_eq!(node_height(4) - node_height(3), NODE_LINE_H);
    // Even an empty box has to fit its title and bottom padding.
    assert_eq!(none, NODE_HEAD_H + NODE_PAD_BOTTOM);
}

/// The diagram is tight on space, so its relative times are abbreviated
/// rather than spelled out the way the rest of the UI does.
#[test]
fn compact_ago_abbreviates_every_bucket() {
    let now = sharerr_core::endpoint::now_epoch();

    assert_eq!(compact_ago(now), "now");
    assert_eq!(compact_ago(now - 120), "2m");
    assert_eq!(compact_ago(now - 7_200), "2h");
    assert_eq!(compact_ago(now - 172_800), "2d");
}

#[test]
fn a_readable_library_reports_its_counts_and_is_ok() {
    let node = library_node(
        Path::new("/media/tv"),
        &DirOutcome::Ready {
            items: Vec::new(),
            skipped: 2,
        },
    );

    assert_eq!(node.status, NodeStatus::Ok);
    assert_eq!(node.label, "tv", "the box is named by the leaf directory");
    let text: Vec<&str> = node.lines.iter().map(|l| l.text.as_str()).collect();
    assert!(text.iter().any(|t| t.contains("/media/tv")));
    assert!(text.iter().any(|t| t.contains("2 unclassified")));
}

/// A clean scan must not render an empty "skipped" row -- zero skipped is
/// the normal case and a row saying so is noise in a box this small.
#[test]
fn a_library_with_nothing_skipped_omits_the_skipped_row() {
    let node = library_node(
        Path::new("/media/tv"),
        &DirOutcome::Ready {
            items: Vec::new(),
            skipped: 0,
        },
    );

    assert!(!node.lines.iter().any(|l| l.tag == "skipped"));
}

/// Empty is not a fault -- there is simply nothing to share yet -- while
/// the other three are. Collapsing them would make a missing mount look
/// like an empty directory.
#[test]
fn library_outcomes_map_to_distinct_statuses() {
    let at = Path::new("/media/tv");

    assert_eq!(
        library_node(at, &DirOutcome::Empty).status,
        NodeStatus::Unknown
    );
    assert_eq!(
        library_node(at, &DirOutcome::Missing).status,
        NodeStatus::Error
    );
    assert_eq!(
        library_node(at, &DirOutcome::NotADirectory).status,
        NodeStatus::Error
    );
    assert_eq!(
        library_node(at, &DirOutcome::Unreadable("permission denied".to_owned())).status,
        NodeStatus::Error
    );
}

/// "Unreadable" alone cannot distinguish a permission problem from a
/// vanished mount, and the scan already knows which it was.
#[test]
fn an_unreadable_library_says_why() {
    let node = library_node(
        Path::new("/media/tv"),
        &DirOutcome::Unreadable("permission denied".to_owned()),
    );

    assert!(
        node.lines
            .iter()
            .any(|l| l.text.contains("permission denied")),
        "{:?}",
        node.lines
    );
}

#[test]
fn truncate_leaves_short_labels_alone() {
    assert_eq!(truncate("Sam"), "Sam");
}

#[test]
fn truncate_shortens_a_long_label_with_an_ellipsis() {
    let long = "a very long friend name indeed";
    let short = truncate(long);
    assert!(short.chars().count() <= MAX_LABEL);
    assert!(short.ends_with('…'));
}

/// The split that matters: the first two octets stay (an operator still
/// recognises their own network) and the last two go (nothing identifies
/// one machine on it), with the port keeping only its leading half.
#[test]
fn mask_address_hides_the_last_two_octets_and_the_port_tail() {
    assert_eq!(mask_address("203.0.113.9:51413"), "203.0.•••.•:514••");
    assert_eq!(mask_address("1.2.3.4"), "1.2.•.•");
    assert_eq!(
        mask_address("http://198.51.100.7:6881/"),
        "http://198.51.•••.•:68••/"
    );
}

/// A version string is full of dot-separated digits but is not an
/// address; redacting it would be both wrong and alarming.
#[test]
fn mask_address_leaves_non_address_text_alone() {
    assert_eq!(mask_address("Not seen"), "Not seen");
    assert_eq!(mask_address("v4.0.1"), "v4.0.1");
    assert_eq!(mask_address("12 file(s)"), "12 file(s)");
}

/// A hostname carries no octets to split, so it survives whole — the
/// port is still redacted.
#[test]
fn mask_address_leaves_a_hostname_intact() {
    assert_eq!(
        mask_address("http://seed.example.com:51413/"),
        "http://seed.example.com:514••/"
    );
}

#[test]
fn peer_color_cycles_past_the_palette() {
    assert_eq!(peer_color(0), peer_color(PEER_COLORS.len()));
    assert_ne!(peer_color(0), peer_color(1));
}

#[test]
fn friend_node_with_no_endpoints_reads_as_unseen_on_both_channels() {
    let node = friend_node("Sam", &[], peer_color(0));
    assert!(node.indexer.addr.is_none());
    assert!(node.client.addr.is_none());
    assert_eq!(node.indexer.style, EdgeStyle::None);
    assert_eq!(node.client.style, EdgeStyle::None);
}

#[test]
fn friend_node_keeps_indexer_and_client_channels_independent() {
    let endpoints = vec![
        PeerEndpoint {
            kind: EndpointKind::Api,
            addr: "203.0.113.5:1".to_owned(),
            observed_at: 100,
            via: ObservedVia::Gossip,
        },
        PeerEndpoint {
            kind: EndpointKind::Client,
            addr: "203.0.113.9:2".to_owned(),
            observed_at: 900,
            via: ObservedVia::Direct,
        },
    ];

    let node = friend_node("Sam", &endpoints, peer_color(0));

    assert_eq!(node.indexer.addr.as_deref(), Some("203.0.113.5:1"));
    assert_eq!(node.indexer.style, EdgeStyle::Dashed);
    assert_eq!(node.client.addr.as_deref(), Some("203.0.113.9:2"));
    assert_eq!(node.client.style, EdgeStyle::Solid);
}

#[test]
fn channel_edge_style_follows_observed_via() {
    for (via, style) in [
        (ObservedVia::Direct, EdgeStyle::Solid),
        (ObservedVia::Gossip, EdgeStyle::Dashed),
        (ObservedVia::Lighthouse, EdgeStyle::Dotted),
        (ObservedVia::Restored, EdgeStyle::Sparse),
    ] {
        let endpoints = vec![PeerEndpoint {
            kind: EndpointKind::Api,
            addr: "203.0.113.5:1".to_owned(),
            observed_at: 1,
            via,
        }];
        assert_eq!(
            channel(&endpoints, EndpointKind::Api).style,
            style,
            "{via:?}"
        );
    }
}

// -------------------------------------------------------------- gather

/// A fresh instance — no sources, no vault, no friends — must still
/// render a page rather than panicking, mirroring
/// `diagnostics::gather_on_an_unconfigured_instance_degrades_gracefully`.
/// The client check compares against what the *store* believes is seeding,
/// so this is the half that decides which torrents a disagreement is even
/// measured over.
#[tokio::test]
async fn expected_seeding_lists_only_seeding_items_that_have_a_torrent() {
    use sharerr_core::{MediaSpec, ShareState, SharedItem};

    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let store = serve
        .store()
        .await
        .expect("store opens with an empty vault");

    let base = |file_id: i64, title: &str| SharedItem {
        id: None,
        source: MediaSource::Radarr,
        source_id: 1,
        file_id,
        spec: MediaSpec::Movie {
            title: title.to_owned(),
            year: None,
        },
        release_title: format!("{title}.2019-SYNTH"),
        arr_path: "/data/x.mkv".into(),
        size: 1,
        ids: sharerr_core::ExternalIds::default(),
        media: None,
        info_hash: None,
        announce_token_fp: None,
        created_by_sharerr: true,
        state: ShareState::Seeding,
        last_error: None,
        created_at: None,
        achieved_ratio: None,
        ratio_limit_reported: None,
    };

    // Seeding with a torrent: the only shape the client can be asked about.
    store
        .upsert(&SharedItem {
            info_hash: Some("aabb".to_owned()),
            ..base(1, "Counted")
        })
        .await
        .unwrap();
    // Seeding but no torrent yet -- nothing exists for the client to hold,
    // so counting it would report every pending item as a disagreement.
    store.upsert(&base(2, "No torrent yet")).await.unwrap();
    // Has a torrent but is not seeding: the client is not expected to.
    store
        .upsert(&SharedItem {
            info_hash: Some("ccdd".to_owned()),
            state: ShareState::Unshared,
            ..base(3, "Withdrawn")
        })
        .await
        .unwrap();

    let state = web_state(serve);
    let expected = expected_seeding(&stored_items(&state).await);

    assert_eq!(expected.len(), 1, "{expected:?}");
    assert_eq!(expected[0].0, "aabb");
    assert_eq!(expected[0].1, "Counted");
}

/// An instance sharing nothing expects nothing of the client, so a clean
/// install reconciles as healthy rather than as "everything is missing".
///
/// Only the empty-library path: `fixtures::unconfigured` still opens a
/// working store, so the `Err` arm of `stored_items` -- which also
/// yields an empty list -- is not reachable from tier 1. See CLAUDE.md on
/// what these fixtures do and do not stand up.
#[tokio::test]
async fn expected_seeding_is_empty_for_a_library_with_nothing_in_it() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let state = web_state(serve);

    assert!(expected_seeding(&stored_items(&state).await).is_empty());
}

#[tokio::test]
async fn gather_on_an_unconfigured_instance_degrades_gracefully() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let state = web_state(serve);

    let page = gather(&state).await;

    assert_eq!(page.nodes.len(), 2, "just sharerr and the torrent client");
    assert!(page.width > 0 && page.height > 0);
}

#[tokio::test]
async fn gather_includes_a_configured_but_unreachable_arr_source() {
    let (dir, serve) = crate::state::fixtures::unconfigured();
    let config = Config {
        data_dir: dir.path().to_path_buf(),
        sonarr: Some(sharerr_core::config::ServiceConfig {
            url: url::Url::parse("http://sonarr.example:8989").unwrap(),
        }),
        ..Config::default()
    };
    serve.replace_config(config).await;
    let state = web_state(serve);

    let page = gather(&state).await;

    // sharerr + client + one source.
    assert_eq!(page.nodes.len(), 3);
    let sonarr = page
        .nodes
        .iter()
        .find(|n| n.label == "Sonarr")
        .expect("sonarr node");
    assert_eq!(sonarr.status, NodeStatus::Error, "{sonarr:?}");
}

#[tokio::test]
async fn gather_lists_a_non_revoked_friend_and_skips_a_revoked_one() {
    let (dir, serve) = crate::state::fixtures::unconfigured();
    let config = Config {
        data_dir: dir.path().to_path_buf(),
        ..Config::default()
    };
    serve.replace_config(config).await;
    let store = serve
        .store()
        .await
        .expect("store opens with an empty vault");

    let sam = store
        .create_peer(
            "Sam",
            &secrecy::SecretString::from("sam-key"),
            sharerr_store::PeerScope::All,
        )
        .await
        .unwrap();
    let gone = store
        .create_peer(
            "Gone",
            &secrecy::SecretString::from("gone-key"),
            sharerr_store::PeerScope::All,
        )
        .await
        .unwrap();
    store.revoke_peer(gone.id).await.unwrap();
    store
        .record_peer_endpoint(
            sam.id,
            EndpointKind::Api,
            "203.0.113.5:1",
            Some(1),
            ObservedVia::Direct,
        )
        .await
        .unwrap();

    let state = web_state(serve);
    let page = gather(&state).await;

    assert!(page.nodes.iter().any(|n| n.label == "Sam"));
    assert!(!page.nodes.iter().any(|n| n.label == "Gone"));
}

// ------------------------------------------------------------ arr_node

/// Every `ArrOutcome` `arr_node` can be handed, each producing the
/// status and headline line an operator actually needs to act on. Pure
/// and synchronous, so every variant is cheap to exercise directly
/// rather than through a live (or wiremocked) *arr app.
#[test]
fn arr_node_reports_every_outcome() {
    let cases: &[(ArrOutcome, NodeStatus, &str)] = &[
        (
            ArrOutcome::Ready {
                version: "4.0".to_owned(),
                app_name: "Sonarr".to_owned(),
                items: Vec::new(),
            },
            NodeStatus::Ok,
            "Sonarr v4.0",
        ),
        (
            ArrOutcome::TagUnused {
                version: "4.0".to_owned(),
            },
            NodeStatus::Warn,
            "nothing carries the tag",
        ),
        (
            ArrOutcome::TagMissing {
                version: "4.0".to_owned(),
            },
            NodeStatus::Error,
            "Tag missing",
        ),
        (ArrOutcome::AuthRejected, NodeStatus::Error, "Key rejected"),
        (
            ArrOutcome::Unreachable("refused".to_owned()),
            NodeStatus::Error,
            "Unreachable",
        ),
        (ArrOutcome::NoCredential, NodeStatus::Error, "No key stored"),
        (
            ArrOutcome::CredentialUnreadable("bad master key".to_owned()),
            NodeStatus::Error,
            "Vault unreadable",
        ),
        (
            ArrOutcome::BadUrl("not a url".to_owned()),
            NodeStatus::Error,
            "Failed",
        ),
        (
            ArrOutcome::Failed("500".to_owned()),
            NodeStatus::Error,
            "Failed",
        ),
        (
            ArrOutcome::NotConfigured,
            NodeStatus::Unknown,
            "Not configured",
        ),
    ];

    for (outcome, expected_status, expected_text) in cases {
        let node = arr_node(MediaSource::Sonarr, None, outcome);
        assert_eq!(node.status, *expected_status, "{outcome:?}");
        assert!(
            node.lines.iter().any(|l| l.text.contains(expected_text)),
            "{outcome:?} -> {:?}",
            node.lines
        );
    }
}

// -------------------------------------------------------- library_node

/// A library path with no final component (the filesystem root) must
/// still get a label, falling back to the whole displayed path rather
/// than panicking or rendering blank.
#[test]
fn library_node_falls_back_to_the_full_path_with_no_file_name() {
    let node = library_node(Path::new("/"), &DirOutcome::Empty);
    assert_eq!(node.label, "/");
}

// ----------------------------------------------------- instance_lines

#[tokio::test]
async fn instance_lines_reports_ok_once_an_address_is_advertised() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    serve
        .endpoint()
        .observe("http://203.0.113.9:51413".parse().unwrap());
    let state = web_state(serve);

    let (lines, status) = instance_lines(&Config::default(), &state).await;

    assert_eq!(status, NodeStatus::Ok);
    assert!(lines.iter().any(|l| l.text.contains("203.0.113.9")));
}

/// A gluetun poller is configured but has not resolved anything yet — a
/// different state from "not advertised" (no poller at all), and one an
/// operator should read as "give it a moment", not "broken".
#[tokio::test]
async fn instance_lines_waits_on_gluetun_before_anything_is_observed() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let state = web_state(serve);
    let config = Config {
        gluetun: sharerr_core::config::GluetunConfig {
            control_url: Some(url::Url::parse("http://127.0.0.1:8000").unwrap()),
            ..Config::default().gluetun
        },
        ..Config::default()
    };

    let (lines, status) = instance_lines(&config, &state).await;

    assert_eq!(status, NodeStatus::Unknown);
    assert!(lines.iter().any(|l| l.text.contains("Waiting on gluetun")));
}

// ---------------------------------------------------------- swarm_rows

fn announce_request(hash: &str, peer_id: [u8; 20]) -> sharerr_torrent::AnnounceRequest {
    sharerr_torrent::AnnounceRequest {
        info_hash: sharerr_torrent::announce::info_hash_from_hex(hash).unwrap(),
        peer_id,
        port: 6881,
        left: 0,
        event: sharerr_torrent::Event::None,
        compact: true,
        numwant: 50,
        declared_ip: None,
    }
}

#[tokio::test]
async fn swarm_rows_names_a_torrent_from_the_stored_title_and_counts_its_peers() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let hash = "ab".repeat(20);
    serve
        .swarms()
        .announce(
            &announce_request(&hash, [1; 20]),
            "203.0.113.1:6881".parse().unwrap(),
        )
        .await;
    let state = web_state(serve);
    let mut titles = HashMap::new();
    titles.insert(hash.as_str(), "Harborlight");

    let rows = swarm_rows(&state, &titles).await;

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "Harborlight");
    assert_eq!(rows[0].peers.len(), 1);
    assert_eq!(rows[0].more, 0);
}

/// A swarm whose hash cannot be matched back to a stored title is still
/// listed, named by its hash rather than silently dropped — the peers
/// connected to it are just as real either way.
#[tokio::test]
async fn swarm_rows_falls_back_to_the_hash_when_untitled() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let hash = "cd".repeat(20);
    serve
        .swarms()
        .announce(
            &announce_request(&hash, [2; 20]),
            "203.0.113.2:6881".parse().unwrap(),
        )
        .await;
    let state = web_state(serve);

    let rows = swarm_rows(&state, &HashMap::new()).await;

    assert_eq!(rows.len(), 1);
    assert!(rows[0].title.starts_with("torrent "), "{:?}", rows[0].title);
}

/// A long list of peers on one torrent is capped and counted, the same
/// shape `reconcile`'s mismatch list uses — a swarm nobody culled from
/// must not blow out the row.
#[tokio::test]
async fn swarm_rows_caps_a_long_peer_list_and_counts_the_rest() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let hash = "ef".repeat(20);
    for i in 0..(MAX_SWARM_PEERS + 3) {
        let mut peer_id = [0u8; 20];
        peer_id[0] = i as u8;
        serve
            .swarms()
            .announce(
                &announce_request(&hash, peer_id),
                format!("203.0.113.{}:6881", 10 + i).parse().unwrap(),
            )
            .await;
    }
    let state = web_state(serve);

    let rows = swarm_rows(&state, &HashMap::new()).await;

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].peers.len(), MAX_SWARM_PEERS);
    assert_eq!(rows[0].more, 3);
}

// --------------------------------------------------------- client_node

/// The two arms that need a vault that actually *opens* to tell apart
/// from `CredentialUnreadable`: no key stored at all, and a key stored
/// but the client failing to answer. Needs a real `SHARERR_MASTER_KEY`,
/// so — per this project's convention — a `figment::Jail` with a
/// manually driven runtime rather than `#[tokio::test]`.
#[test]
#[allow(
    clippy::result_large_err,
    reason = "figment::Error, from Jail::expect_with"
)]
fn client_node_reports_no_credential_then_failed_once_one_is_stored() {
    figment::Jail::expect_with(|jail| {
        jail.set_env("SHARERR_MASTER_KEY", "a-master-key");
        let data_dir = jail.directory().to_path_buf();
        let mut config = Config {
            data_dir: data_dir.clone(),
            ..Config::default()
        };
        // Refused immediately rather than timing out, and never a real
        // service some dev machine happens to have on 8080.
        config.qbittorrent.url = url::Url::parse("http://127.0.0.1:1").unwrap();
        let path = data_dir.join("sharerr.toml");
        let serve = std::sync::Arc::new(crate::state::ServeState::new(config.clone(), path, None));
        let state = web_state(serve);

        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let secret = crate::web::diagnostics::secret_reader(state.serve.open_vault().await);
            let (_, lines, status, check) = client_node(&config, &state, &secret, &[]).await;
            assert_eq!(status, NodeStatus::Error);
            assert!(check.is_none());
            assert!(
                lines.iter().any(|l| l.text.contains("No credential")),
                "{lines:?}"
            );

            let mut vault = sharerr_store::Vault::open(
                config.vault_path(),
                &secrecy::SecretString::from("a-master-key"),
            )
            .unwrap();
            vault
                .put(
                    sharerr_core::config::secret_keys::QBITTORRENT_API_KEY,
                    &secrecy::SecretString::from(sharerr_testkit::mock::QBIT_API_KEY),
                )
                .unwrap();

            // qBittorrent's `login()` is a no-op (API-key auth needs no
            // handshake — see `QbitClient::login`), so an unreachable
            // qBittorrent surfaces once `version()` itself fails, which
            // `check_qbit` reports as `Failed` rather than `Unreachable`
            // or `AuthRejected` — those two arms are only reachable
            // through Transmission/rTorrent's real login dial.
            let secret = crate::web::diagnostics::secret_reader(state.serve.open_vault().await);
            let (_, lines, status, check) = client_node(&config, &state, &secret, &[]).await;
            assert_eq!(status, NodeStatus::Error);
            assert!(check.is_none());
            assert!(lines.iter().any(|l| l.text.contains("Failed")), "{lines:?}");
        });
        Ok(())
    });
}

// --------------------------------------------------------- client_node

use sharerr_testkit::mock::base_url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn transmission_config(url: &url::Url) -> Config {
    Config {
        torrent_backend: sharerr_core::config::TorrentBackend::Transmission,
        transmission: sharerr_core::config::TransmissionConfig {
            url: url.clone(),
            ..Default::default()
        },
        ..Config::default()
    }
}

fn qbit_config(url: &url::Url) -> Config {
    Config {
        torrent_backend: sharerr_core::config::TorrentBackend::Qbittorrent,
        qbittorrent: sharerr_core::config::QbitConfig {
            url: url.clone(),
            ..Default::default()
        },
        ..Config::default()
    }
}

#[allow(clippy::unnecessary_wraps, reason = "matches the reader's signature")]
fn any_secret(_: &'static str) -> Result<Option<SecretString>, String> {
    Ok(Some(SecretString::from(
        sharerr_testkit::mock::QBIT_API_KEY,
    )))
}

async fn transmission_answering(status: u16) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/transmission/rpc"))
        .respond_with(ResponseTemplate::new(status).set_body_json(serde_json::json!({})))
        .mount(&server)
        .await;
    server
}

fn headline(lines: &[NodeLine]) -> Vec<&str> {
    lines.iter().map(|l| l.text.as_str()).collect()
}

/// Every failure `client_node` can be handed, each with the status phrase
/// an operator reads first and — where there is one — the reason.
#[tokio::test]
async fn client_node_reports_every_failure_outcome_with_its_reason() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let state = web_state(serve);

    let none = |_: &'static str| Ok::<Option<SecretString>, String>(None);
    let (_, lines, status, check) = client_node(&Config::default(), &state, &none, &[]).await;
    assert_eq!(status, NodeStatus::Error);
    assert!(
        headline(&lines).contains(&"No credential stored"),
        "{lines:?}"
    );
    assert!(check.is_none());

    let sealed = |_: &'static str| Err::<Option<SecretString>, _>("sealed".to_owned());
    let (_, lines, status, _) = client_node(&Config::default(), &state, &sealed, &[]).await;
    assert_eq!(status, NodeStatus::Error);
    assert!(headline(&lines).contains(&"Vault unreadable"), "{lines:?}");
    assert!(lines.iter().any(|l| l.tag == "why" && l.text == "sealed"));

    // A credential the backend cannot use fails to build a client at all
    // — `build_torrent_client`'s finding, before anything is dialled.
    let not_a_key = |_: &'static str| Ok(Some(SecretString::from("pw")));
    let bad = qbit_config(&url::Url::parse("http://127.0.0.1:1").unwrap());
    let (_, lines, status, _) = client_node(&bad, &state, &not_a_key, &[]).await;
    assert_eq!(status, NodeStatus::Error);
    assert!(headline(&lines).contains(&"Misconfigured"), "{lines:?}");

    let port = sharerr_testkit::net::closed_port();
    let closed =
        transmission_config(&url::Url::parse(&format!("http://127.0.0.1:{port}")).unwrap());
    let (_, lines, status, _) = client_node(&closed, &state, &any_secret, &[]).await;
    assert_eq!(status, NodeStatus::Error);
    assert!(headline(&lines).contains(&"Unreachable"), "{lines:?}");
    assert!(lines.iter().any(|l| l.tag == "why"));

    let server = transmission_answering(401).await;
    let rejected = transmission_config(&base_url(&server));
    let (_, lines, status, _) = client_node(&rejected, &state, &any_secret, &[]).await;
    assert_eq!(status, NodeStatus::Error);
    assert!(
        headline(&lines).contains(&"Credential rejected"),
        "{lines:?}"
    );

    let server = transmission_answering(500).await;
    let failed = transmission_config(&base_url(&server));
    let (label, lines, status, _) = client_node(&failed, &state, &any_secret, &[]).await;
    assert_eq!(status, NodeStatus::Error);
    assert_eq!(label, "Transmission");
    assert!(headline(&lines).contains(&"Failed"), "{lines:?}");
}

fn seeding_item(hash: &str) -> sharerr_core::SharedItem {
    use sharerr_core::{MediaSpec, ShareState, SharedItem};
    SharedItem {
        id: None,
        source: MediaSource::Sonarr,
        source_id: 1,
        file_id: 1,
        spec: MediaSpec::Episode {
            series_title: "Lanternwick Hollow".to_owned(),
            season: 1,
            episode: 1,
        },
        release_title: "Lanternwick.Hollow.S01E01.WEB-DL.x264-SHARERR".to_owned(),
        arr_path: std::path::PathBuf::from("/tv/s01e01.mkv"),
        size: 1,
        ids: Default::default(),
        media: None,
        info_hash: Some(hash.to_owned()),
        announce_token_fp: None,
        created_by_sharerr: true,
        state: ShareState::Seeding,
        last_error: None,
        created_at: None,
        achieved_ratio: None,
        ratio_limit_reported: None,
    }
}

async fn qbit_answering(torrents: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/version"))
        .respond_with(ResponseTemplate::new(200).set_body_string("v5.2.3"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/info"))
        .respond_with(torrents)
        .mount(&server)
        .await;
    server
}

/// `Ready` is the one outcome with more to ask: the client is listed and
/// reconciled against what the store says should be seeding, and a client
/// that is reachable but not seeding it is Warn, not Ok.
#[tokio::test]
async fn client_node_reconciles_a_reachable_client_against_the_store() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    serve
        .client_endpoint()
        .observe(url::Url::parse("http://203.0.113.4:51413").unwrap());
    let state = web_state(serve);
    let hash = "ab".repeat(20);
    let items = [seeding_item(&hash)];

    let server = qbit_answering(
        ResponseTemplate::new(200).set_body_json(serde_json::json!([{
            "hash": hash,
            "name": "Lanternwick.Hollow.S01E01.WEB-DL.x264-SHARERR",
            "state": "uploading",
            "progress": 1.0,
            "category": "sharerr",
            "tags": "",
            "size": 1,
            "ratio": 0.0,
            "content_path": "/tv/s01e01.mkv",
        }])),
    )
    .await;
    let (label, lines, status, check) = client_node(
        &qbit_config(&base_url(&server)),
        &state,
        &any_secret,
        &items,
    )
    .await;
    assert_eq!(label, "qBittorrent");
    assert_eq!(status, NodeStatus::Ok, "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|l| l.tag == "public" && l.text.contains("203.0.113.4"))
    );
    assert!(
        lines
            .iter()
            .any(|l| l.tag == "version" && l.text.contains("5.2.3"))
    );
    assert!(
        lines
            .iter()
            .any(|l| l.tag == "seeding" && l.text == "1 of 1"),
        "{lines:?}"
    );
    let check = check.expect("a reachable client is reconciled");
    assert!(check.healthy);

    // The same client with nothing loaded: reachable, but not seeding what
    // the store says it should be.
    let server =
        qbit_answering(ResponseTemplate::new(200).set_body_json(serde_json::json!([]))).await;
    let (_, lines, status, check) = client_node(
        &qbit_config(&base_url(&server)),
        &state,
        &any_secret,
        &items,
    )
    .await;
    assert_eq!(status, NodeStatus::Warn, "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|l| l.tag == "seeding" && l.text == "0 of 1"),
        "{lines:?}"
    );
    assert!(!check.expect("reconciled").healthy);

    // Signed in, but the listing itself failed: the node says so rather
    // than reporting zero of everything.
    let server = qbit_answering(ResponseTemplate::new(500)).await;
    let (_, lines, status, check) = client_node(
        &qbit_config(&base_url(&server)),
        &state,
        &any_secret,
        &items,
    )
    .await;
    assert_eq!(status, NodeStatus::Warn, "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|l| l.tag == "seeding" && l.text.starts_with("could not list")),
        "{lines:?}"
    );
    assert!(check.expect("reconciled").error.is_some());
}

// ------------------------------------------ instance_lines (reachability)

/// With `[checks] reachability` on, both the tracker and the feed are
/// dialled: a listener that answers is "reachable", one that refuses is
/// "unconfirmed", and a refusal downgrades Ok to Warn rather than Error.
#[tokio::test]
async fn instance_lines_dials_the_tracker_and_the_feed_when_reachability_is_on() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let open = listener.local_addr().unwrap().port();
    let closed = sharerr_testkit::net::closed_port();

    let (_dir, serve) = crate::state::fixtures::unconfigured();
    serve
        .endpoint()
        .observe(format!("http://127.0.0.1:{open}").parse().unwrap());
    let state = web_state(serve);
    let config = Config {
        checks: sharerr_core::config::ChecksConfig { reachability: true },
        // The feed is dialled at `public_base_url`, which falls back to
        // the bind port when nothing is advertised statically.
        server: sharerr_core::config::ServerConfig {
            bind: format!("127.0.0.1:{closed}").parse().unwrap(),
        },
        ..Config::default()
    };

    let (lines, status) = instance_lines(&config, &state).await;

    assert_eq!(status, NodeStatus::Warn, "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|l| l.tag == "tracker" && l.text == "reachable"),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.tag == "feed" && l.text == "unconfirmed (refused)"),
        "{lines:?}"
    );
}

#[tokio::test]
async fn instance_lines_has_no_tracker_address_to_dial_before_one_is_advertised() {
    let closed = sharerr_testkit::net::closed_port();
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let state = web_state(serve);
    let config = Config {
        checks: sharerr_core::config::ChecksConfig { reachability: true },
        server: sharerr_core::config::ServerConfig {
            bind: format!("127.0.0.1:{closed}").parse().unwrap(),
        },
        ..Config::default()
    };

    let (lines, status) = instance_lines(&config, &state).await;

    assert_eq!(status, NodeStatus::Error, "not advertised stays Error");
    assert!(
        lines
            .iter()
            .any(|l| l.tag == "tracker" && l.text == "no address to check"),
        "{lines:?}"
    );
}

// ------------------------------------------------- gather (store paths)

#[tokio::test]
async fn gather_survives_a_store_that_will_not_open() {
    let (_dir, serve) = crate::state::fixtures::store_unopenable();
    let state = web_state(serve);
    let page = gather(&state).await;
    assert_eq!(page.nodes.len(), 2, "sharerr and the client, no friends");
}

#[tokio::test]
async fn gather_labels_the_path_edge_once_a_library_has_been_scanned() {
    let (dir, serve) = crate::state::fixtures::unconfigured();
    let media = tempfile::tempdir().unwrap();
    let library = sharerr_testkit::library::tv_library(media.path()).unwrap();
    let config = Config {
        data_dir: dir.path().to_path_buf(),
        library: vec![sharerr_core::config::LibraryConfig {
            path: library.root.join("tv"),
            kind: sharerr_core::config::LibraryKind::Tv,
        }],
        ..Config::default()
    };
    serve.replace_config(config).await;
    let store = serve.store().await.unwrap();
    store.upsert(&seeding_item(&"cd".repeat(20))).await.unwrap();
    let state = web_state(serve);

    let page = gather(&state).await;

    assert!(
        page.nodes
            .iter()
            .any(|n| matches!(n.icon, NodeIcon::Library)),
        "{:?}",
        page.nodes
    );
    assert!(
        page.edges.iter().any(|e| e.label.ends_with("resolve")),
        "{:?}",
        page.edges
    );
}
