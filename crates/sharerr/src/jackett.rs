//! Jackett compatibility: everything under `/api/v2.0/`.
//!
//! # What this is for
//!
//! Jackett is an aggregator that proxies many trackers, and a large ecosystem of
//! clients is configured to talk to one. sharerr is a single indexer, but there is
//! no reason a client set up for Jackett should have to be reconfigured to use it —
//! the search grammar is the same Torznab sharerr already speaks, and most of the
//! difference is clerical.
//!
//! Two surfaces live here:
//!
//! * **Search**, at `/api/v2.0/indexers/{id}/results/torznab/...`. Pure routing over
//!   [`crate::torznab`] — the query and the document are identical to `/api`'s.
//! * **Admin**, the rest of `/api/v2.0/`. This is where sharerr and Jackett actually
//!   differ, because most of Jackett's admin API exists to manage a *collection* of
//!   indexers that sharerr does not have.
//!
//! # What is deliberately not implemented
//!
//! Everything that writes. Jackett's admin API can add, configure, test and delete
//! indexers; sharerr has exactly one and it is not configurable from here. Those
//! endpoints are not stubbed out to return success — a client told its write
//! succeeded when nothing happened is worse off than one told the endpoint does not
//! exist.
//!
//! Rather than guess at the rest, [`unimplemented_endpoint`] logs any unhandled
//! `/api/v2.0/` path at `warn` with the method and path. A gap therefore shows up
//! as an actionable line in the log of the person who hit it, instead of a silent
//! 404 nobody can act on. That is how this surface should grow: implement what
//! turns up, not what might.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::state::ServeState;
use crate::torznab::{Caller, SearchQuery, collect};

/// The single indexer this instance exposes.
///
/// Jackett ids are per-tracker slugs. sharerr has one thing to serve, so the id is
/// fixed — but note that the *search* path accepts any id, including Jackett's
/// `all` aggregate, because someone migrating from Jackett will have whatever id
/// they used before baked into their client.
const INDEXER_ID: &str = "sharerr";

/// Mounted through [`OpenApiRouter`] so a route and its OpenAPI entry are one
/// declaration — `routes!` takes the path from the handler's own
/// `#[utoipa::path]`. The two exceptions below use `.route`, which mounts
/// without documenting, and say why.
pub(crate) fn api_router() -> OpenApiRouter<Arc<ServeState>> {
    OpenApiRouter::new()
        // Search, and the two alias shapes of the same path: whether a client
        // appends `/api` and whether its base URL ends in a slash both vary.
        // Only the canonical one is documented — three entries for one
        // operation would read as three operations.
        .routes(routes!(torznab_search))
        .route(
            "/api/v2.0/indexers/{indexer}/results/torznab/",
            axum::routing::get(torznab_search),
        )
        .route(
            "/api/v2.0/indexers/{indexer}/results/torznab/api",
            axum::routing::get(torznab_search),
        )
        // Admin, read-only.
        .routes(routes!(indexers))
        .routes(routes!(server_config))
        .routes(routes!(json_results))
        // Anything else under the Jackett prefix, so a gap is visible. Mounted
        // by hand because axum spells a catch-all `{*rest}` and OpenAPI spells
        // the same thing `{rest}`; `routes!` would mount the OpenAPI spelling
        // and match one path segment instead of all of them. The document
        // still carries it — see `crate::openapi`.
        .route(
            "/api/v2.0/{*rest}",
            axum::routing::any(unimplemented_endpoint),
        )
}

