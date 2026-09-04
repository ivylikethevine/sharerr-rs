//! The OpenAPI document for sharerr's machine-facing API.
//!
//! # Why this is generated rather than written
//!
//! A hand-written spec is a second description of the same thing, and this
//! project has already been bitten once by exactly that shape — `doctor` and
//! the web UI's probes disagreeing about the same library, which is why
//! `crate::checks` exists. A spec that drifts from the router is worse than no
//! spec: it is confidently wrong, and the person reading it is a client author
//! who has no way to tell.
//!
//! So the document comes from the handlers. Each one carries a
//! `#[utoipa::path]` attribute next to its own doc comment, and every route is
//! mounted through [`OpenApiRouter`](utoipa_axum::router::OpenApiRouter), which
//! takes the path *from that attribute* — a route cannot be added without an entry, and an entry cannot
//! name a path nothing serves. [`document`] then assembles those routers with
//! no state at all, which is what lets `sharerr openapi` run on a machine with
//! no config and no database.
//!
//! # What is in scope
//!
//! Everything a program calls: the Torznab feed, the Jackett-compatible
//! surface, gossip, the BitTorrent tracker, the operational endpoints, and the
//! lighthouse — which is here because `sharerr serve` merges it onto the
//! frontend listener in some topologies, so it is genuinely part of this
//! service's surface.
//!
//! The web UI is not. Its `/settings/*` and `/peers/*` routes are HTML pages
//! and form posts authenticated by session cookie, answering with redirects and
//! markup; describing them here would imply a contract for something whose
//! whole shape is allowed to change with the templates.
//!
//! # The exceptions
//!
//! A handful of routes are mounted by hand rather than through `routes!`, each for a
//! reason recorded where it is mounted, and so are listed in [`ApiDoc`]'s own
//! `paths(...)` instead:
//!
//! * The five **tracker** routes keep deriving their paths from
//!   `sharerr_torrent`'s constants, which is what stops the announce URLs the
//!   factory bakes into torrents drifting from the routes that serve them.
//! * The Jackett **catch-all** is `{*rest}` in axum and `{rest}` in OpenAPI;
//!   only one of those spellings can be the mounted one.
//!
//! Both are held to the router by the tests at the bottom of this file, which
//! drive the real thing and check that every documented path answers.

use utoipa::openapi::security::{ApiKey, ApiKeyValue, Http, HttpAuthScheme, SecurityScheme};
use utoipa::{Modify, OpenApi};

/// The parts of the document that are not derived from a handler: the
/// preamble, the tag descriptions, the security scheme, and the three
/// hand-mounted routes.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "sharerr",
        description = "\
The machine-facing HTTP API of a sharerr instance: the indexer feed a friend's \
Sonarr or Radarr queries, the BitTorrent tracker their client announces to, the \
gossip exchange that keeps addresses current, the lighthouse rendezvous, and the \
operational endpoints an orchestrator polls.

The server-rendered web UI is deliberately absent — see the `sharerr::openapi` \
module docs.

**Authentication.** Every feed and gossip endpoint takes a peer's own API key as \
an `apikey` query parameter. Each friend holds a different one, so a single \
friend can be cut off without disturbing the others, and every request records \
who made it. There is no unauthenticated read path: a missing key is refused \
exactly the way a wrong one is, because saying \"this instance has no key \
configured\" would confirm the port belongs to sharerr.

