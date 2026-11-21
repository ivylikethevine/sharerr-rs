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

As of 2026-11-20, branch `dev`, two commits past v0.2.5.

- The repo-wide simplification is finished: v0.2.5 shipped all but one of its
  items, and the last, cutting the workflow and Dockerfile headers to the
  why, is in the tree and passes `scripts/check.sh --fast`. Four items that
  change behaviour wait under On hold (10 to 13).
- The public lighthouse is live at `https://lighthouse.edenlabs.dev` (its
  health route answered on 2026-11-20) and is named in `docs/LIGHTHOUSE.md`
  and the README.
- The theme toggle's empty phone-width state is fixed (the toggle is exempt
  from the 32rem rule hiding bar glyphs); it has not been seen in a narrow
  preview.
- The dev profile builds dependencies with line tables only, and the coverage
  build runs without an incremental cache, to cut the size of `target/`.
  Neither the disk saving nor the effect on build and mutant times (5) is
  measured.
- Open questions: who may call the local hooks (6), and the four held-back
  simplifications (10 to 13).

## Order of work

- The items under Now: 4, which publishes outside the repo and so starts on
  the maintainer's word, and 5 a phase at a time.
- 6 and 10 to 13 once their decisions are made, and 7 once Prowlarr allows it.
- The items under Later are not scheduled.

## Now

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

Four simplifications held back from the repo-wide pass, since each changes
behaviour; each waits on a decision.

10. **Cache the notify webhook and heartbeat URLs** (`notify.rs`'s
    `stored_url`, and its caller in `commands/serve.rs`). Saves about 1,440
    Argon2 derivations a day, but a URL the `vault` CLI writes from another
    process would then take a restart to apply.
11. **A `light-dark()` palette** in `web/assets/style.css`. Removes about 60
    lines and narrows browser support to May 2024 and later.
12. **Drop `.wide .wide`** in `style.css`. Inert, but its comment defends it as
    documentation.
13. **Dispatch `preview` and `openapi` before the config loads** in
    `main.rs`. Drops the error log on a bad config.

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
