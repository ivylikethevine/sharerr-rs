//! The read-only management API under `/api/v1/`: items, friends, sync runs
//! and doctor results as JSON, for scripts and dashboards that need more than
//! `/dashboard`'s counts.
//!
//! Off by default (`[api] enabled`) and behind its own bearer token,
//! [`sharerr_core::config::secret_keys::API_TOKEN`], never the metrics one:
//! this names every shared file and friend. [`ApiAuth`] fails closed the way
//! `crate::metrics::MetricsAuth` does, with the same `404` for disabled, no
//! token, an unopenable vault and a wrong token. Nothing here writes; the web
//! UI stays the only way to change anything.
//!
//! Each response is its own struct rather than the store's types, so a field
//! added to the store reaches this contract only when it is added here: a
//! friend's key hash and pubkey, and an item's announce-token fingerprint,
//! never do.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::state::ServeState;

/// How many recent sync runs `GET /api/v1/sync` returns.
const RECENT_RUNS: i64 = 20;

pub(crate) fn routes() -> OpenApiRouter<Arc<ServeState>> {
    OpenApiRouter::new()
        .routes(routes!(items))
        .routes(routes!(friends))
        .routes(routes!(sync))
        .routes(routes!(doctor))
}

/// Authentication as an extractor, so a handler cannot compile without it.
pub(crate) struct ApiAuth;

impl axum::extract::FromRequestParts<Arc<ServeState>> for ApiAuth {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &Arc<ServeState>,
    ) -> Result<Self, Self::Rejection> {
        let refused = || StatusCode::NOT_FOUND.into_response();
        if !state.config().await.api.enabled {
            return Err(refused());
        }
        let required = match state.try_api_token().await {
            Ok(token) => token,
            Err(err) => {
                tracing::warn!(error = %err, "refusing the management API: the vault could not be opened");
                return Err(refused());
            }
        };
        let Some(required) = required.filter(|token| !token.is_empty()) else {
            return Err(refused());
        };
        let Some(supplied) = crate::metrics::bearer_token(&parts.headers) else {
            return Err(refused());
        };
        if crate::secrets::constant_time_eq(&required, &supplied) {
            Ok(Self)
        } else {
            Err(refused())
        }
    }
}

/// The 503 every handler answers with when the database will not open.
fn unavailable(reason: &str) -> Response {
    (StatusCode::SERVICE_UNAVAILABLE, reason.to_owned()).into_response()
}

/// One shared or once-shared file.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct ApiItem {
    /// `sonarr`, `radarr`, `lidarr`, `readarr`, `whisparr` or `directory`.
    source: String,
    file_id: i64,
    /// `episode`, `movie`, `track` or `book`.
    kind: String,
    /// The series, film, album or book.
    title: String,
    /// What the feed advertises.
    release_title: String,
    /// `pending`, `seeding`, `failed` or `unshared`.
    state: String,
    last_error: Option<String>,
    size: u64,
    info_hash: Option<String>,
    /// Whether sharerr built the torrent, rather than adopting one already there.
    created_by_sharerr: bool,
    /// When sharerr first recorded the file, Unix seconds.
    created_at: Option<i64>,
    /// When the share ends on its own, Unix seconds (the start of a local
    /// day); `None` for a share with no end.
    share_until: Option<i64>,
}

/// `GET /api/v1/items` — every item the store holds.
#[utoipa::path(
    get,
    path = "/api/v1/items",
    tag = "management",
    operation_id = "listItems",
    responses(
        (status = 200, description = "Every item, in store order.", body = Vec<ApiItem>),
        (status = 404, description = "Disabled, or the bearer token was missing or wrong — \
            indistinguishable on purpose.", body = String),
        (status = 503, description = "The database could not be read.", body = String),
    ),
    security(("apiToken" = [])),
)]
async fn items(State(state): State<Arc<ServeState>>, _auth: ApiAuth) -> Response {
    let store = match state.store().await {
        Ok(store) => store,
        Err(reason) => return unavailable(&reason),
    };
    let ends = match store.share_ends().await {
        Ok(ends) => ends,
        Err(err) => return unavailable(&format!("reading share-until dates: {err}")),
    };
    match store.all_items().await {
        Ok(items) => axum::Json(
            items
                .into_iter()
                .map(|item| ApiItem {
                    share_until: ends.get(&item.key()).copied(),
                    source: item.source.as_str().to_owned(),
                    file_id: item.file_id,
                    kind: item.spec.kind_tag().to_owned(),
                    title: item.spec.title().to_owned(),
                    release_title: item.release_title,
                    state: item.state.as_str().to_owned(),
                    last_error: item.last_error,
                    size: item.size,
                    info_hash: item.info_hash,
                    created_by_sharerr: item.created_by_sharerr,
                    created_at: item.created_at,
                })
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(err) => unavailable(&format!("reading items: {err}")),
    }
}

/// One friend.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct ApiFriend {
    id: i64,
    label: String,
    /// `all`, `tv`, `movies`, `music` or `books`.
    scope: String,
    created_at: i64,
    /// Their last authenticated feed request or attributed announce.
    last_seen_at: Option<i64>,
    revoked_at: Option<i64>,
    /// Bytes their clients reported downloading from this instance's swarms.
    downloaded: i64,
    /// Bytes their clients reported passing on.
    uploaded: i64,
}

