//! `/topology` — one picture of how this instance connects to everything
//! around it.
//!
//! Every fact drawn here already exists somewhere else: Settings' connection
//! tests, Status' networking panel and path-mapping table, Friends' endpoint
//! list. This page does not check anything new — it calls the same shared
//! [`crate::checks`] primitives those pages do (the "one check, several
//! renderers" pattern [`super::diagnostics`] and [`super::probe`] already
//! establish) and lays the results out as a diagram instead of a table, so
//! "why can't Sam see this torrent" or "which of my two gluetun tunnels is
//! this port actually on" is answerable at a glance. See the README's
//! "Topology" section.

use std::collections::HashMap;
use std::path::Path;

use axum::extract::State;
use axum::response::Response;
use secrecy::SecretString;
use sharerr_core::{Config, MediaSource, SharedItem};
use sharerr_store::{EndpointKind, ObservedVia, PeerEndpoint};

use super::WebState;
use super::templates::{
    AddressCell, ClientCheck, ClientMismatch, Edge, EdgeKind, EdgeStyle, Lane, Node, NodeIcon,
    NodeLine, NodeStatus, SwarmRow, TopologyPage, render,
};
use crate::checks::{self, ArrOutcome, DirOutcome, QbitOutcome};
use crate::gluetun::GluetunTarget;

/// A label longer than this is cut with an ellipsis rather than left to blow
/// out a fixed-width box — operator-typed peer names and library directory
/// names are the only unbounded inputs here.
const MAX_LABEL: usize = 26;

/// The same cap for a detail row, which sits in the same box but at a
/// smaller size, so it fits a little more.
const MAX_LINE: usize = 32;

/// How many of one torrent's live peers to name before summarising the
/// rest — the same shape `doctor`'s `report_capped` caps a long list with.
pub(crate) const MAX_SWARM_PEERS: usize = 12;

const MARGIN: i32 = 40;
const COL_W: i32 = 290;
const COL_GAP: i32 = 200;
const ROW_GAP: i32 = 30;

/// A node's height is its label row plus one row per detail line, so a box
/// grows with what there is to say about it rather than being padded to a
/// fixed size or clipping what does not fit.
const NODE_HEAD_H: i32 = 34;
const NODE_LINE_H: i32 = 20;
const NODE_PAD_BOTTOM: i32 = 10;

fn node_height(lines: usize) -> i32 {
    NODE_HEAD_H + lines as i32 * NODE_LINE_H + NODE_PAD_BOTTOM
}

const ACCENT_NEUTRAL: &str = "var(--line-strong)";
pub(crate) const ACCENT_ARR: &str = "var(--topo-arr)";
pub(crate) const ACCENT_LIBRARY: &str = "var(--topo-library)";
const ACCENT_INSTANCE: &str = "var(--accent)";
const ACCENT_CLIENT: &str = "var(--topo-client)";
const PEER_COLORS: &[&str] = &[
    "var(--topo-peer-1)",
    "var(--topo-peer-2)",
    "var(--topo-peer-3)",
    "var(--topo-peer-4)",
    "var(--topo-peer-5)",
    "var(--topo-peer-6)",
];

/// A unique color for the `index`-th friend, cycling through [`PEER_COLORS`]
/// past the sixth — so a friend's box, and both edges reaching it, read as
/// one connected thing regardless of how many friends are configured.
pub(crate) fn peer_color(index: usize) -> &'static str {
    PEER_COLORS[index % PEER_COLORS.len()]
}

/// A detail row whose value carries no address — a version, a status phrase,
/// a count. Masked and unmasked read identically, so the redaction toggle
/// leaves it alone.
pub(crate) fn line(tag: &'static str, text: impl Into<String>) -> NodeLine {
    let text = truncate_to(&text.into(), MAX_LINE);
    NodeLine {
        tag,
        y: 0,
        masked: text.clone(),
        text,
    }
}

/// A detail row carrying an address, which the redaction toggle redacts.
pub(crate) fn address_line(tag: &'static str, text: impl Into<String>) -> NodeLine {
    let text = truncate_to(&text.into(), MAX_LINE);
    NodeLine {
        tag,
        y: 0,
        masked: mask_address(&text),
        text,
    }
}

pub async fn page(State(state): State<WebState>) -> Response {
    render(&gather(&state).await)
}

/// One column-1 candidate before layout: a configured *arr app or
/// `[[library]]` directory.
///
/// `pub(crate)`, along with [`FriendNode`] and [`layout`] below, so
/// `commands::preview` can build a representative diagram from the real
/// layout math instead of hand-copying coordinates that could drift from it.
pub(crate) struct SourceNode {
    pub(crate) label: String,
    pub(crate) icon: NodeIcon,
    pub(crate) lines: Vec<NodeLine>,
    pub(crate) status: NodeStatus,
    pub(crate) accent: &'static str,
}

