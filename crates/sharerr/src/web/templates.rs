//! Askama template structs and the one place they are turned into responses.
//!
//! Askama escapes `{{ }}` for HTML by default, which is what keeps a username or
//! an error message containing `<` from becoming markup. Nothing here should ever
//! reach for `|safe`.

use askama::Template;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};

/// Render a template, or fail loudly rather than silently serving a blank page.
///
/// A template error is a bug in this crate — a missing field or a bad expression —
/// not something a visitor caused, so it is logged at `error` with the detail and
/// the page says only that it failed.
pub fn render<T: Template>(template: &T) -> Response {
    match template.render() {
        Ok(html) => Html(html).into_response(),
        Err(err) => {
            tracing::error!(error = %err, "failed to render a template");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "the page could not be rendered",
            )
                .into_response()
        }
    }
}

#[derive(Debug, Template)]
#[template(path = "setup.html")]
pub struct SetupPage {
    /// Drives the nav in `layout.html`. Always false here: nobody can be signed in
    /// on an instance that has no account yet.
    signed_in: bool,
    username: String,
    error: Option<String>,
    min_password_len: usize,
}

impl SetupPage {
    pub fn blank() -> Self {
        Self {
            signed_in: false,
            username: String::new(),
            error: None,
            min_password_len: super::auth::MIN_PASSWORD_LEN,
        }
    }

    /// Re-render with the username preserved.
    ///
    /// Losing what they typed on a mismatched-password error is a small thing that
    /// makes a first-run form feel broken.
    pub fn rejected(username: &str, error: &str) -> Self {
        Self {
            username: username.to_owned(),
            error: Some(error.to_owned()),
            ..Self::blank()
        }
    }
}

#[derive(Debug, Template)]
#[template(path = "login.html")]
pub struct LoginPage {
    signed_in: bool,
    username: String,
    error: Option<String>,
}

impl LoginPage {
    pub fn blank() -> Self {
        Self {
            signed_in: false,
            username: String::new(),
            error: None,
        }
    }

    pub fn rejected(username: &str, error: &str) -> Self {
        Self {
            signed_in: false,
            username: username.to_owned(),
            error: Some(error.to_owned()),
        }
    }
}

/// What the library is made of, three ways — see `web::composition`.
#[derive(Debug)]
pub struct Composition {
    pub items: usize,
    /// Bytes across the whole library, pre-rendered.
    pub total_size: String,
    pub breakdowns: Vec<Breakdown>,
}

/// One roll-up: a stacked bar and the table that carries the same figures for a
/// reader who cannot see it.
#[derive(Debug)]
pub struct Breakdown {
    pub title: &'static str,
    pub hint: &'static str,
    pub segments: Vec<Segment>,
    pub rows: Vec<CompositionRow>,
    pub width: i32,
    pub height: i32,
}

/// One slice of a stacked bar, in user units. Colour is a CSS modifier suffix,
/// not a value — the same arrangement `RunBar` uses, so the palette lives in one
/// stylesheet rather than being computed per request.
#[derive(Debug)]
pub struct Segment {
    pub x: i32,
    pub w: i32,
    pub h: i32,
    pub accent: &'static str,
    pub title: String,
}

/// A [`Segment`]'s figures, for the table beneath the bar. The bar is
/// `aria-hidden`; this is what is actually read out.
#[derive(Debug)]
pub struct CompositionRow {
    pub label: String,
    pub accent: &'static str,
    pub count: usize,
    pub size: String,
    pub share: String,
}

/// The status page's four headline numbers on their own, for `/status/tiles`.
///
/// The same partial `status.html` includes, rendered without the page around it
/// so htmx can swap it in place. It carries `Glance` and nothing else on purpose:
/// the moment this needs a field from `DiagnosticsData`, polling it starts firing
/// live requests at every configured *arr app on a timer.
#[derive(Debug, Template)]
#[template(path = "_stat_tiles.html")]
pub struct StatTiles {
    pub glance: Option<Glance>,
}

/// The one page a signed-in operator lands on: what is working, what is not,
/// and why. Status and Diagnostics live together here because they answer
/// the same underlying question ("is this instance healthy") at two
/// different levels of detail, and splitting them made a person chasing
/// "why isn't this working" hunt for a second page.
#[derive(Debug, Template)]
#[template(path = "status.html")]
pub struct StatusPage {
    pub signed_in: bool,
    /// The one-glance answer to "is it working?" — n items shared, last sync,
    /// friends seen, peers in swarms. `None` only when the database itself is
    /// unavailable, which the banners below already explain.
    pub glance: Option<Glance>,
    /// Why reconciliation is not running, or `None` when it is. Carries the same
    /// string `/ready` reports, so the page and the probe cannot drift apart.
    pub blocked: Option<String>,
    /// Why `sharerr.toml` did not load, when it did not. Takes the place of
    /// `blocked`, which in this state is only relaying the same sentence.
    pub config_error: Option<String>,
    pub recovery_secs: u64,
    pub master_key_present: bool,
    pub tag: String,
    /// The *configured* torrent client — showing the unused section's URL on the
    /// "what is this instance using" page sent operators debugging the wrong
    /// service.
    pub client_name: &'static str,
    pub client_url: String,
    pub sync_enabled: bool,
    pub sync_interval_secs: u64,
    pub config_path: String,

