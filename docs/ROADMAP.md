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
- [Open work, by scope](#open-work-by-scope)
- [The lighthouse](#the-lighthouse)

### What's left

Three feature-sized items — **[rTorrent tier-2
coverage](#torrent-clients-what-actually-seeds)**, **[request
flow](#functionality)**, and **[publishing to crates.io](#publishing-to-cratesio)**
— plus the open items from the 2026-08-21 code review. All of them, features
and findings alike, are ranked together in [Open work, by
scope](#open-work-by-scope) below.

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

## Open work, by scope

Everything still ahead, in one list, smallest first — by how much each item
touches, not how long it would take to get right. The review items come from
a whole-codebase pass on 2026-08-21 (8 finder angles, every candidate
independently verified: **CONFIRMED** = reproduced from the code, **PLAUSIBLE**
= depends on ordering/config); four batches of fixes landed on 2026-08-24
and what is listed here is what remains. File references are as of the review
commit and may have drifted.

### Small — one function or one file

1. **Items page renders the full announce URL, token included —
   CONFIRMED.** `items.rs` `current_announce_url` builds
   `announce_url(&base, tracker_token())` and `items.html` prints it per row,
   contradicting `settings.rs`'s "a stored secret is never rendered back".
   Behind `require_auth`, and the token already ships inside every
   `.torrent`. *Fix:* render `announce_url(base, None)` with a `…/<token>`
   placeholder, or the token's fingerprint.
2. **Duplicated `fn unreachable`** — `sharerr-transmission` and
   `sharerr-rtorrent` are byte-identical apart from `base` vs `endpoint`.
   Lift next to `sharerr_client::http_client()`.
3. **`ago`/`compact_ago` — PLAUSIBLE.** `peers.rs` and `topology.rs` share the
   60/3600/86400 ladder; `compact_ago`'s doc says the *format* difference is
   intentional. Optionally express both through one function so one test
   covers both thresholds.
4. **`TorrentBackend` lacks `ALL` + `parse()`** — unlike `LighthouseMount`,
   `NotifyKind`, `LibraryKind`, `MediaSource`. Hand-matched in `web/probe.rs`,
   `web/settings.rs` (twice), the probe tests, and `web/mod.rs`'s per-backend
   routes; a fourth client means ~7 sites, and missing the probe arm makes its
   Test button say "Unknown service".
5. **Dual-token admission on the items page** — `items.rs` `token_status`
   consults only the current fingerprint, so a previous-token item renders
   Stale while the tracker admits it. The doc comment frames that as
   intended; listed so the decision is a decision.

### Medium — one subsystem, a few files

6. **Vault mutations across processes — CONFIRMED, partly fixed.**
   `Vault::put`/`remove` now reload under a process-wide mutex, which covers
   every writer inside `serve`; `sharerr vault set` against a running `serve`
   is still last-write-wins with a shared `vault.tmp`. *Fix:* a file lock —
   `std::fs::File::lock` is Rust 1.89, one past the MSRV, so `fd-lock`/`fs4`.
7. **`hash_of_last_add` can throttle an unrelated torrent — PLAUSIBLE.**
   `sharerr-rtorrent` takes `rows.last()` of `d.multicall2(main, d.hash=)`
   after `load.raw_start`; rTorrent loads asynchronously, a duplicate hash is
   only logged, and `main` honours any `view.sort_current` in `.rtorrent.rc`.
   *Fix:* carry the info hash on `AddRequest` — the caller (`factory.rs`)
   already knows it — and stop guessing.
8. **`resolve_sharerr` and `resolve()` disagree — PLAUSIBLE.** `paths.rs`
   `resolve()` is first-match-in-order with `qbit.unwrap_or(sharerr)`;
   `resolve_sharerr()` is most-specific-match over only rules *with* `qbit`.
   A specific rule without `qbit` ahead of a general one with it yields two
   qBittorrent paths for one file by entry point; with `skip_checking =
   true` the torrent points at a non-existent file. *Fix:* one
   most-specific-match resolver used by both.
9. **`dotted()`/`loose_eq()` drop every non-ASCII character — CONFIRMED.**
   `title.rs` maps anything non-ASCII-alphanumeric to a space and empty to
   `"Unknown"`: `千と千尋の神隠し (2001)` → `Unknown.2001.WEB-DL…` (every CJK,
   Cyrillic, Greek title collapses to the same name); `Amélie` → `Am.lie`,
   loose key `amlie`. The release title is the only matchable data for
   directory-sourced items. *Fix:* NFKD-transliterate before the ASCII
   filter, and fall back to the hash rather than `Unknown`.
10. **Vanished torrent re-hashes although the `.torrent` is cached.**
    `sync/seed.rs` `share()` never passes `known.info_hash` to `seed()`, so
    `find_existing` → `build()` runs `LavaTorrentFactory.create` (gigabytes
    of CPU) and rewrites the very file that already exists — after a client
    reinstall or wiped session, every item. *Fix:* pass the hash in; if the
    cached file reads, `rewrite_announce` it (the logic `refresh_announce`
    already has) and `add` those bytes.
11. **Torrent-client credential resolution exists four times** — `sync/mod.rs`,
    `web/probe.rs`, `commands/doctor.rs`, `web/topology.rs`, each with its
    own semantics (`doctor` never calls `TorrentCredential::choose`);
    `checks.rs` already admits "three of which resolve secrets from three
    different places". *Fix:* one `resolve_torrent_credential` next to
    `checks::build_torrent_client`.
12. **`doctor` and `sync` only check the tracker's gluetun tunnel.**
    `doctor.rs` reads only `config.gluetun.control_url` + `GLUETUN_API_KEY`;
    `[gluetun_client]` is never checked, while `gluetun.rs` and
    `web/diagnostics.rs` cover both. A dual-VPN operator with a wrong
    client-tunnel key gets a clean `doctor`.
13. **Per-tick `reqwest::Client` rebuild and vault re-open** — `gossip.rs`
    (per exchange), `lighthouse_client.rs` (per tick), `notify.rs` (per
    event) each build a fresh client and open the vault (Argon2, ~19 MiB).
    `gluetun.rs` keeps one client and documents why. Gossip's per-peer keys
    legitimately need the vault; the client rebuild does not.
14. **`topology::gather` duplicates `diagnostics::gather`**, and both run the
    arr probes and the library scan sequentially, already diverging
    (diagnostics reports a panicked scan; topology drops it). *Fix:* one
    `checks::snapshot(...)` with `tokio::join!` — wall time of the two slowest
    pages drops from sum to max.
15. **Feed `.torrent` links are `http://localhost:<port>/…` on a gluetun-only
    deployment — CONFIRMED.** `torznab.rs` `Matched::download_url`, Jackett's
    `site_link`, and the peers page's `feed_url` all come from
    `Config::public_base_url()`, which only knows the static advertised base
    and otherwise yields `http://localhost:<bind port>` — while the magnet
    tiers in the same response use the live `endpoint().recent()`. On the
    README-recommended setup (no `advertised_host`) a friend's Sonarr grabs
    `http://localhost:8477/torrents/<hash>.torrent` on *their* box and
    fails; the `magneturl` is correct and the feed preview looks healthy.
    *Fix:* route the `.torrent`/site links through the same
    `AdvertisedEndpoint`.
16. **rTorrent tier-2 coverage.** Test infrastructure only: wire a real
    rTorrent + ruTorrent container into `run_docker_tests.sh`, the way
    qBittorrent already is — see [Torrent
    clients](#torrent-clients-what-actually-seeds). The XML-RPC parser and
    throttle-method bugs fixed on 2026-08-24 were exactly the kind the
    hand-mocked server cannot catch.

### Large — a protocol, a data model, or a release process

17. **A "Reused" pre-existing torrent — CONFIRMED.** `sync/seed.rs`
    `find_existing` matches *any* torrent in the client (fed by `list(None)`,
    not just sharerr's category) and returns `SeedOutcome::Reused` without
    `set_trackers` or a cached `.torrent`; `set_seeding` then records it as
    Seeding with the current token fingerprint. When library path ==
    download path, or an operator cross-seeds: (a) `tracker::torrent_file`
    404s on every friend download and the local client never announces to
    sharerr's tracker, so friends join an empty swarm; (b) removing the tag
    makes `withdraw_untagged` `remove()` a torrent sharerr did not create —
    against the "preserve existing torrents" rule. *Fix:* on reuse, insert
    sharerr's tracker and cache the `.torrent` bytes from the client; and
    record `created_by_sharerr` per item so untag only removes what sharerr
    added.
18. **Lighthouse `report` is unpinned — CONFIRMED.** `key_hash` is SHA-256 of
    the shared API key, never bound to `record.pubkey`; `verify()` only checks
    the record is self-consistent under whatever pubkey it carries. Anyone who
    learns a key hash (a URL path segment, visible in proxy logs) can displace
    the genuine record with one under their own keypair. The client catches
    the impersonation (`record.pubkey != expected_pubkey` → decoy) but the
    legitimate record is gone and rendezvous for that pair breaks. The
    future-timestamp clamp, capacity cap with TTL sweep, and lowercase
    canonicalisation are done. *Fix:* first-writer pins the pubkey for that
    key hash and later reports must carry the same one, or derive `key_hash`
    from the pubkey as well as the key — either way a wire-format decision.
19. **Publishing to crates.io** — two concrete packaging blockers plus nine
    crates' worth of release process; see [its own
    section](#publishing-to-cratesio).
20. **Request flow** — a new inbound request queue and approve step, touching
    the sync engine and the web UI on both sides of a friendship; see
    [Functionality](#functionality).

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