/// One friend's box before layout: a name and up to two address channels —
/// the indexer/feed traffic sharerr has seen from them
/// ([`EndpointKind::Api`]) and their torrent client
/// ([`EndpointKind::Client`]) — each with its own trust level and therefore
/// its own edge into this box, the same way this instance's own tracker and
/// client endpoints are already two independent lanes.
pub(crate) struct FriendNode {
    pub(crate) label: String,
    pub(crate) accent: &'static str,
    pub(crate) indexer: Channel,
    pub(crate) client: Channel,
    /// Their own sharerr's announce/feed endpoint, as their gossip reports
    /// it — the address *their* friends announce to. Stored and listed on
    /// the Friends page all along, and previously the one channel the
    /// diagram left out.
    pub(crate) tracker: Channel,
}

/// One address channel on a [`FriendNode`]: `addr` is `None` when nothing
/// has been observed on this channel yet, in which case no edge is drawn for
/// it at all — matching Friends' own "not yet introduced" treatment.
pub(crate) struct Channel {
    pub(crate) addr: Option<String>,
    pub(crate) style: EdgeStyle,
    pub(crate) edge_label: String,
}

impl Channel {
    fn unseen() -> Self {
        Self {
            addr: None,
            style: EdgeStyle::None,
            edge_label: String::new(),
        }
    }

    /// One detail row, tagged with which channel it is — an address with no
    /// word beside it was the single hardest thing to read on this diagram.
    fn row(&self, tag: &'static str) -> NodeLine {
        match &self.addr {
            Some(addr) => address_line(tag, addr.clone()),
            None => line(tag, "Not seen"),
        }
    }
}

impl FriendNode {
    fn lines(&self) -> Vec<NodeLine> {
        vec![
            self.indexer.row("indexer"),
            self.client.row("client"),
            self.tracker.row("tracker"),
        ]
    }

    /// The channels in row order — the index is the detail row each one's
    /// edge lands on, so this is the one place that order is decided.
    fn channels(&self) -> [&Channel; 3] {
        [&self.indexer, &self.client, &self.tracker]
    }
}

async fn gather(state: &WebState) -> TopologyPage {
    let config = state.serve.config().await;

    // One vault open for the whole page, same reasoning as `diagnostics::gather`:
    // opening it derives the key with Argon2, and paying that once per
    // configured service turned this into the slowest page in the UI.
    let secret = super::diagnostics::secret_reader(state.serve.open_vault().await);

    // Shared with `web::diagnostics::gather` — see `checks::snapshot`'s docs
    // for why the arr probes, library scan, and path check live there instead
    // of being duplicated per page.
    let checks::Snapshot {
        sources: probed,
        libraries,
        paths,
    } = checks::snapshot(&config, &secret).await;

    let mut sources: Vec<SourceNode> = probed
        .iter()
        .map(|(kind, outcome)| arr_node(*kind, config.service(*kind).map(|s| &s.url), outcome))
        .collect();
    match &libraries {
        checks::LibraryScan::Scanned(scanned) => {
            sources.extend(
                scanned
                    .iter()
                    .map(|(library, outcome)| library_node(&library.path, outcome)),
            );
        }
        checks::LibraryScan::Panicked(err) => {
            // Previously silently dropped here while `diagnostics::gather`
            // reported it — see `checks::snapshot`'s docs. A configured
            // [[library]] must not vanish from the diagram just because its
            // scan panicked.
            sources.push(SourceNode {
                label: "library".to_owned(),
                icon: NodeIcon::Library,
                lines: vec![line("", format!("the scan did not complete: {err}"))],
                status: NodeStatus::Error,
                accent: ACCENT_LIBRARY,
            });
        }
    }

    // `paths.checked` is `discovered.len()` from `checks::snapshot` — nonzero
    // exactly when either phase found something.
    let scanned = paths.checked > 0;
    let path_edge_label = if !scanned {
        String::new()
    } else if paths.is_failure() {
        format!(
            "{} of {} missing",
            paths.missing.len() + paths.invalid.len(),
            paths.checked
        )
    } else {
        format!("{} resolve", paths.checked)
    };

    // The store is read once for the whole page: the client check needs every
    // seeding item and the swarm table needs a title per live info hash, and
    // both come out of the same `all_items` rather than a query per swarm.
    let items = stored_items(state).await;
    let titles: HashMap<&str, &str> = items
        .iter()
        .filter_map(|item| {
            item.info_hash
                .as_deref()
                .map(|hash| (hash, item.spec.title()))
        })
        .collect();

    // Four independent probes — the live client, the two reachability dials,
    // the friends list, the swarm snapshot — run together rather than one
    // after another.
    let (
        (instance_lines, instance_status),
        (client_label, client_lines, client_status, client_check),
        friends,
        swarms,
    ) = tokio::join!(
        instance_lines(&config, state),
        client_node(&config, state, &secret, &items),
        friend_nodes(state),
        swarm_rows(state, &titles),
    );

    let Layout {
        nodes,
        edges,
        width,
        height,
    } = layout(LayoutInput {
        sources: &sources,
        instance_lines: &instance_lines,
        instance_status,
        client_label: &client_label,
        client_lines: &client_lines,
        client_status,
        path_edge_label: &path_edge_label,
        friends: &friends,
    });

    TopologyPage {
        signed_in: true,
        width,
        height,
        nodes,
        edges,
        swarms,
        client_check,
    }
}

