# Design brief

The original statement of intent for sharerr, kept verbatim because the
implementation is answerable to it, followed by the two places where building it
proved a premise wrong.

## Table of contents

- [The brief](#the-brief)
- [Corrections the implementation forced](#corrections-the-implementation-forced)
  - [qBittorrent does not publish an RSS feed](#qbittorrent-does-not-publish-an-rss-feed)
  - [The no-egress requirement is not enforced by the test stack](#the-no-egress-requirement-is-not-enforced-by-the-test-stack)
- [What the brief got right](#what-the-brief-got-right)

## The brief

This is a project that aims to help users with content share that content over
already existing tools, as a friend-to-friend system. It aims to be as friction-less
as possible, and slim on resource and configuration requirements. It will be
run typically as a docker image, accessable via a web interface.

I want the tech stack to use Rust as the base layer. There are existing APIs
for the services I would like to integrate with. The idea is as follows:

1. Existing selfhosted deployments have libraries of content, as well as all
   the required metadata to accurately request and serve that content.
2. qbittorrent has a built-in rss feed & an optional embedded tracker
3. Prowlarr can index torrents from qbittorrent's rss feed
4. I would like sharerr to connect to sonarr and radarr to find any content tagged
   "sharerr", and serve it over the existing tools, making the new torrent as easily
   searchable/indexable by those existing tools as possible.
5. One would then be able to connect to another friend's sharerr instance to
   share content.
6. Eventually, I would like a web interface to manage sharerr instances and content,
   and the ability for a sonarr or radarr instance to directly request content from
   a sharerr instance, again with as much metadata as possible, using the sonarr/radarr
   data as well as metadata found with the file. I want to also preserve any existing
   torrents if possible, instead of moving the file & causing errors.

The services:

1. Sonarr - library and request system for tv shows
2. Radarr - library and request system for movies
3. Qbittorrent - torrent client
4. Prowlarr - torrent indexer
5. Docker - container runtime

Requirements:

1. Security - use API tokens for the services, and store them securely. Only send
   them with requests as necessary. Do not store them in plaintext, and maintain
   them between service restarts.
2. Network usage - do not make any requests outside of the configured services
   unless absolutely necessary.
3. Testing - use docker to run the various services with test configurations.
   Do not use any real files or filenames.

Assumptions:

1. The user has a system with currently deployed sonarr (and/or) radarr.
   This is typically via docker.
2. The user has a torrent client (qbittorrent) and indexer (prowlarr) running.
3. The user may or may not be using a VPN, or a VPN container such as gluetun.
4. The user has their media library accessable to the container(s).

## Corrections the implementation forced

### qBittorrent does not publish an RSS feed

Points 2 and 3 of the brief assume qBittorrent can expose a feed for Prowlarr to
index. It cannot: qBittorrent **consumes** RSS feeds, it does not publish one.
There is therefore no qBittorrent feed to point Prowlarr at.

sharerr serves the feed itself, as **Torznab** — the format Prowlarr's *Generic
Torznab* indexer speaks, and a better fit anyway, since Torznab carries the
TVDB/TMDb/IMDb ids that let a release match a known series or film instead of being
parsed from its name.

qBittorrent's *embedded tracker* is real and was used for a while. It has since
been removed as a backend: two trackers meant two independently built announce
URLs, and the dynamic-endpoint work made that untenable — sharerr's builtin
tracker is now the only one. So in the end *both* halves of the original plan's
point 2 changed, the feed first and the tracker later.

### The no-egress requirement is not enforced by the test stack

Requirement 2 asks for no outbound requests beyond the configured services. The
compose test stack originally enforced this with an `internal: true` network,
giving the containers no route off the host.

That had to be dropped. An internal bridge also severs the host→container path
that *published ports* travel, and the test stack's entire control plane runs over
those ports — readiness probes, API-key scraping, database seeding, and the browser
URLs in the documentation. With the network isolated, readiness probes hung against
containers that were perfectly healthy.

The requirement still stands as a property of the code, and the hermetic test suite
covers it: the service clients are exercised against wiremock on loopback and reach
nothing else. It is simply no longer *proved* by the kernel refusing to route. See
[docker/README.md](../docker/README.md).

## What the brief got right

Worth recording, since the corrections above are the exceptions:

- **Never move the file.** Point 6's aside about preserving existing torrents
  instead of moving files became the constraint the whole torrent layer is built
  around, and the assertion that justifies the entire tier-2 test suite: after a
  real sync through a real qBittorrent, every media file has the same inode, mtime,
  and length it started with.
- **Tag-driven discovery.** Point 4's "content tagged sharerr" is exactly the
  mechanism, and it turned out to be the right granularity — it is the one control
  an operator already understands from the *arr apps.
- **Metadata fidelity.** Point 6's "with as much metadata as possible" is what
  Torznab's id attributes deliver, and it is the difference between a friend's
  Sonarr recognising a release and merely seeing a filename.