/// Jackett's Torznab endpoint, which is the same Torznab at a different address.
///
/// The indexer id is accepted and ignored. Jackett namespaces each tracker it
/// proxies; sharerr *is* the one thing it serves, so every id — including `all` —
/// means the same feed. Rejecting unfamiliar ids would only break someone pasting
/// the id from their old Jackett config.
///
/// Download links need nothing: the enclosure URLs are absolute and already point
/// at this instance, so a client follows them whichever path it searched through.
///
/// Three paths reach this handler — with and without a trailing slash, and with a
/// trailing `/api` — because whether a client appends `/api` and whether its base
/// URL ends in a slash both vary. Only the canonical one is documented; the other
/// two are the same operation and listing them would suggest a difference.
#[utoipa::path(
    get,
    path = "/api/v2.0/indexers/{indexer}/results/torznab",
    tag = "jackett",
    operation_id = "jackettTorznab",
    security(("peerApiKey" = [])),
    params(
        ("indexer" = String, Path, description =
         "Jackett namespaces each tracker it proxies; sharerr *is* the one thing it \\
          serves, so any id — `sharerr`, `all`, or whatever was pasted from an old \\
          Jackett config — means the same feed."),
        crate::torznab::SearchQuery,
    ),
    responses(
        (status = 200, content_type = "application/xml", description =
         "Identical to `GET /api` — the same Torznab at a different address. Two \
          more paths reach it: the same path with a trailing slash, and with `/api` \
          appended.", body = String),
        (status = 400, content_type = "application/xml",
         description = "No such Torznab function.", body = String),
        (status = 401, content_type = "application/xml",
         description = "No `apikey`, or one that matches no active peer.", body = String),
    ),
)]
async fn torznab_search(
    State(state): State<Arc<ServeState>>,
    caller: Caller,
    Path(indexer): Path<String>,
    Query(query): Query<SearchQuery>,
) -> Response {
    tracing::debug!(indexer, "torznab request over the jackett path");
    crate::torznab::api(State(state), caller, Query(query)).await
}

// ---------------------------------------------------------------------------
// Admin
// ---------------------------------------------------------------------------

/// One entry in Jackett's indexer list.
///
/// Field names are Jackett's, lowercase and underscored, because clients match on
/// them literally. `#[serde(rename_all)]` would not help — Jackett's own casing is
/// inconsistent between this DTO and the results one below, and matching it is the
/// entire point of the module.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = JackettIndexer)]
pub(crate) struct IndexerEntry {
    id: &'static str,
    name: &'static str,
    description: String,
    /// Jackett distinguishes `public`, `private` and `semi-private`. sharerr's feed
    /// is closed without a key, which is `private` by any reading.
    #[serde(rename = "type")]
    kind: &'static str,
    configured: bool,
    site_link: String,
    language: &'static str,
    last_error: &'static str,
    caps: Vec<Capability>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = JackettCapability)]
pub(crate) struct Capability {
    #[serde(rename = "ID")]
    id: String,
    #[serde(rename = "Name")]
    name: &'static str,
}

/// `GET /api/v2.0/indexers` — the list of indexers, which for sharerr is one.
///
/// Jackett supports `?configured=true`. sharerr's single indexer is always
/// configured, so the filter is accepted and changes nothing — no `Query`
/// extractor means every filter a client sends is accepted and ignored, because
/// a client that sends one and gets an error learns nothing useful.
#[utoipa::path(
    get,
    path = "/api/v2.0/indexers",
    tag = "jackett",
    operation_id = "jackettIndexers",
    security(("peerApiKey" = [])),
    params(
        ("configured" = Option<bool>, Query, description =
         "Accepted and ignored. sharerr's single indexer is always configured, so \
          every value of this filter yields the same one-element list."),
    ),
    responses(
        (status = 200, description = "A one-element list: sharerr itself.",
         body = Vec<IndexerEntry>),
        (status = 401, content_type = "application/xml",
         description = "No `apikey`, or one that matches no active peer.", body = String),
    ),
)]
async fn indexers(State(state): State<Arc<ServeState>>, _caller: Caller) -> Response {
    // The live endpoint, not `config.public_base_url()` — see
    // `ServeState::public_base_url`'s docs: a gluetun-only deployment must
    // advertise the resolved address here too, not just in the feed.
    let base = state.public_base_url().await;

    let entry = IndexerEntry {
        id: INDEXER_ID,
        name: "sharerr",
        description: format!("Content shared by a friend running sharerr ({base})"),
        kind: "private",
        configured: true,
        site_link: base,
        language: "en-US",
        last_error: "",
        caps: crate::torznab::CATEGORIES
            .iter()
            .map(|(id, name)| Capability {
                id: id.to_string(),
                name,
            })
            .collect(),
    };

    json([entry])
}

