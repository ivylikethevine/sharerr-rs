# Roadmap

Where sharerr is going next. Nothing below is a release commitment; the
ordering is by size, not a schedule. An item is removed the moment it ships.
An idea that gets declined instead moves to
[`docs/COMPATIBILITY.md`](COMPATIBILITY.md#not-supported), with the reason
attached.

## Open work

Smallest first, by how much each item touches, with what is blocked upstream
last:

- **Mutation survivors past `sharerr-torrent`.** `scripts/check.sh mutants`
  exists and `sharerr-torrent`'s survivors worth killing are killed.
  `sharerr-core` has had one package-scoped run (46 survivors, not re-measured
  since; many are likely caught by the `sharerr` crate's tests, which the
  `--test-workspace` attempt did not settle) and the rest of the workspace
  none; see [`docs/TESTING.md`](TESTING.md#mutation-testing).
- **Simplifications that would change behaviour.** Each saves work but needs a
  decision first: cache the webhook and heartbeat URLs on `ServeState` (a fresh
  Argon2 derivation each use today, but a `sharerr vault set` against a running
  process would then go unseen until the next save); hold a lock across web-started
  sync passes so they cannot overlap the loop's (Retry and Rebuild would wait for a
  running pass); skip re-reading a cached `.torrent` already confirmed against the
  current announce (a deleted cache file would go unnoticed); batch rTorrent's
  `throttle.up` and `d.throttle_name.set` (a refused throttle would still be
  assigned).
- **One announce fingerprint per sync pass.** `Syncer::share` recomputes the same
  SHA-256 per item; passing it down would take that function past clippy's argument
  limit, so it needs the parameters grouped first.
- **Unraid and TrueNAS app listings.** Where much of the *arr audience
  installs from; a Community Applications template and a TrueNAS app.
- **A public lighthouse.** The software and a Linode module to host it
  ([`docker/deploy/lighthouse/terraform/linode/`](https://github.com/ivylikethevine/sharerr-rs/tree/main/docker/deploy/lighthouse/terraform/linode))
  are done; what is missing is a running public instance, with its URL in
  [`docs/LIGHTHOUSE.md`](LIGHTHOUSE.md), for a friend group that would
  rather not run their own.
- **Season packs and albums as one torrent.** One media file per torrent means
  a friend's season search returns single episodes; a multi-file torrent over
  the existing folder still moves nothing, and `seeding.sidecars` already builds
  one.
- **Hybrid v1/v2 torrents (BEP 52).** Weighed but low value while
  Transmission's and rTorrent's v2 support stays partial.
- **Adopting a friend's torrent.** When a friend's Sonarr imports a release
  and they tag it, their sharerr builds a second info hash for the same data.
  Gossip carrying info hash → item would let the second instance adopt the
  original torrent, so both seed one swarm.
- **Request flow.** Discovery is one-way today. An inbound request queue
  with an approve step is the other half of the original idea. Seerr (the
  merged Overseerr and Jellyseerr) is the request app most friends already
  run, so its request model is the one to fit.
- **A Prowlarr catalog listing.** Blocked upstream. A per-host definition
  friends install by hand exists ([`docs/prowlarr/`](prowlarr/README.md)); one
  listing in Prowlarr's own catalog, so a friend picks "sharerr" and types one
  key, needs Prowlarr to accept a custom base URL for Cardigann indexers. That
  doc says why.
