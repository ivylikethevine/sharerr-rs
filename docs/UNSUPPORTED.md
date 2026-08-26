# Not supported

Things that were tried and removed, or considered and deliberately left out —
kept here as one place to check before re-proposing them, rather than
re-litigating a decision that already has a reason attached. For what *is*
supported, see [`SUPPORTED.md`](SUPPORTED.md); for what has been considered
without being either committed to or declined, see [`IDEAS.md`](IDEAS.md).

## Table of contents

- [Media-server library sources (Jellyfin, Emby, Plex)](#media-server-library-sources-jellyfin-emby-plex)
- [Readarr as a direct indexer](#readarr-as-a-direct-indexer)
- [Transmission-compatible forks, as their own tier-2 target](#transmission-compatible-forks-as-their-own-tier-2-target)
- [qBittorrent's embedded tracker as a second backend](#qbittorrents-embedded-tracker-as-a-second-backend)
- [Publishing to crates.io](#publishing-to-cratesio)

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

## Publishing to crates.io

Decided against. `cargo install sharerr` as an alternative to the Docker
image was investigated and found technically feasible — the
migrations-outside-the-crate and `sharerr-testkit` dev-dependency issues
both had known fixes — but the project isn't taking on a ten-crate
dependency-ordered release process for a distribution path this project
doesn't intend to support. Nothing in the manifests enforces this (only
`sharerr-testkit` carries `publish = false`); it is a decision, not a guard.
The Docker image remains the only supported way to run sharerr.