**Base URL.** None is listed. A sharerr instance is reached at whatever address \
its operator advertises — often behind a VPN with a rotating forwarded port, \
which is the whole reason the gossip and lighthouse endpoints exist.",
        license(name = "MIT"),
        version = env!("CARGO_PKG_VERSION"),
    ),
    tags(
        (name = "torznab", description = "\
The indexer feed. This is the surface a friend's Sonarr, Radarr, Lidarr or \
Prowlarr talks to, and the one to check first when a client rejects the feed."),
        (name = "jackett", description = "\
The same feed at Jackett's URL shapes, plus its two read-only admin endpoints, \
for clients configured as though sharerr were a Jackett instance. The search \
runs through the same code as `torznab`, so the two cannot report different \
libraries."),
        (name = "gossip", description = "\
How friends tell each other where they have moved to. Records are signed by the \
peer they describe, so a relayed one needs no trust in the relayer."),
        (name = "tracker", description = "\
The built-in BitTorrent tracker, and the `.torrent` files the feed links to. It \
admits announces only for torrents this instance is currently sharing, so it \
never becomes an open tracker."),
        (name = "lighthouse", description = "\
Key-hash-to-endpoint rendezvous, for two friends whose addresses both rotated \
while neither was watching. Answers a lookup with a *fabricated* record rather \
than an error when the key is unknown, so a probe cannot learn that an instance \
exists — verify the signature against the pubkey you expect."),
        (name = "ops", description = "\
Liveness, readiness, and the two hooks gluetun calls when a forwarded port \
appears or goes away."),
    ),
    modifiers(&PeerApiKey, &MetricsToken),
    paths(
        // See the module docs: mounted by hand, so listed by hand.
        crate::tracker::announce,
        crate::tracker::announce_with_token,
        crate::tracker::scrape,
        crate::tracker::scrape_with_token,
        crate::tracker::torrent_file,
        crate::jackett::unimplemented_endpoint,
    ),
)]
struct ApiDoc;

/// The `apikey` query parameter every feed and gossip endpoint takes.
///
/// Declared here rather than on each operation because a security scheme is a
/// document-level object; the operations only reference it by name.
struct PeerApiKey;

impl Modify for PeerApiKey {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        // `components` is always populated by the derive above — every tagged
        // surface has at least one schema — but building it here rather than
        // unwrapping keeps this correct if that ever stops being true.
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "peerApiKey",
            SecurityScheme::ApiKey(ApiKey::Query(ApiKeyValue::with_description(
                "apikey",
                "The API key this instance issued to one friend. Issued from \
                 Settings → Peers, shown once, and stored only as a hash — losing \
                 one means issuing a new one, which is the correct behaviour for a \
                 bearer credential.",
            ))),
        );
    }
}

/// The `Authorization: Bearer` token `/metrics` and `/dashboard` require —
/// see [`crate::metrics`]. A separate scheme from [`PeerApiKey`] because it is
/// a different credential entirely: one shared secret for every caller, set
/// from Settings → Metrics, not one key per friend.
struct MetricsToken;

impl Modify for MetricsToken {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "metricsToken",
            SecurityScheme::Http(Http::new(HttpAuthScheme::Bearer)),
        );
    }
}

/// The assembled document.
///
/// Built from the same `OpenApiRouter`s `sharerr serve` mounts, with no state:
/// the routers are split apart for their OpenAPI half and the axum half is
/// dropped, so this needs neither a database nor a readable config.
pub fn document() -> utoipa::openapi::OpenApi {
    let mut doc = ApiDoc::openapi();
    for part in [
        crate::torznab::api_router().split_for_parts().1,
        crate::commands::serve::ops_router().split_for_parts().1,
        sharerr_lighthouse::api_spec(),
    ] {
        doc.merge(part);
    }
    doc
}

/// The document as pretty JSON, which is what `sharerr openapi` prints and what
/// `docs/openapi.json` holds.
pub fn to_json() -> anyhow::Result<String> {
    Ok(doc_to_json(&document())?)
}

