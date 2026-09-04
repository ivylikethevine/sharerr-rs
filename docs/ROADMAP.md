# Roadmap

Where sharerr is going next.

sharerr is **experimental**. Nothing below is a release commitment, and the
ordering is a judgement about value, not a schedule — some entries are firm
intentions, others are ideas that have only been weighed so far, and the size
next to each is honest about how much is still open. What has already shipped
lives in [the README](../README.md#what-works-today), not here — this page
tracks what is still ahead, from feature-sized commitments down to
not-yet-committed ideas. An idea that gets declined instead moves to
[`SUPPORT.md`](SUPPORT.md#not-supported), each entry there carrying its reason so
the decision does not get re-litigated.

## Table of contents

- [What's left](#whats-left)
- [Before v1](#before-v1)
- [Open work, by scope](#open-work-by-scope)
  - [Medium — a subsystem, or one shape repeated across several files](#medium--a-subsystem-or-one-shape-repeated-across-several-files)
  - [Large — a protocol, a data model, or a release process](#large--a-protocol-a-data-model-or-a-release-process)
- [Transfer accounting](#transfer-accounting)
  - [What the numbers are, and are not](#what-the-numbers-are-and-are-not)
  - [What it would record](#what-it-would-record)
  - [What it unlocks](#what-it-unlocks)

## What's left

The [Open work, by scope](#open-work-by-scope) list below is every idea that
has been thought through but not all committed to — appearing here means the
reasoning is written down, not that it is scheduled. [Before v1](#before-v1)
is a separate list: operational tasks that block a first tagged release, not
features. Design rationale for work that has already shipped lives beside the
feature itself rather than here: see [`LIGHTHOUSE.md`](LIGHTHOUSE.md) for the
rendezvous service.

What sharerr already talks to — library sources, torrent clients, indexers —
and the extension seam each sits behind, along with what is deliberately left
out and why, is [`SUPPORT.md`](SUPPORT.md).

## Before v1

Operational, not architectural. None of these are features; each is something
that has to actually happen, once, before a `v1` tag:

1. **Rehearse the release pipeline's build half; the publish half — and the
   Release it now creates — necessarily execute for real on the first tag.**
   Both images, `docker.yml` and `docker-lighthouse.yml`, accept a manual
   `workflow_dispatch` that rehearses `build` — the same
   push-a-provisional-tag-and-attest path a `v*` push would take (see
   [`RELEASING.md`](RELEASING.md#rehearsing-it)). Neither `publish` nor
   `docker.yml`'s `release` job (which creates the GitHub Release, see
   [`RELEASING.md`](RELEASING.md#the-github-release)) can be reached this way:
   both require `github.event_name == 'push'` in addition to the tag-ref
   check, so a dispatch can never satisfy either — the first real `v*` tag is
   necessarily also the first time `publish` and `release` run. What can and
   should be rehearsed now: run both `build`s, and verify in the repository
   settings — not any workflow file — that the `release` environment actually
   has a required reviewer configured. That gate is a GitHub Settings fact no
   workflow can assert; without it, `publish` (and, downstream of it,
   `release`) runs unattended the moment `build` finishes.
2. **Rehearse one real upgrade across a migration.** Eleven forward-only
   sqlx migrations exist and every one has only ever run against a fresh
   database. Before v1: an older image, a populated `/data`, then the new
   image over it. Forward-only with no downgrade path is a fine policy, but
   it has never been a *tested* policy.
3. **Decide the magnet-link question deliberately, not by default.** Every
   torrent sharerr builds is private, so a magnet can never resolve, and the
   two-instance end-to-end test already confirmed Radarr's own direct
   Torznab client picks the magnet over the working `.torrent` and stalls
   forever — see [`SUPPORT.md`](SUPPORT.md#removing-the-feeds-magnet-link-entirely).
   The current position is to keep `magneturl` in the feed and pull it "the
   moment a real report shows it biting". For v1 the target user is a friend
   pointing an *arr app directly at the feed, the failure mode is a silent
   hang rather than an error, and the first report costs a debugging session
   on both sides of the friendship — reconsider before tagging, not after.
4. **Add snapshot guidance and the master-key warning to `SETTINGS.md`'s
   backup section.** [`SETTINGS.md`](SETTINGS.md#backup-and-restore) already
   documents what config export covers (the effective `sharerr.toml`, nothing
   in the vault or the peers table) and that the `[[peers]]` export block must
   be downloaded before the loss it protects against. Still missing there: a
   volume-snapshot-or-`sqlite3 .backup` line, and the statement — currently
   only in [`SECURITY.md`](SECURITY.md) — that losing `SHARERR_MASTER_KEY` is
   unrecoverable by design.
5. **Resolve the login-rate-limiting tension before it is a support ticket.**
   [`SECURITY.md`](SECURITY.md) lists the absence of login rate limiting as
   by-design, while the deploy docs separately present "just forward 8477 as
   it is" as a workable option. Individually defensible, jointly
   uncomfortable — one of the two positions should move.
6. **Add the query-string caveat to `SECURITY.md`'s by-design list.** The feed
   API key, the `.torrent` download token, and the tracker's scrape token all
   travel in query strings — consistent with the stated threat model, but
   landing in any reverse-proxy access log in front of an instance. (The
   announce token itself is a path segment, not a query parameter.) Worth a
   line where the rest of the by-design tradeoffs already live; it currently
   is not there.

## Open work, by scope

Everything still ahead, in one list, smallest first — by how much each item
touches, not how long it would take to get right.

### Medium — a subsystem, or one shape repeated across several files

1. **The remaining notification triggers** — `[notifications]` has a
   per-trigger enable set and fires on six events: a sync failing outright, a
   friend going quiet, the advertised endpoint rotating, items newly shared,
   items failing to share, and a friend's key being revoked. Four more
   triggers would each need more than a `notify::send()` call beside code
   that already runs: **a new friend's first contact** (needs a check for "is
   this the very first sighting" ahead of `touch_peer`'s own throttle-window
   logic, which today conflates the two); **the tracker becoming
   unreachable** (needs a new periodic polling loop, modelled on the existing
   `quiet_peers_loop`, that diffs against last-known state so a
   persistently-broken thing does not notify every cycle); **a `[[library]]`
   path going unreadable** (no new loop needed — `SourceScan.complete`
   already detects this during a normal sync pass and feeds `sources_failed`;
   only the trigger and the `notify::send()` call are missing); and an
   **Uptime-Kuma-style heartbeat push** (needs a trigger built from scratch,
   with no existing detection to hang off of).

2. **A public lighthouse.** A one-liner Docker deploy and a
   [`docker/deploy/lighthouse/`](../docker/deploy/lighthouse/) compose recipe
   both exist now, and a single operator can also run the lighthouse embedded
   in one of sharerr's own listeners instead of standing up a third container
   (`[lighthouse] mount = "tracker"` or `"frontend"`) — see the README's
   lighthouse section. What is still open is no longer code: no public
   instance exists yet for a friend group that would rather not run any
   lighthouse of its own. See [`LIGHTHOUSE.md`](LIGHTHOUSE.md) for the design
   rationale a public instance would build on.

3. **Seeding limits that apply retroactively.** The upload cap and ratio
   goal bind at add time only, through whatever native mechanism each
   client offers for it — see
   [`SUPPORT.md`](SUPPORT.md#torrent-clients-what-actually-seeds). A user
   who discovers their link saturated changes the setting and watches
   nothing happen; applying a changed limit to an already-seeding torrent
   is the same one-shape-per-client problem the initial add already solved,
   done a second time on update.

### Large — a protocol, a data model, or a release process

4. **Transfer accounting** — the largest gap between what sharerr _knows_
   and what it _keeps_; see [Transfer accounting](#transfer-accounting) below
   for the full write-up, including the caveats that matter before building
   it.

5. **Request flow.** The original design brief wanted a friend's Sonarr/Radarr
   to _request_ content; today discovery is one-way, they find what you
   already share. An inbound request queue with an approve step is the other
   half of that idea, touching the sync engine and the web UI on both sides
   of a friendship.

6. **Multi-user.** The `users` table already exists; only the first-run claim
   ever creates a row in it (an already-claimed instance's own password
   change updates that one row rather than adding another). A second user
   means deciding what a friendship, a library, and a torrent client belong
   to — per instance, as today, or per user — before any access-control
   surface can be built on top.

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
  shape. Accumulate in memory beside `Swarms` and flush on a timer, in the
  manner of `swarm_history::poll_loop` — though that sampler flushes
  aggregate totals rather than per-key deltas, so it is a starting shape to
  adapt, not a drop-in one.
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
