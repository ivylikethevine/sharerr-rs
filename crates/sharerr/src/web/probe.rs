//! "Test connection" — the button next to each service on the settings page.
//!
//! This exists because every way sharerr fails to reach a service produces the
//! same symptom: nothing happens. `sharerr doctor` turns that into an actionable
//! line, but it is a CLI command, and the whole point of the web UI is that an
//! operator should never need one. So each service gets a button that answers the
//! two questions a wrong setting raises — *can I reach it* and *does it accept the
//! credential* — separately, because the fixes are different.
//!
//! Deliberately narrower than `doctor`: path-mapping resolution and tracker state
//! are not checked here. Those need a library to walk and are worth a page of
//! their own rather than a one-line badge.

use axum::extract::{Path, State};
use axum::response::{Html, IntoResponse, Response};
use secrecy::SecretString;
use sharerr_arr::ArrClient;
use sharerr_core::config::secret_keys;
use sharerr_core::{Config, MediaSource};
use sharerr_qbit::QbitClient;

use super::WebState;
use crate::commands::doctor::chain;

/// Run one service's check and return the HTML fragment htmx swaps in.
///
/// Always `200`, even when the check fails: htmx does not swap non-2xx responses
/// by default, so a `502` here would leave the operator staring at a button that
/// appears to do nothing — the exact failure mode this page exists to remove.
pub async fn test(State(state): State<WebState>, Path(service): Path<String>) -> Response {
    let config = state.serve.config().await;

    let outcome = match service.as_str() {
        "sonarr" => check_arr(MediaSource::Sonarr, &state, &config).await,
        "radarr" => check_arr(MediaSource::Radarr, &state, &config).await,
        "qbittorrent" => check_qbit(&state, &config).await,
        _ => Outcome::Bad("Unknown service.".to_owned()),
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

/// Fetch one stored secret.
///
/// Returns `Ok(None)` for "not stored yet", which is a different message from "the
/// vault will not open" — the first is a field to fill in, the second is a missing
/// environment variable.
async fn secret(state: &WebState, key: &'static str) -> Result<Option<SecretString>, String> {
    state
        .serve
        .open_vault()
        .await?
        .get(key)
        .map_err(|err| err.to_string())
}

async fn check_arr(kind: MediaSource, state: &WebState, config: &Config) -> Outcome {
    let (service, key) = match kind {
        MediaSource::Sonarr => (config.sonarr.as_ref(), secret_keys::SONARR_API_KEY),
        MediaSource::Radarr => (config.radarr.as_ref(), secret_keys::RADARR_API_KEY),
    };

    let Some(service) = service else {
        return Outcome::Bad("No URL configured. Save one first.".to_owned());
    };

    let api_key = match secret(state, key).await {
        Ok(Some(api_key)) => api_key,
        Ok(None) => return Outcome::Bad("No API key stored. Save one first.".to_owned()),
        Err(reason) => return Outcome::Bad(reason),
    };

    let client = match ArrClient::new(kind, &service.url, api_key) {
        Ok(client) => client,
        Err(err) => return Outcome::Bad(chain(&err)),
    };

    let version = match client.system_status().await {
        Ok(status) => status.version,
        Err(err) if err.is_auth_failure() => {
            return Outcome::Bad("Reached it, but the API key was rejected.".to_owned());
        }
        Err(err) if err.is_unreachable() => {
            return Outcome::Bad(format!("Could not reach it: {}", chain(&err)));
        }
        Err(err) => return Outcome::Bad(chain(&err)),
    };

    // The tag is checked in the same breath because a correct URL and key with no
    // matching tag is the most common "sharerr does nothing" state, and it looks
    // identical to a working setup from the outside.
    match client.tag_id(&config.tag).await {
        Ok(_) => Outcome::Good(format!("Connected to {version}; the tag exists.")),
        Err(_) => Outcome::Bad(format!(
            "Connected to {version}, but no tag named {:?} exists there yet — \
             create it and tag something, or nothing will ever be shared.",
            config.tag
        )),
    }
}

async fn check_qbit(state: &WebState, config: &Config) -> Outcome {
    let password = match secret(state, secret_keys::QBITTORRENT_PASSWORD).await {
        Ok(Some(password)) => password,
        Ok(None) => return Outcome::Bad("No password stored. Save one first.".to_owned()),
        Err(reason) => return Outcome::Bad(reason),
    };

    let client = match QbitClient::new(
        &config.qbittorrent.url,
        &config.qbittorrent.username,
        password,
    ) {
        Ok(client) => client,
        Err(err) => return Outcome::Bad(chain(&err)),
    };

    if let Err(err) = client.login().await {
        return if err.is_auth_failure() {
            Outcome::Bad("Reached it, but the username or password was rejected.".to_owned())
        } else {
            Outcome::Bad(format!("Could not reach it: {}", chain(&err)))
        };
    }

    match client.version().await {
        Ok(version) => Outcome::Good(format!("Signed in to qBittorrent {version}.")),
        Err(err) => Outcome::Bad(format!("Signed in, but: {}", chain(&err))),
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