    // ------------------------------------------------------ diagnostics
    /// The deeper checks, gathered by `diagnostics::gather` — the template
    /// reads them as `diag.x`.
    pub diag: DiagnosticsData,
}

/// What the lighthouse poller is doing, pre-rendered.
///
/// `None` on [`StatusPage`] when no lighthouse is configured — the section is
/// then omitted entirely rather than shown empty, since lighthouse is an
/// opt-in fallback and most instances never set one up.
#[derive(Debug)]
pub struct LighthouseView {
    /// Configured lighthouse URLs, whether or not one has been contacted yet.
    pub configured: usize,
    /// Rendered relative time of the last completed pass, or `None` before the
    /// first — a real state, given the 15-minute interval.
    pub last_pass: Option<String>,
    pub rows: Vec<LighthouseRow>,
    /// When a lookup last recovered a friend's address, and whose. The only
    /// evidence a lighthouse has ever actually helped.
    pub last_recovery: Option<String>,
    pub last_recovery_peer: Option<String>,
    /// Friends quiet enough to be worth looking up in the last pass. Zero is
    /// the healthy case, so the template says so rather than showing a bare 0.
    pub lookups_attempted: usize,
    /// Whether every configured lighthouse has accepted a report. Drives the
    /// section's one-line verdict.
    pub healthy: bool,
}

/// One lighthouse's report state, pre-rendered.
#[derive(Debug)]
pub struct LighthouseRow {
    pub url: String,
    /// Rendered relative time of the last accepted report, or `None` if none
    /// has ever been accepted.
    pub last_success: Option<String>,
    pub last_error: Option<String>,
}

/// The numbers an operator actually came to check, in one strip.
#[derive(Debug)]
pub struct Glance {
    /// Items currently seeding.
    pub items_shared: i64,
    /// Their combined size, pre-rendered ("412 GiB") — what "128 items"
    /// amounts to on disk, which is the number a friend's quota actually
    /// feels. Empty when nothing is seeding.
    pub shared_size: String,
    /// Rendered relative time of the last finished sync, or `None` for never.
    pub last_sync: Option<String>,
    /// What that sync amounted to — "3 added, 1 failed" — or empty when it was
    /// an uneventful pass. The error string when the run failed outright.
    pub last_sync_note: String,
    /// Whether the note is a failure, so the template can colour it honestly.
    pub last_sync_failed: bool,
    /// Friends whose key was used within the last hour — the working proxy for
    /// "connected", since a healthy Prowlarr polls well inside that.
    pub friends_recent: usize,
    pub friends_total: usize,
    /// Live peers across the tracker's swarms right now, and how many of them
    /// have the whole file. First-hand data, not an estimate — this process is
    /// the tracker.
    pub swarm_peers: usize,
    pub swarm_seeders: usize,
    /// How many distinct torrents have a live swarm. Computed alongside the
    /// two counts above and previously discarded — without it, twenty peers on
    /// one torrent and one peer on each of twenty read identically.
    pub swarm_torrents: usize,
    /// When the next periodic sync is due, rendered relative ("in ~4 min",
    /// "due now") — derived from the last finished run plus the configured
    /// interval, since the sync loop stores no deadline of its own. Empty when
    /// periodic sync is off or nothing has run yet.
    pub next_sync: String,
    /// Current CPU utilization across every core, pre-rendered ("12.3%").
    /// `None` before the background sampler's first tick has completed — see
    /// `crate::system_stats`.
    pub cpu_percent: Option<String>,
    /// Memory in use versus total, pre-rendered ("4.2 GiB of 15.6 GiB").
    /// `None` before the first sample.
    pub memory_usage: Option<String>,
    /// Disk usage of the filesystem holding the data directory, pre-rendered
    /// the same way. `None` before the first sample, or if no mounted
    /// filesystem was found covering it.
    pub disk_usage: Option<String>,
}

/// One row of the path-mapping table.
///
/// Strings rather than `PathBuf` because this is form state: a half-typed path is
/// a legitimate thing for the page to be holding, and it has to round-trip back
/// into the input exactly as typed.
#[derive(Debug, Default, Clone)]
pub struct PathRow {
    pub arr: String,
    pub sharerr: String,
    pub qbit: String,
}

impl From<&sharerr_core::config::PathMapping> for PathRow {
    fn from(m: &sharerr_core::config::PathMapping) -> Self {
        Self {
            arr: m.arr.display().to_string(),
            sharerr: m.sharerr.display().to_string(),
            qbit: m
                .qbit
                .as_ref()
                .map(|q| q.display().to_string())
                .unwrap_or_default(),
        }
    }
}

