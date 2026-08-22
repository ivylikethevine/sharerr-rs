# Roadmap

Where sharerr is going next.

sharerr is **experimental**. Nothing below is a release commitment, and the
ordering is a judgement about value, not a schedule. What has already shipped
lives in [the README](../README.md#what-works-today), not here — this page
tracks what is still ahead.

## Table of contents

- [What's left](#whats-left)
- [Library sources (where tagged content comes from)](#library-sources-where-tagged-content-comes-from)
- [Torrent clients (what actually seeds)](#torrent-clients-what-actually-seeds)
- [Indexers (what consumes the feed)](#indexers-what-consumes-the-feed)
- [Functionality](#functionality)
- [The lighthouse](#the-lighthouse)

### What's left

Three open items remain, ordered smallest first — by how much they touch, not
how long they'd take to get right:

1. **[rTorrent tier-2 coverage](#torrent-clients-what-actually-seeds).** Test
   infrastructure only: wire a real rTorrent + ruTorrent container into
   `run_docker_tests.sh`, the way qBittorrent already is. No application code
   changes.
2. **[Request flow](#functionality).** A new inbound request queue and
   approve step — touches the sync engine and the web UI on both sides of a
   friendship, not just one subsystem.
3. **[A topology visualization](#functionality).** The largest: a shared
   topology model spanning the tracker, gluetun, gossip, and the lighthouse
   client — none of which currently agree on one — plus a genuinely new
   diagram UI to render it.

### Library sources (where tagged content comes from)

Today: **Sonarr**, **Radarr**, **Lidarr**, **Readarr** and **Whisparr** via
tag-driven discovery, and the **plain tagged directory** (`[[library]]`). Both
shapes sit behind the `LibrarySource` seam, which is where a future source would
plug in — none is currently planned. A media-server-backed source (Jellyfin,
Emby, Plex) was tried and removed: the *arr apps and a plain directory cover
the two shapes of "where content lives" this project actually wants to support.

### Torrent clients (what actually seeds)

Today: **qBittorrent**, **Transmission**, and **rTorrent / ruTorrent**, behind
the `TorrentClient` trait in `sharerr-client`. That trait is deliberately
narrow, which is what made a second and third client tractable — clients
disagree about almost everything except "add this torrent, with the data
already at this path". Announces always go to sharerr's own tracker, so a
client needs no tracker of its own.

Adding another is mostly writing one file. What a new client must answer
honestly: whether it can remove a torrent _without_ deleting the data, how it
replaces a torrent's tracker list in place (`set_trackers`, for endpoint
rotation), and how it expresses `AddRequest::upload_limit_kib`/`ratio_limit`
when either is set — the one deliberate exception to "ratios belong to the
client," a seeding goal stated once at add time through whatever native
mechanism the client offers for it, same as qBittorrent (inline on
`torrents/add`) and Transmission (a follow-up `torrent-set`) already do.
`sharerr-rtorrent` answers the tracker-replacement question honestly by *not*
fully answering it: rTorrent's XML-RPC has never grown a way to remove a
tracker (open upstream as
[rakshasa/rtorrent#165](https://github.com/rakshasa/rtorrent/issues/165)
since 2013), so `set_trackers` there can only insert a fresh tier ahead of
whatever is already on the torrent, not replace it — see the crate's module
docs for the full reasoning.

**Gap: no tier-2 coverage for rTorrent.** `run_docker_tests.sh` drives real
Sonarr, Radarr, and qBittorrent containers; it does not drive a real rTorrent.
`sharerr-rtorrent`'s tests instead run against a hand-mocked XML-RPC server,
which proves the crate parses the requests and responses it expects — not
that those are the requests and responses a real rTorrent expects. Standing
up rTorrent + ruTorrent in the docker compose stack, the same way qBittorrent
already is, would close this; not yet done.

**Transmission-compatible forks:** not planned as separate work. `sharerr-transmission` has no
version pinning or fork detection — it only speaks the documented session-id handshake and
standard `torrent-*`/`session-get` methods, which is the same protocol surface a compatible fork
presents. Standing up a real fork for tier-2 was investigated and set aside: the one
actively-maintained candidate has no published Docker image and its own maintainers describe its
RPC compatibility as imperfect.

### Indexers (what consumes the feed)

Today: **Prowlarr** (_Generic Torznab_), **Jackett**-shaped URLs, and
**Sonarr/Radarr/Lidarr direct**, each confirmed against a real instance in the
tier-2 suite. **Readarr direct is explicitly out of scope**: this project
targets small-scale homelab media-file sharing, and books are a different,
much smaller scale of content than the audio/video files everything else here
shares — existing Readarr library-source support is unaffected, this is only
about the indexer direction. No further indexer work is currently planned.

## Functionality

**Request flow.** The original design brief wanted a friend's Sonarr/Radarr to
_request_ content. Today discovery is one-way: they find what you already share.
An inbound request queue with an approve step is the other half of that idea.

**A topology visualization.** Every fact this would draw already exists
somewhere in the running instance, just spread across separate pages as
tables: the internal *arr-stack side (this instance's own Sonarr, Radarr,
qBittorrent/Transmission/rTorrent, and gluetun, plus how a container's own
view of a path maps to sharerr's and to the torrent client's — see
`sharerr doctor`'s path-mapping check and the Status page's endpoint table),
and the external swarm side (this instance's own advertised host/port, each
peer's known endpoints from [`PeerEndpointView`], and whether each was
learned by a direct sighting, gossip, or a lighthouse — see "Friends finding
each other" in the README). Nobody currently has to hold all of that in their
head at once to answer "why can't Sam see this torrent" or "which of my two
gluetun tunnels is this port actually on" — a diagram naming each hop's
IP:port, container name, and last-known-good time would answer it at a
glance instead of a page of prose. Not started: this is a genuinely new UI
surface (an SVG or similar diagram, not another table), not an extension of
an existing page, and touches every subsystem that already tracks an
endpoint — the tracker, gluetun, gossip, and the lighthouse client all keep
their own notion of "where things are" today and none of them are wired to a
shared topology model yet.

---

## The lighthouse

Shipped — see [the README](../README.md#the-lighthouse) for how to use it. The
design rationale below is kept here because it explains *why* the rendezvous
works the way it does, which the README's usage-focused section does not
restate.

Gossip only helps peers who can still reach _somebody_; two friends whose
addresses both rotated while neither was watching have no path back to each
other. The lighthouse is the rendezvous for that case: a tiny separate service,
deliberately knowing nothing but `key hash → latest IP and port`, that a sharerr
instance reports its endpoint to and a friend queries with the API key that peer
issued them. The privacy property is the point and shapes the whole design: a
request without a valid key gets a _plausible fabricated_ IP and port rather
than an error, so an unauthenticated probe cannot be distinguished from a valid
lookup — the lighthouse never confirms that an instance exists, and scraping it
yields only noise. That makes semi-anonymous tracking of sharerr instances
possible without any instance exposing its IP publicly. It ships as its own
docker image on its own port — not another route on sharerr's listener — so it
can be self-hosted by anyone, placed on neutral ground away from any particular
library, and carries no database worth stealing: key hashes and last-seen
addresses only. A sharerr instance treats it as one more observation source
feeding peer endpoint memory, ranked below a direct sighting of the same peer.

The fabricated answers create the opposite problem for the _legitimate_ caller:
a friend holding a valid key must be able to tell a real record from a decoy, or
the noise defeats them too. So a genuine record is verifiable — the natural shape
is the same signed endpoint record gossip uses, signed by the peer it describes
when that peer reported in, so the lighthouse relays proof it could not forge
and a JWT-style signature check separates record from decoy. A decoy carries
random bytes where the signature would be: identical on the wire to an observer
without the peer's public key, and never verifying for anyone. The deterministic
fallback where signing is unavailable: derive the decoy from a keyed hash of the
queried key hash, so decoys are at least stable across probes rather than fresh
noise that flags itself by changing.
