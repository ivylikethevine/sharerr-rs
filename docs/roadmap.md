# Roadmap

Where sharerr is and where it is going. Status is honest: _Done_ means implemented
and covered by tests, not merely written.

sharerr is **experimental**. Nothing below is a release commitment, and the
ordering is a judgement about value, not a schedule.

## Status

| Milestone | Scope | Status |
|---|---|---|
| — | The interchange: semi-anonymous endpoint rendezvous, its own image and port | Next |
| — | Ratio and bandwidth control | Next |
| — | List view of shared items | Next |
| — | Plex as a library source | Next |

**Shipped, and removed from this list:** the core (M1), the builtin tracker and
Torznab feed (M2), the web UI (M3), friend/peer management (M4), Jackett
compatibility (M5), the plain tagged directory source (M6), per-friend selective
sharing, Transmission support, Lidarr/Readarr/Whisparr
support, the whole M3 follow-up backlog, the second test stack behind gluetun,
the removal of the qBittorrent-embedded tracker backend (builtin only now, one
announce-URL resolver, a breaking `tracker.backend` config error that says so),
first-class gluetun support (dynamic public IP and forwarded port, announce
lists, torrent rewrite and client re-announce on rotation, the `/gluetun/refresh`
push, `tracker.advertised_url` and `tracker.bind`, doctor reachability), peer
endpoint memory and signed endpoint gossip, and Jellyfin/Emby as a library
source. The code and `git log` are the record — carrying finished work here only
makes the list harder to read.

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

**Jellyfin / Emby** shipped behind the same seam: tag a movie, series, album, or
book in Jellyfin and sharerr discovers it (`sharerr-jellyfin`, one client for
both servers). The prediction held — `ProviderIds` are passed through but are
only as good as Jellyfin's metadata match, there is no scene name, and a tag on
an individual episode is deliberately not discovered: tag the series. Its items
are scoped per item by kind, like directory items, because one Jellyfin holds
every kind at once.

| Service                      | Why                                                                                                                                                                                                                       | Difficulty |
| ---------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------- |
| **Plex**                     | Same idea as Jellyfin, but the API is more awkward and its "collections" map badly onto a share tag.                                                                                                                       | Medium     |

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

The tracker constraint resolved itself in the simplest direction, and has now
shipped that way: `tracker.backend` is gone, the builtin tracker is the only
one, and no client below has to answer the question. (A config still naming the
old backend fails to load with an error that says exactly this.) What a new
client must still answer honestly: whether it can remove a torrent *without*
deleting the data, and it now also implements `set_trackers` so a rotated
endpoint can be repointed in place.

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

**VPN containers (gluetun and friends).** Shipped, both halves.
`./run_docker_tests.sh --vpn` runs the whole suite with qBittorrent inside a VPN
container's network namespace, `docker/deploy/` documents the real shape —
gluetun owning the namespace with qBittorrent *and* sharerr inside it — and the
dynamic side landed as the `[gluetun]` config section: the control server is
polled as the source of truth, `VPN_PORT_FORWARDING_UP_COMMAND` nudges
`/gluetun/refresh` so a reconnect is reacted to in seconds, and the faked
control server lives in tier 1 (wiremock) as predicted. Where the provider
grants no port, sharerr says so and degrades to the static endpoint. Untested
against a *real* forwarding provider — the self-hosted WireGuard endpoint in
tier 2 cannot grant a port — so that last mile remains an honest gap.

**Reverse proxies and IPv6.** Shipped alongside the dynamic endpoint, exactly
because both rewrite the same construction: `tracker.advertised_url` carries
scheme, port, and path prefix; a bare IPv6 `advertised_host` is bracketed; and
one resolver (`sharerr_core::endpoint`) builds every advertised URL, so the feed
links and the announce URLs can no longer drift.

**Magnet links / DHT.** The feed serves `.torrent` files only. Magnet URIs are cheap
to add and some clients prefer them.

**Unraid / Synology templates.** Both communities install almost entirely from
templates; publishing one is packaging work rather than code, and reaches an
audience that will never run `docker run` by hand.

---

## Functionality