/// One row of the `[[library]]` table — form state, same as [`PathRow`].
#[derive(Debug, Clone)]
pub struct LibraryRow {
    pub path: String,
    /// The selected kind's lowercase name; the blank spare row defaults to the
    /// first option the `<select>` offers.
    pub kind: &'static str,
}

impl Default for LibraryRow {
    fn default() -> Self {
        Self {
            path: String::new(),
            kind: sharerr_core::config::LibraryKind::Tv.as_str(),
        }
    }
}

/// Which page of the guided first-run is showing. Order matches the
/// progression the roadmap describes: services, then paths, then tracker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardStep {
    Welcome,
    Services,
    Paths,
    Tracker,
    Done,
}

/// The guided first-run: a handful of the same forms `SettingsPage` renders,
/// one step at a time, each submitting to the very same `/settings/*`
/// handlers with `?next=` set so a save lands back on the wizard instead of
/// on the full Settings page. Not a separate configuration path — every
/// field here is also on Settings, unlocked and re-orderable at any time.
#[derive(Debug, Template)]
#[template(path = "wizard.html")]
pub struct WizardPage {
    pub signed_in: bool,
    pub step: WizardStep,
    pub saved: Option<String>,
    pub master_key_present: bool,
    pub locks: std::collections::BTreeMap<String, String>,

    pub tag: String,
    /// Sonarr and Radarr only — the two most instances run. The rest of the
    /// *arr apps stay on the full Settings page, same as their secondary
    /// disclosure there.
    pub arrs: Vec<ArrSection>,

    pub qbit_url: String,
    pub qbit_api_key_set: bool,
    pub qbit_category: String,
    pub qbit_tag: String,
    pub qbit_skip_checking: bool,

    pub path_map: Vec<PathRow>,

    pub tracker_advertised_host: String,
    pub tracker_port: String,
    pub tracker_advertised_url: String,
    pub tracker_token_set: bool,
}

#[derive(Debug, Template)]
#[template(path = "settings.html")]
pub struct SettingsPage {
    pub signed_in: bool,
    pub saved: Option<String>,
    pub error: Option<String>,
    /// Why `sharerr.toml` did not load, when it did not. Unlike `error` this is not
    /// about the submission that just happened — it persists until the file is
    /// repaired, and it is the reason every field below shows a default.
    pub config_error: Option<String>,
    /// Set only when the file will not even parse, and so cannot be edited in
    /// place: says where the original is kept when a save replaces it.
    pub config_notice: Option<String>,
    pub master_key_present: bool,
    /// Config paths currently pinned by a `SHARERR_*` variable, mapped to the
    /// variable's name. Consulted per field by the template.
    pub locks: std::collections::BTreeMap<String, String>,

    pub tag: String,

    /// One section per *arr app, in [`sharerr_core::MediaSource::ARRS`] order.
    /// The template renders these with a single loop, so a new app appears on
    /// this page without anyone editing HTML.
    pub arrs: Vec<ArrSection>,
    /// Whether any non-primary app is actually configured — the disclosure the
    /// secondary sections live in starts open in that case, because hiding a
    /// configured service behind a fold reads as it having vanished.
    pub secondary_arr_configured: bool,
    /// How many library sources are set up at all — *arr apps with a URL plus
    /// `[[library]]` directories — for the chip beside the section heading.
    pub library_sources_configured: usize,

    /// `"qbittorrent"`, `"transmission"`, or `"rtorrent"` — which client
    /// `torrent_backend` currently selects to actually seed. Only this one's
    /// settings render inline; the other two sit behind a fold.
    pub torrent_backend: &'static str,
    /// Whether a torrent client *other* than the selected one already holds a
    /// credential — the fold those live in starts open in that case, same
    /// reasoning as [`Self::secondary_arr_configured`].
    pub unselected_client_configured: bool,

    pub qbit_url: String,
    /// Whether a qBittorrent API key is stored — the sole credential qBittorrent
    /// authenticates with; there is no username/password fallback.
    pub qbit_api_key_set: bool,
    pub qbit_category: String,
    pub qbit_tag: String,
    pub qbit_skip_checking: bool,

    pub transmission_url: String,
    pub transmission_username: String,
    /// Whether a Transmission password is stored — its RPC has no API-key
    /// alternative, unlike qBittorrent.
    pub transmission_password_set: bool,
    pub transmission_label: String,

    /// The exact XML-RPC endpoint — not a base a path is appended to, since
    /// rTorrent has no one standard path. See `sharerr_rtorrent`'s module
    /// docs.
    pub rtorrent_url: String,
    pub rtorrent_username: String,
    /// Whether an rTorrent password is stored. rTorrent's own XML-RPC has no
    /// credential of its own — this authenticates against whatever reverse
    /// proxy fronts it.
    pub rtorrent_password_set: bool,
    pub rtorrent_label: String,

