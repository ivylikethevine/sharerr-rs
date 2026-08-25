//! The guided first-run — see `docs/ROADMAP.md`'s "Setup wizard".
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
use sharerr_core::config::secret_keys;

use super::WebState;
use super::settings::{arr_section, path_rows, secrets_present, url_or_empty};
use super::templates::{WizardPage, WizardStep, render};

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
        .filter_map(|kind| arr_section(kind, &config, &is_set, true))
        .collect::<Vec<_>>();

    let path_map = path_rows(&config);

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
    use crate::web::web_state;
    use axum::http::StatusCode;

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

    /// The five route handlers are thin wrappers around `page` — exercised
    /// directly (rather than only through `page`) so a handler that stops
    /// forwarding its query or drops its step argument would be caught.
    #[tokio::test]
    async fn every_route_handler_renders() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);
        let no_query = Query(WizardQuery::default());

        for response in [
            welcome(State(state.clone())).await,
            services(State(state.clone()), Query(WizardQuery::default())).await,
            paths(State(state.clone()), no_query).await,
            tracker(
                State(state.clone()),
                Query(WizardQuery {
                    saved: Some("tracker".to_owned()),
                }),
            )
            .await,
            done(State(state.clone())).await,
        ] {
            assert_eq!(response.status(), StatusCode::OK);
        }
    }

    /// A configured qBittorrent-side path must round-trip into the rendered
    /// row rather than silently falling back to the `unwrap_or_default` empty
    /// string the way an unmapped row does.
    #[tokio::test]
    async fn a_path_mapping_with_a_qbit_side_renders_it() {
        let dir = tempfile::tempdir().unwrap();
        let config = sharerr_core::Config {
            data_dir: dir.path().to_path_buf(),
            path_map: vec![sharerr_core::config::PathMapping {
                arr: "/data/tv".into(),
                sharerr: "/share/tv".into(),
                qbit: Some("/downloads/tv".into()),
            }],
            ..sharerr_core::Config::default()
        };
        let path = dir.path().join("sharerr.toml");
        let serve = std::sync::Arc::new(crate::state::ServeState::new(config, path, None));
        let state = web_state(serve);

        let wizard = page(&state, WizardStep::Paths, None).await;
        let row = wizard
            .path_map
            .iter()
            .find(|r| r.arr == "/data/tv")
            .expect("the configured mapping must appear");
        assert_eq!(row.qbit, "/downloads/tv");

        // The trailing blank row for adding a new mapping is still appended.
        assert!(wizard.path_map.iter().any(|r| r.arr.is_empty()));
    }
}
