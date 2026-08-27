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
- [Closed: the two-instance end-to-end test's last mile](#closed-the-two-instance-end-to-end-tests-last-mile)
- [The lighthouse](#the-lighthouse)

### What's left

One feature-sized item — **[request flow](#functionality)**. The metadata
cluster closed on 2026-08-26: Lidarr and Readarr now carry the `mediaInfo`
they had already computed, `MediaMeta` holds sample rate and bit depth, and a
synthesised music title names the format the file actually is rather than
always claiming FLAC. The same day closed its last two pieces: an audio
backend for `sharerr-probe` (`symphonia`, metadata-only like its MKV and
ISO-BMFF siblings) now covers the directory-sourced music the *arr-managed
path never needed to reach, and achieved ratio gives the items page what each
torrent client itself reports for a torrent's ratio and per-torrent limit,
rather than only ever showing what sharerr asked for at add time. The
two-instance end-to-end test closed on 2026-08-27
(`./run_docker_tests_two_instance.sh`, see `docker/README.md`'s "The
two-instance stack") — the byte-for-byte transfer assertion now passes
end to end; see the item's own history below for what the last-mile stall
actually was, since "environment quirk" turned out to be the wrong
diagnosis. The 2026-08-21 code review is otherwise closed out: what is left
of it is a single entry kept only so its documented behaviour reads as a
decision rather than an oversight. Past those, the rest of
[Open work, by scope](#open-work-by-scope) below is ideas that have been
thought through but not all committed to — appearing here means the
reasoning is written down, not that it is scheduled.

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
instead of vanishing), a twentieth on 2026-08-26 closed the media-metadata
cluster along with two candidates promoted out of the ideas list — the
library-composition roll-up and the polled status tiles — and a twenty-first
on 2026-08-27 closed three more candidates outright: the dashboard-widget
JSON endpoint, swarm history, and a metrics endpoint, plus the two-instance
end-to-end test's last mile below. File references are as of the review
commit and may have drifted.

### Small — one function or one file

1. **Dual-token admission on the items page** — `items.rs` `token_status`
   consults only the current fingerprint, so a previous-token item renders
   Stale while the tracker admits it. The doc comment frames that as
   intended; listed so the decision is a decision.

### Medium — a subsystem, or one shape repeated across several files

2. **A per-item detail page** — `/items` is a wide table with no drill-down,
   so everything about one item has to fit in a row or be omitted. A detail
   page would mostly be re-composition rather than new work — the path chain
   is already computed by `checks.rs`, the swarm by `Swarms`, the scope match
   by the same predicate the feed uses. The entry worth naming on its own is
   **release title against torrent name, side by side**. Conflating those two
   strings is the first trap `CLAUDE.md` lists, it stalls seeding at 0%, and
   there is currently no view anywhere in the product that shows both at
   once. A page that does would make the distinction legible instead of
   folkloric. The rest of the page: media metadata, which friends can see it,
   token status, current swarm, and the full `last_error` rather than a
   truncated cell.

3. **More notification triggers** — `crates/sharerr/src/notify.rs` fires on
   two things: a sync that failed, and a friend gone quiet. Everything routes
   through a single `send()`, so adding triggers is cheap — the cost is not
   plumbing, it is restraint. The ones that seem worth having, on the test of
   *would an operator want to be told without having to look*: an item newly
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

4. **Manual per-item actions** — discovery is tag-driven end to end, which is
   the right default and the one control an operator already understands. But
   it means there is no way to retry a single `failed` item, force a torrent
   to be rebuilt, or stop sharing one file without going to Sonarr and editing
   tags. The web UI can see the failure and can do nothing about it. A small
   set of per-item actions would close that loop. The constraint is absolute
   and worth restating in any implementation: none of them may move, rename,
   re-link, or delete data. "Unshare" means removing the torrent from the
   client *without* deleting its files, which `TorrentClient` already
   distinguishes because every backend had to answer that question to be
   supported at all.

5. **Config backup and restore** — master-key loss is unrecoverable by
   design, and the vault is doing exactly what it should. What is missing is
   the *other* half: a way to capture the configuration — sources, mappings,
   peers, scopes — so that rebuilding an instance does not mean retyping
   everything from screenshots. Secrets stay out of any export, and that is
   the point rather than a limitation: an export containing recoverable
   credentials would be a plaintext copy of the vault, which is the thing the
   vault exists to prevent. A restore path therefore ends with re-entering
   secrets, and the documentation should say so plainly instead of leaving
   it to be discovered.

### Large — a protocol, a data model, or a release process

6. **Transfer accounting** — the largest gap between what sharerr *knows*
   and what it *keeps*; see [Transfer accounting](#transfer-accounting) below
   for the full write-up, including the caveats that matter before building
   it.

7. **Request flow** — a new inbound request queue and approve step, touching
   the sync engine and the web UI on both sides of a friendship; see
   [Functionality](#functionality).

---

## Closed: the two-instance end-to-end test's last mile

Closed on 2026-08-27, and written out at length because the diagnosis
changed twice before landing on the real defect — worth keeping so the same
wrong turn is not taken again.

`docker/compose.two-instance.yml`, `run_docker_tests_two_instance.sh`, and
`crates/sharerr/tests/e2e_two_instance.rs` had the whole chain up to the
actual file transfer independently verified against a real stack: sharerr's
tracker returns a byte-correct bencoded peer list, a hand-fed `.torrent`
transfers between the two containers' qBittorrents instantly and
byte-perfectly, and Radarr-B's real automatic search finds, grabs, and hands
the release to its download client. What did not complete was the BitTorrent
transfer itself: qBittorrent-B connected to qBittorrent-A (confirmed via
packet capture — a real TCP handshake, BT handshake, and extended handshake
all completed), but the `ut_metadata` (BEP9) exchange that would hand it the
actual torrent metadata never finished — qBittorrent-A's only reply was a
bare 5-byte control message, never a metadata piece.

That was first suspected to be an environment-specific quirk of the
sandboxed build environment, since every other explanation had been ruled
out by direct testing: not the tracker, not plain connectivity, not
encryption, protocol, DHT/PEX/LSD, or upload-slot configuration. It was not
an environment quirk, and re-running on a plain Docker host would have
reproduced it identically — the real cause was structural. Every torrent
sharerr builds is private (`sharerr-torrent/src/factory.rs`'s
`set_privacy(true)`, correctly — that is the whole reason the tracker
exists), and libtorrent does not run `ut_metadata` for a private torrent.
Radarr-B's own qBittorrent record proved which path it had taken:
`has_metadata: false`, and a `magnet_uri` whose `dn=` was the release title
rather than the torrent's real internal filename — meaning Radarr-B added
the release by magnet, not by the `.torrent` enclosure sharerr also
advertises and which fetched perfectly the whole time. A magnet can never
complete against a private torrent, in any environment: nothing in the
swarm will ever answer its metadata request.

Radarr's own direct Torznab client has no setting to prefer the `.torrent`
over the magnet. The one place that preference is actually configurable is
Prowlarr's per-indexer "Prefer Magnet URL" — `false`, i.e. prefer the
`.torrent`, by default. The fix was to put a Prowlarr in front of
instance B's Radarr, the way a real friend's setup should look when their
automation supports it: the two-instance stack gained a third container,
`run_docker_tests_two_instance.sh` now creates the Torznab indexer on
Prowlarr with `torrentBaseSettings.preferMagnetUrl` explicitly pinned to
`false` and confirms the pin took, adds Radarr-B to Prowlarr as a `fullSync`
application, and waits for the indexer to sync down before triggering the
search. Both schemas (indexer and application) are fetched from Prowlarr's
own `/api/v1/*/schema` endpoints rather than hand-typed, so a Prowlarr
upgrade that renames or reorders fields cannot silently stop this from
pinning the one setting it exists to pin. With that in place, the byte-for-byte
transfer assertion in `e2e_two_instance.rs` passes end to end.

What this does not change: sharerr's own Torznab feed still advertises a
`magneturl` alongside the `.torrent` enclosure, for the indexers and clients
where a magnet is the only thing that ever mattered. A friend connecting
Radarr or Sonarr *directly* to a sharerr feed, with no Prowlarr in front,
still has no lever over this — their app decides magnet-or-`.torrent` on its
own, and evidence from this investigation is that at least one popular one
decides wrong for a private torrent. That is a real gap for a direct
connection, tracked in [`UNSUPPORTED.md`](UNSUPPORTED.md) rather than left
implicit, since it was weighed and not acted on here: removing the magnet
entirely would close it for every consumer at the cost of the convenience
Prowlarr-routed and DHT-capable consumers get from it today.

---

## Transfer accounting

The largest gap between what sharerr *knows* and what it *keeps*, and the
entry with the most caveats attached — which is why it is written out at
length rather than as a bullet.

Every BitTorrent client sends `uploaded` and `downloaded` on every announce.
`AnnounceRequest` parses `left` and drops both. And because announce URLs carry
a per-friend token, `authenticate_token` in `crates/sharerr/src/tracker.rs` has
*already resolved which friend this is* by the time the request is handled — a
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
  otherwise. Nothing should ever be *gated* on these numbers.
- **The counters reset.** They are cumulative per client session, so a restart
  or a re-add sends them back to zero. Accounting therefore records monotonic
  deltas and treats a *decrease* as a new session counted from zero, not as
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
(peer, info hash) answer *who has pulled what*; time-bucketed samples answer
*when*, and are what any chart would read. The second is optional and can
follow the first — the totals are useful on their own, and are the cheaper
half.

### What it unlocks

A "served" column on the friends table, so a friend who was added and never
actually used the feed is visible as such. A bytes-out figure on the status
page that means something. A per-item panel answering who pulled this, which
pairs naturally with the [per-item detail page](#open-work-by-scope) above.
And, with the [metrics endpoint](#open-work-by-scope) above, per-peer counters
for anyone who would rather graph it elsewhere.

This needs a migration, a change to the announce parser in `sharerr-torrent`,
an accumulator and flush loop, and the UI on top.

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