    /// Per-torrent upload cap in KiB/s, applied at add time. Empty when unset —
    /// see [`sharerr_core::config::SeedingConfig::upload_limit_kib`].
    pub seeding_upload_limit_kib: String,
    /// Seed-ratio goal, applied at add time. Empty when unset — see
    /// [`sharerr_core::config::SeedingConfig::ratio_limit`].
    pub seeding_ratio_limit: String,

    pub tracker_advertised_host: String,
    pub tracker_port: String,
    /// The expressive alternative to host+port: a full base URL with scheme and
    /// path prefix. Empty when unset.
    pub tracker_advertised_url: String,
    pub tracker_token_set: bool,
    /// Whether a rotation is in progress — the previous token is still being
    /// accepted alongside the current one. See
    /// `crate::web::settings::rotate_tracker_token`.
    pub tracker_token_previous_set: bool,
    /// Rendered relative time the previous token was last actually used to
    /// authenticate, or `None` when either no rotation is in progress or
    /// nothing has used it since this process started.
    pub tracker_token_previous_last_used: Option<String>,

    /// Whether the embedded lighthouse (`crates/sharerr-lighthouse`, run as
    /// extra routes on one of this instance's own listeners) is on.
    pub lighthouse_enabled: bool,
    /// `"frontend"` or `"tracker"` — see
    /// [`sharerr_core::config::LighthouseMount`].
    pub lighthouse_mount: &'static str,
    /// Lighthouse(s) this instance reports to and queries, one URL per line —
    /// see [`sharerr_core::config::LighthouseConfig::urls`].
    pub lighthouse_urls: String,
    /// How many lines `lighthouse_urls` holds, for the section chip — a
    /// template cannot count them itself.
    pub lighthouse_url_count: usize,

    /// gluetun's control server URL, or empty when endpoint resolution is off.
    pub gluetun_control_url: String,
    pub gluetun_enabled: bool,
    pub gluetun_api_key_set: bool,
    pub gluetun_poll_secs: u64,
    /// What the tracker-facing poller last saw and last failed with, rendered
    /// for the settings page's own small status line — the fuller version
    /// lives on Diagnostics.
    pub gluetun_last_observed: Option<String>,
    pub gluetun_last_error: Option<String>,

    /// The second poller — the torrent client's own tunnel. See
    /// `docs/ROADMAP.md`'s "a peer with two addresses".
    pub gluetun_client_control_url: String,
    pub gluetun_client_enabled: bool,
    pub gluetun_client_api_key_set: bool,
    pub gluetun_client_poll_secs: u64,
    pub gluetun_client_last_observed: Option<String>,
    pub gluetun_client_last_error: Option<String>,
    /// Whether the client poller has ever been pointed at anything — the
    /// disclosure it lives in starts open once it has, same reasoning as
    /// `secondary_arr_configured`.
    pub gluetun_client_configured: bool,

    /// A freshly minted secret, shown exactly once on the response that created it.
    /// Never populated by an ordinary page load.
    pub revealed: Option<String>,

    pub sync_enabled: bool,
    pub sync_interval_secs: u64,

    /// Whether the opt-in reachability probe is on — see
    /// [`sharerr_core::config::ChecksConfig`].
    pub checks_reachability: bool,

    /// Whether a webhook URL is stored — see
    /// `secret_keys::NOTIFICATIONS_WEBHOOK_URL`.
    pub notifications_webhook_set: bool,
    pub notifications_kind: &'static str,
    pub notifications_peer_quiet_secs: u64,

    /// One row per `[[library]]` directory, plus a spare blank row.
    pub libraries: Vec<LibraryRow>,

    /// One row per configured mapping, plus a spare blank row.
    pub path_map: Vec<PathRow>,
    /// The number of real rows in `path_map` (the spare excluded), for the
    /// section chip.
    pub path_map_count: usize,

    /// Stated on the change-password form so the rule is visible before the
    /// submission that would otherwise reject it. Comes from the constant the
    /// handler actually enforces, so the two cannot drift.
    pub min_password_len: usize,

    // Shown but not editable: changing either needs a restart, and getting `bind`
    // wrong from the UI would strand the operator on a port nothing is listening on.
    pub data_dir: String,
    pub bind: String,
    pub config_path: String,
}

/// One `<option>` in a `<select>`: the wire value and the text shown for it.
#[derive(Debug)]
pub struct SelectOption {
    pub value: &'static str,
    pub label: String,
}

/// One `<option>` in a peer-scope selector.
pub type ScopeOption = SelectOption;

/// One *arr app's section on the settings page.
#[derive(Debug)]
pub struct ArrSection {
    /// The lowercase name, used in ids, routes and test-button targets.
    pub source: &'static str,
    /// The heading form of the name.
    pub title: String,
    /// The configured URL, or empty when the app is not set up.
    pub url: String,
    pub key_set: bool,
    /// Example URL with the app's documented default port.
    pub placeholder: &'static str,
    /// The app's own upstream documentation, for the section's reference link.
    /// Empty only for a source with no upstream project, which the *arr loop
    /// never renders.
    pub docs_url: &'static str,
    /// The config path of the URL field, for the template's shared lock macros —
    /// a precomputed "is it locked" flag here would hide these fields from the
    /// test that proves every lock key in the template is a real config path.
    pub url_path: &'static str,
    /// Whether the section renders in the always-visible group. Sonarr and
    /// Radarr are what most instances run; the rest fold into a disclosure so
    /// the page is not a wall of identical forms.
    pub primary: bool,
}

