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
    /// Why reconciliation is not running, or `None` when it is. Carries the same
    /// string `/ready` reports, so the page and the probe cannot drift apart.
    pub blocked: Option<String>,
    /// Why `sharerr.toml` did not load, when it did not. Takes the place of
    /// `blocked`, which in this state is only relaying the same sentence.
    pub config_error: Option<String>,
    pub recovery_secs: u64,
    pub master_key_present: bool,
    pub tag: String,
    pub sonarr_url: Option<String>,
    pub radarr_url: Option<String>,
    pub qbit_url: String,
    pub sync_enabled: bool,
    pub sync_interval_secs: u64,
    pub config_path: String,
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

    pub sonarr_url: String,
    pub sonarr_key_set: bool,
    pub radarr_url: String,
    pub radarr_key_set: bool,

    pub qbit_url: String,
    pub qbit_username: String,
    pub qbit_password_set: bool,
    pub qbit_category: String,
    pub qbit_tag: String,
    pub qbit_skip_checking: bool,

    /// Flattened to a bool for the template's benefit — there are exactly two
    /// backends, and a `<select>` with two options needs no more than this.
    pub tracker_builtin: bool,
    pub tracker_advertised_host: String,
    pub tracker_port: String,
    pub tracker_token_set: bool,

    pub torznab_key_set: bool,
    /// The `/api` URL a friend pastes into Prowlarr, built from the advertised host.
    pub torznab_url: String,
    /// Whether the builtin tracker is the selected backend, which is what makes the
    /// announce endpoint on this instance the one in use.
    pub tracker_builtin_selected: bool,
    /// A freshly minted secret, shown exactly once on the response that created it.
    /// Never populated by an ordinary page load.
    pub revealed: Option<String>,

    pub sync_enabled: bool,
    pub sync_interval_secs: u64,

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
    pub created: String,
    /// Rendered relative time, or "never" — the answer to "is my friend actually
    /// set up?", which nothing could report before peers existed.
    pub last_seen: String,
    pub revoked: bool,
}

/// Friends this instance shares with, and the key each one holds.
#[derive(Debug, Template)]
#[template(path = "peers.html")]
pub struct PeersPage {
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
    /// Whether the legacy single shared key is still set. While it is, revoking a
    /// peer does not fully cut them off, so the page has to say so.
    pub shared_key_set: bool,
}

#[derive(Debug)]
pub struct RevealedPeer {
    pub label: String,
    pub key: String,
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
