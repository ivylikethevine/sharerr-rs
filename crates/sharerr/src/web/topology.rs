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

use std::path::Path;

use axum::extract::State;
use axum::response::Response;
use secrecy::SecretString;
use sharerr_core::config::secret_keys;
use sharerr_core::{Config, MediaSource};
use sharerr_store::{EndpointKind, ObservedVia, PeerEndpoint};

use super::WebState;
use super::templates::{
    AddressCell, Edge, EdgeStyle, Node, NodeIcon, NodeLine, NodeStatus, SwarmRow, TopologyPage,
    render,
};
use crate::checks::{self, ArrOutcome, DirOutcome, QbitOutcome, TorrentCredential};
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
const MAX_SWARM_PEERS: usize = 12;

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
        vec![self.indexer.row("indexer"), self.client.row("client")]
    }
}

async fn gather(state: &WebState) -> TopologyPage {
    let config = state.serve.config().await;

    // One vault open for the whole page, same reasoning as `diagnostics::gather`:
    // opening it derives the key with Argon2, and paying that once per
    // configured service turned this into the slowest page in the UI.
    let vault = state.serve.open_vault().await;
    let secret = |key: &'static str| -> Result<Option<SecretString>, String> {
        match &vault {
            Ok(vault) => vault.get(key).map_err(|err| err.to_string()),
            Err(reason) => Err(reason.clone()),
        }
    };

    let mut sources = Vec::new();
    let mut discovered = Vec::new();

    let outcomes = futures::future::join_all(
        config
            .configured_sources()
            .into_iter()
            .filter_map(|kind| secret_keys::api_key_for(kind).map(|key| (kind, key)))
            .map(|(kind, key)| {
                let api_key = secret(key);
                let config = &config;
                async move {
                    let url = config.service(kind).map(|s| &s.url);
                    (
                        kind,
                        checks::check_arr(kind, url, api_key, &config.tag).await,
                    )
                }
            }),
    )
    .await;
    for (kind, outcome) in outcomes {
        sources.push(arr_node(
            kind,
            config.service(kind).map(|s| &s.url),
            &outcome,
        ));
        discovered.extend(outcome.into_items());
    }

    // Filesystem-bound, same as `diagnostics::gather` — off the async loop so
    // a slow mount cannot stall the single worker thread a pinned container
    // may be running on.
    let libraries = config.library.clone();
    let library_results = tokio::task::spawn_blocking(move || {
        libraries
            .iter()
            .map(|library| {
                let outcome = checks::check_library(library);
                (library_node(&library.path, &outcome), outcome.into_items())
            })
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    for (node, items) in library_results {
        sources.push(node);
        discovered.extend(items);
    }

    let scanned = !discovered.is_empty();
    let paths = {
        let config = config.clone();
        tokio::task::spawn_blocking(move || checks::check_paths(&config, &discovered))
            .await
            .unwrap_or_default()
    };
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

    let (instance_lines, instance_status) = instance_lines(&config, state).await;
    let (client_label, client_lines, client_status) = client_node(&config, state, &secret).await;

    let friends = friend_nodes(state).await;
    let swarms = swarm_rows(state).await;

    let (nodes, edges, width, height) = layout(
        &sources,
        &instance_lines,
        instance_status,
        &client_label,
        &client_lines,
        client_status,
        &path_edge_label,
        &friends,
    );

    TopologyPage {
        signed_in: true,
        width,
        height,
        nodes,
        edges,
        swarms,
    }
}

/// Every torrent with a live peer right now, each with its connected
/// peers named — the raw detail the diagram's single "N peer(s)" count on
/// the sharerr box deliberately does not carry. A torrent whose row could
/// not be resolved back to a title (withdrawn mid-swarm, the rare case) is
/// still listed, named by its hash, rather than silently dropped — the
/// peers connected to it are just as real either way.
async fn swarm_rows(state: &WebState) -> Vec<SwarmRow> {
    let Ok(store) = state.serve.store().await else {
        return Vec::new();
    };

    let mut rows = Vec::new();
    for swarm in state.serve.swarms().snapshots().await {
        let hex_hash = hex::encode(swarm.info_hash);
        let title = match store.item_by_info_hash(&hex_hash).await {
            Ok(Some(item)) => item.spec.title().to_owned(),
            _ => format!("torrent {}", &hex_hash[..8]),
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
    let gluetun_error = gluetun_error_suffix(state, GluetunTarget::Tracker).await;

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

        for (tag, base) in [("tracker", tracker.as_ref()), ("feed", feed.as_ref())] {
            let outcome = checks::check_reachable(base).await;
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

/// The most recent gluetun error for `target`, or `None` when that poller is
/// healthy right now — cleared the moment a poll succeeds, same as the
/// Settings page shows, so this never flags a poller that has since
/// recovered.
async fn gluetun_error_suffix(state: &WebState, target: GluetunTarget) -> Option<String> {
    state
        .serve
        .gluetun_status(target)
        .snapshot()
        .await
        .last_error
}

/// The configured torrent client, live-checked the same way Settings' "Test
/// connection" button does — see `crate::web::probe::torrent_client_badge`,
/// whose credential resolution this mirrors.
async fn client_node(
    config: &Config,
    state: &WebState,
    secret: &impl Fn(&'static str) -> Result<Option<SecretString>, String>,
) -> (String, Vec<NodeLine>, NodeStatus) {
    let backend = config.torrent_backend;
    let client = config.torrent_client_for(backend);

    let api_key = secret_or_none(client.api_key_key, secret);
    let password = secret_or_none(client.password_key, secret);
    let credential = match (api_key, password) {
        (Err(reason), _) | (_, Err(reason)) => Err(reason),
        (Ok(api_key), Ok(password)) => Ok(TorrentCredential::choose(api_key, password)),
    };

    let outcome = checks::check_qbit(backend, client.url, client.username, credential).await;
    let label = backend.display_name().to_owned();

    let mut lines = vec![address_line("url", client.url.to_string())];
    let status = match outcome {
        QbitOutcome::Ready { version, kind, .. } => {
            lines.push(line("version", format!("{kind} v{version}")));
            NodeStatus::Ok
        }
        QbitOutcome::NoCredential => {
            lines.push(line("", "No credential stored"));
            NodeStatus::Error
        }
        QbitOutcome::CredentialUnreadable(_) => {
            lines.push(line("", "Vault unreadable"));
            NodeStatus::Error
        }
        QbitOutcome::BadUrl(_) => {
            lines.push(line("", "Misconfigured"));
            NodeStatus::Error
        }
        QbitOutcome::Unreachable(_) => {
            lines.push(line("", "Unreachable"));
            NodeStatus::Error
        }
        QbitOutcome::AuthRejected => {
            lines.push(line("", "Credential rejected"));
            NodeStatus::Error
        }
        QbitOutcome::Failed(_) => {
            lines.push(line("", "Failed"));
            NodeStatus::Error
        }
    };

    if let Some(reason) = gluetun_error_suffix(state, GluetunTarget::Client).await {
        lines.push(line("gluetun", reason));
    }

    (label, lines, status)
}

fn secret_or_none(
    key: Option<&'static str>,
    secret: &impl Fn(&'static str) -> Result<Option<SecretString>, String>,
) -> Result<Option<SecretString>, String> {
    match key {
        Some(key) => secret(key),
        None => Ok(None),
    }
}

fn arr_node(kind: MediaSource, url: Option<&url::Url>, outcome: &ArrOutcome) -> SourceNode {
    let label = super::settings::title_case(kind.as_str());

    let mut lines = Vec::new();
    if let Some(url) = url {
        lines.push(address_line("url", url.to_string()));
    }

    let status = match outcome {
        ArrOutcome::Ready { version, items, .. } => {
            lines.push(line("version", format!("v{version}")));
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
        label: truncate(&label),
        icon: NodeIcon::Arr,
        lines,
        status,
        accent: ACCENT_ARR,
    }
}

fn library_node(path: &Path, outcome: &DirOutcome) -> SourceNode {
    let label = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());

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
        DirOutcome::Unreadable(_) => {
            lines.push(line("", "Unreadable"));
            NodeStatus::Error
        }
    };

    SourceNode {
        label: truncate(&label),
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

    let endpoints = futures::future::join_all(
        active
            .iter()
            .map(|peer| async { store.peer_endpoints(peer.id).await.unwrap_or_default() }),
    )
    .await;

    active
        .into_iter()
        .zip(endpoints)
        .enumerate()
        .map(|(i, (peer, endpoints))| friend_node(&peer.label, &endpoints, peer_color(i)))
        .collect()
}

fn friend_node(label: &str, endpoints: &[PeerEndpoint], accent: &'static str) -> FriendNode {
    FriendNode {
        label: truncate(label),
        accent,
        indexer: channel(endpoints, EndpointKind::Api),
        client: channel(endpoints, EndpointKind::Client),
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
/// table cell, not an edge label.
fn compact_ago(epoch_secs: i64) -> String {
    let seconds = sharerr_core::endpoint::now_epoch().saturating_sub(epoch_secs);
    match seconds {
        s if s < 60 => "now".to_owned(),
        s if s < 3_600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3_600),
        s => format!("{}d", s / 86_400),
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

/// Lays out three fixed swimlanes — sources, this instance (sharerr +
/// torrent client, stacked), friends — left to right. Not a general
/// graph-layout algorithm: the shape of this diagram is always these three
/// lanes, so stacking each lane's boxes is all that is needed.
///
/// Every box sizes itself to the number of detail rows it carries (see
/// [`node_height`]), so lanes are stacked from real heights rather than one
/// fixed row pitch. The tallest lane sets the diagram's content height and
/// the other two center within it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn layout(
    sources: &[SourceNode],
    instance_lines: &[NodeLine],
    instance_status: NodeStatus,
    client_label: &str,
    client_lines: &[NodeLine],
    client_status: NodeStatus,
    path_edge_label: &str,
    friends: &[FriendNode],
) -> (Vec<Node>, Vec<Edge>, i32, i32) {
    let sources_x = MARGIN;
    let instance_x = sources_x + COL_W + COL_GAP;
    let friends_x = instance_x + COL_W + COL_GAP;

    let source_hs: Vec<i32> = sources.iter().map(|s| node_height(s.lines.len())).collect();
    let sharerr_h = node_height(instance_lines.len());
    let client_h = node_height(client_lines.len());
    let friend_hs: Vec<i32> = friends
        .iter()
        .map(|f| node_height(f.lines().len()))
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
            label: source.label.clone(),
            lines: placed(&source.lines, y),
            status: source.status,
            accent: source.accent,
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
        lines: placed(instance_lines, sharerr_y),
        status: instance_status,
        accent: ACCENT_INSTANCE,
    });
    nodes.push(Node {
        x: instance_x,
        y: client_y,
        w: COL_W,
        h: client_h,
        icon: NodeIcon::Client,
        label: client_label.to_owned(),
        lines: placed(client_lines, client_y),
        status: client_status,
        accent: ACCENT_CLIENT,
    });
    edges.push(Edge {
        x1: instance_x + COL_W / 2,
        y1: sharerr_y + sharerr_h,
        x2: instance_x + COL_W / 2,
        y2: client_y,
        label: path_edge_label.to_owned(),
        style: EdgeStyle::Solid,
        accent: ACCENT_NEUTRAL,
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
        });
    }

    let mut y = MARGIN + center_offset(friends_h, content_h);
    for (friend, h) in friends.iter().zip(&friend_hs) {
        let status = if friend.indexer.addr.is_some() || friend.client.addr.is_some() {
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
            label: friend.label.clone(),
            lines: placed(&friend.lines(), y),
            status,
            accent: friend.accent,
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
        let duplicate_label = friend.indexer.style != EdgeStyle::None
            && friend.client.style != EdgeStyle::None
            && friend.indexer.edge_label == friend.client.edge_label;
        for (channel, index) in [(&friend.indexer, 0), (&friend.client, 1)] {
            if channel.style != EdgeStyle::None {
                let label = if duplicate_label && index == 1 {
                    String::new()
                } else {
                    channel.edge_label.clone()
                };
                edges.push(Edge {
                    x1: sharerr_right,
                    y1: sharerr_mid_y,
                    x2: friends_x,
                    y2: row_y(index),
                    label,
                    style: channel.style,
                    accent: friend.accent,
                });
            }
        }
        y += h + ROW_GAP;
    }

    (nodes, edges, width, height)
}

/// Stamp each detail row with its baseline, now that the node's own position
/// is known — see [`NodeLine::y`].
fn placed(lines: &[NodeLine], node_y: i32) -> Vec<NodeLine> {
    lines
        .iter()
        .enumerate()
        .map(|(i, l)| NodeLine {
            y: node_y + NODE_HEAD_H + i as i32 * NODE_LINE_H + 14,
            ..l.clone()
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

    use std::sync::Arc;

    use sharerr_core::Config;

    use super::*;
    use crate::web::auth::Sessions;

    fn web_state(serve: Arc<crate::state::ServeState>) -> WebState {
        WebState {
            serve,
            sessions: Arc::new(Sessions::default()),
        }
    }

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
        let (nodes, edges, width, height) = layout(
            &[],
            &[line("address", "not advertised yet")],
            NodeStatus::Error,
            "qBittorrent",
            &[line("version", "no credential stored")],
            NodeStatus::Error,
            "",
            &[],
        );

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
        let (nodes, edges, ..) = layout(
            &sources,
            &[line("address", "")],
            NodeStatus::Unknown,
            "qBittorrent",
            &[line("version", "")],
            NodeStatus::Unknown,
            "",
            &[],
        );

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
        let (nodes, edges, ..) = layout(
            &[],
            &[line("address", "")],
            NodeStatus::Unknown,
            "qBittorrent",
            &[line("version", "")],
            NodeStatus::Unknown,
            "",
            &friends,
        );

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
            indexer: Channel {
                addr: Some("203.0.113.5:1".to_owned()),
                style: EdgeStyle::Solid,
                edge_label: "direct now".to_owned(),
            },
            client: Channel::unseen(),
        }];
        let (_, edges, ..) = layout(
            &[],
            &[line("address", "")],
            NodeStatus::Unknown,
            "qBittorrent",
            &[line("version", "")],
            NodeStatus::Unknown,
            "",
            &friends,
        );

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
            indexer: same(),
            client: same(),
        }];

        let (_, edges, ..) = layout(
            &[],
            &[line("address", "")],
            NodeStatus::Unknown,
            "qBittorrent",
            &[line("version", "")],
            NodeStatus::Unknown,
            "",
            &friends,
        );

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

        let (_, _, _, short) = layout(
            &few_sources,
            &[line("address", "")],
            NodeStatus::Unknown,
            "qBittorrent",
            &[line("version", "")],
            NodeStatus::Unknown,
            "",
            &[],
        );
        let (_, _, _, tall) = layout(
            &few_sources,
            &[line("address", "")],
            NodeStatus::Unknown,
            "qBittorrent",
            &[line("version", "")],
            NodeStatus::Unknown,
            "",
            &many_friends,
        );

        assert!(tall > short, "5 friends must need more height than 0");
    }

    // -------------------------------------------------------------- nodes

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
                1,
                ObservedVia::Direct,
            )
            .await
            .unwrap();

        let state = web_state(serve);
        let page = gather(&state).await;

        assert!(page.nodes.iter().any(|n| n.label == "Sam"));
        assert!(!page.nodes.iter().any(|n| n.label == "Gone"));
    }
}