/// One service's contribution to the scan behind the diagnostics page.
#[derive(Debug)]
pub struct ServiceLine {
    pub name: String,
    pub message: String,
    pub ok: bool,
    /// Where sharerr reached for it, so a line saying "could not reach it" also
    /// says *what* it could not reach — the misconfiguration is usually visible
    /// in the URL itself. Empty for a line describing a local directory, whose
    /// path is already in `name`.
    pub url: String,
}

/// One file traced through all three views of the library.
#[derive(Debug)]
pub struct SampleRow {
    pub arr: String,
    pub sharerr: String,
    pub qbit: String,
}

/// The gathered results of the checks folded into [`StatusPage`] — kept as one
/// bundle so `diagnostics`'s gathering function has a single return type
/// instead of an eleven-tuple.
#[derive(Debug)]
pub struct DiagnosticsData {
    /// Live connectivity + tag/path checks, one line per *arr app and per
    /// `[[library]]` directory — only for what is actually configured. Shared
    /// with `doctor` via `crate::checks`, so the two cannot disagree about
    /// what they found.
    pub services: Vec<ServiceLine>,
    /// Whether any tagged file was found at all. Distinguishes "everything
    /// resolves" from "there was nothing to resolve", which look identical if you
    /// only count failures.
    pub scanned: bool,
    pub rules: usize,
    pub checked: usize,
    pub unmapped: usize,
    /// Capped for display; `more_missing` carries the remainder and
    /// `missing_total` the sum — the count sentence must name the total,
    /// not the length of the capped list.
    pub missing: Vec<String>,
    pub more_missing: usize,
    pub missing_total: usize,
    pub invalid: Vec<String>,
    pub sample: Option<SampleRow>,
    /// Files that resolved to something sharerr can actually open.
    pub readable: usize,
    /// Whether anything here stops a file being shared. Drives the one-line verdict
    /// at the top, so the answer is visible without reading the whole page.
    pub healthy: bool,
    /// One row per gluetun poller (tracker, then client) — what each is
    /// pointed at, what it last saw, and what it last failed with.
    pub gluetun: Vec<EndpointStatus>,
    /// The last few sync runs, newest first — the glance above only shows the
    /// single latest one.
    pub runs: Vec<RunRow>,
    /// The same runs as a bar strip, oldest to newest. `None` when there is
    /// nothing to draw, so the template omits the figure rather than rendering
    /// an empty box above an empty-state message.
    pub run_chart: Option<RunChart>,
    /// What the lighthouse poller is doing, or `None` when none is configured
    /// — the section is omitted entirely in that case.
    pub lighthouse: Option<LighthouseView>,
}

/// One past sync run, pre-rendered for display.
#[derive(Debug)]
pub struct RunRow {
    pub when: String,
    /// The absolute instant behind `when`, for a `title=` tooltip — see
    /// `peers::absolute`. Empty for a run still in flight.
    pub when_absolute: String,
    /// How long the run took, pre-rendered. Empty while it is still running,
    /// which is also the only case where there is no end to measure to.
    pub took: String,
    /// Either the run's own error, or a summary of what it did.
    pub summary: String,
    pub failed: bool,
    /// How many items the pass found, raw rather than rendered — the one
    /// number the history strip needs a magnitude for. Zero for a run still in
    /// flight and for one that failed before it could scan anything.
    pub discovered: i64,
    /// Whether the pass actually moved anything, which the counts answer but
    /// `summary` does not: with the discovered count leading, a quiet pass and
    /// a busy one both render as a non-empty sentence.
    pub changed: bool,
}

/// One bar in the sync-history strip: a run, placed.
///
/// The same division of labour as [`Node`] — every coordinate is computed by
/// `diagnostics::run_chart` so the template only places what it is handed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunBar {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    /// `ok`, `changed` or `failed` — the modifier suffix, so the stylesheet
    /// owns the colours rather than this struct carrying them.
    pub state: &'static str,
    /// Whether to draw a full-height tint behind this bar.
    ///
    /// Set for a failed run, and it exists because height and importance point
    /// opposite ways there: a pass that broke before it could scan discovered
    /// nothing, so it earns the shortest bar on the strip — the least visible
    /// mark for the one event the strip is meant to make findable. The tint
    /// carries the failure at full height while the bar keeps telling the truth
    /// about magnitude, rather than inflating the bar and lying about both.
    pub wash: bool,
    /// Hover text, built from the row's own `when` and `summary`. Reusing
    /// those rather than re-deriving them is what keeps the strip and the
    /// table beneath it from disagreeing about the same run, the same reason
    /// `RunSummary::describe` is shared.
    pub title: String,
}