/// Jackett's server config DTO, trimmed to the fields a client reads.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = JackettServerConfig)]
pub(crate) struct ServerConfigDto {
    notices: Vec<String>,
    port: u16,
    external: bool,
    /// **Always empty.** Jackett returns its own API key here, which is how its
    /// dashboard bootstraps itself. Echoing a key back would mean this endpoint
    /// hands out the credential that opens the feed to anyone who already has
    /// *a* key — turning one friend's key into every friend's. A client that
    /// genuinely needs a key has one already, because this endpoint required it.
    api_key: &'static str,
    app_version: &'static str,
    /// sharerr has no blackhole directory, no updater, and no FlareSolverr. These
    /// are present and empty because their *absence* makes some clients treat the
    /// response as malformed rather than as a server without those features.
    blackholedir: &'static str,
    updatedisabled: bool,
    prerelease: bool,
    logging: bool,
    basepathoverride: &'static str,
    omdbkey: &'static str,
}

/// `GET /api/v2.0/server/config` — what a client reads to learn who it is talking
/// to.
#[utoipa::path(
    get,
    path = "/api/v2.0/server/config",
    tag = "jackett",
    operation_id = "jackettServerConfig",
    security(("peerApiKey" = [])),
    responses(
        (status = 200, description =
         "Server identity. `api_key` is **always empty**, unlike Jackett's own: \
          echoing a key back would turn one friend's credential into every \
          friend's.", body = ServerConfigDto),
        (status = 401, content_type = "application/xml",
         description = "No `apikey`, or one that matches no active peer.", body = String),
    ),
)]
async fn server_config(State(state): State<Arc<ServeState>>, _caller: Caller) -> Response {
    json(ServerConfigDto {
        notices: Vec::new(),
        port: state.with_config(|c| c.server.bind.port()).await,
        external: false,
        api_key: "",
        app_version: env!("CARGO_PKG_VERSION"),
        blackholedir: "",
        updatedisabled: true,
        prerelease: false,
        logging: false,
        basepathoverride: "",
        omdbkey: "",
    })
}

/// One release in Jackett's JSON results.
///
/// Capitalised field names, unlike the indexer DTO above — that inconsistency is
/// Jackett's, and clients depend on it.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = JackettResult)]
pub(crate) struct JsonResult {
    #[serde(rename = "Tracker")]
    tracker: &'static str,
    #[serde(rename = "TrackerId")]
    tracker_id: &'static str,
    #[serde(rename = "TrackerType")]
    tracker_type: &'static str,
    #[serde(rename = "Title")]
    title: String,
    #[serde(rename = "Guid")]
    guid: String,
    #[serde(rename = "Link")]
    link: String,
    #[serde(rename = "Details")]
    details: String,
    #[serde(rename = "Category")]
    category: Vec<u32>,
    #[serde(rename = "CategoryDesc")]
    category_desc: &'static str,
    #[serde(rename = "Size")]
    size: u64,
    #[serde(rename = "Seeders")]
    seeders: u32,
    #[serde(rename = "Peers")]
    peers: u32,
    #[serde(rename = "InfoHash")]
    info_hash: String,
    /// Jackett serialises the key even when null, and clients read it that way.
    #[serde(rename = "MagnetUri")]
    magnet_uri: Option<String>,
    #[serde(rename = "DownloadVolumeFactor")]
    download_volume_factor: f32,
    #[serde(rename = "UploadVolumeFactor")]
    upload_volume_factor: f32,
    #[serde(rename = "Imdb", skip_serializing_if = "Option::is_none")]
    imdb: Option<i64>,
    #[serde(rename = "TVDBId", skip_serializing_if = "Option::is_none")]
    tvdb: Option<i64>,
    #[serde(rename = "TMDb", skip_serializing_if = "Option::is_none")]
    tmdb: Option<i64>,
    // What the file actually is. Jackett's own names for these, so a client that
    // already reads Jackett needs no special case for sharerr; omitted rather
    // than null when unknown, matching the ids above and the XML feed's
    // attributes. The XML renderer emits the same set from the same
    // `item.media` — the two disagreeing about one release is the failure mode
    // this whole module is kept in step to avoid.
    #[serde(rename = "Resolution", skip_serializing_if = "Option::is_none")]
    resolution: Option<String>,
    #[serde(rename = "VideoCodec", skip_serializing_if = "Option::is_none")]
    video_codec: Option<String>,
    #[serde(rename = "AudioCodec", skip_serializing_if = "Option::is_none")]
    audio_codec: Option<String>,
    #[serde(rename = "AudioChannels", skip_serializing_if = "Option::is_none")]
    audio_channels: Option<String>,
    #[serde(rename = "Languages", skip_serializing_if = "Option::is_none")]
    languages: Option<String>,
    #[serde(rename = "Subs", skip_serializing_if = "Option::is_none")]
    subs: Option<String>,
    #[serde(rename = "Runtime", skip_serializing_if = "Option::is_none")]
    runtime: Option<String>,
    #[serde(rename = "Hdr", skip_serializing_if = "Option::is_none")]
    hdr: Option<String>,
    // Jackett has no established name for either, having never indexed a source
    // that reports them. These follow its `PascalCase` convention so a consumer
    // reading the rest of this object needs no second rule for them.
    #[serde(rename = "AudioSampleRate", skip_serializing_if = "Option::is_none")]
    audio_sample_rate: Option<String>,
    #[serde(rename = "AudioBitDepth", skip_serializing_if = "Option::is_none")]
    audio_bit_depth: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = JackettResults)]
