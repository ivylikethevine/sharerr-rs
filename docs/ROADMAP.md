# Roadmap

Where sharerr is going next.

sharerr is **experimental**. Nothing below is a release commitment, and the
ordering is a judgement about value, not a schedule. What has already shipped
lives in [the README](../README.md#what-works-today), not here — this page
tracks what is still ahead.

## Table of contents

- [What's left](#whats-left)
- [Functionality](#functionality)
- [Open work, by scope](#open-work-by-scope)
- [The lighthouse](#the-lighthouse)

### What's left

Two feature-sized items — **[request flow](#functionality)** and an
**[OpenAPI spec](#functionality)** — plus the open items from the 2026-08-21
code review. All of them, features and findings alike, are ranked together in
[Open work, by scope](#open-work-by-scope) below.

What sharerr already talks to — library sources, torrent clients, indexers —
and the extension seam each sits behind is [`SUPPORTED.md`](SUPPORTED.md);
what was tried and deliberately left out is [`UNSUPPORTED.md`](UNSUPPORTED.md).

## Functionality

**Request flow.** The original design brief wanted a friend's Sonarr/Radarr to
_request_ content. Today discovery is one-way: they find what you already share.
An inbound request queue with an approve step is the other half of that idea.

**OpenAPI spec.** The machine-facing surface — the Torznab feed (`/api`) and
the Jackett-compatible routes (`/api/v2.0/...`) in `torznab.rs` and
`jackett.rs` — has no formal contract today, only the handlers themselves and
the tier-2 suite's assertions against real Sonarr/Radarr/Lidarr/Jackett-shaped
clients. A spec would give an operator (or a client author working out why
their app rejects the feed) one document to check request shapes and response
schemas against, instead of reading the router. Scoped to that feed/API
surface, not the server-rendered settings UI, which is not a machine API.

## Open work, by scope

Everything still ahead, in one list, smallest first — by how much each item
touches, not how long it would take to get right. The review items come from
a whole-codebase pass on 2026-08-21 (8 finder angles, every candidate
independently verified: **CONFIRMED** = reproduced from the code, **PLAUSIBLE**
= depends on ordering/config); sixteen batches of fixes landed on 2026-08-24
(the sixteenth: rTorrent tier-2 coverage — `run_docker_tests.sh --rtorrent`
now drives a real rTorrent + ruTorrent container through the same suite
qBittorrent and Transmission use, which caught two real bugs no hand-mocked
server could: `d.multicall2` needs a leading empty parameter, and an empty
result comes back as a self-closing `<data/>`) and what is listed here is
what remains. File references are as of the review commit and may have
drifted.

### Small — one function or one file

1. **Dual-token admission on the items page** — `items.rs` `token_status`
   consults only the current fingerprint, so a previous-token item renders
   Stale while the tracker admits it. The doc comment frames that as
   intended; listed so the decision is a decision.

### Large — a protocol, a data model, or a release process

2. **A "Reused" pre-existing torrent — CONFIRMED.** `sync/seed.rs`
    `find_existing` matches _any_ torrent in the client (fed by `list(None)`,
    not just sharerr's category) and returns `SeedOutcome::Reused` without
    `set_trackers` or a cached `.torrent`; `set_seeding` then records it as
    Seeding with the current token fingerprint. When library path ==
    download path, or an operator cross-seeds: (a) `tracker::torrent_file`
    404s on every friend download and the local client never announces to
    sharerr's tracker, so friends join an empty swarm; (b) removing the tag
    makes `withdraw_untagged` `remove()` a torrent sharerr did not create —
    against the "preserve existing torrents" rule. _Fix:_ on reuse, insert
    sharerr's tracker and cache the `.torrent` bytes from the client; and
    record `created_by_sharerr` per item so untag only removes what sharerr
    added.
3. **Lighthouse `report` is unpinned — CONFIRMED.** `key_hash` is SHA-256 of
    the shared API key, never bound to `record.pubkey`; `verify()` only checks
    the record is self-consistent under whatever pubkey it carries. Anyone who
    learns a key hash (a URL path segment, visible in proxy logs) can displace
    the genuine record with one under their own keypair. The client catches
    the impersonation (`record.pubkey != expected_pubkey` → decoy) but the
    legitimate record is gone and rendezvous for that pair breaks. The
    future-timestamp clamp, capacity cap with TTL sweep, and lowercase
    canonicalisation are done. _Fix:_ first-writer pins the pubkey for that
    key hash and later reports must carry the same one, or derive `key_hash`
    from the pubkey as well as the key — either way a wire-format decision.
4. **Request flow** — a new inbound request queue and approve step, touching
    the sync engine and the web UI on both sides of a friendship; see
    [Functionality](#functionality).
5. **OpenAPI spec** for the Torznab/Jackett-compatible feed API — see
    [Functionality](#functionality).

---

## The lighthouse

Shipped — see [the README](../README.md#the-lighthouse) for how to use it. The
design rationale below is kept here because it explains _why_ the rendezvous
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
