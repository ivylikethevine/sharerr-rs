# Support

What sharerr talks to today, the seam each category plugs into, and what was
tried and removed or considered and deliberately left out, with the reason
attached so a decision already made is not re-litigated. For usage and config
snippets see [the README](../README.md); for what has been considered without
being either committed to or declined, see
[the README's roadmap](../README.md#roadmap).

## Table of contents

- [Supported services](#supported-services)
  - [Library sources (where tagged content comes from)](#library-sources-where-tagged-content-comes-from)
  - [Torrent clients (what actually seeds)](#torrent-clients-what-actually-seeds)
  - [Indexers (what consumes the feed)](#indexers-what-consumes-the-feed)
  - [The feed's magnet link](#the-feeds-magnet-link)
- [Not supported](#not-supported)
  - [Media-server library sources (Jellyfin, Emby, Plex)](#media-server-library-sources-jellyfin-emby-plex)
  - [Readarr as a direct indexer](#readarr-as-a-direct-indexer)
  - [Transmission-compatible forks, as their own tier-2 target](#transmission-compatible-forks-as-their-own-tier-2-target)
  - [qBittorrent's embedded tracker as a second backend](#qbittorrents-embedded-tracker-as-a-second-backend)
  - [Multi-user](#multi-user)
  - [Publishing to crates.io](#publishing-to-cratesio)
  - [A maintained CHANGELOG.md](#a-maintained-changelogmd)

## Supported services

### Library sources (where tagged content comes from)

**Sonarr**, **Radarr**, **Lidarr**, **Readarr** and **Whisparr** via
tag-driven discovery (Whisparr reuses Sonarr's walk), and the **plain
directory** (`[[library]]`). Both shapes sit behind the `LibrarySource` seam
in `crates/sharerr/src/sync/mod.rs`: `kind()` plus `discover()`, which
returns a scan that also says whether it was _complete_. A partial walk still
shares what it found, but nothing is withdrawn on its behalf. That is where a
future source would plug in; none is currently planned.

### Torrent clients (what actually seeds)

**qBittorrent**, **Transmission**, and **rTorrent / ruTorrent**, behind the
`TorrentClient` trait in `sharerr-client`. The trait is deliberately narrow,
which is what made a second and third client tractable: clients disagree
about almost everything except "add this torrent, with the data already at
this path". Announces always go to sharerr's own tracker, so a client needs
no tracker of its own.

Adding another is mostly writing one file. A new client must answer: whether
it can remove a torrent _without_ deleting the data; how it replaces a
torrent's tracker list in place (`set_trackers`, for endpoint rotation) and
adds to one without disturbing the rest (`add_trackers`, for a torrent
sharerr adopts rather than creates); whether it can hand back a `.torrent` it
already holds (`export`); and how it expresses `upload_limit_kib` and
`ratio_limit`, at add time and again on an existing torrent (`set_limits`,
for a goal that changed), through whatever native mechanism it offers.

Where the three differ:

| | qBittorrent | Transmission | rTorrent |
| --- | --- | --- | --- |
| Skip the hash check on add | Yes (`qbittorrent.skip_checking`) | No, always verifies | No, always verifies |
| Category and tag | Real category plus tags | One flat `labels` list; `label` stands in for both | One free-text `d.custom1`; `label` stands in for both |
| Per-torrent upload cap | Yes | Yes | Yes, via a named throttle |
| Per-torrent ratio limit | Yes | Yes | No. Ratio enforcement is an `.rtorrent.rc` schedule, so `ratio_limit` is accepted and logged as dropped |
| Credentials | WebUI API key (5.2+), sent as a bearer token; no username/password path | HTTP Basic plus the 409 session-id handshake; needs 4.0+ (RPC 17) for `trackerList` | HTTP Basic aimed at the reverse proxy fronting XML-RPC; `rtorrent.url` is that exact endpoint, not a base |
| `export` a held `.torrent` | Yes (`torrents/export`) | No, returns `Ok(None)` | No, returns `Ok(None)` |
| Replace trackers in place | Yes | Yes | Partial: XML-RPC has no way to remove a tracker ([rakshasa/rtorrent#165](https://github.com/rakshasa/rtorrent/issues/165)), so a new tier is inserted ahead of the stale one |

The `export` gap costs Transmission and rTorrent one narrow case: a torrent
that already covers a file, that sharerr did not create and has no cached
copy of, cannot be shared on those two. sharerr fails the item with a message
naming the choice rather than advertise a release with no `.torrent` behind
it.

All three are driven through the real tier-2 stack; see
[`docs/TESTING.md`](TESTING.md#tier-2-the-compose-stacks).

### Indexers (what consumes the feed)

**Prowlarr** (_Generic Torznab_), **Jackett**-shaped URLs, and
**Sonarr/Radarr/Lidarr direct**. The tier-2 script adds sharerr as an indexer
to a real Sonarr and Lidarr and drives its Jackett-shaped routes and Torznab
caps by hand; Prowlarr is an opt-in container in the same stack, and
Radarr-direct is exercised only by hand. The feed also advertises
`book-search` and category 7000, so nothing _stops_ a Readarr pointed at it;
it is just untested, see [below](#readarr-as-a-direct-indexer). No further
indexer work is currently planned.

### The feed's magnet link

Off by default (`[feed] magnet_links`), resolving what used to be an open
roadmap question. Every torrent sharerr builds is private, so a magnet built
from one can never complete: nothing in the swarm will answer a
`ut_metadata` request. Worse, a client that supports both a magnet and a
`.torrent` enclosure often *prefers* the magnet — the two-instance
end-to-end test hit exactly this the hard way, when Radarr's direct Torznab
client picked the magnet over the working `.torrent` and stalled forever
(see [`docker/README.md`](https://github.com/ivylikethevine/sharerr-rs/blob/main/docker/README.md)'s
two-instance section for the Prowlarr `preferMagnetUrl` pin that works
around it, since Radarr/Sonarr's own direct client has no equivalent knob).

Turning `feed.magnet_links` on only ever emits a magnet for an item that is
*also* not private (`[seeding] private = false` — `private` is on by
default). The two settings are independent, and the combination "magnets on,
item private" is made inert rather than left to stall a friend's client.
Turning `private` off is the bigger decision of the two — it hands a client
DHT and PEX, so **revoking a friend no longer removes them from that
torrent's swarm**, the one property `private` exists to guarantee. See
[`docs/SETTINGS.md`](SETTINGS.md#seeding-limits) for both fields' settings-page
documentation.

## Not supported

One place to check before re-proposing something.

### Media-server library sources (Jellyfin, Emby, Plex)

Tried and removed. The *arr apps and a plain tagged directory already cover
the two shapes of "where content lives" this project wants to support; a
media server would have been a third way to answer the same question, not a
new one.

### Readarr as a direct indexer

Out of scope: this project targets homelab media-file sharing, and books are
a much smaller scale of content. Out of scope means untested and unsupported,
not blocked; a Readarr pointed at the feed may well work. Readarr as a
_library source_ is unaffected and fully supported.

### Transmission-compatible forks, as their own tier-2 target

Not planned. `sharerr-transmission` speaks only the documented session-id
handshake and standard `torrent-*`/`session-get` methods, which is the same
surface a compatible fork presents; the one floor is Transmission 4.0 (RPC 17)
for `trackerList`. Standing up a real fork for tier 2 was investigated and set
aside: the one actively maintained candidate has no published Docker image
and describes its own RPC compatibility as imperfect.

### qBittorrent's embedded tracker as a second backend

Used for a while and removed: two tracker backends meant two independently
built announce URLs, and every improvement to endpoint handling had to be
made twice. A `sharerr.toml` still naming `tracker.backend` fails to load
with an error saying exactly this. See [`DESIGN.md`](DESIGN.md)'s
"Corrections the implementation forced".

### Multi-user

Decided against. sharerr has exactly one user: the admin who configures it
and owns the library. The `users` table exists and only the first-run claim
ever creates a row, and that is the design, not a gap. A second user would
mean deciding what a friendship, a library, and a torrent client belong to
before any access-control surface could be built, and nothing this project
does needs that answer: friends are peers with their own instance, not
accounts on yours.

### Publishing to crates.io

Decided against. `cargo install sharerr` was investigated and found feasible,
but the project is not taking on an eleven-crate dependency-ordered release
process for a distribution path it does not intend to support. Nothing in
the manifests enforces this (only `sharerr-testkit` carries
`publish = false`); it is a decision, not a guard. The Docker image is the
only supported way to run sharerr. Since the workspace version became a
placeholder (the tag is the version; see
[`docs/RELEASING.md`](RELEASING.md#cutting-a-release)), every crate would
also publish as `0.0.0-dev` - a second reason, not a new decision.

### A maintained CHANGELOG.md

Decided against. `docker.yml`'s `release` job creates a GitHub Release on
every `v*` tag with notes generated from merged PR titles since the last tag
(see [`docs/RELEASING.md`](RELEASING.md#the-github-release)), which is
exactly the surface a hand-maintained file would duplicate, and duplicate
worse, since it can only drift from what shipped. The README's roadmap stays
a to-do list, not a log: an entry is removed once it ships, not dated and
kept. The Release attaches no binaries; the container image is the only
distribution channel.