pub(crate) struct JsonResults {
    #[serde(rename = "Results")]
    results: Vec<JsonResult>,
    #[serde(rename = "Indexers")]
    indexers: Vec<QueriedIndexer>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = JackettQueriedIndexer)]
pub(crate) struct QueriedIndexer {
    #[serde(rename = "ID")]
    id: &'static str,
    #[serde(rename = "Name")]
    name: &'static str,
    #[serde(rename = "Status")]
    status: u32,
    #[serde(rename = "Results")]
    results: usize,
    #[serde(rename = "Error", skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// `GET /api/v2.0/indexers/{id}/results` — the same search, rendered as JSON.
///
/// Jackett's own clients (and its web dashboard) use this rather than Torznab. It
/// runs through the *same* [`collect`] the XML feed uses, so the two cannot report
/// different libraries — which is the failure this project has already had once,
/// between `doctor` and the web UI's probes.
#[utoipa::path(
    get,
    path = "/api/v2.0/indexers/{indexer}/results",
    tag = "jackett",
    operation_id = "jackettResults",
    security(("peerApiKey" = [])),
    params(
        ("indexer" = String, Path, description =
         "Jackett namespaces each tracker it proxies; sharerr *is* the one thing it \\
          serves, so any id — `sharerr`, `all`, or whatever was pasted from an old \\
          Jackett config — means the same feed."),
        crate::torznab::SearchQuery,
    ),
    responses(
        (status = 200, description =
         "The same search the Torznab feed answers, rendered as Jackett's JSON. It \
          runs through the same code path, so the two surfaces cannot report \
          different libraries.", body = JsonResults),
        (status = 401, content_type = "application/xml",
         description = "No `apikey`, or one that matches no active peer.", body = String),
    ),
)]
async fn json_results(
    State(state): State<Arc<ServeState>>,
    caller: Caller,
    Path(indexer): Path<String>,
    Query(query): Query<SearchQuery>,
) -> Response {
    tracing::debug!(indexer, "jackett json results");

    // Scoped to the friend who asked, exactly as the XML feed is. A second surface
    // that forgot to scope would be a way around the setting rather than a
    // rendering of it.
    let matched = match collect(&state, &query, caller.scope(), caller.key_hash()).await {
        Ok(matched) => matched,
        Err((status, reason)) => {
            // Reported in the shape a JSON client can read, rather than as the XML
            // the Torznab path would send.
            return (
                status,
                json(JsonResults {
                    results: Vec::new(),
                    indexers: vec![QueriedIndexer {
                        id: INDEXER_ID,
                        name: "sharerr",
                        // Jackett uses a non-zero status to mean "this indexer
                        // errored", which is exactly what happened.
                        status: 1,
                        results: 0,
                        error: Some(reason),
                    }],
                }),
            )
                .into_response();
        }
    };

    let results: Vec<JsonResult> = matched
        .items
        .iter()
        .map(|item| {
            let category = crate::torznab::category_for(item);
            let link = matched.download_url(item);
            let info_hash = item.info_hash.clone().unwrap_or_default();
            let magnet = matched.magnet_url(item);

            JsonResult {
                tracker: "sharerr",
                tracker_id: INDEXER_ID,
                tracker_type: "private",
                title: item.release_title.clone(),
                guid: link.clone(),
                details: link.clone(),
                link,
                category: vec![category],
                category_desc: crate::torznab::category_name(category),
                size: item.size,
                // Shared with the XML feed — see the constants' docs.
                seeders: crate::torznab::ADVERTISED_SEEDERS,
                peers: crate::torznab::ADVERTISED_PEERS,
                info_hash,
                magnet_uri: (!magnet.is_empty()).then_some(magnet),
                download_volume_factor: crate::torznab::DOWNLOAD_VOLUME_FACTOR,
                upload_volume_factor: crate::torznab::UPLOAD_VOLUME_FACTOR,
                // Jackett's JSON carries the IMDb id as a bare number, without the
                // `tt`, unlike Torznab's attribute.
                imdb: item.ids.imdb_numeric().and_then(|id| id.parse().ok()),
                tvdb: item.ids.tvdb,
                tmdb: item.ids.tmdb,
                resolution: item.media.as_ref().and_then(|m| m.resolution.clone()),
                video_codec: item.media.as_ref().and_then(|m| m.video_codec.clone()),
                audio_codec: item.media.as_ref().and_then(|m| m.audio_codec.clone()),
                audio_channels: item.media.as_ref().and_then(|m| m.audio_channels.clone()),
                languages: item.media.as_ref().and_then(|m| m.audio_languages.clone()),
                subs: item.media.as_ref().and_then(|m| m.subtitles.clone()),
                runtime: item.media.as_ref().and_then(|m| m.runtime.clone()),
                hdr: item.media.as_ref().and_then(|m| m.dynamic_range.clone()),
                audio_sample_rate: item
                    .media
                    .as_ref()
                    .and_then(|m| m.audio_sample_rate.clone()),
                audio_bit_depth: item.media.as_ref().and_then(|m| m.audio_bit_depth.clone()),
            }
        })
        .collect();

    let count = results.len();
    tracing::debug!(returned = count, of = matched.total, "jackett json search");

    json(JsonResults {
        results,
        indexers: vec![QueriedIndexer {
            id: INDEXER_ID,
            name: "sharerr",
            status: 0,
            results: count,
            error: None,
        }],
    })
}

