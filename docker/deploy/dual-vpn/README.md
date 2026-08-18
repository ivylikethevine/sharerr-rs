# Two stacks, two tunnels

The single-stack layout in `docker/deploy/compose.yaml` puts qBittorrent and
sharerr in the *same* gluetun, so they always share one exit IP. This layout is
for the case where that is not what you want — qBittorrent (the thing that
actually moves bytes, and the thing an ISP or a provider's abuse desk cares
about) goes through one VPN, and sharerr (the tracker/feed a friend's Prowlarr
polls, and the thing that has to answer *arr direct pulls) goes through
another. Two subscriptions, two exits, and each can rotate its address on its
own schedule.

Two `docker compose` projects:

- **`media/`** — gluetun, qBittorrent, Sonarr, Radarr. The library lives here.
- **`sharerr/`** — its own gluetun, and sharerr.

## Why they can still talk to each other

Both gluetun containers additionally join one shared bridge network,
`sharerr-shared`, created once before either stack starts:

```bash
docker network create sharerr-shared
```

qBittorrent and sharerr each use `network_mode: service:gluetun`, which shares
their *entire* network stack with the gluetun container they belong to —
including whatever extra networks that gluetun joins. So attaching each
gluetun to `sharerr-shared` is what lets sharerr reach `media-gluetun:8080`
(qBittorrent's WebUI) by name, without either tunnel knowing about the other's
credentials, exit, or provider. Sonarr and Radarr are not behind a gluetun —
they join `sharerr-shared` directly, the same way `docker/deploy/compose.yaml`
already has them sitting outside the VPN.

`sharerr-shared` is deliberately not the path either tunnel's traffic
travels — it only carries the WebUI/API calls between the two stacks, the
same kind of loopback-only traffic the single-stack layout gives up when it
splits qBittorrent and sharerr onto different hosts.

## Bring it up

```bash
docker network create sharerr-shared

cd media && cp .env.example .env && $EDITOR .env
docker compose up -d
cd ..

cd sharerr && cp .env.example .env && $EDITOR .env sharerr.toml
docker compose up -d
```

Each `.env` configures its own VPN credentials — they are unrelated tunnels
and there is no reason they would share a provider, let alone an exit.

## What this changes about addresses

In the single-stack layout `tracker.advertised_host` and qBittorrent's
inbound peer port move together, because one gluetun forwards both. Here they
don't: `media/.env`'s forwarded port is qBittorrent's BitTorrent listening
port, and `sharerr/.env`'s is where friends reach the tracker and feed over
HTTP. Either tunnel can reconnect and get handed a new exit or a new forwarded
port without the other noticing, and the two exits are never the same address.

sharerr tracks its own two addresses independently via a second, separate
poller: `[gluetun]` for the tracker/feed address, and `[gluetun_client]` for
the torrent client's own address. Point `[gluetun].control_url` at
`sharerr`'s own gluetun (`http://sharerr-gluetun:8000` from inside this
stack, or `localhost:8000` since sharerr shares its namespace) — that keeps
the tracker/feed address correct. Point `[gluetun_client].control_url` at
`media-gluetun:8000` — reachable over `sharerr-shared`, the same as
qBittorrent's WebUI — so sharerr also resolves qBittorrent's own forwarded
port and exit, independently and on its own schedule.
