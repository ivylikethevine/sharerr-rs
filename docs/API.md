# The API

sharerr's machine-facing HTTP surface has a formal contract:
[`openapi.json`](openapi.json), an OpenAPI 3.1 document. Point a viewer at it,
or feed it to a generator — every operation has an `operationId`, so the
generated method names are stable.

## What it covers

| Tag          | What it is                                                              |
| ------------ | ----------------------------------------------------------------------- |
| `torznab`    | The indexer feed a friend's Sonarr/Radarr/Lidarr/Prowlarr queries.       |
| `jackett`    | The same feed at Jackett's URL shapes, plus its read-only admin routes.  |
| `gossip`     | How friends tell each other where they have moved to.                    |
| `tracker`    | The built-in BitTorrent tracker, and the `.torrent` files the feed links to. |
| `lighthouse` | Key-hash-to-endpoint rendezvous, when both friends' addresses rotated.   |
| `ops`        | Liveness, readiness, and gluetun's port-forward hooks.                   |

Every feed and gossip operation is authenticated by a per-peer key in the
query string — the `peerApiKey` scheme in the document — with one exception,
the Jackett catch-all `/api/v2.0/{rest}`, which answers 501 to anyone. Two
Jackett URL shapes that also work (a trailing `/`, and a trailing `/api`) are
mounted but deliberately left out of the document, so the one operation does
not read as three.

The server-rendered web UI is deliberately **not** in it. Its `/`, `/items`,
`/topology`, `/debug`, `/settings/*`, `/peers/*` and `/wizard/*` routes (and
the public `/setup`, `/login`, `/logout`, `/assets/*`) are HTML pages and form
posts authenticated by a session cookie, answering with redirects and markup;
publishing a contract for them would promise stability to something whose
whole shape is allowed to change with the templates.

## Why it cannot go stale

The document is generated from the handlers, not written alongside them.

Each handler carries a `#[utoipa::path]` attribute next to its own doc comment,
and every machine-facing route is mounted through `utoipa-axum`'s
`OpenApiRouter` — almost all via `routes!`, which takes the path **from that
attribute**. So a route cannot be added without an entry, and an entry cannot
name a path that nothing serves. Six paths are mounted with a plain `.route`
and listed in the document by hand, for reasons recorded where they are
mounted — the tracker's announce and scrape paths (with and without a token)
come from `sharerr-torrent`'s constants, and axum spells the Jackett catch-all
`{*rest}` where OpenAPI spells it `{rest}` — and those are held to the router
by tests that drive the real thing.

A committed file can still drift from the code that generates it, so
`the_committed_document_is_current` fails the build when this one does. That
matters more here than in most places: the person reading it is a client author
working out why their app rejects the feed, and a confidently wrong answer is
worse for them than no answer at all.

## Regenerating it

```bash
cargo run -- openapi --output docs/openapi.json
```

`sharerr openapi` opens no vault and no database and needs no running
instance — it works anywhere the binary does. Like every subcommand it does
load `--config` first, but a missing file is fine, and a malformed one only
logs an error (to stdout, so redirect `--output` to a file rather than
piping). Without `--output` (`-o`) it prints to stdout.

## What is not served at runtime

There is no `/openapi.json` route. A sharerr instance answers unauthenticated
callers as little as it can — the tracker refuses without confirming what it
holds, and the lighthouse fabricates an answer rather than admitting an
instance exists — and an unauthenticated endpoint that hands out a document
titled "sharerr" would undo a good deal of that. The document ships with the
source instead, which is where a client author is anyway.
