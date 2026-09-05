//! Askama template structs and the one place they are turned into responses.
//!
//! Askama escapes `{{ }}` for HTML by default, which is what keeps a username or
//! an error message containing `<` from becoming markup. Nothing here should ever
//! reach for `|safe`.

use askama::Template;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};

mod topology;
pub use topology::*;

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

/// The running binary's version, for the page footer — so a bug report or a
/// "which build has that fix" question can be answered from any page.
pub fn version() -> &'static str {
    crate::VERSION
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
    /// Rendered relative time of the last hourly sample that saw at least one
    /// peer — see `migrations/0011_swarm_samples.sql`. Only meaningful, and
    /// only rendered, when `swarm_peers` is `0`: it is what tells "nobody is
    /// here right now" apart from "nobody has been here in a fortnight",
    /// which otherwise read identically. `None` when no sample has ever seen
    /// a peer, including before the sampler's first hour has passed.
    pub swarm_quiet_since: Option<String>,
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

    /// The tracker-facing poller's own section on the settings page.
    pub gluetun: GluetunSection,
    /// The second poller — the torrent client's own tunnel, for a peer
    /// reachable at two different addresses depending on which one answers.
    pub gluetun_client: GluetunSection,
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
    pub notifications_trigger_sync_failed: bool,
    pub notifications_trigger_peer_quiet: bool,
    pub notifications_trigger_endpoint_rotated: bool,
    pub notifications_trigger_items_shared: bool,
    pub notifications_trigger_item_failed: bool,
    pub notifications_trigger_peer_revoked: bool,

    /// Whether `/metrics` and the dashboard-widget endpoint answer at all —
    /// see [`sharerr_core::config::MetricsConfig`].
    pub metrics_enabled: bool,
    /// Whether the bearer token they require is stored — see
    /// `secret_keys::METRICS_TOKEN`.
    pub metrics_token_set: bool,

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

/// One gluetun poller's section on the settings page — the tracker poller and
/// the torrent client's own poller share this shape and one form macro,
/// distinguished by `target`.
#[derive(Debug)]
pub struct GluetunSection {
    /// Which poller this is — resolves the form action, config paths, and
    /// vault key on the Rust side, and the copy that differs between the two
    /// sections in the template.
    pub target: crate::gluetun::GluetunTarget,
    /// The configured control server URL, or empty when off.
    pub control_url: String,
    pub enabled: bool,
    pub api_key_set: bool,
    pub poll_secs: u64,
    /// The three config paths for this target's fields, for the template's
    /// shared lock macros — precomputed rather than derived in the template,
    /// same reasoning as `ArrSection::url_path`.
    pub enabled_path: &'static str,
    pub control_url_path: &'static str,
    pub poll_secs_path: &'static str,
    /// What this poller last saw and last failed with, rendered for the
    /// settings page's own small status line — the fuller version lives on
    /// Diagnostics.
    pub last_observed: Option<String>,
    pub last_error: Option<String>,
}

impl GluetunSection {
    /// The three path fields are derived from `target`, so a caller only
    /// supplies what actually varies per poller.
    pub fn new(
        target: crate::gluetun::GluetunTarget,
        control_url: String,
        enabled: bool,
        api_key_set: bool,
        poll_secs: u64,
        last_observed: Option<String>,
        last_error: Option<String>,
    ) -> Self {
        let (enabled_path, control_url_path, poll_secs_path) = target.config_paths();
        Self {
            target,
            control_url,
            enabled,
            api_key_set,
            poll_secs,
            enabled_path,
            control_url_path,
            poll_secs_path,
            last_observed,
            last_error,
        }
    }
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
    /// Up to a fortnight of hourly swarm-activity samples, oldest to newest —
    /// see `migrations/0011_swarm_samples.sql`. `None` when nothing has been
    /// sampled yet, same convention as `run_chart`.
    pub swarm_chart: Option<SwarmChart>,
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

/// One hour's swarm sample, drawn as a contiguous bar — see [`SwarmChart`]
/// for why these have no gap and no state colour, unlike [`RunBar`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwarmBar {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    /// Hover text: when this sample was taken and what it saw.
    pub title: String,
}

/// The swarm-history strip: up to a fortnight of hourly samples, oldest to
/// newest, plus one pre-rendered sentence answering what the chart shows at
/// a glance.
///
/// A fortnight of hourly bars is up to 336 of them — an order of magnitude
/// more than [`RunChart`] ever draws, so bars here are contiguous and sized
/// to fill a fixed total width rather than [`RunBar`]'s fixed per-bar width
/// and gap; at this count a gap would vanish and a fixed width would make
/// the strip several thousand pixels wide. Individual hourly samples are
/// also not the individually-meaningful events sync runs are, so the
/// accessible equivalent here is `summary`, one sentence, rather than
/// `RunChart`'s full per-row table — a table of 336 nearly-identical rows
/// would tell a screen reader user less than the sentence does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwarmChart {
    pub bars: Vec<SwarmBar>,
    pub width: i32,
    pub height: i32,
    pub summary: String,
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
    /// The natural-key pair a detail-page link or action form addresses this
    /// item by — items have no single-column id, see `SharedItem::key`.
    pub source: &'static str,
    pub file_id: i64,
}

