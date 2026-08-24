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

The server-rendered web UI is deliberately **not** in it. Its `/settings/*`,
`/peers/*` and `/wizard/*` routes are HTML pages and form posts authenticated
by a session cookie, answering with redirects and markup; publishing a contract
for them would promise stability to something whose whole shape is allowed to
change with the templates.

## Why it cannot go stale

The document is generated from the handlers, not written alongside them.

Each handler carries a `#[utoipa::path]` attribute next to its own doc comment,
and every machine-facing route is mounted through `utoipa-axum`'s
`OpenApiRouter`, which takes the path **from that attribute**. So a route
cannot be added without an entry, and an entry cannot name a path that nothing
serves. Three routes are mounted by hand for reasons recorded where they are
mounted — the tracker's five paths still come from `sharerr-torrent`'s
constants, and axum spells a catch-all `{*rest}` where OpenAPI spells it
`{rest}` — and those are held to the router by tests that drive the real thing.

A committed file can still drift from the code that generates it, so
`the_committed_document_is_current` fails the build when this one does. That
matters more here than in most places: the person reading it is a client author
working out why their app rejects the feed, and a confidently wrong answer is
worse for them than no answer at all.

## Regenerating it

```bash
cargo run -- openapi --output docs/openapi.json
```

`sharerr openapi` reads no configuration, opens no vault and no database, and
needs no running instance — it works anywhere the binary does. Without
`--output` it prints to stdout.

## What is not served at runtime

There is no `/openapi.json` route. A sharerr instance answers unauthenticated
callers as little as it can — the tracker refuses without confirming what it
holds, and the lighthouse fabricates an answer rather than admitting an
instance exists — and an unauthenticated endpoint that hands out a document
titled "sharerr" would undo a good deal of that. The document ships with the
source instead, which is where a client author is anyway.
