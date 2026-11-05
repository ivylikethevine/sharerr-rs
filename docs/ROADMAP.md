# Roadmap

Where sharerr is going next. Nothing below is a release commitment; the
ordering is by size, not a schedule. An item is removed the moment it ships.
An idea that gets declined instead moves to
[`docs/COMPATIBILITY.md`](COMPATIBILITY.md#not-supported), with the reason
attached.

## Open work

Smallest first, by how much each item touches, with what is blocked upstream
last:

- **Mutation survivors past `sharerr-torrent`, in phases.** `scripts/check.sh
mutants` exists and `sharerr-torrent`'s survivors worth killing are killed;
  the rest of the workspace is below, cut so each phase is a sitting of about
  30 minutes at `-j 4` (16CPU/60GB). Counts are from
  `cargo mutants --list --workspace` on 2026-11-04 and move with the code. The
  leaf-crate rate is measured (393 `sharerr-torrent` mutants in 3 minutes);
  the `sharerr` crate's is not, so phase 3 times itself and the later phases
  are re-cut from what it finds. Each phase records its survivors in
  [`docs/TESTING.md`](TESTING.md#mutation-testing) and kills only the ones
  whose behaviour matters; `MUTANTS_ARGS` takes `-p` for a crate and `--file`
  for a file.
  1. `sharerr-core` (218) and `sharerr-store` (354): one `-p` run each; the
     store's suite opens SQLite per test, so it is slower than the measured
     rate. `sharerr-core` had one run before (46 survivors, not re-measured).
  2. The service crates in one sitting: `sharerr-arr` (167), `sharerr-rtorrent`
     (113), `sharerr-qbit` (91), `sharerr-client` (81), `sharerr-probe` (68),
     `sharerr-lighthouse` (64), `sharerr-deluge` (43), `sharerr-transmission`
     (38).
  3. `sharerr`, calibration: `web/auth.rs` (100) and `totp.rs` (95) by
     `--file`. Note the wall time and the per-mutant cost, then re-cut phases
     4 to 8 to 30 minutes at that cost before starting them.
  4. `sharerr`, sync: `sync/mod.rs` (122), `sync/seed.rs` (50), `library.rs`
     (83), `tracker.rs` (37).
  5. `sharerr`, items and settings: `web/items/mod.rs` (163),
     `web/settings/mod.rs` (134), `web/config_io/mod.rs` (52).
  6. `sharerr`, friends: `web/topology/mod.rs` (203), `web/peers.rs` (121),
     `gossip.rs` (48), `pairing.rs` (33), and `catalog.rs`, added since the
     count.
  7. `sharerr`, feeds and diagnostics: `torznab/mod.rs` (116),
     `web/diagnostics.rs` (79), `web/composition.rs` (55), `checks.rs` (58),
     `api.rs` (27), `jackett.rs` (12).
  8. `sharerr`, the rest: `commands/doctor/mod.rs` (91), `state.rs` (82),
     `commands/preview.rs` (71), `notify.rs` (61), `commands/serve.rs` (54),
     `metrics.rs` (43), `gluetun.rs` (33), and the files under 35 mutants each
     (about 330 together).
- **A public lighthouse.** The software and a Linode module to host it
  ([`docker/deploy/lighthouse/terraform/linode/`](https://github.com/ivylikethevine/sharerr-rs/tree/main/docker/deploy/lighthouse/terraform/linode))
  are done; what is missing is a running public instance, with its URL in
  [`docs/LIGHTHOUSE.md`](LIGHTHOUSE.md), for a friend group that would
  rather not run their own.
- **Unraid and TrueNAS app listings.** Where much of the \*arr audience
  installs from; a Community Applications template and a TrueNAS app.
- **Season packs and albums as one torrent.** One media file per torrent means
  a friend's season search returns single episodes; a multi-file torrent over
  the existing folder still moves nothing, and `seeding.sidecars` already builds
  one.
- **Request flow.** Discovery is one-way today. An inbound request queue
  with an approve step is the other half of the original idea. Seerr (the
  merged Overseerr and Jellyseerr) is the request app most friends already
  run, so its request model is the one to fit.
- **A Prowlarr catalog listing.** Blocked upstream. A per-host definition
  friends install by hand exists ([`docs/prowlarr/`](prowlarr/README.md)); one
  listing in Prowlarr's own catalog, so a friend picks "sharerr" and types one
  key, needs Prowlarr to accept a custom base URL for Cardigann indexers. That
  doc says why.