/// Anything under `/api/v2.0/` that is not implemented.
///
/// Logged at `warn` rather than returning a bare 404, because the whole strategy
/// for this surface is to implement what clients actually call. A 404 tells the
/// operator nothing; this tells them exactly which method and path to ask for.
///
/// Answers 501, not 404: the path exists as a concept and sharerr simply does not
/// implement it, and a client distinguishing the two behaves better than one
/// guessing.
#[utoipa::path(
    method(get, post, put, delete, patch, head, options, trace),
    path = "/api/v2.0/{rest}",
    tag = "jackett",
    operation_id = "jackettUnimplemented",
    params(
        ("rest" = String, Path, description =
         "Any remaining path under the Jackett prefix. Matched as a wildcard, so it \
          may contain slashes."),
    ),
    responses(
        (status = 501, description =
         "sharerr implements only Jackett's read-only endpoints. Answered as 501 \
          rather than 404 on purpose — the path exists as a concept and sharerr \
          simply does not implement it, which is a different thing for a client to \
          handle. Every call here is logged at `warn`, naming the method and path, \
          because that is the list of what would be worth adding.", body = String),
    ),
)]
async fn unimplemented_endpoint(
    method: axum::http::Method,
    axum::extract::OriginalUri(uri): axum::extract::OriginalUri,
) -> Response {
    tracing::warn!(
        %method,
        path = %uri.path(),
        "a Jackett client called an endpoint sharerr does not implement — \
         if this matters, it is the thing to add"
    );

    (
        StatusCode::NOT_IMPLEMENTED,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        r#"{"error":"sharerr implements only Jackett's read-only endpoints"}"#,
    )
        .into_response()
}

