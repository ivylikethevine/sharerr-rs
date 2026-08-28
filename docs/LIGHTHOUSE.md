# The lighthouse

Shipped — see [the README](../README.md#the-lighthouse) for how to use it.
The design rationale below is kept here because it explains _why_ the
rendezvous works the way it does, which the README's usage-focused section
does not restate.

## Table of contents

- [Why it exists](#why-it-exists)
- [The privacy property](#the-privacy-property)
- [Telling a real record from a decoy](#telling-a-real-record-from-a-decoy)
- [Binding a key hash to a keypair](#binding-a-key-hash-to-a-keypair)

## Why it exists

Gossip only helps peers who can still reach _somebody_; two friends whose
addresses both rotated while neither was watching have no path back to each
other. The lighthouse is the rendezvous for that case: a tiny separate
service, deliberately knowing nothing but `key hash → latest IP and port`,
that a sharerr instance reports its endpoint to and a friend queries with the
API key that peer issued them.

## The privacy property

The privacy property is the point and shapes the whole design: a request
without a valid key gets a _plausible fabricated_ IP and port rather than an
error, so an unauthenticated probe cannot be distinguished from a valid
lookup — the lighthouse never confirms that an instance exists, and scraping
it yields only noise. That makes semi-anonymous tracking of sharerr
instances possible without any instance exposing its IP publicly. It ships
as its own docker image on its own port — not another route on sharerr's
listener — so it can be self-hosted by anyone, placed on neutral ground away
from any particular library, and carries no database worth stealing: key
hashes and last-seen addresses only. A sharerr instance treats it as one
more observation source feeding peer endpoint memory, ranked below a direct
sighting of the same peer.

## Telling a real record from a decoy

The fabricated answers create the opposite problem for the _legitimate_
caller: a friend holding a valid key must be able to tell a real record from
a decoy, or the noise defeats them too. So a genuine record is verifiable —
the natural shape is the same signed endpoint record gossip uses, signed by
the peer it describes when that peer reported in, so the lighthouse relays
proof it could not forge and a JWT-style signature check separates record
from decoy. A decoy carries random bytes where the signature would be:
identical on the wire to an observer without the peer's public key, and
never verifying for anyone. The deterministic fallback where signing is
unavailable: derive the decoy from a keyed hash of the queried key hash, so
decoys are at least stable across probes rather than fresh noise that flags
itself by changing.

## Binding a key hash to a keypair

A verifiable record answers "is this really them?" but not "does this
keypair belong under this key hash?" — and a key hash is a URL path segment,
so it is visible in every proxy log along the way. So a key hash is claimed
by the first keypair to report under it and holds that claim until the
record ages out, which is the same trust-on-first-use gossip binds a peer's
identity with. That keeps the rendezvous working under a leaked key hash,
where before an attacker could mint a record of their own and displace the
real one. What it cannot do is protect a key hash nobody has claimed yet:
whoever reports first wins, and if that is an attacker the pair needs a new
key rather than a new lighthouse.
