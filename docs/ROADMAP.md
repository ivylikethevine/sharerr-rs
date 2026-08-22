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
- [Publishing to crates.io](#publishing-to-cratesio)
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
3. **[Publishing to crates.io](#publishing-to-cratesio).** Two concrete
   packaging blockers plus nine crates' worth of release process — no new
   behaviour, but not a one-commit job either.

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

## Publishing to crates.io

`cargo install sharerr` (and `sharerr-lighthouse`) as an alternative to the
Docker image. Nothing about the code makes this impossible — the two hard
crates.io requirements, `license` and `description`, are already set on every
crate, internal dependencies already carry both a `path` and a `version`, there
is no `build.rs`, and sqlx here builds queries at runtime rather than through
the compile-time-checked macros, so no live database or `.sqlx` cache is needed
at build time. Two specific things are in the way, though, and neither is
obvious until a publish is actually attempted:

**The migrations live outside the crate that embeds them.**
`sharerr-store/src/db.rs` calls `sqlx::migrate!("../../migrations")`, reaching
up to the repository root. `cargo package` only includes files under the
crate's own directory, and crates.io's verification build unpacks exactly that
tarball — so the path resolves to nothing and the crate fails to compile both
there and for anyone consuming it. The migrations directory has to move under
`crates/sharerr-store/` first.

**`sharerr-testkit` is `publish = false`, but is depended on with a version.**
Cargo drops a dev-dependency from the published manifest only when it is
path-only; one carrying a version stays in, and would demand a
`sharerr-testkit` on crates.io that by definition cannot exist. Six crates
(`sharerr-arr`, `sharerr-qbit`, `sharerr-rtorrent`, `sharerr-transmission`,
`sharerr-torrent`, `sharerr`) reach it through `{ workspace = true }`, which
supplies a version from the root table, so each needs an explicit path-only
`[dev-dependencies]` entry instead.

Beyond those: nine crates would have to be published in dependency order
before `cargo publish -p sharerr` succeeds (`sharerr-lighthouse` is
independent of the rest and can go on its own). There are currently no git
tags at all, and no crates.io step in CI, so the release process itself is
new work rather than an extension of the existing `v*`-tagged GHCR build.

Worth deciding before starting: a `cargo install` user gets a binary whose
defaults (`/data`, `/config/sharerr.toml`) describe the container's
filesystem, not theirs. Those are overridable today, but shipping to people
who are *not* using the image probably means changing the defaults or being
loud about them in the README.

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
