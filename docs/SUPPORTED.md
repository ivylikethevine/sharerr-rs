# Supported services

What sharerr talks to today, and the seam each category plugs into. For usage
and config snippets, see [the README](../README.md); for what was tried and
deliberately left out, see [`UNSUPPORTED.md`](UNSUPPORTED.md).

## Table of contents

- [Library sources (where tagged content comes from)](#library-sources-where-tagged-content-comes-from)
- [Torrent clients (what actually seeds)](#torrent-clients-what-actually-seeds)
- [Indexers (what consumes the feed)](#indexers-what-consumes-the-feed)

## Library sources (where tagged content comes from)

**Sonarr**, **Radarr**, **Lidarr**, **Readarr** and **Whisparr** via
tag-driven discovery, and the **plain tagged directory** (`[[library]]`). Both
shapes sit behind the `LibrarySource` seam, which is where a future source
would plug in — none is currently planned.

## Torrent clients (what actually seeds)

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
`AddRequest::upload_limit_kib`/`ratio_limit` when either is set — the one deliberate exception to "ratios belong to the
client," a seeding goal stated once at add time through whatever native
mechanism the client offers for it, same as qBittorrent (inline on
`torrents/add`) and Transmission (a follow-up `torrent-set`) already do.
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

`run_docker_tests.sh --rtorrent` drives a real rTorrent + ruTorrent container
(`crazymax/rtorrent-rutorrent`) through the same tier-2 suite the plain and
Transmission stacks use — confirming the requests and responses a real
rTorrent actually sends, not just the ones the crate's hand-mocked unit
tests expect. It already caught two bugs neither a hand-mocked server nor a
human reading rTorrent's docs had: `d.multicall2` rejects every call
without a leading empty parameter, and a real rTorrent answers an empty
result with a self-closing `<data/>` rather than `<data></data>`.

## Indexers (what consumes the feed)

**Prowlarr** (_Generic Torznab_), **Jackett**-shaped URLs, and
**Sonarr/Radarr/Lidarr direct**, each confirmed against a real instance in the
tier-2 suite. No further indexer work is currently planned.
