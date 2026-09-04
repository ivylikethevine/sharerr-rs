# The API

sharerr's machine-facing HTTP surface has a formal contract:
[`openapi.json`](openapi.json), an OpenAPI 3.1 document. Point a viewer at it,
or feed it to a generator; every operation has an `operationId`, so generated
method names are stable.

## Table of contents

- [What it covers](#what-it-covers)
- [Why it cannot go stale](#why-it-cannot-go-stale)
- [Regenerating it](#regenerating-it)
- [What is not served at runtime](#what-is-not-served-at-runtime)

## What it covers

| Tag          | What it is                                                                      |
| ------------ | ------------------------------------------------------------------------------- |
| `torznab`    | The indexer feed a friend's Sonarr/Radarr/Lidarr/Prowlarr queries.              |
| `jackett`    | The same feed at Jackett's URL shapes, plus its read-only admin routes.         |
| `gossip`     | How friends tell each other where they have moved to.                           |
| `tracker`    | The built-in BitTorrent tracker, and the `.torrent` files the feed links to.    |
| `lighthouse` | Key-hash-to-endpoint rendezvous, when both friends' addresses rotated.          |
| `ops`        | Liveness, readiness, gluetun's port-forward hooks, `/metrics` and `/dashboard`. |

Every feed and gossip operation is authenticated by a per-peer key in the
query string (the `peerApiKey` scheme), with one exception: the Jackett
catch-all `/api/v2.0/{rest}`, which answers 501 to anyone. `/metrics` and
`/dashboard` use a separate bearer token (`metricsToken`, see
[`SETTINGS.md`](SETTINGS.md#metrics)). Two Jackett URL shapes that also work
(a trailing `/`, and a trailing `/api`) are mounted but left out of the
document, so one operation does not read as three.

The server-rendered web UI is deliberately **not** in it. Its `/`, `/items`,
`/topology`, `/debug`, `/status/tiles`, `/diagnostics`, `/settings/*`,
`/peers/*` and `/wizard/*` routes (and the public `/setup`, `/login`,
`/logout`, `/assets/*`) are HTML pages and form posts authenticated by a
session cookie; publishing a contract for them would promise stability to
something whose whole shape is allowed to change with the templates.

## Why it cannot go stale

The document is generated from the handlers, not written alongside them.
Each handler carries a `#[utoipa::path]` attribute, and every machine-facing
route is mounted through `utoipa-axum`'s `OpenApiRouter`, almost all via
`routes!`, which takes the path from that attribute. So a route cannot be
added without an entry, and an entry cannot name a path nothing serves. The
six paths mounted with a plain `.route` (the tracker's announce and scrape
paths, and the Jackett catch-all, for reasons recorded where they are
mounted) are held to the router by tests that drive the real thing. The
committed file is held to the generator by `the_committed_document_is_current`,
which fails `cargo test` when they differ.

## Regenerating it

```bash
cargo run -- openapi --output docs/openapi.json
```

`sharerr openapi` opens no vault and no database and needs no running
instance. Like every subcommand it loads `--config` first, but a missing file
is fine, and a malformed one only logs an error to stdout, so use `--output`
rather than piping.

## What is not served at runtime

There is no `/openapi.json` route. A sharerr instance answers unauthenticated
callers as little as it can, and an unauthenticated endpoint handing out a
document titled "sharerr" would undo a good deal of that. The document ships
with the source, which is where a client author is anyway.