/// The sync-history strip: bars left to right, oldest to newest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunChart {
    pub bars: Vec<RunBar>,
    pub width: i32,
    pub height: i32,
}

/// One gluetun-tracked endpoint's state, pre-rendered for display.
#[derive(Debug)]
pub struct EndpointStatus {
    pub label: &'static str,
    /// Whether this poller is turned on. A poller can be `configured` (has a
    /// control server URL) but not `enabled` — the whole point of the on/off
    /// switch.
    pub enabled: bool,
    /// Whether a control server URL is set at all.
    pub configured: bool,
    /// What sharerr would advertise right now — the dynamic observation if
    /// there is one, else the static configured address, else nothing.
    pub current: Option<String>,
    /// What gluetun last actually reported, with when — `None` even for a
    /// configured poller that has not resolved yet.
    pub last_observed: Option<String>,
    pub last_poll: Option<String>,
    pub last_success: Option<String>,
    pub last_error: Option<String>,
}

/// One friend, as the peers page lists them.
///
/// Timestamps arrive pre-rendered as strings: the template has no clock and no
/// formatter, and "never" is a legitimate value for `last_seen` that a number
/// cannot express.
#[derive(Debug)]
pub struct PeerRow {
    pub id: i64,
    pub label: String,
    /// The stored scope value (`all`/`tv`/`movies`), for pre-selecting the control.
    pub scope: &'static str,
    /// How to say it to a person — "everything", "TV only", "films only".
    pub scope_label: &'static str,
    pub created: String,
    /// The absolute instant behind `created`, for a `title=` tooltip.
    pub created_absolute: String,
    /// Rendered relative time, or "never" — the answer to "is my friend actually
    /// set up?", which nothing could report before peers existed.
    pub last_seen: String,
    /// The absolute instant behind `last_seen`, empty when it is "never".
    pub last_seen_absolute: String,
    pub revoked: bool,
    /// How many seeding items this friend's scope admits right now, or
    /// `None` when the store could not say. What "Can see: TV only"
    /// actually amounts to in files.
    pub sharing: Option<usize>,
    /// The combined size of those items, pre-rendered ("41 GiB"). Empty
    /// when `sharing` is `None` or nothing is admitted.
    pub sharing_size: String,
    /// When the key was revoked, rendered relative — stored all along and never
    /// shown, so "(revoked)" gave no clue whether it happened today or a year
    /// ago. Empty for a friend who is not revoked.
    pub revoked_when: String,
    /// A truncated render of their gossip identity, or `None` until their
    /// sharerr has introduced itself.
    pub pubkey_short: Option<String>,
    /// Where their sharerr can be pulled from, or empty.
    pub gossip_url: String,
    /// Whether the key *they* issued us is stored in the vault.
    pub gossip_key_set: bool,
    /// Recently observed addresses, newest first.
    pub endpoints: Vec<PeerEndpointView>,
}

/// One observed address, rendered for the friends page.
#[derive(Debug)]
pub struct PeerEndpointView {
    pub kind: &'static str,
    pub addr: String,
    pub seen: String,
    /// "direct" or "gossip" — worth showing, because a first-hand sighting and a
    /// relayed one deserve different confidence.
    pub via: &'static str,
}

/// Friends this instance shares with, and the key each one holds.
#[derive(Debug, Template)]
#[template(path = "peers.html")]
pub struct PeersPage {
    /// One row per [`sharerr_store::PeerScope`], in `ALL` order, so both
    /// `<select>`s render from the enum — a scope added there appears here
    /// without editing HTML, and the strict form decoder can never 400 on a
    /// value this page itself offered.
    pub scope_options: Vec<ScopeOption>,
    pub signed_in: bool,
    pub peers: Vec<PeerRow>,
    pub error: Option<String>,
    /// A freshly minted peer key, shown exactly once on the response that created
    /// it — the same reveal-once rule the Torznab key follows, and for the same
    /// reason: it has to be copied into someone else's Prowlarr, but it is never
    /// readable again.
    pub revealed: Option<RevealedPeer>,
    /// The feed URL a friend pastes alongside their key.
    pub feed_url: String,
}

#[derive(Debug)]
pub struct RevealedPeer {
    pub label: String,
    pub key: String,
}

/// One `<option>` in the items page's source/state filters.
pub type FilterOption = SelectOption;

/// One column header on the items page, pre-rendered as a link that toggles
/// direction on the next click — the template has no scripting to do this
/// itself.
#[derive(Debug)]
pub struct SortLink {
    pub label: &'static str,
    /// One sentence for the header's tooltip, saying what the column means.
    pub hint: &'static str,
    pub href: String,
    /// Whether this is the column the list is currently sorted by.
    pub active: bool,
    /// "asc" / "desc" when active, empty otherwise — the template renders it
    /// as a small arrow.
    pub dir: &'static str,
}

