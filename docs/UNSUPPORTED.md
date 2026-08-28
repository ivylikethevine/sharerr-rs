# Not supported

Things that were tried and removed, or considered and deliberately left out —
kept here as one place to check before re-proposing them, rather than
re-litigating a decision that already has a reason attached. For what *is*
supported, see [`SUPPORTED.md`](SUPPORTED.md); for what has been considered
without being either committed to or declined, see
[`ROADMAP.md`](ROADMAP.md#open-work-by-scope).

## Table of contents

- [Media-server library sources (Jellyfin, Emby, Plex)](#media-server-library-sources-jellyfin-emby-plex)
- [Readarr as a direct indexer](#readarr-as-a-direct-indexer)
- [Transmission-compatible forks, as their own tier-2 target](#transmission-compatible-forks-as-their-own-tier-2-target)
- [qBittorrent's embedded tracker as a second backend](#qbittorrents-embedded-tracker-as-a-second-backend)
- [Removing the feed's magnet link entirely](#removing-the-feeds-magnet-link-entirely)
- [Publishing to crates.io](#publishing-to-cratesio)
- [Internal refactors weighed and left alone](#internal-refactors-weighed-and-left-alone)

## Media-server library sources (Jellyfin, Emby, Plex)

A media-server-backed library source was tried and removed. The *arr apps and
a plain tagged directory already cover the two shapes of "where content
lives" this project wants to support; a media server would have been a third
way to answer the same question, not a new one. None is currently planned.

## Readarr as a direct indexer

Explicitly out of scope: this project targets small-scale homelab media-file
sharing, and books are a different, much smaller scale of content than the
audio/video files everything else here shares. Out of scope means untested
and unsupported, not blocked — the feed advertises book search and the book
category for Readarr-as-a-source's sake, so a Readarr pointed at it may well
work. This is only about the *indexer* direction — Readarr as a *library source* (tag-driven discovery) is
unaffected and fully supported; see [`SUPPORTED.md`](SUPPORTED.md).

## Transmission-compatible forks, as their own tier-2 target

Not planned as separate work. `sharerr-transmission` has no version pinning
or fork detection — it only speaks the documented session-id handshake and
standard `torrent-*`/`session-get` methods, which is the same protocol
surface a compatible fork presents. The one de-facto floor is Transmission 4.0
(RPC 17), for the `trackerList` field the tracker-rotation calls need.
Standing up a real fork for tier-2 was investigated and set aside: the one actively-maintained candidate has no
published Docker image and its own maintainers describe its RPC
compatibility as imperfect.

## qBittorrent's embedded tracker as a second backend

qBittorrent's embedded tracker was used for a while as an alternative to
sharerr's own builtin tracker, and was removed: two tracker backends meant
two independently built announce URLs, and every improvement to endpoint
handling had to be made twice. A `sharerr.toml` still naming
`tracker.backend` fails to load with an error saying exactly this. See
[`DESIGN.md`](DESIGN.md)'s "Corrections the implementation forced" for the
fuller reasoning.

## Removing the feed's magnet link entirely

Considered and left in, for now. Every torrent sharerr builds is private
(the whole reason its own tracker exists), and a magnet can never actually
complete against one — nothing in the swarm will ever answer a
`ut_metadata` request. That was confirmed the hard way, in the two-instance
end-to-end test: Radarr's own direct Torznab client picked the magnet over
the working `.torrent` enclosure and stalled forever. The fix there was a
Prowlarr in front of the requesting Radarr, with its `preferMagnetUrl`
pinned to `false` — the one place that preference is actually
configurable; see `docker/README.md`'s "The two-instance stack" for how
`run_docker_tests_two_instance.sh` exercises this.

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

## Publishing to crates.io

Decided against. `cargo install sharerr` as an alternative to the Docker
image was investigated and found technically feasible — the
migrations-outside-the-crate and `sharerr-testkit` dev-dependency issues
both had known fixes — but the project isn't taking on a ten-crate
dependency-ordered release process for a distribution path this project
doesn't intend to support. Nothing in the manifests enforces this (only
`sharerr-testkit` carries `publish = false`); it is a decision, not a guard.
The Docker image remains the only supported way to run sharerr.

## Internal refactors weighed and left alone

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