/// How many mismatched torrents to name before summarising the rest. A
/// library whose client was wiped has *every* torrent absent, and a page
/// listing ten thousand of them buries the one line that explains it.
const MAX_MISMATCHES: usize = 20;

/// Compare what the store believes is seeding against what the client is
/// actually doing with it.
///
/// Pure, and takes plain `(hash, title)` pairs rather than a `SharedItem` or a
/// live client, so the interesting half — which side of a disagreement a
/// torrent falls on — is testable without either. See `CLAUDE.md` on
/// preferring store-backed logic as plain parameters.
///
/// Hashes are compared lowercased: `TorrentSummary::hash` documents itself as
/// lowercase hex, but a hash that came from a `.torrent` sharerr did not build
/// has no such guarantee, and a case mismatch here would report every torrent
/// as absent.
pub(crate) fn reconcile(
    expected: &[(String, String)],
    listed: &[sharerr_client::TorrentSummary],
) -> ClientCheck {
    use std::collections::HashMap;

    let present: HashMap<String, bool> = listed
        .iter()
        .map(|t| (t.hash.to_lowercase(), t.is_seeding))
        .collect();

    let mut absent = Vec::new();
    let mut idle = Vec::new();
    let mut confirmed = 0usize;

    for (hash, title) in expected {
        match present.get(&hash.to_lowercase()) {
            Some(true) => confirmed += 1,
            Some(false) => idle.push(ClientMismatch {
                title: title.clone(),
                hash: hash.clone(),
            }),
            None => absent.push(ClientMismatch {
                title: title.clone(),
                hash: hash.clone(),
            }),
        }
    }

    let more_absent = absent.len().saturating_sub(MAX_MISMATCHES);
    let more_idle = idle.len().saturating_sub(MAX_MISMATCHES);
    absent.truncate(MAX_MISMATCHES);
    idle.truncate(MAX_MISMATCHES);

    ClientCheck {
        expected: expected.len(),
        confirmed,
        healthy: more_absent == 0 && more_idle == 0 && absent.is_empty() && idle.is_empty(),
        absent,
        more_absent,
        idle,
        more_idle,
        error: None,
    }
}

/// The `ClientCheck` for a client that answered the version probe but not the
/// listing. Distinct from an unreachable client, which produces no check at
/// all — here sharerr *can* talk to it and still cannot say what it holds.
fn client_check_failed(reason: String) -> ClientCheck {
    ClientCheck {
        expected: 0,
        confirmed: 0,
        absent: Vec::new(),
        more_absent: 0,
        idle: Vec::new(),
        more_idle: 0,
        error: Some(reason),
        healthy: false,
    }
}

/// Every item the store holds, or nothing when the store is unavailable —
/// the rest of the page still has a useful answer without it.
async fn stored_items(state: &WebState) -> Vec<SharedItem> {
    let Ok(store) = state.serve.store().await else {
        return Vec::new();
    };
    store.all_items().await.unwrap_or_default()
}

/// What the store says should be seeding, as `(info hash, title)` pairs.
///
/// An item with no info hash has no torrent yet, so there is nothing for the
/// client to be holding — it is not a disagreement, and counting it as one
/// would make every pending item look like a fault.
fn expected_seeding(items: &[SharedItem]) -> Vec<(String, String)> {
    items
        .iter()
        .filter(|item| item.state == sharerr_core::ShareState::Seeding)
        .filter_map(|item| {
            item.info_hash
                .as_ref()
                .map(|hash| (hash.clone(), item.spec.title().to_owned()))
        })
        .collect()
}

/// Every torrent with a live peer right now, each with its connected
/// peers named — the raw detail the diagram's single "N peer(s)" count on
/// the sharerr box deliberately does not carry. A torrent whose row could
/// not be resolved back to a title (withdrawn mid-swarm, the rare case) is
/// still listed, named by its hash, rather than silently dropped — the
/// peers connected to it are just as real either way.
///
/// `titles` is info hash to title for every stored item that has one, built
/// once by [`gather`] from the same read the client check uses.
async fn swarm_rows(state: &WebState, titles: &HashMap<&str, &str>) -> Vec<SwarmRow> {
    let mut rows = Vec::new();
    for swarm in state.serve.swarms().snapshots().await {
        let hex_hash = hex::encode(swarm.info_hash);
        let title = match titles.get(hex_hash.as_str()) {
            Some(title) => (*title).to_owned(),
            None => format!("torrent {}", &hex_hash[..8]),
        };

        let more = swarm.peers.len().saturating_sub(MAX_SWARM_PEERS);
        let peers = swarm
            .peers
            .iter()
            .take(MAX_SWARM_PEERS)
            .map(|addr| {
                let full = addr.to_string();
                AddressCell {
                    masked: mask_address(&full),
                    full,
                }
            })
            .collect();

        rows.push(SwarmRow {
            title,
            complete: swarm.complete,
            incomplete: swarm.incomplete,
            peers,
            more,
        });
    }
    rows
}

