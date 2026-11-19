# Roadmap

<!-- markdownlint-disable-file MD029 -->
<!-- a list after a heading goes on from the number before it -->

Where sharerr is going next. Nothing below is a release commitment; within a
section the ordering is by size, not a schedule. Items are numbered through
the whole file, so a number names one item, and a step of an item is a letter
on its number. An item is removed the moment it ships. An idea that gets
declined instead moves to
[`docs/COMPATIBILITY.md`](COMPATIBILITY.md#not-supported), with the reason
attached.

## Where it stands

As of 2026-11-17, branch `dev`.

- The theme toggle's empty phone-width state is fixed in the tree (the
  toggle is exempt from the 32rem rule hiding bar glyphs); it has not been
  seen in a narrow preview.
- The repo-wide simplification (2) is applied but for one of its 138 items:
  cutting the workflow and Dockerfile headers to the why. Four more are held
  back, since each changes behaviour. The test-helper, compose, CI and docs
  changes of this batch are edited but not yet built or tested; the
  path-filter `not-code` anchor in `prepare.yml` relies on paths-filter
  flattening a YAML alias, which only a CI run proves.
- The dev profile now builds dependencies with line tables only, and the
  coverage build runs without an incremental cache, to cut the size of
  `target/`. Neither the disk saving nor the effect on build and mutant times
  (5) is measured.
- The Linode lighthouse module (3) could not create its instance without an
  ssh key: the provider it pins wants a key or a root password with an
  image. It now generates the password; edited on 2026-11-18, not yet
  validated (`scripts/check.sh terraform`, which also adds the `random`
  provider to the target's lock file) or applied.
- Open questions: who may call the local hooks (6), and the four held-back
  simplifications (2).

## Order of work

- The items under Now, smallest first: 2, 3, 4, then 5 a phase at a time.
  3 and 4 publish outside the repo, so they start on the maintainer's word.
- 6 once its decision is made, and 7 once Prowlarr allows it.
- The items under Later are not scheduled.

## Now

2. **The repo-wide simplification.** Quality only, with no behaviour change:
   its checklist,
   [`docs/plans/repo-simplification.md`](plans/repo-simplification.md), has
   the one open item, the four held back for a decision, and what was decided
   and learned while applying the rest. It is deleted, and this item with it,
   once every item is applied or recorded as held back.
3. **A public lighthouse.** The software and a Linode module to host it
   ([`docker/deploy/lighthouse/terraform/linode/`](https://github.com/ivylikethevine/sharerr-rs/tree/main/docker/deploy/lighthouse/terraform/linode))
   are done; what is missing is a running public instance, with its URL in
   [`docs/LIGHTHOUSE.md`](LIGHTHOUSE.md), for a friend group that would
   rather not run their own.
4. **Unraid and TrueNAS app listings.** Where much of the \*arr audience
   installs from; a Community Applications template and a TrueNAS app.
5. **Mutation survivors in `sharerr`, in phases.** `scripts/check.sh mutants`
   exists, and every other crate, plus `sharerr`'s `web/auth.rs` and
   `totp.rs`, has had its survivors worth killing killed (what is left and
   why: [`docs/TESTING.md`](TESTING.md#mutation-testing)). A `sharerr` mutant
   costs about 19 seconds at `-j 4` (16CPU/60GB), measured before the dev
   profile cut dependencies' debug info, because each one runs the crate's
   whole suite, so a 30-minute sitting covers about 95 mutants. The
   rest of the crate is 2,173 mutants, about 11.5 hours or 23 sittings, below
   by phase with counts from `cargo mutants --list -p sharerr` on 2026-11-06
   (they move with the code; re-list a phase before starting it, then split
   it by file into sittings of about 95). Each phase records its survivors in
   `docs/TESTING.md` and kills only the ones whose behaviour matters;
   `MUTANTS_ARGS` takes `-p` for a crate, `--file` for a file, and `--iterate`
   to re-test only the last run's survivors after adding tests.
   - 5a. Sync, 340 (4 sittings): `sync/mod.rs` (128), `sync/seed.rs` (92),
     `library.rs` (83), `tracker.rs` (37).
   - 5b. Items and settings, 350 (4): `web/items/mod.rs` (164),
     `web/settings/mod.rs` (134), `web/config_io/mod.rs` (52).
   - 5c. Friends, 415 (5): `web/topology/mod.rs` (203), `web/peers.rs` (121),
     `gossip.rs` (48), `pairing.rs` (33), `catalog.rs` (10).
   - 5d. Feeds and diagnostics, 350 (4): `torznab/mod.rs` (119),
     `web/diagnostics.rs` (79), `checks.rs` (58), `web/composition.rs` (55),
     `api.rs` (27), `jackett.rs` (12).
   - 5e. Commands and state, 435 (5): `commands/doctor/mod.rs` (91),
     `state.rs` (82), `commands/preview.rs` (71), `notify.rs` (61),
     `commands/serve.rs` (54), `metrics.rs` (43), `gluetun.rs` (33).
   - 5f. The rest, 283 (3): the 21 files under 35 mutants each.

## On hold

6. **IPv6 link-local and IPv4-mapped addresses in `is_private_ip`.** Waits on
   a decision. Its doc says link-local, but the IPv6 branch covers only
   loopback and `fc00::/7`, so `fe80::/10` reads as public; and it never
   canonicalises, so with `server.bind = "[::]:8477"` an IPv4 neighbour
   arrives as `::ffff:a.b.c.d` and reads as public too. Three callers would
   change: the gluetun refresh hook (`commands/serve.rs`'s `refuse_public`)
   would admit those neighbours, the advertised-host check in `web/settings`
   would refuse a typed `fe80::` address, and an announce from `fe80::` would
   take the peer's declared IP. Widens who may call the local hooks, so it
   needs a decision first; the code is a line plus a test case.
7. **A Prowlarr catalog listing.** Blocked upstream. A per-host definition
   friends install by hand exists ([`docs/prowlarr/`](prowlarr/README.md)); one
   listing in Prowlarr's own catalog, so a friend picks "sharerr" and types one
   key, needs Prowlarr to accept a custom base URL for Cardigann indexers. That
   doc says why.

## Later

8. **Season packs and albums as one torrent.** One media file per torrent means
   a friend's season search returns single episodes; a multi-file torrent over
   the existing folder still moves nothing, and `seeding.sidecars` already builds
   one. `seeding.hybrid` and joining a friend's swarm take single-file torrents
   only, so each needs its multi-file case too.
9. **Request flow.** Discovery is one-way today. An inbound request queue
   with an approve step is the other half of the original idea. Seerr (the
   merged Overseerr and Jellyseerr) is the request app most friends already
   run, so its request model is the one to fit.

## Decisions

A declined idea is recorded in
[`docs/COMPATIBILITY.md`](COMPATIBILITY.md#not-supported) with its reason, and
none is kept here.

## Done

Nothing is listed: an item is removed the moment it ships.