/// `GET /api/v1/friends` — every friend, revoked ones included.
#[utoipa::path(
    get,
    path = "/api/v1/friends",
    tag = "management",
    operation_id = "listFriends",
    responses(
        (status = 200, description = "Every friend. Keys are never included.", body = Vec<ApiFriend>),
        (status = 404, description = "Disabled, or the bearer token was missing or wrong.", body = String),
        (status = 503, description = "The database could not be read.", body = String),
    ),
    security(("apiToken" = [])),
)]
async fn friends(State(state): State<Arc<ServeState>>, _auth: ApiAuth) -> Response {
    let store = match state.store().await {
        Ok(store) => store,
        Err(reason) => return unavailable(&reason),
    };
    match store.list_peers().await {
        Ok(peers) => axum::Json(
            peers
                .into_iter()
                .map(|peer| ApiFriend {
                    id: peer.id,
                    label: peer.label,
                    scope: peer.scope.as_str().to_owned(),
                    created_at: peer.created_at,
                    last_seen_at: peer.last_seen_at,
                    revoked_at: peer.revoked_at,
                    downloaded: peer.downloaded,
                    uploaded: peer.uploaded,
                })
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(err) => unavailable(&format!("reading friends: {err}")),
    }
}

/// One sync pass.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct ApiRun {
    started_at: i64,
    /// `None` while the pass is still running.
    finished_at: Option<i64>,
    discovered: i64,
    added: i64,
    unshared: i64,
    failed: i64,
    /// Why the pass failed outright, when it did.
    error: Option<String>,
}

/// The sync loop's settings and recent history.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct ApiSync {
    /// Whether passes run on a timer (`[sync] enabled`).
    enabled: bool,
    interval_secs: u64,
    /// Why reconciliation cannot run right now, or `None` when it can.
    blocked: Option<String>,
    /// Newest first.
    runs: Vec<ApiRun>,
}

/// `GET /api/v1/sync` — the sync loop's state and its last passes.
#[utoipa::path(
    get,
    path = "/api/v1/sync",
    tag = "management",
    operation_id = "syncStatus",
    responses(
        (status = 200, description = "Sync settings, what blocks it, and recent runs.", body = ApiSync),
        (status = 404, description = "Disabled, or the bearer token was missing or wrong.", body = String),
        (status = 503, description = "The database could not be read.", body = String),
    ),
    security(("apiToken" = [])),
)]
async fn sync(State(state): State<Arc<ServeState>>, _auth: ApiAuth) -> Response {
    let config = state.config().await.sync;
    let store = match state.store().await {
        Ok(store) => store,
        Err(reason) => return unavailable(&reason),
    };
    let runs = match store.recent_runs(RECENT_RUNS).await {
        Ok(runs) => runs,
        Err(err) => return unavailable(&format!("reading sync runs: {err}")),
    };
    axum::Json(ApiSync {
        enabled: config.enabled,
        interval_secs: config.effective_interval_secs(),
        blocked: state.blocked_reason().await,
        runs: runs
            .into_iter()
            .map(|run| ApiRun {
                started_at: run.started_at,
                finished_at: run.finished_at,
                discovered: run.summary.discovered,
                added: run.summary.added,
                unshared: run.summary.unshared,
                failed: run.summary.failed,
                error: run.summary.error,
            })
            .collect(),
    })
    .into_response()
}

