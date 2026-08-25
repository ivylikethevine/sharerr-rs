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
docker network create --subnet 172.31.90.0/24 sharerr-shared
```

The subnet is not decorative: both gluetuns list `172.31.90.0/24` in
`FIREWALL_OUTBOUND_SUBNETS`, so a network Docker numbered on its own would be
one the killswitch refuses to route.

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
docker network create --subnet 172.31.90.0/24 sharerr-shared

cd media && cp .env.example .env && $EDITOR .env
docker compose up -d
cd ..

cd sharerr && cp .env.example .env && $EDITOR .env config/sharerr.toml
docker compose up -d
```

Each `.env` configures its own VPN credentials — they are unrelated tunnels
and there is no reason they would share a provider, let alone an exit. Both
share one `MEDIA_PATH`: sharerr has to see the very files qBittorrent seeds,
just mounted at `/media` instead of `/downloads`. `sharerr/.env` also carries
`SHARERR_MASTER_KEY`; the credentials themselves (qBittorrent's API key, the
*arr keys) go into the vault through Settings or `sharerr vault set`, never
into `config/sharerr.toml`.

sharerr is published on `127.0.0.1:8477` by its gluetun, and its `/health`
healthcheck is what keeps the container up — an unpopulated vault answers 503
on `/ready` by design and must not restart the container out from under you.

## What this changes about addresses

In the single-stack layout `tracker.advertised_host` and qBittorrent's
inbound peer port move together, because one gluetun forwards both. Here they
don't: `media/.env`'s forwarded port is qBittorrent's BitTorrent listening
port, and `sharerr/.env`'s is where friends reach the tracker and feed over
HTTP. Either tunnel can reconnect and get handed a new exit or a new forwarded
port without the other noticing, and the two exits are never the same address.

sharerr can track the two addresses independently, with two separate pollers:
`[gluetun]` for the tracker/feed address, and `[gluetun_client]` for the torrent
client's own. The shipped `sharerr/config/sharerr.toml` enables only the first,
pointed at `http://localhost:8000` — sharerr shares its gluetun's namespace, so
that is this stack's own control server — which keeps the tracker/feed address
correct. To have sharerr also follow qBittorrent's exit and forwarded port, add
a `[gluetun_client]` section with `control_url = "http://media-gluetun:8000"`,
reachable over `sharerr-shared` the same way the WebUI is; it is off
(`control_url` unset) by default.

gluetun `v3.40` gates its control server routes behind an auth config, so each
poller has a matching vault key — `gluetun.api_key` and
`gluetun_client.api_key` — that Settings manages; sharerr names the missing one
in its error when the control server answers 401.
