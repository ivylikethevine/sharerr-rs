//! The topology diagram's page struct and the node/edge/legend types it
//! draws — split out of `templates/mod.rs` for size; nothing here is used
//! outside `crate::web::topology` and `commands::preview`, both of which
//! reach it through the flat `crate::web::templates::*` re-export.

use super::*;

/// A picture of how this instance connects to everything around it: the
/// library sources it discovers from, itself and its torrent client, and the
/// friends it shares with — one glance instead of the four separate pages
/// (Settings' connection tests, Status' networking panel and path-mapping
/// table, Friends' endpoint list) each fact otherwise lives on. See the
/// README's "Topology" section.
///
/// Rendered as an inline `<svg>` in `topology.html`, not a raw string built
/// in Rust — every label here is an ordinary Askama `{{ }}` interpolation and
/// gets escaped exactly like any other page's text, so a peer's operator-typed
/// `label` cannot become markup. [`Node`]/[`Edge`] carry precomputed pixel
/// coordinates rather than abstract positions, the same way every other page
/// here precomputes display strings (`ago()`, `human_size()`) before the
/// template ever sees them — the template only draws what it is given.
#[derive(Debug, Template)]
#[template(path = "topology.html")]
pub struct TopologyPage {
    /// Whether the torrent client is seeding what the store says it is.
    /// `None` when the client could not be reached at all — the diagram's own
    /// client node already says so, and repeating it as a table of zeros
    /// would read as "nothing is seeding".
    pub client_check: Option<ClientCheck>,
    pub signed_in: bool,
    pub width: i32,
    pub height: i32,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    /// Torrents with at least one peer connected right now, each with its
    /// live peers named — the raw swarm data the diagram above summarizes
    /// only as a single "N peer(s)" count on the sharerr box. Only torrents
    /// with a live peer are listed; an idle share has nothing to show here.
    pub swarms: Vec<SwarmRow>,
}

impl TopologyPage {
    /// The legend's "what each box is" row, in lane order. Built from
    /// [`NodeIcon`] itself so the legend can never show a glyph the diagram
    /// does not draw, or the other way round.
    pub fn legend(&self) -> &'static [LegendEntry] {
        LEGEND
    }
}

/// One swatch in the topology legend: the glyph, the color it is drawn in
/// on the diagram, and what it stands for.
#[derive(Debug, Clone, Copy)]
pub struct LegendEntry {
    pub icon: NodeIcon,
    pub lane: Lane,
    /// The same `var(--...)` reference the matching node's accent bar uses.
    pub accent: &'static str,
    pub name: &'static str,
    pub meaning: &'static str,
}

const LEGEND: &[LegendEntry] = &[
    LegendEntry {
        icon: NodeIcon::Arr,
        lane: Lane::Source,
        accent: "var(--topo-arr)",
        name: "*arr app",
        meaning: "Sonarr, Radarr, Lidarr or Readarr — a source sharerr reads tagged files from",
    },
    LegendEntry {
        icon: NodeIcon::Library,
        lane: Lane::Source,
        accent: "var(--topo-library)",
        name: "Library",
        meaning: "A plain [[library]] directory sharerr scans",
    },
    LegendEntry {
        icon: NodeIcon::Instance,
        lane: Lane::Instance,
        accent: "var(--accent)",
        name: "sharerr",
        meaning: "This instance: the tracker friends announce to and the feed they pull",
    },
    LegendEntry {
        icon: NodeIcon::Client,
        lane: Lane::Client,
        accent: "var(--topo-client)",
        name: "Torrent client",
        meaning: "The client that actually seeds — qBittorrent, Transmission or rTorrent",
    },
    LegendEntry {
        icon: NodeIcon::Friend,
        lane: Lane::Friend,
        accent: "var(--topo-peer-1)",
        name: "Friend",
        meaning: "One friend. Each gets a color of their own, shared by their box and both lines reaching it",
    },
];

/// One torrent's live swarm, for the "Active swarms" table below the
/// diagram.
#[derive(Debug, Clone)]
pub struct SwarmRow {
    pub title: String,
    pub complete: usize,
    pub incomplete: usize,
    pub peers: Vec<AddressCell>,
    /// How many live peers exist beyond what `peers` lists — capped the same
    /// way `doctor`'s `report_capped` caps a long list, so one very popular
    /// torrent cannot turn this into a page of addresses.
    pub more: usize,
}