/// This instance's own box: the address friends reach it on, the announce
/// port, and the live swarm count. A gluetun poller error becomes its own
/// row rather than a suffix — this app's convention (`doctor`'s
/// `report.info` vs `report.fail`) is to only speak up about a poller when it
/// has something to say, and a row that is simply absent says nothing.
async fn instance_lines(config: &Config, state: &WebState) -> (Vec<NodeLine>, NodeStatus) {
    let swarm = state.serve.swarms().stats().await;
    let gluetun_error =
        super::settings::gluetun_last_error(&state.serve, GluetunTarget::Tracker).await;

    let mut lines = Vec::new();
    let status = match state.serve.endpoint().current() {
        Some(base) => {
            lines.push(address_line("address", base.to_string()));
            if gluetun_error.is_some() {
                NodeStatus::Warn
            } else {
                NodeStatus::Ok
            }
        }
        None if config.gluetun.control_url.is_some() => {
            lines.push(line("address", "Waiting on gluetun"));
            NodeStatus::Unknown
        }
        None => {
            lines.push(line("address", "Not advertised yet"));
            NodeStatus::Error
        }
    };

    lines.push(line(
        "swarm",
        format!("{} peer(s), {} seeding", swarm.peers, swarm.seeders),
    ));
    if let Some(reason) = gluetun_error {
        lines.push(line("gluetun", reason));
    }

    // Opt-in, because dialling our own public address exercises NAT
    // hairpinning and plenty of healthy setups refuse it — see
    // `ChecksConfig::reachability`. A refusal downgrades the box to Warn, not
    // Error: it means "could not confirm", never "your port is shut".
    let mut status = status;
    if config.checks.reachability {
        let tracker = state.serve.endpoint().current();
        let feed = url::Url::parse(&config.public_base_url()).ok();

        // Two dials, each bounded by its own timeout — made together so a
        // silent drop on one does not delay the other.
        let targets = [("tracker", tracker.as_ref()), ("feed", feed.as_ref())];
        let outcomes = futures::future::join_all(
            targets
                .iter()
                .map(|(_, base)| checks::check_reachable(*base)),
        )
        .await;
        for ((tag, _), outcome) in targets.iter().zip(outcomes) {
            let text = match &outcome {
                checks::ReachOutcome::Reachable => "reachable".to_owned(),
                checks::ReachOutcome::NotConfigured => "no address to check".to_owned(),
                checks::ReachOutcome::Unusable(_) => "address unusable".to_owned(),
                checks::ReachOutcome::Refused(_) => "unconfirmed (refused)".to_owned(),
                checks::ReachOutcome::TimedOut => "unconfirmed (timed out)".to_owned(),
            };
            if !outcome.is_reachable() && status == NodeStatus::Ok {
                status = NodeStatus::Warn;
            }
            lines.push(line(tag, text));
        }
    }

    (lines, status)
}

