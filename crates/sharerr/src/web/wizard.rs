//! The guided first-run — see `docs/roadmap.md`'s "Setup wizard".
//!
//! Not a separate configuration path: every step here submits to the very
//! same `/settings/*` handlers [`super::settings`] exposes, with `?next=`
//! appended to the form's `action` so a successful save lands back on the
//! wizard step instead of on the full Settings page (see
//! [`super::settings::NextQuery`]). A validation failure still renders the
//! ordinary Settings page — [`super::settings::reject`] has no notion of the
//! wizard — which is a real seam in the guided flow, but a page that
//! explains what went wrong beats one that cannot show the error at all.
//!
//! Nothing here is itself a source of truth. `sanitize_next` in `settings.rs`
//! is what actually keeps a step's forms from becoming an open redirect;
//! this module only ever builds the `next=` values it hands back to itself.

use axum::extract::{Query, State};
use axum::response::Response;
use serde::Deserialize;
use sharerr_core::MediaSource;
use sharerr_core::config::{config_paths, secret_keys};

use super::WebState;
use super::settings::{secrets_present, title_case, url_or_empty, url_placeholder};
use super::templates::{ArrSection, PathRow, WizardPage, WizardStep, render};

#[derive(Debug, Default, Deserialize)]
pub struct WizardQuery {
    saved: Option<String>,
}

pub async fn welcome(State(state): State<WebState>) -> Response {
    render(&page(&state, WizardStep::Welcome, None).await)
}

pub async fn services(State(state): State<WebState>, Query(q): Query<WizardQuery>) -> Response {
    render(&page(&state, WizardStep::Services, q.saved).await)
}

pub async fn paths(State(state): State<WebState>, Query(q): Query<WizardQuery>) -> Response {
    render(&page(&state, WizardStep::Paths, q.saved).await)
}

pub async fn tracker(State(state): State<WebState>, Query(q): Query<WizardQuery>) -> Response {
    render(&page(&state, WizardStep::Tracker, q.saved).await)
}

pub async fn done(State(state): State<WebState>) -> Response {
    render(&page(&state, WizardStep::Done, None).await)
}

/// Gathers the same handful of fields regardless of which step is showing —
/// cheap enough (one config read, one vault-key-names read, no network) that
/// there is nothing to gain from fetching only what one step needs.
async fn page(state: &WebState, step: WizardStep, saved: Option<String>) -> WizardPage {
    let config = state.serve.config().await;
    let secrets = secrets_present(&config).await;
    let is_set = |key: &str| secrets.contains(key);
    let locks = super::config_io::env_overrides();

    // Sonarr and Radarr only, same primary/secondary split Settings draws —
    // the rest stay behind that page's disclosure rather than crowding a
    // first-run step meant to take a minute.
    let arrs = [MediaSource::Sonarr, MediaSource::Radarr]
        .into_iter()
        .filter_map(|kind| {
            let url_path = config_paths::url_for(kind)?;
            let key = secret_keys::api_key_for(kind)?;
            Some(ArrSection {
                source: kind.as_str(),
                title: title_case(kind.as_str()),
                url: config
                    .service(kind)
                    .map(|s| s.url.to_string())
                    .unwrap_or_default(),
                key_set: is_set(key),
                placeholder: url_placeholder(kind),
                url_path,
                primary: true,
            })
        })
        .collect::<Vec<_>>();

    let path_map = config
        .path_map
        .iter()
        .map(|m| PathRow {
            arr: m.arr.display().to_string(),
            sharerr: m.sharerr.display().to_string(),
            qbit: m
                .qbit
                .as_ref()
                .map(|q| q.display().to_string())
                .unwrap_or_default(),
        })
        .chain(std::iter::once(PathRow::default()))
        .collect();

    WizardPage {
        signed_in: true,
        step,
        saved,
        master_key_present: sharerr_store::master_key_from_env().is_ok(),
        locks,

        tag: config.tag.clone(),
        arrs,

        qbit_url: config.qbittorrent.url.to_string(),
        qbit_api_key_set: is_set(secret_keys::QBITTORRENT_API_KEY),
        qbit_category: config.qbittorrent.category.clone(),
        qbit_tag: config.qbittorrent.tag.clone(),
        qbit_skip_checking: config.qbittorrent.skip_checking,

        path_map,

        tracker_advertised_host: config.tracker.advertised_host.clone().unwrap_or_default(),
        tracker_port: config
            .tracker
            .port
            .map(|p| p.to_string())
            .unwrap_or_default(),
        tracker_advertised_url: url_or_empty(config.tracker.advertised_url.as_ref()),
        tracker_token_set: is_set(secret_keys::TRACKER_TOKEN),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use axum::http::StatusCode;

    fn web_state(serve: std::sync::Arc<crate::state::ServeState>) -> WebState {
        WebState {
            serve,
            sessions: std::sync::Arc::new(crate::web::auth::Sessions::default()),
        }
    }

    /// Every step must render — this is the whole guard against a template
    /// referencing a field the Rust side stopped populating.
    #[tokio::test]
    async fn every_step_renders_on_an_unconfigured_instance() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);

        for step in [
            WizardStep::Welcome,
            WizardStep::Services,
            WizardStep::Paths,
            WizardStep::Tracker,
            WizardStep::Done,
        ] {
            let response = render(&page(&state, step, None).await);
            assert_eq!(response.status(), StatusCode::OK, "{step:?}");
        }
    }

    #[tokio::test]
    async fn only_sonarr_and_radarr_appear_in_the_services_step() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);

        let wizard = page(&state, WizardStep::Services, None).await;
        let sources: Vec<_> = wizard.arrs.iter().map(|a| a.source).collect();
        assert_eq!(sources, vec!["sonarr", "radarr"]);
    }
}
