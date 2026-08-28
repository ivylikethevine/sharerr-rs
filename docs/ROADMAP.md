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

### Small — one function or one file

1. **Dual-token admission on the items page** — `items.rs` `token_status`
   consults only the current fingerprint, so a previous-token item renders
   Stale while the tracker admits it. The doc comment frames that as
   intended; listed so the decision is a decision.

2. **Parallel tracker announces in the rTorrent adapter** —
   `sharerr-rtorrent`'s `set_trackers` awaits one full HTTP round trip per
   URL, sequentially, across the whole seeded set after every VPN
   reconnect. `system.multicall` — the adapter already parses arrays of
   arrays through `call_multi` — or a `buffered(N)` the way
   `sharerr-arr/src/lib.rs` already does for its own concurrent calls would
   collapse it to one round trip. Left sequential for now: the module's own
   doc comment says insertion order at group 0 decides the new trackers'
   relative tier priority, and no test guards that order, so this needs a
   test pinning tier order before it is safe to parallelise.

### Medium — a subsystem, or one shape repeated across several files

3. **More notification triggers** — `crates/sharerr/src/notify.rs` fires on
   two things: a sync that failed, and a friend gone quiet. Everything routes
   through a single `send()`, so adding triggers is cheap — the cost is not
   plumbing, it is restraint. The ones that seem worth having, on the test of
   _would an operator want to be told without having to look_: an item newly
   shared (digested, not one message per file), an item that failed and why,
   a new friend's first contact, a friend revoked, the tracker becoming
   unreachable, and a `[[library]]` path that has stopped being readable. The
   strongest of them is **the advertised endpoint rotating**. When gluetun
   hands over a new IP or forwarded port, every announce URL sharerr publishes
   moves with it — that is the single event most likely to break a friend's
   downloads while everything on this end still looks healthy. Anything added
   here needs a per-trigger enable set under `[notifications]`; without one
   this becomes noise and the operator mutes the whole channel, including the
   two triggers that were worth having in the first place. Also worth
   recording so it is not mistaken for separate work: an Uptime-Kuma-style
   heartbeat push is one more trigger through the same `send()`, not a
   feature of its own.

4. **Config backup and restore** — master-key loss is unrecoverable by
   design, and the vault is doing exactly what it should. What is missing is
   the _other_ half: a way to capture the configuration — sources, mappings,
   peers, scopes — so that rebuilding an instance does not mean retyping
   everything from screenshots. Secrets stay out of any export, and that is
   the point rather than a limitation: an export containing recoverable
   credentials would be a plaintext copy of the vault, which is the thing the
   vault exists to prevent. A restore path therefore ends with re-entering
   secrets, and the documentation should say so plainly instead of leaving
   it to be discovered.

5. **One gluetun template, not two parallel copies** — `settings.html`
   carries twelve near-identical Askama fields, six per gluetun section
   (the main poller and the client-only poller). The functional
   duplication behind it is already gone: `sharerr::gluetun::GluetunTarget`
   covers config, secret, and config-path lookups for both slots, and
   `save_gluetun_section` takes a `GluetunTarget` instead of loose
   constants. What is left is purely template and view-model — an Askama
   macro parameterised by section, plus the Rust-side struct that currently
   repeats each field by name. Left alone for now: that is a materially
   bigger, riskier change for a cosmetic-only win once the functional
   duplication it would have been fixing is already gone.

### Large — a protocol, a data model, or a release process

6. **Transfer accounting** — the largest gap between what sharerr _knows_
   and what it _keeps_; see [Transfer accounting](#transfer-accounting) below
   for the full write-up, including the caveats that matter before building
   it.

7. **Request flow** — a new inbound request queue and approve step, touching
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
