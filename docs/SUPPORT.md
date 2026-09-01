# Support

What sharerr talks to today, and the seam each category plugs into, together
with what was tried and removed or considered and deliberately left out — kept
in one place so a reader deciding whether something is usable only has to
check one file, and so a decision already made isn't re-litigated by a later
proposal. For usage and config snippets, see [the README](../README.md); for
what has been considered without being either committed to or declined, see
[`ROADMAP.md`](ROADMAP.md#open-work-by-scope).

## Table of contents

- [Supported services](#supported-services)
  - [Library sources (where tagged content comes from)](#library-sources-where-tagged-content-comes-from)
  - [Torrent clients (what actually seeds)](#torrent-clients-what-actually-seeds)
  - [Indexers (what consumes the feed)](#indexers-what-consumes-the-feed)
- [Not supported](#not-supported)
  - [Media-server library sources (Jellyfin, Emby, Plex)](#media-server-library-sources-jellyfin-emby-plex)
  - [Readarr as a direct indexer](#readarr-as-a-direct-indexer)
  - [Transmission-compatible forks, as their own tier-2 target](#transmission-compatible-forks-as-their-own-tier-2-target)
  - [qBittorrent's embedded tracker as a second backend](#qbittorrents-embedded-tracker-as-a-second-backend)
  - [Removing the feed's magnet link entirely](#removing-the-feeds-magnet-link-entirely)
  - [Publishing to crates.io](#publishing-to-cratesio)
  - [A maintained CHANGELOG.md](#a-maintained-changelogmd)
  - [Internal refactors weighed and left alone](#internal-refactors-weighed-and-left-alone)

## Supported services

### Library sources (where tagged content comes from)

**Sonarr**, **Radarr**, **Lidarr**, **Readarr** and **Whisparr** via
tag-driven discovery (Whisparr reuses Sonarr's walk), and the **plain
directory** (`[[library]]`). Both shapes sit behind the `LibrarySource` seam
in `crates/sharerr/src/sync/mod.rs` — `kind()` plus `discover()`, which
returns a scan that also says whether it was _complete_: a partial walk still
shares what it found, but nothing is withdrawn on its behalf. That is where a
future source would plug in — none is currently planned.

### Torrent clients (what actually seeds)

**qBittorrent**, **Transmission**, and **rTorrent / ruTorrent**, behind
the `TorrentClient` trait in `sharerr-client`. That trait is deliberately
narrow, which is what made a second and third client tractable — clients
disagree about almost everything except "add this torrent, with the data
already at this path". Announces always go to sharerr's own tracker, so a
client needs no tracker of its own.

Adding another is mostly writing one file. What a new client must answer
honestly: whether it can remove a torrent _without_ deleting the data, how it
replaces a torrent's tracker list in place (`set_trackers`, for endpoint
rotation) and how it _adds_ to one without disturbing the rest
(`add_trackers`, for a torrent sharerr adopts rather than creates), whether it
can hand back a `.torrent` it already holds (`export`), and how it expresses
`AddRequest::upload_limit_kib`/`ratio_limit` when either is set — the one
deliberate exception to "ratios belong to the client," a seeding goal stated
once at add time through whatever native mechanism the client offers for it,
as qBittorrent (inline on `torrents/add`) and Transmission (a follow-up
`torrent-set`) do. rTorrent applies the upload cap but has no per-torrent
ratio limit — its ratio enforcement is an `.rtorrent.rc` schedule keyed to a
view — so it logs a warning and drops `ratio_limit`; the seeding form does
not know which backend is selected, so nothing warns there.

Two more things the three answer differently. Only qBittorrent can skip the
hash check on add (`qbittorrent.skip_checking`); Transmission and rTorrent
always verify, and sharerr passes `skip_checking = false` to both. And only
qBittorrent has a real category plus tags: Transmission merges both into its
flat `labels` list and filters by category client-side, and rTorrent has one
free-text `d.custom1` slot, which takes the category and drops the tags. The
config collapses both to a single `label` for those two.

Credentials differ too. qBittorrent takes only a WebUI API key (5.2+, sent as
a bearer token; there is no username/password path). Transmission takes HTTP
Basic plus the 409 session-id handshake, and needs 4.0+ (RPC 17) for the
`trackerList` calls behind `set_trackers`/`add_trackers`. rTorrent's XML-RPC
has no credential of its own, so its username/password are HTTP Basic aimed
at whatever reverse proxy fronts the RPC endpoint, and `rtorrent.url` is that
exact endpoint, not a base.

Only qBittorrent answers the `export` question — `torrents/export` returns
the file itself. Transmission and rTorrent can each name a path to it on the
daemon's own filesystem, which in a container deployment is not a filesystem
sharerr can read, so both return `Ok(None)` rather than guess. That costs them
one narrow case: a torrent that already covers a file, that sharerr did not
create and has no cached copy of, cannot be shared on those two — the feed
would advertise a release with no `.torrent` behind it, so sharerr fails the
item with a message naming the choice instead.

`sharerr-rtorrent` answers the tracker-replacement question honestly by *not*
fully answering it: rTorrent's XML-RPC has never grown a way to remove a
tracker (open upstream as
[rakshasa/rtorrent#165](https://github.com/rakshasa/rtorrent/issues/165)
since 2013), so `set_trackers` there can only insert a fresh tier ahead of
whatever is already on the torrent, not replace it — see the crate's module
docs for the full reasoning.

`scripts/run_docker_tests.sh --rtorrent` drives a real rTorrent + ruTorrent container
(`crazymax/rtorrent-rutorrent`) through the same tier-2 suite the plain,
`--transmission` and `--vpn` stacks use — confirming the requests and responses a real
rTorrent actually sends, not just the ones the crate's hand-mocked unit
tests expect. It already caught two bugs neither a hand-mocked server nor a
human reading rTorrent's docs had: `d.multicall2` rejects every call
without a leading empty parameter, and a real rTorrent answers an empty
result with a self-closing `<data/>` rather than `<data></data>`.

### Indexers (what consumes the feed)

**Prowlarr** (_Generic Torznab_), **Jackett**-shaped URLs, and
**Sonarr/Radarr/Lidarr direct**. The tier-2 script (`scripts/run_docker_tests.sh`)
adds sharerr as an indexer to a real Sonarr, and to a real Lidarr on the plain
stack, and drives its own Jackett-shaped routes and Torznab caps by hand;
Prowlarr is an opt-in container in that compose file for the manual exercise
in the README, and Radarr-direct is exercised only by hand. The feed also
advertises `book-search` and category 7000, so nothing _stops_ a Readarr
pointed at it — it is just untested; see
[below](#readarr-as-a-direct-indexer). No further indexer work is currently
planned.

## Not supported

Kept here as one place to check before re-proposing something, rather than
re-litigating a decision that already has a reason attached.

### Media-server library sources (Jellyfin, Emby, Plex)

A media-server-backed library source was tried and removed. The *arr apps and
a plain tagged directory already cover the two shapes of "where content
lives" this project wants to support; a media server would have been a third
way to answer the same question, not a new one. None is currently planned.

### Readarr as a direct indexer

Explicitly out of scope: this project targets small-scale homelab media-file
sharing, and books are a different, much smaller scale of content than the
audio/video files everything else here shares. Out of scope means untested
and unsupported, not blocked — the feed advertises book search and the book
category for Readarr-as-a-source's sake, so a Readarr pointed at it may well
work. This is only about the *indexer* direction — Readarr as a *library source* (tag-driven discovery) is
unaffected and fully supported; see [above](#library-sources-where-tagged-content-comes-from).

### Transmission-compatible forks, as their own tier-2 target

Not planned as separate work. `sharerr-transmission` has no version pinning
or fork detection — it only speaks the documented session-id handshake and
standard `torrent-*`/`session-get` methods, which is the same protocol
surface a compatible fork presents. The one de-facto floor is Transmission 4.0
(RPC 17), for the `trackerList` field the tracker-rotation calls need.
Standing up a real fork for tier-2 was investigated and set aside: the one actively-maintained candidate has no
published Docker image and its own maintainers describe its RPC
compatibility as imperfect.

### qBittorrent's embedded tracker as a second backend

qBittorrent's embedded tracker was used for a while as an alternative to
sharerr's own builtin tracker, and was removed: two tracker backends meant
two independently built announce URLs, and every improvement to endpoint
handling had to be made twice. A `sharerr.toml` still naming
`tracker.backend` fails to load with an error saying exactly this. See
[`DESIGN.md`](DESIGN.md)'s "Corrections the implementation forced" for the
fuller reasoning.

### Removing the feed's magnet link entirely

Considered and left in, for now. Every torrent sharerr builds is private
(the whole reason its own tracker exists), and a magnet can never actually
complete against one — nothing in the swarm will ever answer a
`ut_metadata` request. That was confirmed the hard way, in the two-instance
end-to-end test: Radarr's own direct Torznab client picked the magnet over
the working `.torrent` enclosure and stalled forever. The fix there was a
Prowlarr in front of the requesting Radarr, with its `preferMagnetUrl`
pinned to `false` — the one place that preference is actually
configurable; see `docker/README.md`'s "The two-instance stack" for how
`scripts/run_docker_tests_two_instance.sh` exercises this.

That fix does nothing for a friend who points Radarr or Sonarr *directly*
at a sharerr feed, no Prowlarr in between: their app decides magnet-or-`.torrent`
on its own, with no setting to override it, and at least one popular one has
been observed deciding wrong. Stripping the `magneturl` attribute from the
feed (and Jackett's `magnet_uri`) would close that gap for every consumer at
once, but was left in rather than removed: it still helps a genuinely DHT-capable
consumer, and pulling it is a one-line change that can be made the moment a
real report shows it biting a direct connection, rather than a
speculative fix for a failure mode confirmed on exactly one integration
shape so far.

### Publishing to crates.io

Decided against. `cargo install sharerr` as an alternative to the Docker
image was investigated and found technically feasible — the
migrations-outside-the-crate and `sharerr-testkit` dev-dependency issues
both had known fixes — but the project isn't taking on a ten-crate
dependency-ordered release process for a distribution path this project
doesn't intend to support. Nothing in the manifests enforces this (only
`sharerr-testkit` carries `publish = false`); it is a decision, not a guard.
The Docker image remains the only supported way to run sharerr.

### A maintained CHANGELOG.md

Decided against: git history is already the ledger, and a hand-maintained
file recording the same information a second time only ever drifts from it.
`docs/ROADMAP.md` holds what's still ahead and stays a to-do list, not a
running log — an entry is deleted once it ships, not dated and kept.

Sharerr's `publish` job never creates a GitHub Release object at all — see
[`docs/RELEASING.md`](RELEASING.md) — it only retags container images, so
there is no release page a `CHANGELOG.md` or generated notes would even
attach to. What a `v*` tag changed is visible the same way any commit range
is: `git log v1.2.2..v1.2.3` or GitHub's own tag-compare view. Revisit if a
GitHub Release (or an equivalent changelog surface) ever gets added for a
different reason — `gh api POST repos/.../releases/generate-notes`, generating
notes from merged PR titles at publish time, is the model to reach for
then, not a `CHANGELOG.md` written by hand.

### Internal refactors weighed and left alone

Candidates from a whole-codebase simplify pass, checked and rejected —
kept here so the same shape isn't re-proposed by a later pass over the same
code. Unlike the rest of this file these are implementation details with no
user-facing effect either way; listed for the same reason as everything
else here, not because they belong beside a feature decision.

- **The three `poll_loop`s** (`system_stats.rs`, `gluetun.rs`,
  `swarm_history.rs`) share only a three-line `loop { work; sleep }` over
  genuinely different bodies and intervals. Over-abstraction to unify.
- **`tracker.rs`'s `#[allow(dead_code)]`** on `AnnounceParams`/`ScrapeParams`
  are documentation-only utoipa shapes with a stated reason. Correct as
  written.
- **`doctor.rs` vs `checks.rs`.** The parallel `check_arr`/`check_library`/
  `check_qbit`/`check_paths` names look like duplication. They are not:
  `doctor.rs` delegates into `checks::` and its own functions are thin
  reporting wrappers.
- **`sharerr-transmission` as one file.** At ~550 production lines it is
  under the threshold where a `sharerr-qbit`-style module split pays for
  itself.
- **`sharerr-probe`'s two metadata loops** (Matroska, ISO-BMFF) look
  similar, but track types, codec accessors, and the `und`-language case
  differ enough that sharing costs more than the few lines saved.
- **`sharerr-probe` vs `MediaMeta::scene_*`.** The probe deliberately does
  not duplicate core's scene-token mapping; the split and the tests that
  verify it holds are documented in `sharerr-probe` itself.
- **`sharerr-arr`'s `api_prefix`** restates `MediaSource::api_version`, but
  both sides carry comments arguing for the split deliberately.
- **A shared trait across the three torrent-client backends** already
  exists (`sharerr-client`'s `TorrentClient`) at the right altitude — there
  is no further trait to invent.
- **Collapsing `Config::torrent_client_for`'s three match arms.** Only 2 of
  its 10 `TorrentClientConfig` fields are genuinely backend-agnostic; the
  other 8 correctly vary per arm. Every extraction shape tried — a helper
  struct, placeholder-then-mutate — cost more in indirection than the
  handful of duplicated lines it would have removed.
