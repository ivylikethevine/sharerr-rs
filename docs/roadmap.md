# Roadmap

Where sharerr is and where it is going. Status is honest: *Done* means implemented
and covered by tests, not merely written.

sharerr is **experimental**. Nothing below is a release commitment, and the
ordering is a judgement about value, not a schedule.

## Status

| Milestone | Scope | Status |
|---|---|---|
| M1 | Core: config, encrypted vault, SQLite store, Sonarr/Radarr + qBittorrent clients, `doctor`, torrent construction, seeding, reconciliation loop | **Done** |
| M2 | Builtin tracker + Torznab feed | **Done** |
| M3 | Web UI: setup/login, settings, per-service connection tests, status page | **Done** |
| M4 | Friend/peer management — per-peer keys, peers page, last-seen | **Done** |
| M5 | Jackett compatibility — search half done, admin half open | **Partly done** |
| M6 | A second test stack, behind gluetun | Next |
| M7+ | Everything below | Backlog |

The M3 follow-up backlog is now cleared: the checks `doctor` and the web UI run are
shared rather than duplicated, path-mapping diagnostics have a page of their own,
an operator can change their own password, the recovery loop backs off, config paths
are constants with tests on both sides, `ServeState` no longer lives in the CLI
layer, and the router is covered by tests that drive the real middleware stack.

---

## M4 — Friend/peer management — **done**

Shipped. The Torznab feed used to be guarded by a single `torznab.api_key` handed
to everybody, which made two ordinary things impossible: seeing whether a friend
had actually got set up, and cutting one person off without cutting off everyone.

What exists now:

- **A `peers` table** (`migrations/0003_peers.sql`). Each friend is a label, a
  hashed key, and created/last-seen/revoked timestamps.
- **Per-peer Torznab auth.** A peer's key is checked first, by indexed lookup; the
  old shared key still works so upgrading breaks nobody, and the Friends page warns
  while it is still set — because until it is cleared, revoking someone does not
  actually cut them off.
- **`last_seen`**, recorded on each authenticated request. "never" is the answer
  that matters: the friend has the key but has not finished setting up.
- **A Friends page** at `/peers` — add, revoke, delete, with the key revealed once
  on creation alongside the feed URL to paste into their Prowlarr.

**One design note worth keeping.** Peer keys are hashed with SHA-256, not Argon2.
Argon2 exists to make *low-entropy human passwords* expensive to guess; a peer key
is 160 bits from the system CSPRNG, so a slow hash buys nothing against that threat
— and costs something real against one that matters, because a slow hash cannot be
looked up by index. It would mean verifying the presented key against every peer
row in turn on every single feed request. See the header of `0003_peers.sql`.

**Still open, deliberately:**

- **Per-peer announce tokens.** `tracker.token` is still one shared secret, so the
  builtin tracker cannot tell peers apart the way the feed now can. Same idea,
  applied one layer down.
- **Retiring the shared key.** It is kept for compatibility and flagged in the UI,
  but nothing yet walks an operator through migrating their friends off it.
- **Selective sharing per peer** — see Functionality below. Now expressible for the
  first time, since there is finally an identity to scope against.

---

## M5 — Jackett compatibility

**The search half is done.** A client configured for Jackett does not send a
different *query* — the grammar is the same Torznab already implemented. What
differed was only where it sends it: Jackett namespaces each tracker it proxies
under `/api/v2.0/indexers/<id>/results/torznab/`, and clients append `api?t=...`.
So anything set up for Jackett failed against a bare `/api` for purely clerical
reasons.

All three shapes a client may produce now serve the same feed — with and without
the trailing `/api`, with and without the trailing slash — and the document is
asserted byte-identical to `/api`'s so the two cannot drift. The indexer id is
accepted and ignored, including Jackett's `all` aggregate: sharerr is the only
thing it serves, so every id means the same feed, and rejecting unfamiliar ones
would only break someone pasting the id from their old config. Authentication is
unchanged — the Jackett path is not a way around it, and a test says so.

Download links needed nothing: the enclosure URLs in the feed are absolute and
already point at this instance, so a client follows them whichever path it searched
through.

