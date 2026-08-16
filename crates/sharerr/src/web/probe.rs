//! "Test connection" — the button next to each service on the settings page.
//!
//! This exists because every way sharerr fails to reach a service produces the
//! same symptom: nothing happens. `sharerr doctor` turns that into an actionable
//! line, but it is a CLI command, and the whole point of the web UI is that an
//! operator should never need one. So each service gets a button that answers the
//! two questions a wrong setting raises — *can I reach it* and *does it accept the
//! credential* — separately, because the fixes are different.
//!
//! The checking itself lives in [`crate::checks`], shared with `doctor`. This file
//! is only the renderer: it turns one outcome into one badge. Deciding what is true
//! in two places is what let the two tools drift into describing different
//! conditions in similar words.
//!
//! Deliberately narrower than `doctor`: path-mapping resolution and tracker state
//! are not checked here. Those need a library to walk and are worth a page of
//! their own rather than a one-line badge.

use axum::extract::{Path, State};
use axum::response::{Html, IntoResponse, Response};
use sharerr_core::config::secret_keys;
use sharerr_core::{Config, MediaSource};

use super::WebState;
use crate::checks::{ArrOutcome, QbitOutcome, check_arr, check_qbit};

/// Run one service's check and return the HTML fragment htmx swaps in.
///
/// Always `200`, even when the check fails: htmx does not swap non-2xx responses
/// by default, so a `502` here would leave the operator staring at a button that
/// appears to do nothing — the exact failure mode this page exists to remove.
pub async fn test(State(state): State<WebState>, Path(service): Path<String>) -> Response {
    let config = state.serve.config().await;

    let outcome = if service == "qbittorrent" {
        qbit_badge(&state, &config).await
    } else if let Some(kind) = MediaSource::parse(&service) {
        arr_badge(kind, &state, &config).await
    } else {
        Outcome::Bad("Unknown service.".to_owned())
    };

    outcome.into_fragment()
}

enum Outcome {
    Good(String),
    Bad(String),
}

impl Outcome {
    /// Render as an escaped inline badge.
    ///
    /// The messages interpolate a service's own error text, which is remote input;
    /// escaping it here is what stops a hostile *arr response becoming markup on
    /// the settings page.
    fn into_fragment(self) -> Response {
        let (class, message) = match self {
            Self::Good(message) => ("ok", message),
            Self::Bad(message) => ("error", message),
        };

        Html(format!(
            r#"<span class="{class}">{}</span>"#,
            askama::filters::escape(&message, askama::filters::Html)
                .map_or_else(|_| String::from("(unprintable)"), |e| e.to_string())
        ))
        .into_response()
    }
}

async fn arr_badge(kind: MediaSource, state: &WebState, config: &Config) -> Outcome {
    let (service, key) = (config.service(kind), secret_keys::api_key_for(kind));

    let api_key = state.secret(key).await;
    let outcome = check_arr(
        kind,
        service.map(|service| &service.url),
        api_key,
        &config.tag,
    )
    .await;

    // Second person and a next action, which is what distinguishes these from
    // `doctor`'s report — the operator is looking at the field they just filled in.
    match outcome {
        ArrOutcome::NotConfigured => Outcome::Bad("No URL configured. Save one first.".to_owned()),
        ArrOutcome::NoCredential => Outcome::Bad("No API key stored. Save one first.".to_owned()),
        ArrOutcome::CredentialUnreadable(reason) | ArrOutcome::BadUrl(reason) => {
            Outcome::Bad(reason)
        }
        ArrOutcome::Unreachable(reason) => Outcome::Bad(format!("Could not reach it: {reason}")),
        ArrOutcome::AuthRejected => {
            Outcome::Bad("Reached it, but the API key was rejected.".to_owned())
        }
        ArrOutcome::Failed(reason) => Outcome::Bad(reason),
        ArrOutcome::TagMissing { version } => Outcome::Bad(format!(
            "Connected to {version}, but no tag named {:?} exists there yet — \
             create it and tag something, or nothing will ever be shared.",
            config.tag
        )),
        // Not an error: the tag is there and simply has nothing on it yet, which is
        // the normal state right after setup. Saying so is still worth a badge,
        // because it is the most common reason sharerr appears to do nothing.
        ArrOutcome::TagUnused { version } => Outcome::Good(format!(
            "Connected to {version}; the tag exists but nothing carries it yet.",
        )),
        ArrOutcome::Ready { version, items, .. } => Outcome::Good(format!(
            "Connected to {version}; {} file(s) tagged {:?}.",
            items.len(),
            config.tag
        )),
    }
}

async fn qbit_badge(state: &WebState, config: &Config) -> Outcome {
    // Which client, and therefore which URL and which vault key, is a configuration
    // choice — the button cannot infer it from the URL, because two clients can
    // live on the same host.
    let client = config.torrent_client();

    let password = state.secret(client.password_key).await;
    let outcome = check_qbit(
        config.torrent_backend,
        client.url,
        client.username,
        password,
    )
    .await;

    match outcome {
        QbitOutcome::NoCredential => Outcome::Bad("No password stored. Save one first.".to_owned()),
        QbitOutcome::CredentialUnreadable(reason) | QbitOutcome::BadUrl(reason) => {
            Outcome::Bad(reason)
        }
        QbitOutcome::Unreachable(reason) => Outcome::Bad(format!("Could not reach it: {reason}")),
        QbitOutcome::AuthRejected => {
            Outcome::Bad("Reached it, but the username or password was rejected.".to_owned())
        }
        QbitOutcome::Failed(reason) => Outcome::Bad(format!("Signed in, but: {reason}")),
        QbitOutcome::Ready { version, kind, .. } => {
            Outcome::Good(format!("Signed in to {kind} {version}."))
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use axum::body::to_bytes;

    async fn body_of(outcome: Outcome) -> String {
        let response = outcome.into_fragment();
        let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn a_hostile_error_message_is_escaped() {
        // The failure text can carry a remote service's own words straight through,
        // which makes it the one attacker-influenced string on this page.
        let html = body_of(Outcome::Bad("<img src=x onerror=alert(1)>".to_owned())).await;

        assert!(!html.contains("<img"), "{html}");
        assert!(html.contains("class=\"error\""), "{html}");
    }

    #[tokio::test]
    async fn success_and_failure_are_distinguishable_by_class() {
        assert!(
            body_of(Outcome::Good("fine".to_owned()))
                .await
                .contains("class=\"ok\"")
        );
        assert!(
            body_of(Outcome::Bad("nope".to_owned()))
                .await
                .contains("class=\"error\"")
        );
    }
}
