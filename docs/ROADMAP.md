# Roadmap

Where sharerr is going next. Nothing below is a release commitment; the
ordering is by size, not a schedule. An item is removed the moment it ships.
An idea that gets declined instead moves to
[`docs/COMPATIBILITY.md`](COMPATIBILITY.md#not-supported), with the reason
attached.

## Open work

Smallest first, by how much each item touches, with what is blocked upstream
last:

- **IPv6 link-local and IPv4-mapped addresses in `is_private_ip`.** Its doc
  says link-local, but the IPv6 branch covers only loopback and `fc00::/7`, so
  `fe80::/10` reads as public; and it never canonicalises, so with
  `server.bind = "[::]:8477"` an IPv4 neighbour arrives as `::ffff:a.b.c.d`
  and reads as public too. Three callers would change: the gluetun refresh
  hook (`commands/serve.rs`'s `refuse_public`) would admit those neighbours,
  the advertised-host check in `web/settings` would refuse a typed `fe80::`
  address, and an announce from `fe80::` would take the peer's declared IP.
  Widens who may call the local hooks, so it needs a decision first; the code
  is a line plus a test case.
- **A public lighthouse.** The software and a Linode module to host it
  ([`docker/deploy/lighthouse/terraform/linode/`](https://github.com/ivylikethevine/sharerr-rs/tree/main/docker/deploy/lighthouse/terraform/linode))
  are done; what is missing is a running public instance, with its URL in
  [`docs/LIGHTHOUSE.md`](LIGHTHOUSE.md), for a friend group that would
  rather not run their own.
- **Unraid and TrueNAS app listings.** Where much of the \*arr audience
  installs from; a Community Applications template and a TrueNAS app.
- **Mutation survivors in `sharerr`, in phases.** `scripts/check.sh mutants`
  exists, and every other crate has had its survivors worth killing killed
  (what is left and why: [`docs/TESTING.md`](TESTING.md#mutation-testing)).
  The `sharerr` crate is below, cut so each phase is a sitting of about 30
  minutes at `-j 4` (16CPU/60GB). Counts are from
  `cargo mutants --list --workspace` on 2026-11-04, before joining a friend's
  swarm landed: `catalog.rs` is new since, and `sync/seed.rs` and
  `sync/mod.rs` have grown, so phase 2's counts and phase 4's total are low.
  Re-list a phase before starting it. Measured rates run from 0.2 seconds a
  mutant (`sharerr-core`) through 0.45 (the service crates) to 2.9
  (`sharerr-store`, whose suite opens SQLite per test); the `sharerr` crate's
  is not measured, so phase 1 times itself and the later phases are re-cut
  from what it finds. Each phase records its survivors in `docs/TESTING.md`
  and kills only the ones whose behaviour matters; `MUTANTS_ARGS` takes `-p`
  for a crate, `--file` for a file, and `--iterate` to re-test only the last
  run's survivors after adding tests.
  1. `sharerr`, calibration: `web/auth.rs` (100) and `totp.rs` (95) by
     `--file`. Note the wall time and the per-mutant cost, then re-cut phases
     2 to 6 to 30 minutes at that cost before starting them.
  2. `sharerr`, sync: `sync/mod.rs` (122), `sync/seed.rs` (50), `library.rs`
     (83), `tracker.rs` (37); `sync/seed.rs` has grown by about half since
     (1,733 to 2,681 lines).
  3. `sharerr`, items and settings: `web/items/mod.rs` (163),
     `web/settings/mod.rs` (134), `web/config_io/mod.rs` (52).
  4. `sharerr`, friends: `web/topology/mod.rs` (203), `web/peers.rs` (121),
     `gossip.rs` (48), `pairing.rs` (33), `catalog.rs` (not counted).
  5. `sharerr`, feeds and diagnostics: `torznab/mod.rs` (116),
     `web/diagnostics.rs` (79), `web/composition.rs` (55), `checks.rs` (58),
     `api.rs` (27), `jackett.rs` (12).
  6. `sharerr`, the rest: `commands/doctor/mod.rs` (91), `state.rs` (82),
     `commands/preview.rs` (71), `notify.rs` (61), `commands/serve.rs` (54),
     `metrics.rs` (43), `gluetun.rs` (33), and the files under 35 mutants each
     (about 330 together).
- **Season packs and albums as one torrent.** One media file per torrent means
  a friend's season search returns single episodes; a multi-file torrent over
  the existing folder still moves nothing, and `seeding.sidecars` already builds
  one. `seeding.hybrid` and joining a friend's swarm take single-file torrents
  only, so each needs its multi-file case too.
- **Request flow.** Discovery is one-way today. An inbound request queue
  with an approve step is the other half of the original idea. Seerr (the
  merged Overseerr and Jellyseerr) is the request app most friends already
  run, so its request model is the one to fit.
- **A Prowlarr catalog listing.** Blocked upstream. A per-host definition
  friends install by hand exists ([`docs/prowlarr/`](prowlarr/README.md)); one
  listing in Prowlarr's own catalog, so a friend picks "sharerr" and types one
  key, needs Prowlarr to accept a custom base URL for Cardigann indexers. That
  doc says why.