/// The configured torrent client, live-checked the same way Settings' "Test
/// connection" button does — see `crate::web::probe::torrent_client_badge`,
/// whose credential resolution this mirrors.
async fn client_node(
    config: &Config,
    state: &WebState,
    secret: &impl Fn(&'static str) -> Result<Option<SecretString>, String>,
    items: &[SharedItem],
) -> (String, Vec<NodeLine>, NodeStatus, Option<ClientCheck>) {
    let backend = config.torrent_backend;
    let client = config.torrent_client_for(backend);

    let credential = checks::resolve_torrent_credential(&client, secret);

    let outcome = checks::check_qbit(backend, client.url, client.login, credential).await;
    let label = backend.display_name().to_owned();

    let mut lines = vec![address_line("url", client.url.to_string())];
    // The address the client is *advertised* on — what friends' clients
    // actually dial, and the thing this page is about. Only present on the
    // split-VPN deployment, where it differs from the URL sharerr uses.
    if let Some(public) = state.serve.client_endpoint().current() {
        lines.push(address_line("public", public.to_string()));
    }
    // `Ready` carries the authenticated client precisely so a caller with more
    // to ask does not build a second one — see `QbitOutcome::Ready`. This page
    // has more to ask: every other check reports what sharerr *believes*, and
    // only the client can say what it is actually doing.
    let mut check = None;
    let status = match outcome {
        QbitOutcome::Ready {
            version,
            kind,
            client: connected,
        } => {
            lines.push(line("version", format!("{kind} v{version}")));
            // `None` rather than sharerr's own category: a torrent moved to a
            // different category is still seeding the file, and filtering here
            // would report it as absent. The hashes are the join key, so the
            // category adds nothing but a way to be wrong.
            let reconciled = match connected.list(None).await {
                Ok(listed) => reconcile(&expected_seeding(items), &listed),
                Err(err) => client_check_failed(sharerr_client::error_chain(&err)),
            };
            lines.push(line(
                "seeding",
                if let Some(reason) = &reconciled.error {
                    format!("could not list: {reason}")
                } else {
                    format!("{} of {}", reconciled.confirmed, reconciled.expected)
                },
            ));
            let degraded = !reconciled.healthy;
            check = Some(reconciled);
            // A client that is reachable but not seeding what sharerr thinks
            // is not "Ok" — that is exactly the state this check exists to
            // stop looking healthy.
            if degraded {
                NodeStatus::Warn
            } else {
                NodeStatus::Ok
            }
        }
        QbitOutcome::NoCredential => {
            lines.push(line("", "No credential stored"));
            NodeStatus::Error
        }
        // Each of these carries the reason it failed. Collapsing it to the bare
        // category left the diagram saying "Unreachable" where the answer —
        // wrong port, refused connection, expired certificate — was already in
        // hand. `line` truncates to the node's width, and the node's <title>
        // repeats every line for the full text on hover.
        QbitOutcome::CredentialUnreadable(reason) => {
            lines.push(line("", "Vault unreadable"));
            lines.push(line("why", reason));
            NodeStatus::Error
        }
        QbitOutcome::BadUrl(reason) => {
            lines.push(line("", "Misconfigured"));
            lines.push(line("why", reason));
            NodeStatus::Error
        }
        QbitOutcome::Unreachable(reason) => {
            lines.push(line("", "Unreachable"));
            lines.push(line("why", reason));
            NodeStatus::Error
        }
        QbitOutcome::AuthRejected => {
            lines.push(line("", "Credential rejected"));
            NodeStatus::Error
        }
        QbitOutcome::Failed(reason) => {
            lines.push(line("", "Failed"));
            lines.push(line("why", reason));
            NodeStatus::Error
        }
    };

    if let Some(reason) =
        super::settings::gluetun_last_error(&state.serve, GluetunTarget::Client).await
    {
        lines.push(line("gluetun", reason));
    }

    (label, lines, status, check)
}

fn arr_node(kind: MediaSource, url: Option<&url::Url>, outcome: &ArrOutcome) -> SourceNode {
    let label = super::settings::title_case(kind.as_str());

    let mut lines = Vec::new();
    if let Some(url) = url {
        lines.push(address_line("url", url.to_string()));
    }

    let status = match outcome {
        ArrOutcome::Ready {
            version,
            items,
            app_name,
            ..
        } => {
            lines.push(line("version", format!("{app_name} v{version}")));
            lines.push(line("tagged", format!("{} file(s)", items.len())));
            NodeStatus::Ok
        }
        ArrOutcome::TagUnused { version } => {
            lines.push(line("version", format!("v{version}")));
            lines.push(line("tagged", "nothing carries the tag"));
            NodeStatus::Warn
        }
        ArrOutcome::TagMissing { .. } => {
            lines.push(line("", "Tag missing"));
            NodeStatus::Error
        }
        ArrOutcome::AuthRejected => {
            lines.push(line("", "Key rejected"));
            NodeStatus::Error
        }
        ArrOutcome::Unreachable(_) => {
            lines.push(line("", "Unreachable"));
            NodeStatus::Error
        }
        ArrOutcome::NoCredential => {
            lines.push(line("", "No key stored"));
            NodeStatus::Error
        }
        ArrOutcome::CredentialUnreadable(_) => {
            lines.push(line("", "Vault unreadable"));
            NodeStatus::Error
        }
        ArrOutcome::BadUrl(_) | ArrOutcome::Failed(_) => {
            lines.push(line("", "Failed"));
            NodeStatus::Error
        }
        ArrOutcome::NotConfigured => {
            lines.push(line("", "Not configured"));
            NodeStatus::Unknown
        }
    };

    SourceNode {
        label,
        icon: NodeIcon::Arr,
        lines,
        status,
        accent: ACCENT_ARR,
    }
}

fn library_node(path: &Path, outcome: &DirOutcome) -> SourceNode {
    let label = path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );

    let mut lines = vec![line("path", path.display().to_string())];
    let status = match outcome {
        DirOutcome::Ready { items, skipped } => {
            lines.push(line("files", format!("{} shareable", items.len())));
            if *skipped > 0 {
                lines.push(line("skipped", format!("{skipped} unclassified")));
            }
            NodeStatus::Ok
        }
        DirOutcome::Empty => {
            lines.push(line("", "Empty"));
            NodeStatus::Unknown
        }
        DirOutcome::Missing => {
            lines.push(line("", "Missing"));
            NodeStatus::Error
        }
        DirOutcome::NotADirectory => {
            lines.push(line("", "Not a directory"));
            NodeStatus::Error
        }
        DirOutcome::Unreadable(reason) => {
            lines.push(line("", "Unreadable"));
            // The scan error is already in hand -- a permission problem and a
            // vanished mount both read as "Unreadable" without it.
            lines.push(line("why", reason.clone()));
            NodeStatus::Error
        }
    };

    SourceNode {
        label,
        icon: NodeIcon::Library,
        lines,
        status,
        accent: ACCENT_LIBRARY,
    }
}

