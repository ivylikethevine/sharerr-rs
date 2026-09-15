# One bridge, no tunnel

sharerr, qBittorrent, Sonarr and Radarr on one ordinary docker bridge, with
ports forwarded on your router. The simplest layout, and the one to start
from if you are not sure. What every layout needs (the master key, the two
volumes, path mappings) is in [the deploy README](../README.md#what-every-layout-needs);
this page is the walkthrough for this one. `compose.yaml`'s header comments
are the long form of everything here.

## Contents

- [When to pick it](#when-to-pick-it)
- [Bring it up](#bring-it-up)
- [Gotchas](#gotchas)

## When to pick it

Pick it when the exit address is already yours: a VPS, a seedbox, a box with
a static IP or a dynamic-DNS name at home. There is no tunnel to keep
sharerr's advertised address in step with, so `tracker.advertised_host` is
simply true. Pick [`vpn/`](../vpn/README.md) instead if you would rather not
have a home connection in a swarm's peer list, and
[`sidecar/`](../sidecar/README.md) if qBittorrent and the \*arr apps already
run elsewhere. The full comparison is
[the "Which one" table](../README.md#which-one).

## Bring it up

1. Copy the environment file and fill it in:

   ```bash
   cd docker/deploy/direct
   cp .env.example .env
   ```

   - `SHARERR_MASTER_KEY`: generate once with `openssl rand -base64 32` and
     keep it; losing it loses every stored credential.
   - `MEDIA_PATH`: the library on the host. Sonarr and Radarr get its `tv/`
     and `movies/` subdirectories; qBittorrent and sharerr get the whole tree
     read-only, at `/downloads` and `/media`.
   - `PUID`, `PGID`, `TZ`, `RUST_LOG` have working defaults.

2. Edit `config/sharerr.toml`. At minimum set `tracker.advertised_host` to
   the address friends resolve this instance as (it ships as
   `CHANGE-ME.example.com`), and check the two `[[path_map]]` entries match
   your library's layout.

3. Start the stack:

   ```bash
   docker compose up -d
   ```

4. In qBittorrent (5.2 or newer, on `127.0.0.1:8080`), generate an API key
   under Options → Web UI → API key, then store it in sharerr's vault, either
   through Settings in the web UI or:

   ```bash
   printf %s "$KEY" | docker compose exec -T sharerr sharerr vault set qbittorrent.api_key
   ```

   Add the Sonarr and Radarr API keys the same way, through Settings.

5. Check the result:

   ```bash
   docker compose exec sharerr sharerr doctor
   ```

   It checks credentials, reachability, the tag, and each path mapping
   against the files that are actually there.

6. Forward **6881** (qBittorrent's peers) and **8477** (sharerr) on your
   router to the docker host, after reading the first gotcha below.

## Gotchas

- **8477 carries the web UI too.** The tracker, the Torznab feed, the gossip
  endpoints and the login page share one listener, and sharerr serves no
  TLS. Before forwarding it, either put a TLS reverse proxy in front (forward
  443 to it, set `tracker.advertised_url` to the `https://` address, and have
  the proxy set `X-Forwarded-Proto`), or bind a tracker-only listener with
  `tracker.bind`, publish that port in `compose.yaml`, and forward it instead
  while 8477 stays on loopback. [One port, several
  audiences](../README.md#one-port-several-audiences) weighs the options.
- **Service names, not `localhost`.** On a bridge every container reaches
  the others by service name (`http://qbittorrent:8080`,
  `http://sonarr:8989`). The gluetun layouts say `localhost` because their
  containers share a namespace; config copied from one to the other does not
  work.
- **A closed port fails silently.** A closed 6881 looks like a slow swarm; a
  closed 8477 means friends see your feed entries and never download from
  you, with no error anywhere. `doctor` cannot see either from inside; use
  [the reachability check](../../../README.md#checking-that-you-are-actually-reachable)
  from outside.
- **The WebUIs are on loopback on purpose.** qBittorrent (8080), Sonarr (8989)
  and Radarr (7878) are published to `127.0.0.1` only. Reach them over SSH
  forwarding or a reverse proxy.
- **Nothing else goes in `environment:`.** Any `SHARERR_*` variable pins its
  setting, and the Settings page then renders that field disabled and
  discards a save. Configure in `config/sharerr.toml` or the UI.
- **`skip_checking = true` trusts the mappings.** While you are still proving
  the `[[path_map]]` entries right, set it `false` so qBittorrent verifies
  the data instead of seeding a mismatch.
- **Already running Sonarr and Radarr?** Delete both services and point the
  `[sonarr]`/`[radarr]` URLs at wherever they live, or start from
  [`sidecar/`](../sidecar/README.md), which is this stack with everything but
  sharerr removed.
