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

    pub sync_enabled: bool,
    pub sync_interval_secs: u64,

    pub path_map: Vec<PathRow>,

    // Shown but not editable: changing either needs a restart, and getting `bind`
    // wrong from the UI would strand the operator on a port nothing is listening on.
    pub data_dir: String,
    pub bind: String,
    pub config_path: String,
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
}
