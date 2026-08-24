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

One feature-sized item — **[request flow](#functionality)**. The 2026-08-21
code review is otherwise closed out: what is left of it is a single entry kept
only so its documented behaviour reads as a decision rather than an oversight.
Both are in [Open work, by scope](#open-work-by-scope) below.

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
= depends on ordering/config); nineteen batches of fixes landed on 2026-08-24
(the nineteenth: the lighthouse's `report` now pins a key hash to the first
keypair that claims it, so a leaked key hash can no longer be used to displace
the genuine record — and a refused report is logged by the reporting instance
instead of vanishing) and what is listed here is what remains. File references
are as of the review commit and may have drifted.

### Small — one function or one file

1. **Dual-token admission on the items page** — `items.rs` `token_status`
   consults only the current fingerprint, so a previous-token item renders
   Stale while the tracker admits it. The doc comment frames that as
   intended; listed so the decision is a decision.

### Large — a protocol, a data model, or a release process

2. **Request flow** — a new inbound request queue and approve step, touching
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

A verifiable record answers "is this really them?" but not "does this keypair
belong under this key hash?" — and a key hash is a URL path segment, so it is
visible in every proxy log along the way. So a key hash is claimed by the first
keypair to report under it and holds that claim until the record ages out,
which is the same trust-on-first-use gossip binds a peer's identity with. That
keeps the rendezvous working under a leaked key hash, where before an attacker
could mint a record of their own and displace the real one. What it cannot do
is protect a key hash nobody has claimed yet: whoever reports first wins, and
if that is an attacker the pair needs a new key rather than a new lighthouse.