/// `axum::Json` rather than a hand-rolled serializer: it sets the content type and
/// handles a serialization failure, and this module has no shape it cannot express.
fn json<T: Serialize>(value: T) -> Response {
    axum::Json(value).into_response()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::state::fixtures::unconfigured;
    use axum::body::Body;
    use axum::http::Request;
    use secrecy::SecretString;
    use tower::ServiceExt;

    /// A state with one peer, so requests can authenticate.
    async fn with_peer() -> (tempfile::TempDir, Arc<ServeState>) {
        let (dir, state) = unconfigured();
        state
            .store()
            .await
            .unwrap()
            .create_peer(
                "Sam",
                &SecretString::from("sam-key"),
                sharerr_store::PeerScope::All,
            )
            .await
            .unwrap();
        (dir, state)
    }

    /// The same, plus one release that is actually seeding.
    ///
    /// Without this every comparison between the two renderers is satisfied by both
    /// returning nothing, which would pass whatever they did.
    async fn with_a_release() -> (tempfile::TempDir, Arc<ServeState>) {
        use sharerr_core::model::{ExternalIds, MediaSource, MediaSpec, ShareState, SharedItem};

        let (dir, state) = with_peer().await;
        let store = state.store().await.unwrap();

        let item = SharedItem {
            id: None,
            source: MediaSource::Sonarr,
            source_id: 7,
            file_id: 1,
            spec: MediaSpec::Episode {
                series_title: "Lanternwick Hollow".to_owned(),
                season: 2,
                episode: 1,
            },
            release_title: "Lanternwick.Hollow.S02E01.1080p.WEB-DL-FAKEGRP".to_owned(),
            arr_path: std::path::PathBuf::from("/tv/lanternwick.s02e01.mkv"),
            size: 2_147_483_648,
            ids: ExternalIds {
                tvdb: Some(918_273),
                tmdb: None,
                tvmaze: None,
                imdb: Some("tt1234567".to_owned()),
                ..ExternalIds::default()
            },
            media: Some(sharerr_core::MediaMeta {
                resolution: Some("1920x1080".to_owned()),
                video_codec: Some("HEVC".to_owned()),
                audio_codec: Some("EAC3".to_owned()),
                audio_channels: Some("5.1".to_owned()),
                subtitles: Some("English".to_owned()),
                ..sharerr_core::MediaMeta::default()
            }),
            info_hash: None,
            announce_token_fp: None,
            created_by_sharerr: true,
            state: ShareState::Pending,
            last_error: None,
            created_at: None,
        };
        store.upsert(&item).await.unwrap();
        // `seeding_items` requires both, which is what makes a release visible to
        // the feed at all.
        store
            .set_info_hash(
                MediaSource::Sonarr,
                1,
                "0123456789abcdef0123456789abcdef01234567",
            )
            .await
            .unwrap();
        store
            .set_state(MediaSource::Sonarr, 1, ShareState::Seeding, None)
            .await
            .unwrap();

        (dir, state)
    }

    async fn send(state: &Arc<ServeState>, uri: &str) -> (StatusCode, String) {
        let response = crate::torznab::routes(Arc::clone(state))
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    fn parse(body: &str) -> serde_json::Value {
        serde_json::from_str(body).unwrap_or_else(|e| panic!("not JSON: {e}\n{body}"))
    }

    #[tokio::test]
    async fn the_indexer_list_describes_this_instance() {
        let (_dir, state) = with_peer().await;

        let (status, body) = send(&state, "/api/v2.0/indexers?apikey=sam-key").await;
        assert_eq!(status, StatusCode::OK);

        let json = parse(&body);
        let list = json.as_array().expect("Jackett returns an array");
        assert_eq!(list.len(), 1, "sharerr is a single indexer");
        assert_eq!(list[0]["id"], "sharerr");
        assert_eq!(list[0]["configured"], true);
        // Closed without a key, which is what `private` means to a client.
        assert_eq!(list[0]["type"], "private");
        assert!(
            !list[0]["caps"].as_array().unwrap().is_empty(),
            "a client decides what to search from these"
        );
    }

    /// On a gluetun-only deployment (no static `tracker.advertised_host`),
    /// `site_link` must track the live resolved endpoint — not fall back to
    /// `http://localhost:<port>`, which only works from the box sharerr
    /// itself runs on.
    #[tokio::test]
    async fn the_site_link_tracks_the_live_endpoint_not_localhost() {
        let (_dir, state) = with_peer().await;
        state
            .endpoint()
            .observe(url::Url::parse("http://203.0.113.9:41234/").unwrap());

        let (status, body) = send(&state, "/api/v2.0/indexers?apikey=sam-key").await;
        assert_eq!(status, StatusCode::OK);

        let json = parse(&body);
        assert_eq!(json[0]["site_link"], "http://203.0.113.9:41234");
    }

    /// Jackett's own filter, accepted rather than rejected — sharerr's one indexer
    /// is always configured, so it changes nothing, but failing the request would
    /// tell the client nothing useful.
    #[tokio::test]
    async fn the_configured_filter_is_accepted() {
        let (_dir, state) = with_peer().await;

        let (status, _) = send(&state, "/api/v2.0/indexers?configured=true&apikey=sam-key").await;
        assert_eq!(status, StatusCode::OK);
    }

    /// The one that matters for security. Jackett puts its own API key in this
    /// response, because its dashboard bootstraps from it. Doing the same here would
    /// turn any one friend's key into a way to obtain the credential — so the field
    /// exists, for clients that read it, and is always empty.
    #[tokio::test]
    async fn the_server_config_never_hands_back_a_key() {
        let (_dir, state) = with_peer().await;

        let (status, body) = send(&state, "/api/v2.0/server/config?apikey=sam-key").await;
        assert_eq!(status, StatusCode::OK);

        let json = parse(&body);
        assert_eq!(json["api_key"], "", "an API key must never be echoed back");
        assert!(
            !body.contains("sam-key"),
            "the presented key leaked: {body}"
        );
        assert!(json["app_version"].is_string());
    }

    #[tokio::test]
    async fn json_results_are_shaped_the_way_a_jackett_client_expects() {
        let (_dir, state) = with_peer().await;

        let (status, body) =
            send(&state, "/api/v2.0/indexers/sharerr/results?apikey=sam-key").await;
        assert_eq!(status, StatusCode::OK);

        let json = parse(&body);
        // Capitalised keys, unlike the indexer list. That inconsistency is
        // Jackett's, and clients match on it literally.
        assert!(json["Results"].is_array(), "{body}");
        assert!(json["Indexers"].is_array(), "{body}");
        assert_eq!(json["Indexers"][0]["ID"], "sharerr");
        assert_eq!(json["Indexers"][0]["Status"], 0, "0 means no error");
    }

    /// Every admin endpoint is behind the same key as the feed. An unauthenticated
    /// caller must not be able to learn that this port is sharerr, let alone what it
    /// shares.
    #[tokio::test]
    async fn every_admin_endpoint_requires_the_key() {
        let (_dir, state) = with_peer().await;

        for uri in [
            "/api/v2.0/indexers",
            "/api/v2.0/server/config",
            "/api/v2.0/indexers/sharerr/results",
        ] {
            let (status, _) = send(&state, uri).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri} with no key");

            let (status, _) = send(&state, &format!("{uri}?apikey=wrong")).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri} with a bad key");
        }
    }

    /// An unimplemented endpoint answers 501 rather than 404, and says so in JSON.
    /// The 404 would be indistinguishable from a wrong URL; this is the signal that
    /// tells an operator what to ask for.
    #[tokio::test]
    async fn an_unimplemented_endpoint_says_so_rather_than_404ing() {
        let (_dir, state) = with_peer().await;

        let (status, body) = send(&state, "/api/v2.0/server/logs?apikey=sam-key").await;
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
        assert!(parse(&body)["error"].is_string(), "{body}");
    }

    /// The catch-all must not shadow the endpoints that *are* implemented — a
    /// wildcard route that wins over a specific one is an easy and silent mistake.
    #[tokio::test]
    async fn the_catch_all_does_not_swallow_implemented_routes() {
        let (_dir, state) = with_peer().await;

        for uri in [
            "/api/v2.0/indexers?apikey=sam-key",
            "/api/v2.0/server/config?apikey=sam-key",
            "/api/v2.0/indexers/sharerr/results?apikey=sam-key",
            "/api/v2.0/indexers/sharerr/results/torznab/api?t=caps&apikey=sam-key",
        ] {
            let (status, _) = send(&state, uri).await;
            assert_eq!(
                status,
                StatusCode::OK,
                "{uri} fell through to the catch-all"
            );
        }
    }

    /// The XML feed and the JSON results must describe the same library. Running
    /// the query twice, once per renderer, is exactly how `doctor` and the web UI's
    /// probes drifted apart before `crate::checks` existed.
    #[tokio::test]
    async fn the_json_and_xml_paths_report_the_same_number_of_releases() {
        let (_dir, state) = with_a_release().await;

        let (_, xml) = send(
            &state,
            "/api/v2.0/indexers/sharerr/results/torznab/api?t=search&apikey=sam-key",
        )
        .await;
        let (_, json_body) = send(
            &state,
            "/api/v2.0/indexers/sharerr/results?t=search&apikey=sam-key",
        )
        .await;

        let xml_count = xml.matches("<item>").count();
        let json_count = parse(&json_body)["Results"].as_array().unwrap().len();
        assert_eq!(
            xml_count, json_count,
            "the two renderers disagree about the library"
        );
        assert_eq!(
            xml_count, 1,
            "both returned nothing, so this proved nothing: {xml}"
        );
    }

    /// The JSON release must carry the same ids the XML does, or a friend's client
    /// matches on the parsed title instead — which is the whole thing Torznab ids
    /// exist to avoid.
    #[tokio::test]
    async fn a_json_release_carries_its_ids_and_download_link() {
        let (_dir, state) = with_a_release().await;

        let (status, body) = send(
            &state,
            "/api/v2.0/indexers/sharerr/results?t=search&apikey=sam-key",
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let result = parse(&body)["Results"][0].clone();
        assert_eq!(
            result["Title"],
            "Lanternwick.Hollow.S02E01.1080p.WEB-DL-FAKEGRP"
        );
        assert_eq!(result["TVDBId"], 918_273);
        // Jackett's JSON carries IMDb as a bare number; Torznab uses the `tt` form.
        assert_eq!(result["Imdb"], 1_234_567);
        assert_eq!(result["Size"], 2_147_483_648_u64);
        assert!(
            result["Link"].as_str().unwrap().ends_with(".torrent"),
            "{result}"
        );
    }

    /// The JSON renderer and the XML feed read the same `item.media`, and the two
    /// disagreeing about one release is the failure this module is kept in step
    /// to avoid — so the same fields have to arrive here too.
    #[tokio::test]
    async fn a_json_release_carries_its_media_metadata() {
        let (_dir, state) = with_a_release().await;

        let (status, body) = send(
            &state,
            "/api/v2.0/indexers/sharerr/results?t=search&apikey=sam-key",
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let result = parse(&body)["Results"][0].clone();
        assert_eq!(result["Resolution"], "1920x1080");
        assert_eq!(result["VideoCodec"], "HEVC");
        assert_eq!(result["AudioCodec"], "EAC3");
        assert_eq!(result["AudioChannels"], "5.1");
        assert_eq!(result["Subs"], "English");
        // Unset fields are omitted, matching the ids above rather than arriving
        // as JSON nulls a client has to special-case.
        assert!(result.get("Runtime").is_none(), "{result}");
        assert!(result.get("Hdr").is_none(), "{result}");
        assert!(result.get("Languages").is_none(), "{result}");
    }
}
