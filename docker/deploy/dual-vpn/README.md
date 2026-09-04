# Two stacks, two tunnels

The single-stack layout in `docker/deploy/vpn/` puts qBittorrent and sharerr
in the _same_ gluetun, so they always share one exit IP. This layout is for
when that is not what you want: qBittorrent (the thing that moves bytes)
goes through one VPN, and sharerr (the tracker and feed a friend's Prowlarr
polls) goes through another. Two subscriptions, two exits, each rotating on
its own schedule.

Two `docker compose` projects:

- **`media/`**: gluetun, qBittorrent, Sonarr, Radarr. The library lives here.
- **`sharerr/`**: its own gluetun, and sharerr.

## Table of contents

- [Why they can still talk to each other](#why-they-can-still-talk-to-each-other)
- [Bring it up](#bring-it-up)
  - [Then the two gluetun keys, which are not optional](#then-the-two-gluetun-keys-which-are-not-optional)
- [What this changes about addresses](#what-this-changes-about-addresses)
- [Provider port forwarding](#provider-port-forwarding)

## Why they can still talk to each other

Both gluetun containers additionally join one shared bridge network,
`sharerr-shared`, created once before either stack starts:

```bash
docker network create --subnet 172.31.90.0/24 sharerr-shared
```

The subnet is not decorative: both gluetuns list `172.31.90.0/24` in
`FIREWALL_OUTBOUND_SUBNETS`, so a network Docker numbered on its own would be
one the killswitch refuses to route.

qBittorrent and sharerr each use `network_mode: service:gluetun`, which
shares their _entire_ network stack with their gluetun, including whatever
extra networks that gluetun joins. So attaching each gluetun to
`sharerr-shared` is what lets sharerr reach `media-gluetun:8080`
(qBittorrent's WebUI) by name, without either tunnel knowing about the
other's credentials, exit, or provider. Sonarr and Radarr are not behind a
gluetun; they join `sharerr-shared` directly. (The single-stack `vpn/` layout
carries no *arr services at all; its `config/sharerr.toml` shows where they
would sit, outside the tunnel.)

`sharerr-shared` carries only the WebUI/API calls between the two stacks,
never either tunnel's traffic.

## Bring it up

```bash
docker network create --subnet 172.31.90.0/24 sharerr-shared

cd media && cp .env.example .env && $EDITOR .env
docker compose up -d
cd ..

cd sharerr && cp .env.example .env && $EDITOR .env config/sharerr.toml
docker compose up -d
```

Each `.env` configures its own VPN credentials. Both share one `MEDIA_PATH`:
sharerr has to see the very files qBittorrent seeds, mounted at `/media`
instead of `/downloads`. `sharerr/.env` also carries `SHARERR_MASTER_KEY`;
the credentials themselves go into the vault through Settings or
`sharerr vault set`, never into `config/sharerr.toml`.

sharerr is published on `127.0.0.1:8477` by its gluetun. The image's own
healthcheck hits `/health`, not `/ready`, on purpose: an unpopulated vault
answers 503 on `/ready` by design, and `restart: unless-stopped` must not
cycle the container out from under you while you are still setting it up.

### Then the two gluetun keys, which are not optional

Both `.env` files carry a `GLUETUN_API_KEY`, different values for different
tunnels. Neither does anything until the matching key is also in sharerr's
vault. Run both from `sharerr/`, where the container lives:

```bash
printf %s "$(grep ^GLUETUN_API_KEY= sharerr/.env | cut -d= -f2-)" | \
  docker compose exec -T sharerr sharerr vault set gluetun.api_key
printf %s "$(grep ^GLUETUN_API_KEY= media/.env   | cut -d= -f2-)" | \
  docker compose exec -T sharerr sharerr vault set gluetun_client.api_key
```

Note which is which: this stack's own key is `gluetun.api_key`, and the
_media_ stack's is `gluetun_client.api_key`. Swapping them produces two
pollers that each authenticate against the wrong control server, a 401 on
both, and a warning that reads as one problem rather than two.
`docker compose exec sharerr sharerr doctor` names a missing key outright.

## What this changes about addresses

In the single-stack layout `tracker.advertised_host` and qBittorrent's
inbound peer port move together, because one gluetun forwards both. Here
they don't, so sharerr runs two pollers, both enabled in the shipped
`sharerr/config/sharerr.toml`, whose comments are the reference:

- **`[gluetun]`** → `http://localhost:8000`, this stack's own control server,
  on loopback because sharerr shares its gluetun's namespace. This is the
  tracker/feed address; it drives the announce URL in every `.torrent`.
- **`[gluetun_client]`** → `http://media-gluetun:8000`, across
  `sharerr-shared`, which is why that control server listens on `:8000` and
  has `8000` in its `FIREWALL_INPUT_PORTS`. Nothing rewrites a torrent from
  this one; sharerr's gossip self-record reads it so friends can see where
  the swarm traffic actually comes from.

For per-route roles instead of one key per server (worth it for `media/`'s,
which is reachable by anything else on `sharerr-shared`), see
`../gluetun-auth.example.toml`.

## Provider port forwarding

Both stacks ship the `VPN_PORT_FORWARDING` block commented out, because
gluetun accepts it only for a few providers, not for the `custom` provider
the `.env.example` files default to.

If your providers do support it, the up/down commands are worth
uncommenting: they nudge the right poller the moment a reconnect changes
something. The two stacks nudge differently, and the difference matters:

- `sharerr/` hits `http://localhost:8477/gluetun/refresh` and
  `http://localhost:8477/gluetun/down`: same namespace, no `?target=`, so
  they wake the default `tracker` poller.
- `media/` hits `http://sharerr-gluetun:8477/gluetun/refresh?target=client`
  and `.../gluetun/down?target=client`: across `sharerr-shared`, addressed to
  the _other_ stack's gluetun because sharerr rides in its namespace and has
  no name of its own, with `?target=client` so they wake the second poller.

sharerr rejects both endpoints from non-private source addresses;
`172.31.90.0/24` qualifies. Neither carries a value; they only ask sharerr to
re-poll, and the control server stays the single source of truth.
