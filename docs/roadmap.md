# Roadmap

Where sharerr is and where it is going. Status is honest: _Done_ means implemented
and covered by tests, not merely written.

sharerr is **experimental**. Nothing below is a release commitment, and the
ordering is a judgement about value, not a schedule.

## Status

| Milestone | Scope | Status |
|---|---|---|
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

Adding a third is now mostly writing one file. The two things a new client must
answer honestly are whether it can remove a torrent *without* deleting the data,
and whether it has an embedded tracker (`None` is the normal answer, and means
sharerr's own tracker is required).

| Client                            | Notes                                                                                                                                                  |
| --------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **Deluge**                        | JSON-RPC, plus the `label` plugin standing in for qBittorrent's category.                                                                              |
| **rTorrent / ruTorrent**          | XML-RPC. Popular on seedboxes, which is exactly where someone would want to share a large library.                                                     |
| **Transmission-compatible forks** | Anything speaking the Transmission RPC should already work — the client is the same. Untested, and cheap to confirm.                                   |

The tracker constraint is now proven rather than predicted: Transmission has no
embedded tracker, so the Transmission stack runs with `tracker.backend = "builtin"`
and `doctor` fails with a sentence naming that fix if it does not. Every client
below will be the same.

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

**VPN containers (gluetun and friends).** Covered: `./run_docker_tests.sh --vpn`
runs the whole suite with qBittorrent inside a VPN container's network namespace.
What is *not* covered is a real provider with a forwarded port, which is where
`tracker.advertised_host` has to name the tunnel's external address rather than the
host's — the one part of that topology a self-hosted tunnel cannot reproduce.

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

**Ratio and bandwidth control.** No upload limits, no seeding goals. Sharing a
library with no cap on what it costs you is a real deterrent to running this.

**Request flow.** The original design brief wanted a friend's Sonarr/Radarr to
_request_ content. Today discovery is one-way: they find what you already share.
An inbound request queue with an approve step is the other half of that idea.

**Cross-seed awareness.** The brief called for preserving existing torrents rather
than creating duplicates. If a file is already seeding in qBittorrent under another
torrent, sharerr should recognise it rather than adding a second entry for the same
bytes.

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
