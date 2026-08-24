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

Two feature-sized items — **[rTorrent tier-2 coverage](SUPPORTED.md#torrent-clients-what-actually-seeds)**
and **[request flow](#functionality)** — plus the open items from the
2026-08-21 code review. All of them, features and findings alike, are ranked
together in [Open work, by scope](#open-work-by-scope) below.

What sharerr already talks to — library sources, torrent clients, indexers —
and the extension seam each sits behind is [`SUPPORTED.md`](SUPPORTED.md);
what was tried and deliberately left out is [`UNSUPPORTED.md`](UNSUPPORTED.md).

## Functionality

**Request flow.** The original design brief wanted a friend's Sonarr/Radarr to
_request_ content. Today discovery is one-way: they find what you already share.
An inbound request queue with an approve step is the other half of that idea.

## Open work, by scope

Everything still ahead, in one list, smallest first — by how much each item
touches, not how long it would take to get right. The review items come from
a whole-codebase pass on 2026-08-21 (8 finder angles, every candidate
independently verified: **CONFIRMED** = reproduced from the code, **PLAUSIBLE**
= depends on ordering/config); nine batches of fixes landed on 2026-08-24
(the ninth: `dotted()`/`loose_eq()` NFKD-transliteration, plus a stable
per-title placeholder for scripts with no ASCII form instead of a colliding
literal `"Unknown"`) and what is listed here is what remains. File
references are as of the review commit and may have drifted.

### Small — one function or one file

1. **Dual-token admission on the items page** — `items.rs` `token_status`
   consults only the current fingerprint, so a previous-token item renders
   Stale while the tracker admits it. The doc comment frames that as
   intended; listed so the decision is a decision.

### Medium — one subsystem, a few files

2. **Vanished torrent re-hashes although the `.torrent` is cached.**
   `sync/seed.rs` `share()` never passes `known.info_hash` to `seed()`, so
   `find_existing` → `build()` runs `LavaTorrentFactory.create` (gigabytes
   of CPU) and rewrites the very file that already exists — after a client
   reinstall or wiped session, every item. _Fix:_ pass the hash in; if the
   cached file reads, `rewrite_announce` it (the logic `refresh_announce`
   already has) and `add` those bytes.
3. **Torrent-client credential resolution exists four times** — `sync/mod.rs`,
   `web/probe.rs`, `commands/doctor.rs`, `web/topology.rs`, each with its
   own semantics (`doctor` never calls `TorrentCredential::choose`);
   `checks.rs` already admits "three of which resolve secrets from three
   different places". _Fix:_ one `resolve_torrent_credential` next to
   `checks::build_torrent_client`.
4. **`doctor` and `sync` only check the tracker's gluetun tunnel.**
   `doctor.rs` reads only `config.gluetun.control_url` + `GLUETUN_API_KEY`;
   `[gluetun_client]` is never checked, while `gluetun.rs` and
   `web/diagnostics.rs` cover both. A dual-VPN operator with a wrong
   client-tunnel key gets a clean `doctor`.
5. **Per-tick `reqwest::Client` rebuild and vault re-open** — `gossip.rs`
   (per exchange), `lighthouse_client.rs` (per tick), `notify.rs` (per
   event) each build a fresh client and open the vault (Argon2, ~19 MiB).
   `gluetun.rs` keeps one client and documents why. Gossip's per-peer keys
   legitimately need the vault; the client rebuild does not.
6. **`topology::gather` duplicates `diagnostics::gather`**, and both run the
    arr probes and the library scan sequentially, already diverging
    (diagnostics reports a panicked scan; topology drops it). _Fix:_ one
    `checks::snapshot(...)` with `tokio::join!` — wall time of the two slowest
    pages drops from sum to max.
7. **Feed `.torrent` links are `http://localhost:<port>/…` on a gluetun-only
    deployment — CONFIRMED.** `torznab.rs` `Matched::download_url`, Jackett's
    `site_link`, and the peers page's `feed_url` all come from
    `Config::public_base_url()`, which only knows the static advertised base
    and otherwise yields `http://localhost:<bind port>` — while the magnet
    tiers in the same response use the live `endpoint().recent()`. On the
    README-recommended setup (no `advertised_host`) a friend's Sonarr grabs
    `http://localhost:8477/torrents/<hash>.torrent` on _their_ box and
    fails; the `magneturl` is correct and the feed preview looks healthy.
    _Fix:_ route the `.torrent`/site links through the same
    `AdvertisedEndpoint`.
8. **rTorrent tier-2 coverage.** Test infrastructure only: wire a real
    rTorrent + ruTorrent container into `run_docker_tests.sh`, the way
    qBittorrent already is — see [Torrent
    clients](SUPPORTED.md#torrent-clients-what-actually-seeds). The XML-RPC
    parser and throttle-method bugs fixed on 2026-08-24 were exactly the kind
    the hand-mocked server cannot catch.

### Large — a protocol, a data model, or a release process

9. **A "Reused" pre-existing torrent — CONFIRMED.** `sync/seed.rs`
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
10. **Lighthouse `report` is unpinned — CONFIRMED.** `key_hash` is SHA-256 of
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
11. **Request flow** — a new inbound request queue and approve step, touching
    the sync engine and the web UI on both sides of a friendship; see
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
