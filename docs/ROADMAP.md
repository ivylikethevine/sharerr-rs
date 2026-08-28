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

The seven below are all from an altitude review of the `[[peers]]`
import/export commit (2026-08-28), each verified against the code but
deliberately left as a note rather than acted on in that pass.

1. **`lenient = Gossip` no longer means "the least trusted rank"** —
   `sharerr-store/src/endpoints.rs`'s `ObservedVia` enum documents its
   lenient-parse fallback as "the *less* trusted rank, which is the safe
   direction for a value a newer version may have written," but adding
   `Restored` below `Lighthouse` made that false: an unrecognised value now
   resolves to something *more* trusted than the actual floor. A one-line fix
   (point `lenient` at `Restored`, update the sentence) whenever this file is
   next touched.

2. **`import_one_peer` re-implements "add a friend" and drops what the first
   implementation learned** — `commands/serve.rs`'s importer stores
   `gossip_url` raw (`.filter(|s| !s.is_empty())` only), where
   `web::peers::set_gossip` parses it with `url::Url::parse` and stores
   `parsed.to_string()`. `Store::set_peer_gossip_url` validates nothing
   itself, so the invariant currently lives in one HTTP handler rather than
   the type or the store method — the importer is a second, silently looser
   entry point. Fix: push URL validation down into
   `Store::set_peer_gossip_url` (or a `GossipUrl` newtype), the way peer-label
   trimming already lives in `Store::create_peer` rather than being redone by
   each caller.

3. **`strip_peers_block` skips two of the three guarantees every other
   config write gets** — `ConfigFile::open` → `clear_peers` → `write_validated`
   never calls `settings::validate` (so a `sharerr.toml` `toml_edit` can
   round-trip but `Config` would reject goes to disk unchecked) and never
   takes `ServeState::lock_config_write`. Harmless only because it runs
   before any listener binds — an invisible ordering precondition, not a
   property of the function. Fix: route through a shared
   `settings::edit_config`-style helper that holds the lock, validates, and
   writes, the way `web/settings.rs`'s `prepare_config`/`commit_config` do for
   every UI-driven save.

4. **A restored peer endpoint's precedence is stamped, not scoped** —
   `Store::record_peer_endpoint`'s upsert is purely temporal
   (`WHERE excluded.observed_at > peer_endpoints.observed_at`), so
   `import_one_peer` has to invent `now_epoch()` for a sighting whose whole
   documented meaning is "no idea how stale this already was"
   (`ObservedVia::Restored`'s own doc comment). That lets a fresh restore's
   guess outrank a genuine `Direct` sighting on the same row. Fix: give
   `ObservedVia` a trust rank and have `record_peer_endpoint` refuse to
   downgrade `via` on an existing row, or accept an `Option<i64>` timestamp so
   a caller with no real one does not have to fabricate it.

5. **`web::peers::export`'s `ExportDocument` spells `[[peers]]` a third
   time** — independently of `Config::peers` and `ConfigFile::clear_peers`'s
   `"peers"` literal, with nothing tying the three together but convention. A
   `#[serde(rename)]` on any of them would silently desync the pair. Fix:
   move a `PeerImportDocument` type (and its `to_toml()`) next to
   `PeerImport` in `sharerr-core::config`, so the write side and the read side
   share one spelling.

6. **`secret_keys::validate_value` is a call-site convention, not an
   enforced one** — its own doc comment claims to be "the checks every path
   that stores a secret must agree on," but has exactly two callers
   (`commands/vault.rs`, `web/settings.rs`); `import_one_peer`'s `vault.put`
   is a third secret-writing path that does not call it. No practical effect
   today (only `TRACKER_TOKEN` has a real shape to check), but the fix that
   makes the doc comment true is moving the check inside `Vault::put` itself
   — `sharerr-store` already depends on `sharerr-core`.

7. **`fresh_password()` lives in a module documented as wiremock-only** —
   `sharerr-testkit::mock`'s own doc comment says it holds "shared wiremock
   scaffolding," but `fresh_password` (added for the CodeQL
   hard-coded-cryptographic-value sweep) is a plain credential generator with
   no HTTP involved. `sharerr-store`'s dev-dependencies now pull in wiremock
   and sqlx for a hex timestamp. Fix: a `sharerr_testkit::secrets` module
   (or similar) that `mock` and any non-HTTP crate can both depend on without
   the mismatch.

### Medium — a subsystem, or one shape repeated across several files

8. **The remaining notification triggers** — `[notifications]` now has a
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

9. **Transfer accounting** — the largest gap between what sharerr _knows_
   and what it _keeps_; see [Transfer accounting](#transfer-accounting) below
   for the full write-up, including the caveats that matter before building
   it.

10. **Request flow** — a new inbound request queue and approve step, touching
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