/// One peer's address, full and redacted — see [`NodeLine::masked`]'s
/// doc comment for what the redaction is and why.
#[derive(Debug, Clone)]
pub struct AddressCell {
    pub full: String,
    pub masked: String,
}

/// One box in the diagram: a source, this instance, its torrent client, or a
/// friend.
#[derive(Debug, Clone)]
pub struct Node {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    /// The glyph drawn beside the label, naming what *kind* of thing this is
    /// without spending a word on it.
    pub icon: NodeIcon,
    /// Fitted to the box width — see `topology::truncate`.
    pub label: String,
    /// The untruncated name, for the box's tooltip.
    pub full_label: String,
    /// Detail rows under the label, each with its own short tag naming what
    /// the value is ("url", "indexer", "client") — an unlabelled address is
    /// the thing this page was hardest to read without.
    pub lines: Vec<NodeLine>,
    pub status: NodeStatus,
    /// Which color groups this node with others: a category (source kind,
    /// this instance, the torrent client) or, for a friend, a color unique
    /// to that one peer — so a friend's box and the edges reaching it read
    /// as one connected thing at a glance. A CSS `var(--...)` reference,
    /// applied to the box's left accent bar and its icon.
    pub accent: &'static str,
    /// Which column of the diagram this box stands in — what the "networking
    /// only" toggle keys on to hide the sources lane, and what the legend's
    /// swatches name.
    pub lane: Lane,
}

/// The three swimlanes of the diagram, plus the client's own slot in the
/// middle one. Rendered as a `data-lane` attribute so the page's script can
/// hide a whole lane without knowing anything about coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// An *arr app or `[[library]]` directory — where media comes from.
    Source,
    /// This instance.
    Instance,
    /// This instance's torrent client.
    Client,
    /// A friend.
    Friend,
}

impl Lane {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Instance => "instance",
            Self::Client => "client",
            Self::Friend => "friend",
        }
    }
}

/// One detail row inside a [`Node`].
#[derive(Debug, Clone)]
pub struct NodeLine {
    /// A short word naming what `text` is. Empty for a value that speaks for
    /// itself (a status phrase like "Unreachable").
    pub tag: &'static str,
    /// Baseline for this row, filled in by `topology::layout` once the node's
    /// own position is known — the template does no arithmetic of its own, the
    /// same way every other page here hands the template finished values.
    pub y: i32,
    pub text: String,
    /// `text` with the host half of any IPv4 literal and the tail of any port
    /// replaced by `•` — what the "hide addresses" toggle (on by default,
    /// see `topology.html`'s inline script) shows instead. Equal to `text`
    /// when the line carries no address at all.
    pub masked: String,
}

/// Which glyph a [`Node`] draws. Stroke-drawn 16×16 paths rather than an icon
/// font or an image set: the container ships as one static binary with no
/// asset pipeline (see `assets/style.css`'s own note), and five small paths
/// cost nothing next to either of those.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeIcon {
    /// An *arr app — drawn as a film frame.
    Arr,
    /// A `[[library]]` directory — a folder.
    Library,
    /// This instance, which is also the tracker — a broadcast mast.
    Instance,
    /// The torrent client — a download arrow.
    Client,
    /// A friend — a person.
    Friend,
}

impl NodeIcon {
    /// The `d` of a 16×16 glyph, positioned by the template's `transform`.
    pub fn path(self) -> &'static str {
        match self {
            Self::Arr => "M2 3.5h12v9H2zM5.5 3.5v9M10.5 3.5v9",
            Self::Library => "M2 12.5v-9h4l1.5 2H14v7z",
            Self::Instance => {
                "M8 6.5a1.5 1.5 0 100 3 1.5 1.5 0 000-3zM4.8 4.3a4.5 4.5 0 000 7.4\
                 M11.2 4.3a4.5 4.5 0 010 7.4M8 10v3.5"
            }
            Self::Client => "M8 3v6.5M5.2 7.7 8 10.5l2.8-2.8M3.5 13h9",
            Self::Friend => "M8 7.5a2 2 0 100-4 2 2 0 000 4zM4 13a4 4 0 018 0",
        }
    }
}

/// A node's health, driving the same ok/warn/error color language
/// Status/Items already use — so the diagram's colors mean the same thing the
/// tables it summarizes already taught the operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeStatus {
    Ok,
    Warn,
    Error,
    /// Configured but not yet checked, or nothing to check — a library
    /// directory before its first scan, a friend never yet seen.
    Unknown,
}

