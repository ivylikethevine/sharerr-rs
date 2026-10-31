# Roadmap

Where sharerr is going next. Nothing below is a release commitment; the
ordering is a judgement about value, not a schedule. An item is removed the
moment it ships. An idea that gets declined instead moves to
[`docs/COMPATIBILITY.md`](COMPATIBILITY.md#not-supported), with the reason
attached.

## Open work

Smallest first, by how much each item touches:

- **Mutation survivors past `sharerr-torrent`.** `scripts/check.sh mutants`
  exists and `sharerr-torrent`'s survivors worth killing are killed.
  `sharerr-core` has had one package-scoped run (46 survivors, many likely
  caught by the `sharerr` crate's tests) and the rest of the workspace none;
  see [`docs/TESTING.md`](TESTING.md#mutation-testing).
- **Unraid and TrueNAS app listings.** Where much of the *arr audience
  installs from; a Community Applications template and a TrueNAS app.
- **A Prowlarr catalog listing.** A per-host definition friends install by
  hand exists ([`docs/prowlarr/`](prowlarr/README.md)); one listing in
  Prowlarr's own catalog, so a friend picks "sharerr" and types one key, is
  blocked upstream until Prowlarr accepts a custom base URL for Cardigann
  indexers. That doc says why.
- **A public lighthouse.** The software and a Linode module to host it
  ([`docker/deploy/lighthouse/terraform/linode/`](https://github.com/ivylikethevine/sharerr-rs/tree/main/docker/deploy/lighthouse/terraform/linode))
  are done; what is missing is a running public instance, with its URL in
  [`docs/LIGHTHOUSE.md`](LIGHTHOUSE.md), for a friend group that would
  rather not run their own.
- **Per-friend tags and quality scope.** A tag such as `sharerr-alice`
  shares one item with one friend, the control *arr users already reach
  for; and a scope by resolution or size cap, from the media metadata the
  feed already carries, for a friend on a metered line.
- **Settings in sections.** One page shown in four screenshots is the tell:
  a sticky section nav, per-section save, and an unsaved-changes guard.
- **Deluge as a fourth client.** The main client still missing; its Web
  JSON-RPC API covers add-without-moving, labels and per-torrent limits.
- **Sidecar files.** The directory source skips subtitles and `.nfo` files
  on purpose. An opt-in multi-file torrent over an episode and its subtitle
  files, still moving nothing, is narrower than season packs below, and
  builds the multi-file torrent they also need.
- **Season packs and albums as one torrent.** One file per torrent means a
  friend's season search returns single episodes; a multi-file torrent over
  the existing folder still moves nothing.
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
- **Keep or delete the scratch scripts nothing names.** No tracked file names
  `scratch/uncovered.sh` or `scratch/ui_probe.mjs`, and both last changed on
  2026-10-17. Each header says what it does; delete what is no longer wanted.