**Dynamic external endpoint.** _Shipped_ — see the gluetun entry under
Deployment shapes. Torrents carry an announce list spanning the recently held
endpoints, and an endpoint change triggers an immediate pass that rewrites the
cached `.torrent` files (the info hash is untouched — announce lives outside the
info dictionary) and repoints the tracker lists inside the torrent client via
the new `TorrentClient::set_trackers` operation.

**Separate tracker port.** _Shipped_ as `tracker.bind`: an optional second
listener carrying only the tracker routes, sharing one swarm map with the main
listener, both with their connect-info service. The single-listener layout stays
the default.

**Peer endpoint memory.** _Shipped_: `peer_endpoints` keeps a short, timestamped
history per peer with the API, torrent-client, and tracker addresses recorded
**separately** (the dual-VPN case), newest first and bounded, so a reconnect
that briefly returns an old exit is remembered rather than trusted. Direct
observations come from a feed pull's source address (throttled with the
last-seen window); the client and tracker addresses arrive via gossip, because a
tracker announce carries no peer identity — associating announces would need
per-peer announce tokens, which remains open.

**Endpoint gossip.** _Shipped_: signed Ed25519 endpoint records ride the
peer-authenticated `/api` (`GET`/`POST /api/gossip/endpoints`). A peer's pubkey
is bound trust-on-first-use from their first self-record; records are signed by
the peer they describe and carry `signed_at`, so an older sighting cannot
overwrite a newer one and no friend can rewrite somebody else's address. Spread
is scoped *stronger* than `PeerScope`: a pull names the pubkeys the caller
already knows and gets only the intersection with ours, so nobody learns of a
peer they are not already sharing with. The outbound half — their sharerr's URL
and the key they issued us — is configured per friend on the Friends page.

**The interchange.** Gossip only helps peers who can still reach *somebody*; two
friends whose addresses both rotated while neither was watching have no path back
to each other. The interchange is the rendezvous for that case: a tiny separate
service, deliberately knowing nothing but `key hash → latest IP and port`, that a
sharerr instance reports its endpoint to and a friend queries with the API key
that peer issued them. The privacy property is the point and shapes the whole
design: a request without a valid key gets a *plausible fabricated* IP and port
rather than an error, so an unauthenticated probe cannot be distinguished from a
valid lookup — the interchange never confirms that an instance exists, and
scraping it yields only noise. That makes semi-anonymous tracking of sharerr
instances possible without any instance exposing its IP publicly. It ships as its
own docker image on its own port — not another route on sharerr's listener — so
it can be self-hosted by anyone, placed on neutral ground away from any
particular library, and carries no database worth stealing: key hashes and
last-seen addresses only. A sharerr instance treats it as one more observation
source feeding peer endpoint memory, ranked below a direct sighting of the same
peer.

The fabricated answers create the opposite problem for the *legitimate* caller:
a friend holding a valid key must be able to tell a real record from a decoy, or
the noise defeats them too. So a genuine record is verifiable — the natural shape
is the same signed endpoint record gossip uses, signed by the peer it describes
when that peer reported in, so the interchange relays proof it could not forge
and a JWT-style signature check separates record from decoy. A decoy carries
random bytes where the signature would be: identical on the wire to an observer
without the peer's public key, and never verifying for anyone. The deterministic
fallback where signing is unavailable: derive the decoy from a keyed hash of the
queried key hash, so decoys are at least stable across probes rather than fresh
noise that flags itself by changing.

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
Resolving it from gluetun (now shipped) removes the guess entirely where that is
available; where it is not, it is still a hand-typed value worth validating before
it is saved.

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
- ~~**Builtin tracker only.**~~ _Shipped_, exactly as scoped: the breaking config
  error names the change and the fix.
- ~~**One announce-URL resolver.**~~ _Shipped_ as `sharerr_core::endpoint`,
  before the value started changing at runtime.
- ~~**Reachability in `doctor`.**~~ _Shipped_: doctor compares the advertised
  address against gluetun's reported exit, reports the forwarded port and which
  mechanism is in use, and attempts a TCP connection to the advertised endpoint
  (a failure is a warning, because some networks cannot hairpin their own public
  address even when it works from outside).