/// One row of the items list — every file this instance has ever discovered,
/// in whatever state it is in.
#[derive(Debug)]
pub struct ItemRow {
    pub title: String,
    /// The scene-style name the feed advertises. `items::page` already filters
    /// on it, so leaving it unrendered meant matching against a string the
    /// operator could not see. Distinct from `title` by design — see
    /// `sharerr-torrent` on why conflating the two stalls seeding at 0%.
    pub release_title: String,
    /// The path exactly as the *arr app reported it, before any mapping. The
    /// first thing to check when an item will not share, and previously
    /// visible only as the single `sample` row on the status page.
    pub arr_path: String,
    /// `episode` / `movie` / `track` / `book`, for the small kind badge.
    pub kind: &'static str,
    pub source_label: String,
    /// Pre-rendered human size (`"1.5 GiB"`) — see `web::items::human_size`.
    pub size: String,
    pub state_label: String,
    /// A short explanation for a state that would otherwise read as a dead
    /// end — `Pending` with no `last_error` looks identical whether it is
    /// mid-sync or has been stuck since a crash, and `Unshared` gives no hint
    /// that it is not a fault at all. `None` for `Seeding` and `Failed`,
    /// which already explain themselves (the second via `last_error`).
    pub state_hint: Option<&'static str>,
    /// What the torrent client reports for this item's achieved ratio —
    /// see `web::items::ratio_cell`. Empty before a torrent has reported
    /// anything, rendered as a dash the same way `peers` is.
    pub ratio: String,
    /// The client's own per-torrent limit if it reports a fixed one, or an
    /// explanation of why it doesn't, for hover.
    pub ratio_hint: String,
    /// Which friends' scopes admit this item, joined for display — empty
    /// unless the item is actually seeding, since nothing else reaches a
    /// friend's feed.
    pub visible_to: String,
    pub since: String,
    /// The full 40-character hash, or `None` before a torrent exists.
    pub info_hash: Option<String>,
    /// The first twelve characters of `info_hash`, for the cell itself; the
    /// full value rides on the tooltip and the copy button.
    pub info_hash_short: Option<String>,
    /// `"2↑ 1↓"`: who the tracker currently sees in this torrent's swarm.
    /// Empty when nobody is announcing, which the template shows as a dash.
    pub peers: String,
    /// The long form of `peers` — `"2 seeding · 1 downloading"` — for hover.
    pub peers_hint: String,
    /// `"Sonarr series 42, file 1337"`: the *arr's own identifiers, shown on
    /// hover over the source cell because they are what an operator greps
    /// the *arr's logs for.
    pub source_hint: String,
    /// Where this torrent currently announces — the same URL a freshly built
    /// torrent would carry, computed live rather than stored, since it tracks
    /// whatever the endpoint currently resolves to. `None` before a torrent
    /// exists, or when nothing is configured to announce to.
    pub announce_url: Option<String>,
    /// A short fingerprint of the token this item's torrent was last confirmed
    /// to announce with, and whether it still matches the currently configured
    /// one — see `sync::token_fingerprint`. `None` alongside
    /// [`TokenStatus::None`] before a torrent exists.
    pub token_fp: Option<String>,
    pub token_status: TokenStatus,
    /// The metadata IDs carried in the feed — "tvdb 12345 · imdb tt0111161" —
    /// which are what a friend's *arr matches the release on. Empty when
    /// the item has none, which is itself the reason a friend's app
    /// would ignore it.
    pub ids: String,
    pub last_error: Option<String>,
    /// Whether sharerr added this torrent itself, rather than reusing one that
    /// already covered the file. Changes what withdrawing the item does, so it
    /// is worth seeing before withdrawing anything — see
    /// `SharedItem::created_by_sharerr`. Meaningless before a torrent exists,
    /// which `info_hash: None` already says.
    pub created_by_sharerr: bool,
    /// The absolute instant behind `since`, for a `title=` tooltip.
    pub since_absolute: String,
}

/// Whether an item's confirmed announce-token fingerprint still matches the
/// currently configured token. Three states, not two: "no token in use" is
/// not a fault the way "used to match and no longer does" is, and collapsing
/// them would make an instance that has never set a tracker token look
/// exactly like one whose token just rotated out from under every torrent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenStatus {
    /// No token configured, currently or as last recorded — nothing to check.
    None,
    /// Matches the currently configured token.
    Valid,
    /// Does not — either the token rotated since this item's torrent was last
    /// confirmed, or it has never been confirmed at all.
    Stale,
}

impl TokenStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "no token",
            Self::Valid => "valid",
            Self::Stale => "changed",
        }
    }

    /// The `.field-status--*` modifier this status renders with — reusing the
    /// settings page's set/unset pill styling rather than inventing a second
    /// small-badge vocabulary for the same shape of question.
    pub fn css_class(self) -> &'static str {
        match self {
            Self::None => "field-status--unset",
            Self::Valid => "field-status--set",
            Self::Stale => "field-status--stale",
        }
    }
}

