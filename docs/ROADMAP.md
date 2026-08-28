# Roadmap

Where sharerr is going next.

sharerr is **experimental**. Nothing below is a release commitment, and the
ordering is a judgement about value, not a schedule — some entries are firm
intentions, others are ideas that have only been weighed so far, and the size
next to each is honest about how much is still open. What has already shipped
lives in [the README](../README.md#what-works-today), not here — this page
tracks what is still ahead, from feature-sized commitments down to
not-yet-committed ideas. An idea that gets declined instead moves to
[`UNSUPPORTED.md`](UNSUPPORTED.md), each entry there carrying its reason so
the decision does not get re-litigated.

## Table of contents

- [What's left](#whats-left)
- [Functionality](#functionality)
- [Open work, by scope](#open-work-by-scope)
- [Transfer accounting](#transfer-accounting)

### What's left

One feature-sized item — **[request flow](#functionality)**. Past that, the
rest of [Open work, by scope](#open-work-by-scope) below is ideas that have
been thought through but not all committed to — appearing here means the
reasoning is written down, not that it is scheduled. Design rationale for
work that has already shipped lives beside the feature itself rather than
here: see [`LIGHTHOUSE.md`](LIGHTHOUSE.md) for the rendezvous service.

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
= depends on ordering/config). File references are as of the review commit
and may have drifted.

### Medium — a subsystem, or one shape repeated across several files

1. **The remaining notification triggers** — `[notifications]` now has a
   per-trigger enable set and fires on six events: a sync failing outright, a
   friend going quiet, the advertised endpoint rotating, items newly shared,
   items failing to share, and a friend's key being revoked. Four candidates
   from the original review are still open, each needing more than a
   `notify::send()` call beside code that already runs: **a new friend's
   first contact** (needs a check for "is this the very first sighting"
   before `touch_peer`'s own throttle-window logic, which today conflates the
   two); **the tracker becoming unreachable** and **a `[[library]]` path
   going unreadable** (both need a new periodic polling loop, modelled on the
   existing `quiet_peers_loop`, that diffs against last-known state so a
   persistently-broken thing does not notify every cycle); and an
   **Uptime-Kuma-style heartbeat push** (needs a trigger built from scratch,
   with no existing detection to hang off of).

### Large — a protocol, a data model, or a release process

2. **Transfer accounting** — the largest gap between what sharerr _knows_
   and what it _keeps_; see [Transfer accounting](#transfer-accounting) below
   for the full write-up, including the caveats that matter before building
   it.

3. **Request flow** — a new inbound request queue and approve step, touching
   the sync engine and the web UI on both sides of a friendship; see
   [Functionality](#functionality).

---

## Transfer accounting

The largest gap between what sharerr _knows_ and what it _keeps_, and the
entry with the most caveats attached — which is why it is written out at
length rather than as a bullet.

Every BitTorrent client sends `uploaded` and `downloaded` on every announce.
`AnnounceRequest` parses `left` and drops both. And because announce URLs carry
a per-friend token, `authenticate_token` in `crates/sharerr/src/tracker.rs` has
_already resolved which friend this is_ by the time the request is handled — a
successful attribution writes to peer endpoint memory today, so the seam
exists and is already covered by tests.

So the data is first-hand and it is free. Nothing else in the stack has it:
qBittorrent knows per-torrent totals but not which friend they belong to, and
the *arr apps know nothing at all. It answers the question a sharing tool ought
to be able to answer and currently cannot — **is anyone actually using this?**

### What the numbers are, and are not

Stated plainly, in the manner of [`SECURITY.md`](SECURITY.md)'s by-design list,
because each of these is a property to accept rather than a bug to fix later:

- **Advisory, not authoritative.** The values are whatever the friend's client
  reports, and a client can report anything. This is a friend-to-friend tool;
  the value here is insight, not enforcement, and any UI must not imply
  otherwise. Nothing should ever be _gated_ on these numbers.
- **The counters reset.** They are cumulative per client session, so a restart
  or a re-add sends them back to zero. Accounting therefore records monotonic
  deltas and treats a _decrease_ as a new session counted from zero, not as
  negative traffic. Getting this backwards produces plausible-looking totals
  that are quietly wrong.
- **Peer ids churn.** Re-adding a torrent yields a fresh `peer_id`, so the
  durable key is (peer, info hash) with the peer id as a session discriminator
  underneath it.
- **The announce path is hot.** A database write per announce is the wrong
  shape. Accumulate in memory beside `Swarms` and flush on a timer — the same
  arrangement `Swarms` already has, for the same reason.
- **Not every announce has a friend attached.** The shared instance token is
  deliberately unattributed, so those rows need somewhere to go that is not a
  peer.

### What it would record

Two shapes, and they answer different questions. Lifetime totals per
(peer, info hash) answer _who has pulled what_; time-bucketed samples answer
_when_, and are what any chart would read. The second is optional and can
follow the first — the totals are useful on their own, and are the cheaper
half.

### What it unlocks

A "served" column on the friends table, so a friend who was added and never
actually used the feed is visible as such. A bytes-out figure on the status
page that means something. A per-item panel answering who pulled this, which
pairs naturally with the existing per-item detail page. And, alongside the
existing metrics endpoint, per-peer counters for anyone who would rather
graph it elsewhere.

This needs a migration, a change to the announce parser in `sharerr-torrent`,
an accumulator and flush loop, and the UI on top.