/// `GET /api/v1/doctor` — `sharerr doctor --json`, run now.
///
/// The read-only checks only: no `--fix`, no path suggestions. It dials the
/// *arr apps and the torrent client, so it takes as long as they do.
#[utoipa::path(
    get,
    path = "/api/v1/doctor",
    tag = "management",
    operation_id = "doctor",
    responses(
        (status = 200, description = "`{\"ok\", \"failures\", \"warnings\", \"sections\"}`, each \
            section its title and lines of `{\"level\", \"message\"}` — the same document \
            `sharerr doctor --json` prints.", body = serde_json::Value),
        (status = 404, description = "Disabled, or the bearer token was missing or wrong.", body = String),
    ),
    security(("apiToken" = [])),
)]
async fn doctor(State(state): State<Arc<ServeState>>, _auth: ApiAuth) -> Response {
    let config = state.config().await;
    let config_error = state.config_error().await;
    axum::Json(crate::commands::doctor::json_report(&config, config_error.as_deref()).await)
        .into_response()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::result_large_err)]

    use axum::body::Body;
    use axum::http::Request;
    use secrecy::SecretString;
    use sharerr_core::config::secret_keys;
    use tower::ServiceExt;

    use super::*;

    const TOKEN: &str = "api-tests-bearer-token";

    async fn get(state: &Arc<ServeState>, uri: &str, bearer: Option<&str>) -> (StatusCode, String) {
        let (router, _) = routes().with_state(Arc::clone(state)).split_for_parts();
        let mut request = Request::builder().uri(uri);
        if let Some(token) = bearer {
            request = request.header(axum::http::header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let response = router
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    /// A state whose vault opens, with the API and metrics both on and
    /// `seed` run against the vault before the first request. `Jail` for the
    /// reason CLAUDE.md gives: it scopes the master key and serialises
    /// against every other test that sets one.
    fn with_open_vault<F, Fut>(seed: impl FnOnce(&mut sharerr_store::Vault), body: F)
    where
        F: FnOnce(Arc<ServeState>) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        figment::Jail::expect_with(|jail| {
            jail.set_env("SHARERR_MASTER_KEY", "api-tests-master-key");
            let config = sharerr_core::Config {
                data_dir: jail.directory().to_path_buf(),
                api: sharerr_core::config::ApiConfig { enabled: true },
                metrics: sharerr_core::config::MetricsConfig { enabled: true },
                ..Default::default()
            };
            let path = jail.directory().join("sharerr.toml");
            let state = Arc::new(ServeState::new(config, path, None));
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                let mut vault = state.open_vault().await.unwrap();
                seed(&mut vault);
                drop(vault);
                body(state).await;
            });
            Ok(())
        });
    }

    fn put(vault: &mut sharerr_store::Vault, key: &str, value: &str) {
        vault
            .put(key, &SecretString::from(value.to_owned()))
            .unwrap();
    }

    #[tokio::test]
    async fn every_route_is_a_404_while_the_api_is_off() {
        let (_dir, state) = crate::state::fixtures::unconfigured();
        for uri in [
            "/api/v1/items",
            "/api/v1/friends",
            "/api/v1/sync",
            "/api/v1/doctor",
        ] {
            assert_eq!(
                get(&state, uri, Some(TOKEN)).await.0,
                StatusCode::NOT_FOUND,
                "{uri}"
            );
        }
    }

    /// The point of a separate token: the metrics one opens nothing here.
    #[test]
    fn the_metrics_token_is_not_an_api_token() {
        with_open_vault(
            |vault| put(vault, secret_keys::METRICS_TOKEN, TOKEN),
            |state| async move {
                assert_eq!(
                    get(&state, "/api/v1/items", Some(TOKEN)).await.0,
                    StatusCode::NOT_FOUND
                );
            },
        );
    }

    #[test]
    fn a_missing_or_wrong_bearer_is_a_404() {
        with_open_vault(
            |vault| put(vault, secret_keys::API_TOKEN, TOKEN),
            |state| async move {
                assert_eq!(
                    get(&state, "/api/v1/items", None).await.0,
                    StatusCode::NOT_FOUND
                );
                assert_eq!(
                    get(&state, "/api/v1/items", Some("not-it")).await.0,
                    StatusCode::NOT_FOUND
                );
            },
        );
    }

    #[test]
    fn the_right_token_reads_items_friends_sync_and_doctor_without_keys() {
        with_open_vault(
            |vault| put(vault, secret_keys::API_TOKEN, TOKEN),
            |state| async move {
                let store = state.store().await.unwrap();
                let friend_key = SecretString::from("a-friends-feed-key".to_owned());
                store
                    .create_peer("Sam", &friend_key, sharerr_store::PeerScope::Tv)
                    .await
                    .unwrap();
                let run = store.begin_run().await.unwrap();
                store
                    .finish_run(
                        run,
                        &sharerr_store::RunSummary {
                            discovered: 3,
                            added: 2,
                            ..Default::default()
                        },
                    )
                    .await
                    .unwrap();

                let (status, body) = get(&state, "/api/v1/items", Some(TOKEN)).await;
                assert_eq!((status, body.as_str()), (StatusCode::OK, "[]"));

                let (status, body) = get(&state, "/api/v1/friends", Some(TOKEN)).await;
                assert_eq!(status, StatusCode::OK);
                let friends: serde_json::Value = serde_json::from_str(&body).unwrap();
                assert_eq!(friends[0]["label"], "Sam");
                assert_eq!(friends[0]["scope"], "tv");
                assert!(
                    !body.contains("key_hash") && !body.contains("pubkey"),
                    "{body}"
                );

                let (status, body) = get(&state, "/api/v1/sync", Some(TOKEN)).await;
                assert_eq!(status, StatusCode::OK);
                let sync: serde_json::Value = serde_json::from_str(&body).unwrap();
                assert_eq!(sync["runs"][0]["discovered"], 3);
                assert_eq!(sync["interval_secs"], 900);

                let (status, body) = get(&state, "/api/v1/doctor", Some(TOKEN)).await;
                assert_eq!(status, StatusCode::OK);
                let doctor: serde_json::Value = serde_json::from_str(&body).unwrap();
                assert!(doctor["sections"].is_array(), "{body}");
                assert!(doctor["ok"].is_boolean(), "{body}");
            },
        );
    }
}