/// Every non-revoked friend, each with up to two address channels — see
/// [`FriendNode`].
async fn friend_nodes(state: &WebState) -> Vec<FriendNode> {
    let Ok(store) = state.serve.store().await else {
        return Vec::new();
    };
    let Ok(peers) = store.list_peers().await else {
        return Vec::new();
    };
    let active: Vec<_> = peers
        .into_iter()
        .filter(|peer| !peer.is_revoked())
        .collect();

    // Every active friend's endpoint history in one round trip rather than
    // one per friend.
    let peer_ids: Vec<i64> = active.iter().map(|peer| peer.id).collect();
    let endpoints_by_peer = store
        .peer_endpoints_for(&peer_ids)
        .await
        .unwrap_or_default();

    active
        .into_iter()
        .enumerate()
        .map(|(i, peer)| {
            let endpoints = endpoints_by_peer.get(&peer.id).cloned().unwrap_or_default();
            friend_node(&peer.label, &endpoints, peer_color(i))
        })
        .collect()
}

fn friend_node(label: &str, endpoints: &[PeerEndpoint], accent: &'static str) -> FriendNode {
    FriendNode {
        label: label.to_owned(),
        accent,
        indexer: channel(endpoints, EndpointKind::Api),
        client: channel(endpoints, EndpointKind::Client),
        tracker: channel(endpoints, EndpointKind::Tracker),
    }
}

/// The most-recently-observed sighting on one [`EndpointKind`], as a
/// [`Channel`] ready to become one row and (when observed) one edge.
fn channel(endpoints: &[PeerEndpoint], kind: EndpointKind) -> Channel {
    let Some(latest) = endpoints
        .iter()
        .filter(|e| e.kind == kind)
        .max_by_key(|e| e.observed_at)
    else {
        return Channel::unseen();
    };

    let style = match latest.via {
        ObservedVia::Direct => EdgeStyle::Solid,
        ObservedVia::Gossip => EdgeStyle::Dashed,
        ObservedVia::Lighthouse => EdgeStyle::Dotted,
        ObservedVia::Restored => EdgeStyle::Sparse,
    };

    Channel {
        addr: Some(latest.addr.clone()),
        style,
        edge_label: format!(
            "{} {}",
            latest.via.as_str(),
            compact_ago(latest.observed_at)
        ),
    }
}

/// A relative time short enough to fit beside a line in a fixed-width
/// column gap — `peers::ago`'s "N minute(s) ago" is the right length for a
/// table cell, not an edge label. Shares `peers::ago_bucket`'s ladder, just
/// wrapped in shorter words.
fn compact_ago(epoch_secs: i64) -> String {
    match super::peers::ago_bucket(epoch_secs) {
        super::peers::AgoBucket::Now => "now".to_owned(),
        super::peers::AgoBucket::Minutes(n) => format!("{n}m"),
        super::peers::AgoBucket::Hours(n) => format!("{n}h"),
        super::peers::AgoBucket::Days(n) => format!("{n}d"),
    }
}

pub(crate) fn truncate(label: &str) -> String {
    truncate_to(label, MAX_LABEL)
}

fn truncate_to(label: &str, max: usize) -> String {
    if label.chars().count() <= max {
        return label.to_owned();
    }
    let mut short: String = label.chars().take(max.saturating_sub(1)).collect();
    short.push('…');
    short
}

