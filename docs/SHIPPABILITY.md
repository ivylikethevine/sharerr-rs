# Shippability

An assessment of what stands between the tree as it is and a first tagged
release with real users — written 2026-08-31, against the state of `main` at
that date. This is a point-in-time judgement, not a policy: items here move
to done, to [the roadmap](ROADMAP.md), or to
[deliberately-not](UNSUPPORTED.md) as they are decided, and a reader a year
from now should trust the tree over this page.

"Ship" here means: tag a `v1`, let `latest` exist, and expect a stranger —
well, a friend of a stranger — to run it from the README alone.

## Table of contents

- [Where the project stands](#where-the-project-stands)
- [What blocks a v1](#what-blocks-a-v1)
- [What early users will ask for first](#what-early-users-will-ask-for-first)
- [Assumptions to watch](#assumptions-to-watch)
- [Bottom line](#bottom-line)

## Where the project stands

Feature-complete against its own brief. The README's
[What works today](../README.md#what-works-today) table has no gaps in it,
and the [roadmap](ROADMAP.md) itself says one feature-sized item remains —
request flow, which is the *second half* of the original design brief, not a
hole in the first half: discovery is one-way today and the README scopes it
that way honestly. Shipping v1 without it is fine.

The code is in better shape than the `0.1.0` version number suggests: twelve
crates, no production `TODO`s, near-total per-file test coverage across two
[documented tiers](TESTING.md), and the documentation already does the work
most projects only do after their first angry issue — [`UNSUPPORTED.md`](UNSUPPORTED.md)
and [`SECURITY.md`](SECURITY.md)'s by-design list pre-empt the predictable
criticism with reasons attached.

What has never happened, even once, is everything after the code.

## What blocks a v1

None of these are features. In rough order of how much they would embarrass
a first release:

**The release path has never executed.** [`RELEASING.md`](RELEASING.md) says
so itself. There are no tags, `latest` does not exist, and the `v*` →
build → approval-gated publish pipeline is specified but unexercised.
Rehearse it end to end — both images, `docker.yml` and
`docker-lighthouse.yml` — before the tag that counts. And verify in the
repository settings that the `release` environment actually has a required
reviewer: that gate is a GitHub Settings fact no workflow file can assert,
and without it the publish step runs unattended.

**No user has ever crossed a migration.** Eleven forward-only sqlx
migrations exist and every one has only ever run against a fresh database.
Before v1, rehearse one real upgrade: an older image, a populated `/data`,
then the new image over it. Forward-only with no downgrade path is a fine
policy — but it should be a tested policy.

**The magnet link is a silent hang for exactly the v1 user.** Every torrent
sharerr builds is private, so a magnet can never resolve — and
[`UNSUPPORTED.md`](UNSUPPORTED.md)'s own two-instance test confirmed Radarr's
direct Torznab client picks the magnet over the working `.torrent` and stalls
forever. The current position is to keep `magneturl` in the feed and pull it
"the moment a real report shows it biting". For v1 that calculus changes: the
target user is a friend pointing an *arr app at the feed, the failure is a
hang rather than an error, and the first report will cost a debugging session
on both sides of the friendship. Reconsider before tagging, not after.

**Backup needs foresight the user will not have.** Config export covers the
effective `sharerr.toml` and nothing in the vault or the peers table; the
[`[[peers]]` export block](CONFIGURATION.md) must be downloaded *before* the
loss it protects against; losing `/data` means re-keying every friendship and
losing `SHARERR_MASTER_KEY` is unrecoverable by design. All defensible — but
a short runbook (volume snapshot or `sqlite3 .backup`, plus "export your
peers block now, not later") would close most of the practical gap for the
cost of a documentation page.

## What early users will ask for first

Predictions, so hold them loosely — but each traces to something real:

- **Transfer accounting.** Every announce already carries `uploaded` and
  `downloaded`; sharerr parses `left` and drops the rest. "How much has Sam
  actually pulled?" is the first question a seeding operator asks. The
  [roadmap's write-up](ROADMAP.md#transfer-accounting) has the caveats.
- **An easier lighthouse.** The rendezvous-of-last-resort requires the
  friend group to stand up a third box on a stable address — which
  undermines the "friend whose IP rotated while unwatched" story at exactly
  the scale sharerr targets. A public instance, or a one-liner deploy, would
  change the adoption math. See [`LIGHTHOUSE.md`](LIGHTHOUSE.md).
- **Login rate limiting.** [`SECURITY.md`](SECURITY.md) lists its absence as
  by-design, while the deploy docs present "forward 8477 as it is" as a
  workable option. Those two positions are individually fine and jointly
  uncomfortable; one of them should move.
- **Seeding limits that apply retroactively.** The upload cap and ratio goal
  bind at add time only. A user who discovers their link saturated will
  change the setting and watch nothing happen.
- **Request flow**, eventually — and **multi-user**, eventually (the `users`
  table exists; only the first-run claim ever writes to it).

## Assumptions to watch

Known, documented, and mostly mitigated — listed because they shape the
support load rather than because anything here is news:

- **Path mapping stays the top failure mode.** The deploy README says as
  much. `doctor --suggest-paths` and the diagnostics page are good
  mitigations, not eliminations.
- **Reachability is unverifiable from inside.** No UPnP or NAT-PMP, on
  purpose; a closed port and a quiet swarm look identical from within, and
  `doctor` cannot tell them apart. The `/debug` script exists precisely
  because only an outside vantage can.
- **Peer API keys travel in query strings.** Consistent with the stated
  threat model, but they will land in any reverse-proxy access log in front
  of an instance — worth a line in [`SECURITY.md`](SECURITY.md)'s by-design
  list, where it currently is not.
- **A cached `.torrent` outlives the file it described.** `reuse_cached` in
  `crates/sharerr/src/sync/seed.rs` reuses by info hash with no size or
  mtime check against the file on disk, so an in-place upgrade under the
  same `file_id` seeds a stale hash until someone notices and clicks Force
  rebuild. The one library-divergence case with no automatic detection.

## Bottom line

The distance to v1 is operational, not architectural: rehearse the release,
rehearse an upgrade, decide the magnet question deliberately, and write the
backup runbook. The code has been ready for longer than the process has.