impl NodeStatus {
    pub fn css_class(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Error => "error",
            Self::Unknown => "hint",
        }
    }
}

/// One connection between two [`Node`]s.
#[derive(Debug, Clone)]
pub struct Edge {
    pub x1: i32,
    pub y1: i32,
    pub x2: i32,
    pub y2: i32,
    /// Empty renders as an unlabeled line.
    pub label: String,
    pub style: EdgeStyle,
    /// Same meaning as [`Node::accent`] — a source edge matches its source's
    /// category color, a friend's two edges match that friend's unique
    /// color, and the sharerr-to-client edge is left neutral.
    pub accent: &'static str,
    /// What the line stands for — rendered as `data-kind` so the "networking
    /// only" toggle can drop the source edges along with their boxes.
    pub kind: EdgeKind,
}

/// What an [`Edge`] connects, independent of how it was learned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    /// A source feeding this instance — a media relationship, not a network one.
    Source,
    /// This instance to its torrent client, labelled with the path-mapping result.
    Client,
    /// This instance to a friend's indexer or torrent client.
    Friend,
}

impl EdgeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Client => "client",
            Self::Friend => "friend",
        }
    }
}

/// How an edge was learned, or whatever else distinguishes one connection
/// from another — mirrors `sharerr_store::ObservedVia`'s own trust order
/// (direct firmest, lighthouse least) as a line style instead of a word,
/// since a diagram is exactly the place that reads faster as a glyph than a
/// sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeStyle {
    Solid,
    Dashed,
    Dotted,
    /// Sparser than [`Self::Dotted`] — the least trusted rank an edge can
    /// show, for a `[[peers]]` bootstrap import (see
    /// `sharerr_store::ObservedVia::Restored`): sharerr never saw this
    /// address itself, only an operator's word that it was once current.
    Sparse,
    /// Nothing observed yet — no line at all, just the two boxes.
    None,
}

impl EdgeStyle {
    /// A sentence for the line's hover tooltip: the diagram encodes this as
    /// a dash pattern, and a pattern is exactly the kind of thing a reader
    /// forgets between visits.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Solid => "Observed directly: this instance and theirs have spoken",
            Self::Dashed => "Learned through gossip: another friend relayed their address",
            Self::Dotted => "Learned from a lighthouse: the fallback when nobody has seen them",
            Self::Sparse => "Restored from a backup: nobody has actually seen this address yet",
            Self::None => "",
        }
    }

    /// The SVG `stroke-dasharray` value, or `None` when [`Self::None`] means
    /// the edge should not be drawn at all.
    pub fn dasharray(self) -> Option<&'static str> {
        match self {
            Self::Solid => Some("none"),
            Self::Dashed => Some("6 4"),
            Self::Dotted => Some("1.5 4"),
            Self::Sparse => Some("1 7"),
            Self::None => None,
        }
    }
}

/// Whether the torrent client is actually seeding what sharerr's store says
/// it is.
///
/// Every other check on these pages asks the store or a *arr app what it
/// believes. This one asks the client what it is *doing*, which is the only
/// way to notice a torrent removed from the client behind sharerr's back —
/// the store still says `Seeding` and nothing else contradicts it.
#[derive(Debug)]
pub struct ClientCheck {
    /// Torrents the store says are seeding, i.e. what should be there.
    pub expected: usize,
    /// Of those, the ones the client holds *and* reports as seeding.
    pub confirmed: usize,
    /// Expected torrents the client does not have at all. The serious case:
    /// sharerr believes these are shared and nothing is serving them.
    pub absent: Vec<ClientMismatch>,
    pub more_absent: usize,
    /// Expected torrents the client holds but is not seeding — paused,
    /// errored, or still checking. Recoverable inside the client itself.
    pub idle: Vec<ClientMismatch>,
    pub more_idle: usize,
    /// Set when the listing itself failed, in which case every count above is
    /// zero and means nothing — distinct from "asked, and found nothing wrong".
    pub error: Option<String>,
    /// Whether the client agrees with the store. Drives the one-line verdict.
    pub healthy: bool,
}

/// One torrent the client and the store disagree about.
#[derive(Debug)]
pub struct ClientMismatch {
    pub title: String,
    /// Shown truncated, but carried in full so it can be copied — it is the
    /// join key for looking the torrent up in the client's own UI.
    pub hash: String,
}