/// Redact the identifying half of an address: in an IPv4 literal the last two
/// octets (`1.2.3.4` keeps `1.2`, hides `3` and `4`), and in a port the
/// trailing half of its digits.
///
/// The split is where it is because the first two octets are what an operator
/// recognises as *their own network*, while the last two are what identifies
/// one machine on it — so a redacted diagram is still readable to the person
/// who owns it, and still safe to screenshot.
///
/// A no-op on text with no address in it (a version string, "Not seen"), so
/// it is safe to call unconditionally.
pub(crate) fn mask_address(text: &str) -> String {
    let toks = tokenize(text);

    // Which digit-run tokens to blank entirely: the 3rd and 4th octet of every
    // `d.d.d.d` run. Found first, then applied, so a port's own rule below
    // cannot fight with this one over the same token.
    let mut blank = vec![false; toks.len()];
    for i in 0..toks.len() {
        let quad = [i, i + 2, i + 4, i + 6];
        let dots = [i + 1, i + 3, i + 5];
        let in_range = quad.iter().all(|&j| j < toks.len());
        if !in_range {
            continue;
        }
        let digits = quad.iter().all(|&j| matches!(toks[j], Tok::Digits(_)));
        let dotted = dots.iter().all(|&j| matches!(toks[j], Tok::Other('.')));
        if digits && dotted {
            blank[i + 4] = true;
            blank[i + 6] = true;
        }
    }

    let mut out = String::with_capacity(text.len());
    for (i, tok) in toks.iter().enumerate() {
        match tok {
            Tok::Other(c) => out.push(*c),
            Tok::Digits(run) => {
                if blank[i] {
                    out.extend(std::iter::repeat_n('•', run.chars().count()));
                } else if i > 0 && matches!(toks[i - 1], Tok::Other(':')) {
                    // A port: keep the leading half, hide the tail. Rounded up
                    // so a short port still shows something.
                    let chars: Vec<char> = run.chars().collect();
                    let keep = chars.len().div_ceil(2);
                    out.extend(&chars[..keep]);
                    out.extend(std::iter::repeat_n('•', chars.len() - keep));
                } else {
                    out.push_str(run);
                }
            }
        }
    }
    out
}

enum Tok {
    Digits(String),
    Other(char),
}

fn tokenize(text: &str) -> Vec<Tok> {
    let mut toks = Vec::new();
    let mut digits = String::new();
    for c in text.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
        } else {
            if !digits.is_empty() {
                toks.push(Tok::Digits(std::mem::take(&mut digits)));
            }
            toks.push(Tok::Other(c));
        }
    }
    if !digits.is_empty() {
        toks.push(Tok::Digits(digits));
    }
    toks
}

/// What [`layout`] needs to place every box and edge in the diagram — named
/// fields rather than eight positional arguments a caller has to remember
/// the order of.
pub(crate) struct LayoutInput<'a> {
    pub sources: &'a [SourceNode],
    pub instance_lines: &'a [NodeLine],
    pub instance_status: NodeStatus,
    pub client_label: &'a str,
    pub client_lines: &'a [NodeLine],
    pub client_status: NodeStatus,
    pub path_edge_label: &'a str,
    pub friends: &'a [FriendNode],
}

/// The placed diagram [`layout`] produces: every box and edge, and the
/// canvas size they need.
pub(crate) struct Layout {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub width: i32,
    pub height: i32,
}

