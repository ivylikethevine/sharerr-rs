# The lighthouse

The rendezvous service for friends who have lost each other's addresses: why
it exists, the privacy property it is built around, and how to run one.
[The README](../README.md#the-lighthouse) has the two-paragraph version.

## Table of contents

- [Why it exists](#why-it-exists)
- [The privacy property](#the-privacy-property)
- [Telling a real record from a decoy](#telling-a-real-record-from-a-decoy)
- [Binding a key hash to a keypair](#binding-a-key-hash-to-a-keypair)
- [Using one](#using-one)
- [Running one](#running-one)

## Why it exists

Gossip only helps peers who can still reach _somebody_; two friends whose
addresses both rotated while neither was watching have no path back to each
other. The lighthouse is the rendezvous for that case: a tiny separate
service, deliberately knowing nothing but `key hash → latest IP and port`,
that a sharerr instance reports its endpoint to and a friend queries with the
API key that peer issued them.

## The privacy property

The privacy property is the point and shapes the whole design:

- A request without a valid key gets a _plausible fabricated_ IP and port
  rather than an error, so an unauthenticated probe cannot be distinguished
  from a valid lookup. The lighthouse never confirms that an instance exists,
  and scraping it yields only noise.
- It ships as its own image on its own port, not another route on sharerr's
  listener, so it can be self-hosted by anyone on neutral ground, away from
  any particular library.
- It carries no database worth stealing: key hashes and last-seen addresses
  only.
- A sharerr instance treats it as one more observation source feeding peer
  endpoint memory, ranked below a direct sighting of the same peer.

## Telling a real record from a decoy

The fabricated answers create the opposite problem for the _legitimate_
caller: a friend holding a valid key must be able to tell a real record from
a decoy, or the noise defeats them too. So a genuine record is verifiable:
the same signed endpoint record gossip uses, signed by the peer it describes
when that peer reported in, so the lighthouse relays proof it could not forge
and a signature check separates record from decoy. A decoy carries random
bytes where the signature would be: identical on the wire to an observer
without the peer's public key, and never verifying for anyone. Where signing
is unavailable, the decoy is derived from a keyed hash of the queried key
hash, so decoys are stable across probes rather than fresh noise that flags
itself by changing.

## Binding a key hash to a keypair

A verifiable record answers "is this really them?" but not "does this keypair
belong under this key hash?", and a key hash is a URL path segment, visible
in every proxy log along the way. So a key hash is claimed by the first
keypair to report under it and holds that claim until the record ages out,
the same trust-on-first-use gossip binds a peer's identity with. That keeps
the rendezvous working under a leaked key hash, where before an attacker
could mint a record of their own and displace the real one. What it cannot
do is protect a key hash nobody has claimed yet: whoever reports first wins,
and if that is an attacker the pair needs a new key rather than a new
lighthouse.

## Using one

Settings → Lighthouse, or `lighthouse.urls` in `sharerr.toml`: one or more
lighthouse base URLs, self-hosted by a friend or by you.

```toml
[lighthouse]
urls = ["https://a-friends-lighthouse.example"]
```

With at least one set, sharerr reports its own endpoint to every URL listed,
once per active friend's issued-key hash (a lighthouse indexes by key hash
alone and never learns which reports belong to the same instance), and
queries the same list for any friend who has gone quiet. A lookup result is
only trusted, and folded into peer endpoint memory, once it verifies against
that friend's already-known identity, bound the first time gossip heard from
them. A friend never gossiped with has nothing to check an answer against and
is skipped rather than guessed at.

Using a lighthouse and running one (below) are independent choices, not a
matched pair.

## Running one

Three ways, from the most to the least isolated.

**Its own container.** The `sharerr-lighthouse` image is built from
`docker/Dockerfile`'s `runtime-lighthouse` target and published to GHCR as
its own package, on its own `v*` tag series and behind its own approval, so a
sharerr release is not silently also a lighthouse release. `:latest` tracks
the newest tagged lighthouse release; `sha-<commit>` tracks `main` between
releases instead (see [`docs/RELEASING.md`](RELEASING.md#between-releases-the-sha-tag)),
or build it yourself:

```bash
docker run -d --name sharerr-lighthouse -p 7878:7878 \
  -v lighthouse-data:/data ghcr.io/ivylikethevine/sharerr-lighthouse:latest

# or, from source:
docker build -f docker/Dockerfile --target runtime-lighthouse -t sharerr-lighthouse .
docker run -d --name sharerr-lighthouse -p 7878:7878 -v lighthouse-data:/data sharerr-lighthouse
```

`/data` holds nothing but the decoy secret; losing it reshuffles fabricated
answers after a restart, not a credential. The binary reads two environment
variables: `LIGHTHOUSE_BIND` (default `0.0.0.0:7878`) and
`LIGHTHOUSE_SECRET_FILE` (default `/data/lighthouse.secret`). A compose version of the
same one-liner is at
[`docker/deploy/lighthouse/`](https://github.com/ivylikethevine/sharerr-rs/tree/main/docker/deploy/lighthouse).

**Embedded in sharerr.** For a single operator who would rather not run a
second container, it can run as extra routes on one of sharerr's own
listeners, under Settings → Lighthouse or directly in `sharerr.toml`:

```toml
[lighthouse]
enabled = true
mount = "tracker"   # or "frontend"
```

`mount = "tracker"` puts it on the same port a friend's torrent client
already reaches (`tracker.bind` if set, otherwise the main listener);
`mount = "frontend"` puts it on the main listener regardless. Off by default,
and unrelated to `lighthouse.urls` above. Field reference:
[`docs/SETTINGS.md`](SETTINGS.md#lighthouse).