/// Whether an item's confirmed announce-token fingerprint still matches one
/// of the tokens the tracker currently admits. Four states, not two: "no
/// token in use" is not a fault the way "used to match and no longer does"
/// is, and collapsing them would make an instance that has never set a
/// tracker token look exactly like one whose token just rotated out from
/// under every torrent. A third state sits between those two extremes:
/// during a rotation the tracker admits both the current token and the one
/// it replaced (see `ServeState::tracker_tokens`), so an item still on the
/// previous token is genuinely still being served — showing it identically
/// to one that is fully cut off (`Stale`) would be misleading in the exact
/// window an operator most wants an accurate signal in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenStatus {
    /// No token configured, currently or as last recorded — nothing to check.
    None,
    /// Matches the currently configured token.
    Valid,
    /// Does not match the current token, but does match the previous one —
    /// still admitted while a rotation is in progress, on borrowed time.
    Rotating,
    /// Matches neither the current nor the previous token — either the
    /// token rotated with no grace window covering it, or it has never been
    /// confirmed at all.
    Stale,
}

impl TokenStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "no token",
            Self::Valid => "valid",
            Self::Rotating => "rotating",
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
            Self::Rotating => "field-status--warn",
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

/// One state's share of the library, for the tally above the items table.
#[derive(Debug)]
pub struct StateCount {
    pub label: String,
    pub count: usize,
}

/// Every file sharerr has discovered, sortable and filterable — the page
/// an operator wants right after setup.
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

/// Everything about one item, reached from a row on [`ItemsPage`] — built
/// specifically to put **release title against the file's actual name,
/// side by side**, the reason worth building a whole page for: conflating
/// the two is the first trap
/// `CLAUDE.md` lists, and there was previously no view anywhere that showed
/// both at once. Also carries the manual actions an operator otherwise has
/// no way to trigger without editing tags in the source app.
#[derive(Debug, Template)]
#[template(path = "item_detail.html")]
pub struct ItemDetailPage {
    pub signed_in: bool,
    /// The natural key, for the three action forms' `action=` URLs.
    pub source: &'static str,
    pub file_id: i64,

    pub title: String,
    pub release_title: String,
    /// The file's actual name on disk, from `SharedItem::arr_path` — what a
    /// torrent sharerr built for this item is named, since the torrent's own
    /// name always describes the file where it sits. `None` only if the path
    /// somehow has no final component.
    pub file_name: Option<String>,
    pub kind: &'static str,
    pub source_label: String,
    pub source_hint: String,
    pub size: String,
    pub state_label: String,
    pub state_hint: Option<&'static str>,
    /// The failure reason in full — [`ItemRow::last_error`] is the same
    /// string, but rendered here without the table cell's width constraint.
    pub last_error: Option<String>,
    pub since: String,
    pub since_absolute: String,
    pub info_hash: Option<String>,
    pub created_by_sharerr: bool,
    pub ratio: String,
    pub ratio_hint: String,
    pub token_fp: Option<String>,
    pub token_status: TokenStatus,
    pub announce_url: Option<String>,
    pub ids: String,
    /// Which friends' scopes admit this item — empty unless it is actually
    /// seeding, same rule [`ItemRow::visible_to`] follows.
    pub visible_to: String,
    /// Present only when the item carries something — a probe or an *arr's
    /// own `mediaInfo` found nothing, or nothing has looked yet.
    pub media: Option<sharerr_core::MediaMeta>,

    pub arr_path: String,
    pub sharerr_path: String,
    pub qbit_path: String,
    /// Whether a `[[path_map]]` rule actually matched — `false` means every
    /// container shares the same mounts, or a mapping bug `doctor` would
    /// also catch.
    pub mapping_applied: bool,
    /// Whether the resolved sharerr-side path exists on disk right now.
    /// `None` when the path could not be resolved at all (a non-absolute
    /// `arr_path`, which `doctor` would already be reporting as a problem).
    pub path_exists: Option<bool>,

    /// The tracker's own live view of this torrent's swarm — `None` before a
    /// torrent exists or when nobody is announcing right now.
    pub swarm: Option<SwarmRow>,

    /// Whether the action forms below should render at all — the operator's
    /// last click, or a store error, or nothing to say.
    pub can_retry: bool,
    pub can_rebuild: bool,
    pub can_unshare: bool,

    /// A flash message from the action just taken, and whether it reads as
    /// a failure — same one-shot, redirect-then-render convention every
    /// other mutating page on this app uses.
    pub message: Option<String>,
    pub message_failed: bool,
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
        let page = SetupPage::rejected("operator", "<b>bad</b>");
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

        let template = include_str!("settings.html");
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

    /// Every variant of the two topology enums renders to something the
    /// template can use: a status maps to a CSS class, and every drawn edge
    /// style has both a dash pattern and a tooltip, while `None` has neither
    /// (no line, so nothing to explain).
    #[test]
    fn every_node_status_and_edge_style_renders() {
        assert_eq!(NodeStatus::Ok.css_class(), "ok");
        assert_eq!(NodeStatus::Warn.css_class(), "warn");
        assert_eq!(NodeStatus::Error.css_class(), "error");
        assert_eq!(NodeStatus::Unknown.css_class(), "hint");

        for style in [
            EdgeStyle::Solid,
            EdgeStyle::Dashed,
            EdgeStyle::Dotted,
            EdgeStyle::Sparse,
        ] {
            assert!(style.dasharray().is_some(), "{style:?}");
            assert!(!style.describe().is_empty(), "{style:?}");
        }
        assert_eq!(EdgeStyle::None.dasharray(), None);
        assert_eq!(EdgeStyle::None.describe(), "");
    }
}
