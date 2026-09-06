# Alternatives

sharerr answers one specific question — how does a friend's own Sonarr or
Radarr find _your_ library, matched by TVDB/TMDb/IMDb id rather than a
filename guess, without either of you moving a file or trusting the other
with your infrastructure. Two other approaches answer adjacent-sounding
questions, and it is worth being precise about which is which before
reaching for any of them.

## Table of contents

- [Autobrr and cross-seed](#autobrr-and-cross-seed)
- [A shared or pooled \*arr instance](#a-shared-or-pooled-arr-instance)
- [What sharerr trades away](#what-sharerr-trades-away)

## Autobrr and cross-seed

Both automate a tracker account you already hold; neither has a concept of a
friend's own, separate library.

**[Autobrr](https://autobrr.com/)** watches indexer announces (IRC, RSS, or
Torznab) and pushes a release matching your filters to a download client or
an *arr app, before the RSS feed would even have updated. It is acquisition
automation: getting content into your library faster. It has no notion of a
friend, a private per-recipient torrent, or an instance that is not yours.

**[cross-seed](https://www.cross-seed.org/)** finds a torrent you already
have and adds it to every other tracker you are a member of, matching by
file, or by IMDb/TMDb id when the file has moved or been renamed — so a new
private-tracker account can build ratio from content already on disk instead
of re-downloading it. The id-matching idea is close to what sharerr does for
a friend's Sonarr, but the "other end" is a tracker account you already
hold, not a friend running their own separate stack with no account
relationship to yours.

Neither conflicts with sharerr — both answer "get more into my library
faster or with better ratio," which is a question sharerr does not touch at
all; it only concerns itself with what already-tagged content leaves toward
a friend. Running one of them alongside sharerr is normal.

Where sharerr wins, for the problem it actually solves: a private,
per-friend torrent with revoke, matched by id in a friend's own Sonarr or
Radarr, with no shared tracker account, no credential exchange, and nothing
copied or moved to make it work.

## A shared or pooled *arr instance

The alternative of literally giving a friend an account, or a Plex/Jellyfin
login, on your own *arr stack and media server — or running one shared
instance for a household or group — rather than each person running their
own.

Where pooling wins: it is the simpler answer when everyone already trusts
each other or lives together. One library, one place to browse, no
duplicate storage, no re-downloading what is already on the server, watch
progress that lives in one place instead of being fragmented across
instances. If what you actually want is "let my roommate watch what's on my
server," a shared Plex or Jellyfin login is the right tool, full stop —
sharerr deliberately does not do media serving; see
[Support](SUPPORT.md#media-server-library-sources-jellyfin-emby-plex) for
why that was tried and removed.

Where sharerr wins: no credential or infrastructure sharing at all. A friend
runs their own Sonarr, Radarr, and torrent client, keeps their own quality
profiles, naming, and folder layout, and only ever talks to your instance
the way any indexer talks to an *arr app — over Torznab, with a token. Per-
friend keys with revoke, and per-friend library scoping (see
[Support](SUPPORT.md#supported-services)), mean cutting off one friend never
touches another's access, and neither of you ever holds a credential to the
other's server. That asymmetry is the actual design point, not an
afterthought — sharerr's data model has exactly one user per instance, on
purpose; see [Support's "Multi-user"](SUPPORT.md#multi-user) for why pooling
several people onto one instance was considered and declined. Friends are
peers with their own instance, never accounts on yours.

## What sharerr trades away

Against either alternative: no media streaming, no watch-together, and no
deduplicated storage across friends — each friend's client fully downloads
its own copy, because that is what a private, revocable, per-friend torrent
requires. And unlike Autobrr, sharerr does nothing to help you acquire new
content; it only concerns itself with what you already have, tagged, and
willing to share.
