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
    /// Capped for display; `more_missing` carries the remainder.
    pub missing: Vec<String>,
    pub more_missing: usize,
    pub invalid: Vec<String>,
    pub sample: Option<SampleRow>,
    /// Files that resolved to something sharerr can actually open.
    pub readable: usize,
    /// Whether anything here stops a file being shared. Drives the one-line verdict
    /// at the top, so the answer is visible without reading the whole page.
    pub healthy: bool,
    /// One row per gluetun poller (tracker, then client) — what each is
    /// pointed at, what it last saw, and what it last failed with. See
    /// `docs/ROADMAP.md`'s "gluetun observability" and "a peer with two
    /// addresses".
    pub gluetun: Vec<EndpointStatus>,
    /// Live swarm counts from the tracker's own bookkeeping — not a config
    /// check like the rest of the page, but the other half of "is networking
    /// actually working": credentials can all be green while no peer has
    /// ever announced.
    pub swarm_peers: usize,
    pub swarm_seeders: usize,
    /// The last few sync runs, newest first — the glance above only shows the
    /// single latest one.
    pub runs: Vec<RunRow>,
}

/// The numbers an operator actually came to check, in one strip.
#[derive(Debug)]
pub struct Glance {
    /// Items currently seeding.
    pub items_shared: i64,
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

    /// `"qbittorrent"`, `"transmission"`, or `"rtorrent"` — which client
    /// `torrent_backend` currently selects to actually seed.
    pub torrent_backend: &'static str,

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

    /// Whether the embedded lighthouse (`crates/sharerr-lighthouse`, run as
    /// extra routes on one of this instance's own listeners) is on.
    pub lighthouse_enabled: bool,
    /// `"frontend"` or `"tracker"` — see
    /// [`sharerr_core::config::LighthouseMount`].
    pub lighthouse_mount: &'static str,
    /// Lighthouse(s) this instance reports to and queries, one URL per line —
    /// see [`sharerr_core::config::LighthouseConfig::urls`].
    pub lighthouse_urls: String,

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

    /// Whether a webhook URL is stored — see
    /// `secret_keys::NOTIFICATIONS_WEBHOOK_URL`.
    pub notifications_webhook_set: bool,
    pub notifications_kind: &'static str,
    pub notifications_peer_quiet_secs: u64,

    /// One row per `[[library]]` directory, plus a spare blank row.
    pub libraries: Vec<LibraryRow>,

    pub path_map: Vec<PathRow>,

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

/// One `<option>` in a peer-scope selector.
#[derive(Debug)]
pub struct ScopeOption {
    pub value: &'static str,
    pub label: String,
}

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
    pub services: Vec<ServiceLine>,
    pub scanned: bool,
    pub rules: usize,
    pub checked: usize,
    pub unmapped: usize,
    pub missing: Vec<String>,
    pub more_missing: usize,
    pub invalid: Vec<String>,
    pub sample: Option<SampleRow>,
    pub readable: usize,
    pub healthy: bool,
    pub gluetun: Vec<EndpointStatus>,
    pub swarm_peers: usize,
    pub swarm_seeders: usize,
    pub runs: Vec<RunRow>,
}

/// One past sync run, pre-rendered for display.
#[derive(Debug)]
pub struct RunRow {
    pub when: String,
    /// Either the run's own error, or a summary of what it did.
    pub summary: String,
    pub failed: bool,
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
    /// Rendered relative time, or "never" — the answer to "is my friend actually
    /// set up?", which nothing could report before peers existed.
    pub last_seen: String,
    pub revoked: bool,
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
#[derive(Debug)]
pub struct FilterOption {
    pub value: &'static str,
    pub label: String,
}

/// One column header on the items page, pre-rendered as a link that toggles
/// direction on the next click — the template has no scripting to do this
/// itself.
#[derive(Debug)]
pub struct SortLink {
    pub label: &'static str,
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
    /// Which friends' scopes admit this item, joined for display — empty
    /// unless the item is actually seeding, since nothing else reaches a
    /// friend's feed.
    pub visible_to: String,
    pub since: String,
    /// The full 40-character hash, or `None` before a torrent exists.
    pub info_hash: Option<String>,
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
    pub last_error: Option<String>,
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
    pub source_options: Vec<FilterOption>,
    pub state_options: Vec<FilterOption>,
    pub source_filter: String,
    pub state_filter: String,
    pub q: String,
    pub sort_links: Vec<SortLink>,
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
