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
    /// One row per *arr app, configured or not, in `MediaSource::ARRS` order,
    /// then one per `[[library]]` directory.
    pub services: Vec<ServiceUrl>,
    /// The *configured* torrent client — showing the unused section's URL on the
    /// "what is this instance using" page sent operators debugging the wrong
    /// service.
    pub client_name: &'static str,
    pub client_url: String,
    pub sync_enabled: bool,
    pub sync_interval_secs: u64,
    pub config_path: String,
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

    pub qbit_url: String,
    /// Whether a qBittorrent API key is stored — the sole credential qBittorrent
    /// authenticates with; there is no username/password fallback.
    pub qbit_api_key_set: bool,
    pub qbit_category: String,
    pub qbit_tag: String,
    pub qbit_skip_checking: bool,

    pub tracker_advertised_host: String,
    pub tracker_port: String,
    /// The expressive alternative to host+port: a full base URL with scheme and
    /// path prefix. Empty when unset.
    pub tracker_advertised_url: String,
    pub tracker_token_set: bool,

    /// gluetun's control server URL, or empty when endpoint resolution is off.
    pub gluetun_control_url: String,
    pub gluetun_poll_secs: u64,

    /// A freshly minted secret, shown exactly once on the response that created it.
    /// Never populated by an ordinary page load.
    pub revealed: Option<String>,

    pub sync_enabled: bool,
    pub sync_interval_secs: u64,

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

/// One *arr app's row on the status page.
#[derive(Debug)]
pub struct ServiceUrl {
    pub title: String,
    pub url: Option<String>,
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

/// The check that used to be reachable only from a shell.
///
/// `doctor` resolves the path mappings and reports what it finds; the web UI's
/// per-service "Test connection" buttons deliberately do not, because they answer a
/// one-line question and this needs a library walk. So the check most likely to
/// explain "nothing is shared" was the one an operator using only the browser could
/// never run.
#[derive(Debug, Template)]
#[template(path = "diagnostics.html")]
pub struct DiagnosticsPage {
    pub signed_in: bool,
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

/// One release as this friend's Torznab client would receive it.
#[derive(Debug)]
pub struct FeedPreviewRow {
    pub title: String,
    pub category: &'static str,
    pub size: String,
    pub download_url: String,
    /// Empty until the release has an info hash, same as the real feed.
    pub magnet_url: String,
}

/// A friend's feed, rendered with their own scope and their own links — the
/// honest test of scoping, run from the operator's browser instead of a
/// hand-crafted Torznab query.
#[derive(Debug, Template)]
#[template(path = "feed_preview.html")]
pub struct FeedPreviewPage {
    pub signed_in: bool,
    pub peer_label: String,
    pub peer_scope_label: &'static str,
    pub total: usize,
    pub items: Vec<FeedPreviewRow>,
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
    /// Which friends' scopes admit this item, joined for display — empty
    /// unless the item is actually seeding, since nothing else reaches a
    /// friend's feed.
    pub visible_to: String,
    pub since: String,
    /// A truncated info hash, or `None` before a torrent exists.
    pub info_hash_short: Option<String>,
    pub last_error: Option<String>,
}

/// Every file sharerr has discovered, sortable and filterable — the page
/// `docs/roadmap.md` used to describe as the thing "an operator asks for after
/// setup and the last thing they can currently get".
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
