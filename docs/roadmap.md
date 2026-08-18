# Roadmap

Where sharerr is and where it is going. Status is honest: _Done_ means implemented
and covered by tests, not merely written.

sharerr is **experimental**. Nothing below is a release commitment, and the
ordering is a judgement about value, not a schedule.

## Status

| Milestone | Scope | Status |
|---|---|---|
| — | Plex as a library source | Next |
| — | The interchange: semi-anonymous endpoint rendezvous, its own image and port | Next |
| — | List view of shared items | Next |
| — | Ratio and bandwidth control | Next |

**Shipped, and removed from this list:** the core, the builtin tracker and
Torznab feed, the web UI, friend/peer management, Jackett compatibility, the
plain tagged directory source, per-friend selective sharing, Transmission
support, Lidarr/Readarr/Whisparr support, the test stacks (plain, Transmission,
and behind gluetun), the removal of the qBittorrent-embedded tracker backend,
first-class gluetun support with a dynamically resolved endpoint, peer endpoint
memory and signed endpoint gossip, Jellyfin/Emby as a library source, magnet
links in the feed, and the one-glance status summary. The code and `git log` are
the record — carrying finished work here only makes the list harder to read.

---

## Compatibility

sharerr sits in the middle of a stack it does not own, so most of its value comes
from how many of that stack's normal shapes it tolerates. Grouped by where each
piece plugs in, roughly in order of how much they would widen the audience.

### Library sources (where tagged content comes from)

Today: **Sonarr**, **Radarr**, **Lidarr**, **Readarr** and **Whisparr** via
tag-driven discovery, **Jellyfin / Emby** via item tags, and the **plain tagged
directory** (`[[library]]`). All sit behind the `LibrarySource` seam, which is
exactly where the remaining sources plug in.

| Service                      | Why                                                                                                                                                                                                                       | Difficulty |
| ---------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------- |
| **Plex**                     | Same idea as Jellyfin, but the API is more awkward and its "collections" map badly onto a share tag.                                                                                                                       | Medium     |

### Torrent clients (what actually seeds)

Today: **qBittorrent** and **Transmission**, behind the `TorrentClient` trait in
`sharerr-client`. That trait is deliberately narrow, which is what made the
second client tractable — clients disagree about almost everything except "add
this torrent, with the data already at this path". Announces always go to
sharerr's own tracker, so a client needs no tracker of its own.

Adding a third is now mostly writing one file. What a new client must answer
honestly: whether it can remove a torrent *without* deleting the data, and how
it replaces a torrent's tracker list in place (`set_trackers`, for endpoint
rotation).

| Client                            | Notes                                                                                                                                                  |
| --------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **Deluge**                        | JSON-RPC, plus the `label` plugin standing in for qBittorrent's category.                                                                              |
| **rTorrent / ruTorrent**          | XML-RPC. Popular on seedboxes, which is exactly where someone would want to share a large library.                                                     |
| **Transmission-compatible forks** | Anything speaking the Transmission RPC should already work — the client is the same. Untested, and cheap to confirm.                                   |

### Indexers (what consumes the feed)

Today: **Prowlarr** (_Generic Torznab_), **Jackett**-shaped URLs, and
**Sonarr/Radarr direct** (confirmed against a real Sonarr in the tier-2 suite).

| Consumer                  | Notes                                                                                                                                                 |
| ------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| **NZBHydra2**             | Aggregates Torznab indexers; should already work, but has never been tested against sharerr. Worth confirming rather than assuming.                   |
| **Lidarr/Readarr direct** | Follows from library-source support, since the caps document gates what they will even ask for.                                                       |

One earlier "should already work" row turned out not to (Sonarr direct rejected
the feed over a missing `pubDate`). Confirming NZBHydra2 is still cheap and
still worth more than another feature — the last confirmation found a bug that
made sharerr unusable as a direct indexer.

### Deployment shapes

Today: single container, reverse-proxied (`tracker.advertised_url`), IPv6, and
the gluetun namespace shape with a dynamically resolved endpoint (`[gluetun]`,
`docker/deploy/`). One honest gap in the last of those: it is proven against a
faked control server (tier 1) and a self-hosted WireGuard stack (tier 2), but
not against a real forwarding provider — the test tunnel cannot grant a port.

**Unraid / Synology templates.** Both communities install almost entirely from
templates; publishing one is packaging work rather than code, and reaches an
audience that will never run `docker run` by hand.

---

## Functionality

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

**Per-peer announce tokens.** The tracker's announce token is one shared secret,
so an announce carries no peer identity — which is why peer endpoint memory
cannot attribute a torrent client's address from a direct announce and has to
learn it from gossip instead. Per-peer tokens would close that gap and make
"cut this friend off" reach the tracker too, not just the feed.

**Ratio and bandwidth control.** No upload limits, no seeding goals. Sharing a
library with no cap on what it costs you is a real deterrent to running this.

**Request flow.** The original design brief wanted a friend's Sonarr/Radarr to
_request_ content. Today discovery is one-way: they find what you already share.
An inbound request queue with an approve step is the other half of that idea.

**Cross-seed awareness.** The brief called for preserving existing torrents rather
than creating duplicates. If a file is already seeding in qBittorrent under another
torrent, sharerr should recognise it rather than adding a second entry for the same
bytes.

**Per-peer feed preview in the web UI.** The feed is the thing a friend actually
receives, and the operator currently has no way to see it as a given friend sees
it — scope filtering happens per key, so "why can't Sam find the album" means
hand-crafting a Torznab query with Sam's key. A button on each friend's row that
renders their feed (their scope, their links) answers that in one click, and
doubles as the honest test of scoping: not what the rules *say*, but what the
feed *serves*.

**List view of shared items.** There is no page that simply enumerates what this
instance is sharing. The data is all in the store — title, size, info hash, which
friends can see it, when it last synced — but the only way to read it today is the
per-item status the sync loop logs, or qBittorrent's own torrent list, which
answers a different question. A sortable, filterable list is the first thing an
operator asks for after setup and the last thing they can currently get.

**Health and history in the UI.** The store already records run history
(`recent_runs`), and the status page shows only the latest runs. Per-item state —
why _this_ file is not shared — is the question the UI cannot currently answer.

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

**Better first-run defaults.** `tracker.advertised_host` has no default and a wrong
value silently produces torrents nobody can announce to — detectable at save time.
Resolving it from gluetun removes the guess entirely where that is available;
where it is not, it is still a hand-typed value worth validating before it is
saved.

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
- **Gluetun against a real provider.** The one untested mile of the endpoint
  work: a commercial VPN whose forwarded port actually rotates. Needs an account
  and a manual session rather than code.
- **Remove the legacy qBittorrent username + password.** qBittorrent 5.2's API
  key is the credential; the username/password pair and the
  key-wins-over-password precedence dance exist only to support older setups.
  Nothing has shipped, so there are no older setups — drop the pair, the
  `qbittorrent.username` config field, and the fallback logic.
- **Remove all legacy-settings code.** Same reasoning, applied everywhere: the
  shared `torznab.api_key` fallback (kept "so upgrading does not silently break
  a friend set up before peers existed" — no such friend exists) and any other
  shim that exists to honour a pre-v1 sharerr configuration. v1 has not been
  deployed, so every one of these is extra code, extra tests, and an extra
  branch in an auth path, purchased against a past that never happened.
  (Compatibility with other *software's* versions — qBittorrent's
  `paused`/`stopped` spelling, Transmission's RPC generations — is not legacy
  and stays.)