fn doc_to_json(doc: &utoipa::openapi::OpenApi) -> serde_json::Result<String> {
    serde_json::to_string_pretty(doc)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    /// See `sharerr-lighthouse`'s tests: never a fixed array.
    fn random_secret() -> [u8; 32] {
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).expect("the OS RNG is available");
        secret
    }

    use super::*;

    /// Each operation in the document, as `(method, path, operation)`.
    ///
    /// `PathItem` has one field per HTTP method rather than a map, so the list
    /// is spelled out — and spelling it out is also what makes a method
    /// appearing in the document but not here a compile error rather than a
    /// silently skipped check.
    fn operations() -> Vec<(&'static str, String, utoipa::openapi::path::Operation)> {
        let doc = document();
        let mut out = Vec::new();
        for (path, item) in &doc.paths.paths {
            for (method, operation) in [
                ("get", &item.get),
                ("put", &item.put),
                ("post", &item.post),
                ("delete", &item.delete),
                ("options", &item.options),
                ("head", &item.head),
                ("patch", &item.patch),
                ("trace", &item.trace),
            ] {
                if let Some(operation) = operation {
                    out.push((method, path.clone(), operation.clone()));
                }
            }
        }
        out.sort_by(|a, b| (&a.1, a.0).cmp(&(&b.1, b.0)));
        out
    }

    /// Every documented path, method discarded.
    fn documented() -> Vec<(String, String)> {
        operations()
            .into_iter()
            .map(|(method, path, _)| (method.to_owned(), path))
            .collect()
    }

    #[test]
    fn every_machine_facing_surface_is_present() {
        let paths: Vec<String> = documented().into_iter().map(|(_, p)| p).collect();
        for expected in [
            "/api",
            "/api/gossip/endpoints",
            "/api/v2.0/indexers",
            "/api/v2.0/server/config",
            "/api/v2.0/indexers/{indexer}/results",
            "/api/v2.0/indexers/{indexer}/results/torznab",
            "/api/v2.0/{rest}",
            "/announce",
            "/announce/{token}",
            "/scrape",
            "/scrape/{token}",
            "/torrents/{name}",
            "/health",
            "/ready",
            "/gluetun/refresh",
            "/gluetun/down",
            "/metrics",
            "/dashboard",
            "/lighthouse/v1/health",
            "/lighthouse/v1/report/{key_hash}",
            "/lighthouse/v1/lookup/{key_hash}",
        ] {
            assert!(
                paths.iter().any(|p| p == expected),
                "{expected} is missing from the OpenAPI document:\n{paths:#?}"
            );
        }
    }

    /// The web UI is out of scope, and the way that goes wrong is silently:
    /// somebody annotates a settings handler and the document grows a contract
    /// for a form that was never meant to have one.
    #[test]
    fn the_web_ui_is_not_in_the_document() {
        for (_, path) in documented() {
            assert!(
                !path.starts_with("/settings")
                    && !path.starts_with("/peers")
                    && !path.starts_with("/wizard"),
                "{path} is a web UI route and does not belong in the API document"
            );
        }
    }

    /// The tracker's routes are mounted from `sharerr_torrent`'s constants and
    /// documented from a literal — see the module docs for why the constants
    /// stay. This is the seam that holds the two together: the announce URL the
    /// factory bakes into every torrent is built from the same constants, so a
    /// change here that did not reach the document would publish torrents
    /// pointing at an undocumented path.
    #[test]
    fn the_documented_tracker_paths_match_the_constants() {
        assert_eq!(sharerr_torrent::ANNOUNCE_PATH, "/announce");
        assert_eq!(sharerr_torrent::SCRAPE_PATH, "/scrape");
    }

    /// Every operation needs an `operationId`: it is what a generator names the
    /// method it produces, and utoipa will happily emit an operation without
    /// one.
    #[test]
    fn every_operation_is_named() {
        for (method, path, operation) in operations() {
            assert!(
                operation.operation_id.is_some(),
                "{method} {path} has no operationId"
            );
        }
    }

    /// The feed and gossip are closed surfaces. An operation that lost its
    /// `security` would read as an open endpoint to anyone generating a client
    /// from this — and they would find out otherwise as a 401 they had no
    /// reason to expect.
    #[test]
    fn every_feed_and_gossip_operation_states_that_it_needs_a_key() {
        for (method, path, operation) in operations() {
            // The Jackett catch-all is the one `/api` path that is open: it
            // answers 501 to anyone, which tells them nothing.
            let closed = path.starts_with("/api") && !path.contains("{rest}");
            if closed {
                assert!(
                    operation.security.is_some(),
                    "{method} {path} is a closed surface but claims no security"
                );
            }
        }
    }

    /// Every documented path must actually be routed.
    ///
    /// `routes!` guarantees this for the surfaces that use it, by construction.
    /// The tracker's five routes and the Jackett catch-all are mounted by hand
    /// (see the module docs), and this is what stands in for that guarantee:
    /// the real routers are built and driven, and a path the document names but
    /// nothing serves comes back 404 from axum rather than from a handler.
    #[tokio::test]
    async fn every_documented_path_is_routed() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let tracker = std::sync::Arc::new(crate::tracker::TrackerState::new(serve.clone()));

        for (method, path) in documented() {
            // Substitute something plausible for each templated segment. What
            // comes back does not matter — an unconfigured instance refuses
            // nearly everything — only that *something* was routed to.
            let concrete = path
                .replace("{token}", "sometoken")
                .replace("{name}", "0000000000000000000000000000000000000000.torrent")
                .replace("{key_hash}", &"a".repeat(64))
                .replace("{indexer}", "sharerr")
                .replace("{rest}", "something/unimplemented");

            let router = if path.starts_with("/announce")
                || path.starts_with("/scrape")
                || path.starts_with("/torrents")
            {
                crate::tracker::routes(tracker.clone())
            } else if path.starts_with("/lighthouse/") {
                // Not sharerr's own router: the lighthouse ships as its own
                // service and `serve` merges it in, so its routes are checked
                // against the thing that actually serves them.
                // A random secret rather than a constant. This document
                // generator never exercises a decoy, so the value is
                // irrelevant here — but an all-zero cryptographic key sitting
                // in the tree is exactly what a copy-paste turns into a
                // production default.
                sharerr_lighthouse::routes(std::sync::Arc::new(
                    sharerr_lighthouse::LighthouseState::new(random_secret()),
                ))
            } else if path.starts_with("/api") {
                crate::torznab::routes(serve.clone())
            } else {
                let (ops, _) = crate::commands::serve::ops_router()
                    .with_state(serve.clone())
                    .split_for_parts();
                ops
            };

            // A fallback, because a handler's own 404 and axum's "no such
            // route" 404 are the same status — and `/torrents/{name}` answers
            // 404 for a hash this instance does not share, which is a routed
            // path behaving correctly. Anything that reaches this was not
            // routed at all.
            let router = router.fallback(|| async { axum::http::StatusCode::IM_A_TEAPOT });

            let method = axum::http::Method::from_bytes(method.to_uppercase().as_bytes())
                .expect("a method the document names");
            let response = router
                .oneshot(
                    Request::builder()
                        .method(method.clone())
                        .uri(&concrete)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();

            assert_ne!(
                response.status(),
                axum::http::StatusCode::IM_A_TEAPOT,
                "{method} {path} is in the document but nothing routes {concrete}"
            );
        }
    }

    /// `docs/openapi.json` is the generated document, committed. It is what a
    /// client author reads without building the project, so a stale copy is a
    /// wrong answer given confidently — the failure this whole module exists to
    /// avoid.
    #[test]
    fn the_committed_document_is_current() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/openapi.json");
        let committed = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));

        assert_eq!(
            committed.trim_end(),
            to_json().unwrap(),
            "docs/openapi.json is out of date — regenerate it with \
             `cargo run -- openapi --output docs/openapi.json`"
        );
    }

    #[test]
    fn the_document_serialises() {
        let json = to_json().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["openapi"].as_str().unwrap_or_default()[..1], *"3");
        assert_eq!(parsed["info"]["title"], "sharerr");
        assert!(
            parsed["components"]["securitySchemes"]["peerApiKey"].is_object(),
            "the security scheme should survive serialisation"
        );
    }
}
