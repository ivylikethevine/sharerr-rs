# Ideas

Candidates that have been thought about but not committed to.

An idea in this project ends up in one of three files. [`ROADMAP.md`](ROADMAP.md)
is what is *intended* — ahead, and meant to happen.
[`UNSUPPORTED.md`](UNSUPPORTED.md) is what has been *declined*, each entry
carrying the reason so a decision does not get re-litigated. This file is the
third state: **considered, not committed**. Nothing here is scheduled, nothing
here is a promise, and the ordering within each section is a judgement about
value rather than a plan.

An entry leaves this file in one of two directions — to `ROADMAP.md` once it is
actually intended, or to `UNSUPPORTED.md` once it is actually declined. Until
then it sits here so that the reasoning is written down once instead of being
re-derived, and so there is one place to check before proposing something that
has already been weighed.

Each entry names the seam it would plug into and is sized the way `ROADMAP.md`
sizes things: by how much it *touches*, not by how long it would take to get
right.

## Table of contents

- [Visualisation and insight](#visualisation-and-insight)
- [Transfer accounting](#transfer-accounting)
- [Integrations and interop](#integrations-and-interop)
- [Features and nice-to-haves](#features-and-nice-to-haves)
- [Already settled elsewhere](#already-settled-elsewhere)

## Visualisation and insight

One constraint shapes every entry below, so it is stated once here rather than
three times: **a chart in this project is server-rendered SVG, not a client
library.** The web UI compiles every asset into the binary and reaches no CDN,
because the container is expected to have no egress. The topology diagram and
the sync-history strip are the working proof that this is enough — the
coordinates of both are computed in Rust (`Node`/`Edge` and `RunBar` in
`crates/sharerr/src/web/templates.rs`) and the templates do no arithmetic at
all, they just place what they were handed. Anything here would follow that
same shape.

The other relevant fact: apart from `sync_runs`, **nothing in this project is
written down over time.** There is no samples table, no counters, no request
log. Two of the three entries below are cheap precisely because they only
re-read rows that already exist; the third needs somewhere to put history.

### Library composition

Migration `0009` added `media_json`, and it is read by exactly two things: the
Torznab renderer and the Jackett renderer. Nothing aggregates it. Neither does
anything aggregate `size` beyond one total on the status tile.

Rolled up — by resolution, by codec, by source, by state — it answers a
question an operator currently cannot ask without reading the whole items
table: *is what I am sharing what I think I am sharing?* A library that is
quietly 80% 720p, or one where a third of the rows are `failed`, is not visible
today until you go looking for it.

Reads rows that already exist. The work is the aggregation query and the
drawing, not the data.

**Small–medium** — a store query, a stat block, and the bars.

### Swarm history

`Swarms` (`crates/sharerr-torrent/src/announce.rs`) is deliberately in-memory:
it is rebuilt within one announce interval, so persisting it for correctness
would be pointless. But that also means the "Swarms" stat tile can only ever
say *right now*, and a restart erases the only record that anyone was ever
connected. "Nobody is in the swarm at the moment" and "nobody has been in the
swarm for a fortnight" are very different facts about a sharing tool, and today
they render identically.

A periodic sampler writing `SwarmStats` — and per-torrent complete/incomplete —
into a small table would separate them. Retention has an established shape to
copy: `peer_endpoints` prunes to a handful of newest rows per key on insert
rather than growing without bound or needing a sweeper.

**Medium** — a migration, a sampling loop alongside the existing background
loops, and the rendering.

### A per-item detail page

`/items` is a wide table with no drill-down, so everything about one item has to
fit in a row or be omitted. A detail page would mostly be re-composition rather
than new work — the path chain is already computed by `checks.rs`, the swarm by
`Swarms`, the scope match by the same predicate the feed uses.

The entry worth naming on its own is **release title against torrent name, side
by side**. Conflating those two strings is the first trap `CLAUDE.md` lists,
it stalls seeding at 0%, and there is currently no view anywhere in the product
that shows both at once. A page that does would make the distinction legible
instead of folkloric.

The rest of the page: media metadata, which friends can see it, token status,
current swarm, and the full `last_error` rather than a truncated cell.

**Medium** — one route, one template, several existing gatherers re-composed.

## Transfer accounting

The largest gap between what sharerr *knows* and what it *keeps*, and the entry
with the most caveats attached — which is why it is written out at length
rather than as a bullet.

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
pairs naturally with the detail page above. And, with the metrics endpoint
below, per-peer counters for anyone who would rather graph it elsewhere.

**Large** — a migration, a change to the announce parser in `sharerr-torrent`,
an accumulator and flush loop, and the UI on top.

## Integrations and interop

### A metrics endpoint

`ops_router()` in `crates/sharerr/src/commands/serve.rs` serves `/health` and
`/ready`. There is no `/metrics`, and this is the highest-leverage integration
available for this audience: it hands Grafana every chart this UI will never
ship, for roughly one handler.

Worth exporting: items by state, seeding bytes, sync counters with the last
run's timestamp and duration, swarm totals, peer totals and how many are
active, and the gluetun and lighthouse last-success timestamps — the last two
being exactly the "is the tunnel still up" signal an operator wants alerting on
rather than discovering by reload.

Three things this needs to decide rather than leave open:

- **Authentication.** A bare public `/metrics` reintroduces precisely what
  declining a runtime `/openapi.json` was meant to avoid: an unauthenticated
  endpoint that confirms a sharerr instance exists here, which undoes the
  tracker's and the lighthouse's don't-confirm-existence posture. The
  consistent answer is a bearer token held in the vault and wired through
  `secret_keys::ALL` — the mechanism this project already uses for every other
  credential — with the endpoint off by default.
- **Cardinality.** Per-peer labels are bounded by the friends list and are
  fine. Per-*item* labels are not: a large library would produce a metric per
  file and make the endpoint a liability rather than a feature.
- **Dependency.** Hand-render the text format. This project already hand-writes
  Torznab XML and bencode, both harder; a page of OpenMetrics is in character
  and adds nothing to the dependency tree.

**Medium** — a handler, a vault secret, a config toggle, and its documentation.

### More notification triggers

`crates/sharerr/src/notify.rs` fires on two things: a sync that failed, and a
friend gone quiet. Everything routes through a single `send()`, so adding
triggers is cheap — the cost is not plumbing, it is restraint.

The ones that seem worth having, on the test of *would an operator want to be
told without having to look*: an item newly shared (digested, not one message
per file), an item that failed and why, a new friend's first contact, a friend
revoked, the tracker becoming unreachable, and a `[[library]]` path that has
stopped being readable.

The strongest of them is **the advertised endpoint rotating**. When gluetun
hands over a new IP or forwarded port, every announce URL sharerr publishes
moves with it — that is the single event most likely to break a friend's
downloads while everything on this end still looks healthy.

Anything added here needs a per-trigger enable set under `[notifications]`.
Without one this becomes noise and the operator mutes the whole channel,
including the two triggers that were worth having in the first place.

Also worth recording so it is not mistaken for separate work: an
Uptime-Kuma-style heartbeat push is one more trigger through the same `send()`,
not a feature of its own.

**Medium** — several call sites, one config block, one shared sender unchanged.

### A dashboard-widget JSON endpoint

Homepage, Homarr and Glance are near-universal in this audience, and all three
read a "custom API" JSON endpoint. The `Glance` struct in
`crates/sharerr/src/web/templates.rs` is already exactly that payload — items
shared, size on disk, last sync, friends seen recently, live swarm totals.

So this is one serializer over a struct that exists, behind the same token as
the metrics endpoint, and it is worth framing that way: not a new API surface
to design and maintain, but a second rendering of the status page's own
summary. It stands or falls with the authentication question above; it should
not ship first and invent its own answer.

**Small**, once the metrics endpoint has settled how it is authenticated.

## Features and nice-to-haves

### Manual per-item actions

Discovery is tag-driven end to end, which is the right default and the one
control an operator already understands. But it means there is no way to retry
a single `failed` item, force a torrent to be rebuilt, or stop sharing one file
without going to Sonarr and editing tags. The web UI can see the failure and
can do nothing about it.

A small set of per-item actions would close that loop. The constraint is
absolute and worth restating in any implementation: none of them may move,
rename, re-link, or delete data. "Unshare" means removing the torrent from the
client *without* deleting its files, which `TorrentClient` already distinguishes
because every backend had to answer that question to be supported at all.

**Medium** — routes, store transitions, and the UI affordances.

### Achieved ratio

sharerr sets `ratio_limit` and `upload_limit_kib` at add time and then never
mentions them again. Whether a torrent actually reached its seeding goal is not
visible anywhere, which makes the setting hard to tune — you set a number and
find out nothing.

The honest version shows **what the client reports**, not what sharerr asked
for. That also means the column is partly empty by backend and should say so
rather than render a blank: rTorrent has no per-torrent ratio limit at all (its
enforcement is an `.rtorrent.rc` schedule keyed to a view), so sharerr drops
`ratio_limit` there with a warning. A column that quietly showed nothing for
one of three supported clients would read as a bug.

**Small–medium** — depends on whether `TorrentClient::list` already carries the
field or needs widening across three backends.

### Auto-refreshing status tiles

There is no live view anywhere: no SSE, no websockets, and although htmx is
vendored it is used only by the seven click-to-test buttons — there is not one
`hx-trigger` in the tree. Every number updates only on reload.

The constraint is the interesting part. `status_page` runs live probes of every
*arr app on each load, so polling the *page* would be expensive and would put
real traffic on the *arr apps for a number nobody is watching. Polling a
*fragment* containing only the stat tiles would not. That is the smallest
honest step toward a live view, and it needs the tiles split out of the page
handler first.

**Small–medium** — a fragment route, and the tiles extracted from `status_page`.

### Config backup and restore

Master-key loss is unrecoverable by design, and the vault is doing exactly what
it should. What is missing is the *other* half: a way to capture the
configuration — sources, mappings, peers, scopes — so that rebuilding an
instance does not mean retyping everything from screenshots.

Secrets stay out of any export, and that is the point rather than a limitation:
an export containing recoverable credentials would be a plaintext copy of the
vault, which is the thing the vault exists to prevent. A restore path therefore
ends with re-entering secrets, and the documentation should say so plainly
instead of leaving it to be discovered.

**Medium** — a serializer, a restore path, and its documentation.

### Request flow

Already the one feature-sized item on [`ROADMAP.md`](ROADMAP.md), where the
reasoning lives. Noted here only so a reader working through this list does not
conclude it was overlooked.

## Already settled elsewhere

Several directions that look open from here are not, and the reasoning is
written down rather than absent. Before adding an entry above, check that it is
not one of these.

[`SUPPORTED.md`](SUPPORTED.md) records that no further **library source** and no
further **indexer** work is currently planned — both categories have a stated
extension seam, and the seam existing is not the same as the work being wanted.

[`UNSUPPORTED.md`](UNSUPPORTED.md) declines, each with its reason:
**media-server library sources** (Jellyfin, Emby, Plex — tried and removed),
**Readarr as a direct indexer**, **Transmission-compatible forks** as their own
tier-2 target, **qBittorrent's embedded tracker** as a second tracker backend,
and **publishing to crates.io**.
