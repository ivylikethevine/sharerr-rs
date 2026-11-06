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
  exists, and every other crate, plus `sharerr`'s `web/auth.rs` and
  `totp.rs`, has had its survivors worth killing killed (what is left and
  why: [`docs/TESTING.md`](TESTING.md#mutation-testing)). A `sharerr` mutant
  costs about 19 seconds at `-j 4` (16CPU/60GB), because each one runs the
  crate's whole suite, so a 30-minute sitting covers about 95 mutants. The
  rest of the crate is 2,173 mutants, about 11.5 hours or 23 sittings, below
  by phase with counts from `cargo mutants --list -p sharerr` on 2026-11-06
  (they move with the code; re-list a phase before starting it, then split
  it by file into sittings of about 95). Each phase records its survivors in
  `docs/TESTING.md` and kills only the ones whose behaviour matters;
  `MUTANTS_ARGS` takes `-p` for a crate, `--file` for a file, and `--iterate`
  to re-test only the last run's survivors after adding tests.
  1. Sync, 340 (4 sittings): `sync/mod.rs` (128), `sync/seed.rs` (92),
     `library.rs` (83), `tracker.rs` (37).
  2. Items and settings, 350 (4): `web/items/mod.rs` (164),
     `web/settings/mod.rs` (134), `web/config_io/mod.rs` (52).
  3. Friends, 415 (5): `web/topology/mod.rs` (203), `web/peers.rs` (121),
     `gossip.rs` (48), `pairing.rs` (33), `catalog.rs` (10).
  4. Feeds and diagnostics, 350 (4): `torznab/mod.rs` (119),
     `web/diagnostics.rs` (79), `checks.rs` (58), `web/composition.rs` (55),
     `api.rs` (27), `jackett.rs` (12).
  5. Commands and state, 435 (5): `commands/doctor/mod.rs` (91), `state.rs`
     (82), `commands/preview.rs` (71), `notify.rs` (61), `commands/serve.rs`
     (54), `metrics.rs` (43), `gluetun.rs` (33).
  6. The rest, 283 (3): the 21 files under 35 mutants each.
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
