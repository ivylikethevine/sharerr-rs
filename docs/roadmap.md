# Roadmap

Where sharerr is and where it is going. Status is honest: _Done_ means implemented
and covered by tests, not merely written.

sharerr is **experimental**. Nothing below is a release commitment, and the
ordering is a judgement about value, not a schedule.

## Status

| Milestone | Scope | Status |
|---|---|---|
| — | First-class gluetun support: dynamic public IP and forwarded port | Next |
| — | Peer endpoint memory and gossip | Next |
| — | Builtin tracker only (removes qBittorrent's embedded tracker) | Next |
| — | Jellyfin/Emby as a library source | Next |


**Shipped, and removed from this list:** the core (M1), the builtin tracker and
Torznab feed (M2), the web UI (M3), friend/peer management (M4), Jackett
compatibility (M5), the plain tagged directory source (M6), per-friend selective
sharing, Transmission support, Lidarr/Readarr/Whisparr
support, the whole M3 follow-up backlog, and the second test stack behind gluetun. The code and `git log` are the record — carrying finished work here
only makes the list harder to read.

---

## Compatibility

sharerr sits in the middle of a stack it does not own, so most of its value comes
from how many of that stack's normal shapes it tolerates. Grouped by where each
piece plugs in, roughly in order of how much they would widen the audience.

### Library sources (where tagged content comes from)

Today: **Sonarr**, **Radarr**, **Lidarr**, **Readarr** and **Whisparr**, all via
tag-driven discovery. Adding an *arr app is now a discovery walk plus a config
section; the domain model carries music and books, and the feed advertises the
matching categories.

The **plain tagged directory** (M6) shipped: `[[library]]` entries with a
declared kind, scanned behind the same `LibrarySource` seam the *arr clients now
sit behind. As predicted it loses every external id — the release name is all a
friend's app gets — and that seam is exactly where the remaining sources plug in.

| Service                      | Why                                                                                                                                                                                                                       | Difficulty |
| ---------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------- |
| **Jellyfin / Emby**          | Not everyone runs the *arr apps. A media-server-backed source lets someone share a library they curate elsewhere — but tags are per-user and the external ids are weaker than Sonarr's, so releases match less reliably.  | Medium     |
| **Plex**                     | Same idea, but the API is more awkward and its "collections" map badly onto a share tag.                                                                                                                                  | Medium     |

### Torrent clients (what actually seeds)

Today: **qBittorrent** and **Transmission**, behind the `TorrentClient` trait in
`sharerr-client`. That trait is six operations wide, which is what made the second
client tractable — clients disagree about almost everything except "add this
torrent, with the data already at this path".

Adding a third is now mostly writing one file. The one thing a new client must
answer honestly is whether it can remove a torrent *without* deleting the data.

| Client                            | Notes                                                                                                                                                  |
| --------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **Deluge**                        | JSON-RPC, plus the `label` plugin standing in for qBittorrent's category.                                                                              |
| **rTorrent / ruTorrent**          | XML-RPC. Popular on seedboxes, which is exactly where someone would want to share a large library.                                                     |
| **Transmission-compatible forks** | Anything speaking the Transmission RPC should already work — the client is the same. Untested, and cheap to confirm.                                   |

The tracker constraint resolved itself in the simplest direction. Transmission has
no embedded tracker at all; qBittorrent's was a second announce URL to keep in step
with the first; sharerr's own tracker works regardless of client. So
`tracker.backend` goes away and the builtin tracker becomes the only one, and no
client below has to answer the question.

### Indexers (what consumes the feed)

Today: **Prowlarr**, via _Generic Torznab_, and **Jackett** compatibility is M5.

| Consumer                  | Notes                                                                                                                                                 |
| ------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| **NZBHydra2**             | Aggregates Torznab indexers; should already work, but has never been tested against sharerr. Worth confirming rather than assuming.                   |
| **Sonarr/Radarr direct**  | **Confirmed working**, and a bug was fixed to make it so — the feed had no `pubDate` and Sonarr rejected it outright. `run_docker_tests.sh` now adds sharerr to a real Sonarr and asserts the indexer test passes. |
| **Lidarr/Readarr direct** | Follows from library-source support above, since the caps document gates what they will even ask for.                                                 |

One of the two "should already work" rows turned out not to. Confirming the
remaining one (NZBHydra2) is still cheap and still worth more than another feature —
the last one found a bug that made sharerr unusable as a direct indexer.

### Deployment shapes

**VPN containers (gluetun and friends).** Partly covered:
`./run_docker_tests.sh --vpn` runs the whole suite with qBittorrent inside a VPN
container's network namespace, and `docker/deploy/` describes the shape this is
really aimed at — gluetun owning the namespace with both qBittorrent *and* sharerr
inside it, reaching each other on `localhost` rather than by container name. What
is not covered is everything dynamic about that topology: a real provider's
forwarded port and its rotating exit address, which is what the endpoint work under
_Functionality_ is for. That deployment should become a tracked, documented
topology rather than an untracked directory. The self-hosted WireGuard endpoint the
test stack terminates cannot grant a forwarded port, so proving any of this needs a
faked gluetun control server in tier 1 and an opt-in tier-2 stack for the rest.

Provider breadth belongs here too. gluetun implements port forwarding for only some
of the providers it supports, so sharerr should report which mechanism it is using
and degrade cleanly — a statically configured endpoint — where no port is granted.

**Reverse proxies and IPv6.** `tracker.advertised_host` is a single string, and the
announce URL is a hard-coded `format!("http://{host}:{port}")` — no scheme, no path
prefix, no brackets for an IPv6 literal. Announce URLs behind a proxy, on a
non-default path prefix, or over IPv6 are exactly where self-hosted setups break.
The change that makes the endpoint dynamic is the moment to make it expressive as
well, since both rewrite the same construction.

**Magnet links / DHT.** The feed serves `.torrent` files only. Magnet URIs are cheap
to add and some clients prefer them.

**Unraid / Synology templates.** Both communities install almost entirely from
templates; publishing one is packaging work rather than code, and reaches an
audience that will never run `docker run` by hand.

---

## Functionality

**Dynamic external endpoint.** `tracker.advertised_host` is one hand-typed string,
and the deployment sharerr is actually built for has neither a stable public IP nor
a stable inbound port. The endpoint should be *resolved* instead: poll gluetun's
control server (`/v1/publicip/ip` and `/v1/openvpn/portforwarded`, on `:8000` by
default) as the source of truth, and accept a push from
`VPN_PORT_FORWARDING_UP_COMMAND` on a small sharerr endpoint so a reconnect is
reacted to in seconds rather than at the next poll. The poll is the floor that
recovers a missed push; neither alone is enough.

The expensive consequence is not the discovery, it is what the announce URL is
already attached to. It is resolved once per reconciliation pass and baked into
each `.torrent` at build time, so a rotated port leaves every torrent already
sitting in a friend's client announcing to a dead address. Torrents therefore need
an announce *list* spanning the recently held endpoints, and a change of endpoint
has to trigger a re-announce and a rewrite of the affected `.torrent` files rather
than waiting for the next natural pass.

**Separate tracker port.** `serve` merges the UI, the Torznab feed and the tracker
into one router on one listener, which is the right default and stays the default.
But gluetun forwards exactly one port, and the port that has to be reachable is the
tracker's, not the web UI's. An optional `tracker.bind` gives the tracker its own
listener on the forwarded port while the UI stays on 8477 behind the LAN. Whichever
listener serves `/announce` must keep its connect-info service — the tracker
resolves a peer's address from the real socket, and has nothing to fall back on.

**Peer endpoint memory.** A peer record is a credential — label, key hash, scope,
last seen — and carries no address at all, so sharerr can say that a friend turned
up but not where they are. Peers should keep a short, timestamped history of
recently observed addresses, with the tracker address and the torrent client's
address recorded **separately**: a friend on a dual-VPN setup has the two behind
different exits while both belong to one sharerr. Observations come from a feed
pull's source address, from a tracker announce, and from gossip. Keeping the last
few, most recent first, means a reconnect that briefly returns an old exit is
remembered rather than trusted.

**Endpoint gossip.** If A, B and C share with each other at the same level and A's
address changes, B noticing first should be enough for C to learn it — nobody
should have to be reachable at their old address in order to advertise the new one.
Friends already authenticate to `/api` with a per-peer key, so endpoint records
ride that exchange rather than opening a second protocol surface. Records are
timestamped and signed by the peer they describe, so an older sighting cannot
overwrite a newer one and no friend can rewrite somebody else's address. Spread is
scoped by `PeerScope`: gossip must not tell a friend about the existence, let alone
the address, of a peer they are not already sharing with.

**Ratio and bandwidth control.** No upload limits, no seeding goals. Sharing a
library with no cap on what it costs you is a real deterrent to running this.

**Request flow.** The original design brief wanted a friend's Sonarr/Radarr to
_request_ content. Today discovery is one-way: they find what you already share.
An inbound request queue with an approve step is the other half of that idea.

**Cross-seed awareness.** The brief called for preserving existing torrents rather
than creating duplicates. If a file is already seeding in qBittorrent under another
torrent, sharerr should recognise it rather than adding a second entry for the same
bytes.

**List view of shared items.** There is no page that simply enumerates what this
instance is sharing. The data is all in the store — title, size, info hash, which
friends can see it, when it last synced — but the only way to read it today is the
per-item status the sync loop logs, or qBittorrent's own torrent list, which
answers a different question. A sortable, filterable list is the first thing an
operator asks for after setup and the last thing they can currently get.

**Health and history in the UI.** The store already records run history
(`recent_runs`), and the status page shows very little of it. Per-item state — why
_this_ file is not shared — is the question the UI cannot currently answer.

**Notifications.** Webhook/Discord/Apprise on sync failure or a peer going quiet.
Standard for the *arr ecosystem this lives in.

---

## Ease of use

**Setup wizard.** Settings is a wall of fields. A guided first-run — services, then
paths, then tracker, validating each step — matches how the *arr apps onboard and
would catch misconfiguration at the point it is introduced.

**Auto-detect path mappings.** sharerr knows what Sonarr reports and what it can
see; proposing the mapping instead of asking the operator to derive it removes the
single most error-prone configuration step.

**`sharerr doctor --fix`** for the mechanical cases (missing tag, wrong category).

**One-glance "is it working?"** The status page reports readiness; it does not
plainly say _n items shared, last sync 4 minutes ago, 2 peers connected_.

**Better first-run defaults.** `tracker.advertised_host` has no default and a wrong
value silently produces torrents nobody can announce to — detectable at save time.
Resolving it from gluetun removes the guess entirely where that is available; where
it is not, it is still a hand-typed value worth validating before it is saved.

---

## Engineering

Not user-facing, but load-bearing for everything above.

- **`missing_docs` as an enforced lint.** The 44 primary public items (types,
  traits, methods) are now documented. The lint itself is *not* on, and the earlier
  "roughly fifteen" estimate was wrong by twenty times: `missing_docs` also flags
  every public struct field, which is 290 warnings across the workspace. Most are
  self-describing config fields where a doc comment would be filler that makes the
  code harder to read, not easier. Worth revisiting only if a way to scope it to
  items rather than fields turns up.
- **Router-level coverage of the Torznab search handlers.** The tracker now has it;
  Torznab's own routes are covered by the Jackett tests but not exhaustively.
- **Builtin tracker only.** Removing `TrackerBackend::QbittorrentEmbedded` takes
  `QbitEmbeddedTracker` and `TorrentClient::embedded_tracker_port()` with it, which
  reaches into `sharerr-qbit`, `sharerr-transmission`, `doctor`,
  `docker/config-vpn/`, `docker/deploy/` and the README's "Which tracker" section.
  It is a **breaking config change**: a `sharerr.toml` naming `qbittorrent-embedded`
  will fail to load until it is edited, and the error should say precisely that. The
  justification is the endpoint work above — two tracker backends mean two
  independently built announce URLs, and every dynamic-endpoint change would have to
  be made twice and tested twice.
- **One announce-URL resolver.** `Config::public_base_url()` and the tracker
  providers build the same URL out of the same two fields by different routes. They
  have to collapse into a single resolver *before* the value starts changing at
  runtime, or the two paths will drift the first time one of them is updated.
- **Reachability in `doctor`.** The `tracker.advertised_host` check only warns when
  the field is unset. It should compare the resolved endpoint against gluetun's
  reported public IP and attempt an actual inbound connection, because from inside
  the namespace a closed forwarded port and a quiet swarm look identical.