**The admin half is open**, and is much larger: `/api/v2.0/indexers` CRUD, server
config, indexer definitions. Almost all of it describes a multi-indexer aggregator
that sharerr is not, so the right next step is not to build it speculatively but to
point a real Prowlarr at the Jackett path — the test stack already has one behind
the `indexer` profile — and see which endpoints a client actually calls before
deciding.

---

## Compatibility

sharerr sits in the middle of a stack it does not own, so most of its value comes
from how many of that stack's normal shapes it tolerates. Grouped by where each
piece plugs in, roughly in order of how much they would widen the audience.

### Library sources (where tagged content comes from)

Today: **Sonarr** and **Radarr**, via tag-driven discovery.

| Service | Why | Difficulty |
|---|---|---|
| **Lidarr** (music) | `MediaSource` is already an enum and the v1 API is close to the *arr v3 shape. The Torznab caps document currently advertises `music-search` as unavailable, so the feed side is a one-line change once discovery exists. | Low |
| **Readarr** (books) | Same shape again. Note upstream Readarr is no longer actively maintained, so weigh it against its forks. | Low |
| **Whisparr** | Same *arr codebase; would come almost free alongside Lidarr. | Low |
| **Jellyfin / Emby** | Not everyone runs the *arr apps. A media-server-backed source lets someone share a library they curate elsewhere — but tags are per-user and the external ids are weaker than Sonarr's, so releases match less reliably. | Medium |
| **Plex** | Same idea, but the API is more awkward and its "collections" map badly onto a share tag. | Medium |
| **A plain tagged directory** | The escape hatch: point sharerr at a folder and share what is in it, no *arr app at all. Loses every external id, so a friend's Sonarr falls back to parsing the release name. Worth having as the zero-dependency path. | Low |

### Torrent clients (what actually seeds)

Today: **qBittorrent**, via the WebUI v2 API. This is the hardest deployment
prerequisite sharerr imposes, and the interface it needs is narrow — add a torrent
by file, pin its save path, never move the data — so most clients can satisfy it.

| Client | Notes |
|---|---|
| **Transmission** | RPC API is simple and well documented; `download-dir` per-torrent covers the never-move requirement. The most requested alternative in this ecosystem. |
| **Deluge** | JSON-RPC, plus the `label` plugin standing in for qBittorrent's category. |
| **rTorrent / ruTorrent** | XML-RPC. Popular on seedboxes, which is exactly where someone would want to share a large library. |
| **Transmission-compatible forks** | Anything speaking the Transmission RPC comes free with the above. |

The one real constraint is the tracker: sharerr currently relies on qBittorrent's
*embedded tracker* as the default backend. Every client above would have to use
sharerr's own builtin tracker instead — which already exists, and is the reason it
was built.

### Indexers (what consumes the feed)

Today: **Prowlarr**, via *Generic Torznab*, and **Jackett** compatibility is M5.

| Consumer | Notes |
|---|---|
| **NZBHydra2** | Aggregates Torznab indexers; should already work, but has never been tested against sharerr. Worth confirming rather than assuming. |
| **Sonarr/Radarr direct** | They speak Torznab themselves, so a friend can skip Prowlarr entirely and add sharerr as an indexer directly. Likely already works — again, untested. |
| **Lidarr/Readarr direct** | Follows from library-source support above, since the caps document gates what they will even ask for. |

Confirming the two "should already work" rows is cheap and would let the README
state them, which is worth more than another feature.

### Deployment shapes

**VPN containers (gluetun and friends).** See the test-suite item below — this is
the shape most likely to break sharerr today, and the least covered.

**Reverse proxies and IPv6.** `tracker.advertised_host` is a single string.
Announce URLs behind a proxy, on a non-default path prefix, or over IPv6 are exactly
where self-hosted setups break, and sharerr cannot currently express most of them.

**Magnet links / DHT.** The feed serves `.torrent` files only. Magnet URIs are cheap
to add and some clients prefer them.

**Unraid / Synology templates.** Both communities install almost entirely from
templates; publishing one is packaging work rather than code, and reaches an
audience that will never run `docker run` by hand.

---

## Functionality