/// Lays out three fixed swimlanes — sources, this instance (sharerr +
/// torrent client, stacked), friends — left to right. Not a general
/// graph-layout algorithm: the shape of this diagram is always these three
/// lanes, so stacking each lane's boxes is all that is needed.
///
/// Every box sizes itself to the number of detail rows it carries (see
/// [`node_height`]), so lanes are stacked from real heights rather than one
/// fixed row pitch. The tallest lane sets the diagram's content height and
/// the other two center within it.
pub(crate) fn layout(input: LayoutInput<'_>) -> Layout {
    let LayoutInput {
        sources,
        instance_lines,
        instance_status,
        client_label,
        client_lines,
        client_status,
        path_edge_label,
        friends,
    } = input;
    let sources_x = MARGIN;
    let instance_x = sources_x + COL_W + COL_GAP;
    let friends_x = instance_x + COL_W + COL_GAP;

    let source_hs: Vec<i32> = sources.iter().map(|s| node_height(s.lines.len())).collect();
    let sharerr_h = node_height(instance_lines.len());
    let client_h = node_height(client_lines.len());
    let friend_lines: Vec<Vec<NodeLine>> = friends.iter().map(FriendNode::lines).collect();
    let friend_hs: Vec<i32> = friend_lines
        .iter()
        .map(|lines| node_height(lines.len()))
        .collect();

    let sources_h = stack_height(&source_hs);
    let instance_h = stack_height(&[sharerr_h, client_h]);
    let friends_h = stack_height(&friend_hs);
    let content_h = sources_h.max(instance_h).max(friends_h);
    let height = MARGIN * 2 + content_h;
    let width = friends_x + COL_W + MARGIN;

    let mut nodes = Vec::new();
    let mut edges = Vec::new();

    let mut y = MARGIN + center_offset(sources_h, content_h);
    let mut source_mid_ys = Vec::new();
    for (source, h) in sources.iter().zip(&source_hs) {
        nodes.push(Node {
            x: sources_x,
            y,
            w: COL_W,
            h: *h,
            icon: source.icon,
            label: truncate(&source.label),
            full_label: source.label.clone(),
            lines: placed(&source.lines, y),
            status: source.status,
            accent: source.accent,
            lane: Lane::Source,
        });
        source_mid_ys.push((y + h / 2, source.accent));
        y += h + ROW_GAP;
    }

    let sharerr_y = MARGIN + center_offset(instance_h, content_h);
    let client_y = sharerr_y + sharerr_h + ROW_GAP;
    nodes.push(Node {
        x: instance_x,
        y: sharerr_y,
        w: COL_W,
        h: sharerr_h,
        icon: NodeIcon::Instance,
        label: "sharerr".to_owned(),
        full_label: "sharerr".to_owned(),
        lines: placed(instance_lines, sharerr_y),
        status: instance_status,
        accent: ACCENT_INSTANCE,
        lane: Lane::Instance,
    });
    nodes.push(Node {
        x: instance_x,
        y: client_y,
        w: COL_W,
        h: client_h,
        icon: NodeIcon::Client,
        label: truncate(client_label),
        full_label: client_label.to_owned(),
        lines: placed(client_lines, client_y),
        status: client_status,
        accent: ACCENT_CLIENT,
        lane: Lane::Client,
    });
    edges.push(Edge {
        x1: instance_x + COL_W / 2,
        y1: sharerr_y + sharerr_h,
        x2: instance_x + COL_W / 2,
        y2: client_y,
        label: path_edge_label.to_owned(),
        style: EdgeStyle::Solid,
        accent: ACCENT_NEUTRAL,
        kind: EdgeKind::Client,
    });

    let sharerr_left = instance_x;
    let sharerr_right = instance_x + COL_W;
    let sharerr_mid_y = sharerr_y + sharerr_h / 2;
    for (mid_y, accent) in source_mid_ys {
        edges.push(Edge {
            x1: sources_x + COL_W,
            y1: mid_y,
            x2: sharerr_left,
            y2: sharerr_mid_y,
            label: String::new(),
            style: EdgeStyle::Solid,
            accent,
            kind: EdgeKind::Source,
        });
    }

    let mut y = MARGIN + center_offset(friends_h, content_h);
    for ((friend, lines), h) in friends.iter().zip(friend_lines).zip(&friend_hs) {
        let status = if friend.channels().iter().any(|c| c.addr.is_some()) {
            NodeStatus::Ok
        } else {
            NodeStatus::Unknown
        };
        nodes.push(Node {
            x: friends_x,
            y,
            w: COL_W,
            h: *h,
            icon: NodeIcon::Friend,
            label: truncate(&friend.label),
            full_label: friend.label.clone(),
            lines: place(lines, y),
            status,
            accent: friend.accent,
            lane: Lane::Friend,
        });

        // Each channel's edge lands on its own row, so which line a given
        // connection belongs to is readable without counting.
        //
        // The two edges converge on the same point at the sharerr end, so when
        // both channels were learned the same way at the same time — the
        // common case, since one gossip exchange carries both — their labels
        // landed on top of each other and rendered as unreadable doubled text.
        // Identical labels are drawn once.
        let row_y = |index: i32| y + NODE_HEAD_H + index * NODE_LINE_H - NODE_LINE_H / 3;
        let mut drawn_labels: Vec<&str> = Vec::new();
        for (index, channel) in friend.channels().into_iter().enumerate() {
            if channel.style == EdgeStyle::None {
                continue;
            }
            let label = if drawn_labels.contains(&channel.edge_label.as_str()) {
                String::new()
            } else {
                drawn_labels.push(&channel.edge_label);
                channel.edge_label.clone()
            };
            edges.push(Edge {
                x1: sharerr_right,
                y1: sharerr_mid_y,
                x2: friends_x,
                y2: row_y(index as i32),
                label,
                style: channel.style,
                accent: friend.accent,
                kind: EdgeKind::Friend,
            });
        }
        y += h + ROW_GAP;
    }

    Layout {
        nodes,
        edges,
        width,
        height,
    }
}

/// Stamp each detail row with its baseline, now that the node's own position
/// is known — see [`NodeLine::y`].
fn placed(lines: &[NodeLine], node_y: i32) -> Vec<NodeLine> {
    place(lines.to_vec(), node_y)
}

/// [`placed`] for rows already owned — no second copy of each line.
fn place(lines: Vec<NodeLine>, node_y: i32) -> Vec<NodeLine> {
    lines
        .into_iter()
        .enumerate()
        .map(|(i, l)| NodeLine {
            y: node_y + NODE_HEAD_H + i as i32 * NODE_LINE_H + 14,
            ..l
        })
        .collect()
}

/// The pixel height a stack of boxes occupies, gaps between them included and
/// no trailing gap — zero for an empty lane.
fn stack_height(heights: &[i32]) -> i32 {
    if heights.is_empty() {
        return 0;
    }
    heights.iter().sum::<i32>() + (heights.len() as i32 - 1) * ROW_GAP
}

/// How far down to shift a lane whose own content is shorter than the
/// diagram's overall content height, to center it vertically against the
/// tallest lane.
fn center_offset(lane_h: i32, content_h: i32) -> i32 {
    (content_h - lane_h).max(0) / 2
}

#[cfg(test)]
mod tests {
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
            let serve =
                std::sync::Arc::new(crate::state::ServeState::new(config.clone(), path, None));
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
}