/// This instance's own addresses plus a copy-pasteable script for checking
/// them from another machine — see [`crate::web::debug`] for why the script
/// exists rather than a button that runs the check here.
#[derive(Debug, Template)]
#[template(path = "debug.html")]
pub struct DebugPage {
    pub signed_in: bool,
    pub tracker_base: Option<String>,
    pub client_base: Option<String>,
    pub feed_base: String,
    pub bind: String,
    pub tracker_bind: Option<String>,
    pub script: String,
}

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

/// The running binary's version, for the page footer — so a bug report or a
/// "which build has that fix" question can be answered from any page.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
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

/// One state's share of the library, for the tally above the items table.
#[derive(Debug)]
pub struct StateCount {
    pub label: String,
    pub count: usize,
}

/// Every file sharerr has discovered, sortable and filterable — the page
/// `docs/ROADMAP.md` names as what an operator wants right after setup.
#[derive(Debug, Template)]
#[template(path = "items.html")]
pub struct ItemsPage {
    pub signed_in: bool,
    pub error: Option<String>,
    pub items: Vec<ItemRow>,
    /// Rows before filtering — so "12 of 340" is answerable without a second
    /// query.
    pub total: usize,
    pub shown: usize,
    /// How the whole library breaks down by state, counted before any filter
    /// is applied. "128 of 132 seeding" is the question the page exists to
    /// answer, and it was previously only derivable by filtering four times.
    pub state_counts: Vec<StateCount>,
    /// Bytes across every `Seeding` item in the library, before filters, and
    /// across the rows currently shown — both via `web::items::human_size`.
    pub seeding_size: String,
    pub shown_size: String,
    pub source_options: Vec<FilterOption>,
    pub state_options: Vec<FilterOption>,
    pub kind_options: Vec<FilterOption>,
    pub source_filter: String,
    pub state_filter: String,
    pub kind_filter: String,
    pub q: String,
    pub sort_links: Vec<SortLink>,
    /// How the whole library breaks down by format, state and source — counted
    /// over the same unfiltered rows `state_counts` is, and `None` when there is
    /// nothing to break down.
    pub composition: Option<Composition>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_hostile_username_is_escaped_rather_than_rendered() {
        // The username is echoed back into the form after a failed attempt, which
        // makes it the one attacker-controlled string on these pages.
        let page = LoginPage::rejected("<script>alert(1)</script>", "nope");
        let html = page.render().unwrap();

        assert!(!html.contains("<script>alert(1)</script>"), "{html}");
        assert!(html.contains("&#60;script&#62;"), "{html}");
    }

    #[test]
    fn an_error_message_is_escaped_too() {
        let page = SetupPage::rejected("ivy", "<b>bad</b>");
        let html = page.render().unwrap();
        assert!(!html.contains("<b>bad</b>"), "{html}");
    }

    #[test]
    fn the_setup_page_states_the_password_rule_it_enforces() {
        // The form's `minlength` and the server-side check are the same number, so
        // a user is never told one thing and refused by another.
        let html = SetupPage::blank().render().unwrap();
        assert!(html.contains(&format!(
            "minlength=\"{}\"",
            super::super::auth::MIN_PASSWORD_LEN
        )));
        assert!(html.contains(&format!(
            "At least {} characters",
            super::super::auth::MIN_PASSWORD_LEN
        )));
    }

    #[test]
    fn signed_out_pages_do_not_offer_navigation() {
        let html = LoginPage::blank().render().unwrap();
        assert!(!html.contains("Sign out"), "{html}");
        assert!(!html.contains("/settings"), "{html}");
    }

    /// Every settings path named in `settings.html` must be one the code actually
    /// writes.
    ///
    /// The template decides whether to disable a field by looking its dotted path
    /// up in a map whose keys are *generated* at runtime from `SHARERR_*`
    /// environment variables. Nothing tied those hand-typed literals to the schema,
    /// so a typo here compiled cleanly and simply never matched — rendering a field
    /// as editable while the environment had it pinned, and silently discarding the
    /// save. This is the check that closes that gap from the template side;
    /// `sharerr_core::config` covers the Rust side.
    #[test]
    fn every_lock_key_in_the_template_is_a_known_config_path() {
        use sharerr_core::config::config_paths;

        let template = include_str!("templates/settings.html");
        let mut found = std::collections::BTreeSet::new();

        for line in template.lines() {
            let mut rest = line;
            while let Some(at) = rest
                .find("call lock_attr(\"")
                .or_else(|| rest.find("call locked(\""))
            {
                let after = &rest[at..];
                let Some(open) = after.find('"') else { break };
                let Some(close) = after[open + 1..].find('"') else {
                    break;
                };
                found.insert(after[open + 1..open + 1 + close].to_owned());
                rest = &after[open + 1 + close..];
            }
        }

        assert!(
            !found.is_empty(),
            "parsed no lock keys — the macro syntax changed and this test went blind"
        );

        for key in &found {
            assert!(
                config_paths::ALL.contains(&key.as_str()),
                "settings.html locks {key:?}, which is not in config_paths::ALL — \
                 a typo here disables nothing and the save is discarded silently"
            );
        }
    }
}