**Selective sharing.** The `sharerr` tag is all-or-nothing. Per-peer scoping —
this friend sees the TV library, that one sees films — is the natural pairing with
M4's per-peer identity, and is the feature most likely to be asked for the moment
peers exist.

**Ratio and bandwidth control.** No upload limits, no seeding goals. Sharing a
library with no cap on what it costs you is a real deterrent to running this.

**Request flow.** The original design brief wanted a friend's Sonarr/Radarr to
*request* content. Today discovery is one-way: they find what you already share.
An inbound request queue with an approve step is the other half of that idea.

**Cross-seed awareness.** The brief called for preserving existing torrents rather
than creating duplicates. If a file is already seeding in qBittorrent under another
torrent, sharerr should recognise it rather than adding a second entry for the same
bytes.

**Health and history in the UI.** The store already records run history
(`recent_runs`), and the status page shows very little of it. Per-item state — why
*this* file is not shared — is the question the UI cannot currently answer.

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
plainly say *n items shared, last sync 4 minutes ago, 2 peers connected*.

**Better first-run defaults.** `tracker.advertised_host` has no default and a wrong
value silently produces torrents nobody can announce to — detectable at save time.

---

## A second test stack, behind gluetun

**The gap this closes.** The README's own assumptions say the user "may or may not
be using a VPN, or a VPN container such as gluetun" — and that is the single most
common shape in this ecosystem. The existing `docker/compose.test.yml` does not
reproduce it at all: every service sits on a plain bridge network with its own
address and its own published ports. So the deployment most people actually run is
the one nothing has ever exercised.

**Why it is a genuinely different test, not a variation.** Putting qBittorrent
behind gluetun means `network_mode: "service:gluetun"`, and that changes the things
sharerr is most fragile about:

- **qBittorrent stops having its own address.** It shares gluetun's network
  namespace, so sharerr must reach it at `http://gluetun:8080`, not
  `http://qbittorrent:8080`. Anything that assumed the service name equals the
  hostname breaks here.
- **Ports must be published on gluetun**, not on the container that listens. A port
  declared on a `network_mode: service:` container is a compose error, which is
  exactly the kind of thing a test should catch once instead of every user
  discovering separately.
- **The announce address is no longer the host's.** This is the important one.
  Peers reach the swarm through the VPN's exit address, so `tracker.advertised_host`
  and the embedded tracker's port have to describe the *tunnel*, not the machine. A
  wrong value here produces torrents that look perfect and that nobody can announce
  to — silent, and precisely the failure mode sharerr exists to prevent.
- **Killswitch behaviour.** When the tunnel drops, gluetun severs egress. sharerr
  should degrade legibly — `/ready` explaining itself, the recovery loop backing off
  and picking itself up when the tunnel returns — rather than wedging or
  restart-looping. Nothing tests that path today.

**Shape.** A second file, `docker/compose.vpn.yml`, alongside the existing one
rather than replacing it — the current stack is the fast, dependency-free tier and
should stay that way. Gluetun would run in a loopback or self-hosted-WireGuard mode
so the suite needs no VPN subscription and no real egress; the point is the
*topology*, not the tunnel. Driven by `run_docker_tests.sh --vpn`, reusing the same
fixtures, seeding, and e2e assertions, plus new ones for the announce URL and the
killswitch. Opt-in and local, like the existing tier — it must never become a CI
dependency.

**Why it is worth the weight:** it is the only way to prove the announce-address
logic against a topology where the naive answer is wrong. Every other test in the
project runs somewhere the host address happens to be correct.

---

## Engineering

Not user-facing, but load-bearing for everything above.

- **`missing_docs` on the library crates.** Module docs are excellent; a number of
  primary public items (`ArrClient::new`, `Vault`, the `AddTorrent` builder) carry
  none. Turning the lint on would surface roughly fifteen.
- **A dedicated MSRV job.** Today an MSRV break surfaces as a Docker build failure
  inside the multi-arch matrix rather than as a named, fast-failing job.
- **Dependabot or renovate.** Nothing currently proposes dependency updates.
- **Coverage of the tracker and Torznab handlers**, which have unit tests but no
  router-level tests of the kind `web/` now has.
