# Two stacks, two tunnels

The single-stack layout in `docker/deploy/vpn/compose.yaml` puts qBittorrent and
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

qBittorrent and sharerr each use `network_mode: service:gluetun`, which shares
their *entire* network stack with the gluetun container they belong to —
including whatever extra networks that gluetun joins. So attaching each
gluetun to `sharerr-shared` is what lets sharerr reach `media-gluetun:8080`
(qBittorrent's WebUI) by name, without either tunnel knowing about the other's
credentials, exit, or provider. Sonarr and Radarr are not behind a gluetun —
they join `sharerr-shared` directly, the same way `docker/deploy/vpn/compose.yaml`
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

### Then the two gluetun keys, which are not optional

Both `.env` files carry a `GLUETUN_API_KEY`, and they are different values for
different tunnels. Neither does anything until the matching key is also in
sharerr's vault. Run both from `sharerr/`, where the container lives:

```bash
printf %s "$(grep ^GLUETUN_API_KEY= sharerr/.env | cut -d= -f2-)" | \
  docker compose exec -T sharerr sharerr vault set gluetun.api_key
printf %s "$(grep ^GLUETUN_API_KEY= media/.env   | cut -d= -f2-)" | \
  docker compose exec -T sharerr sharerr vault set gluetun_client.api_key
```

Note which is which: this stack's own key is `gluetun.api_key`, and the *media*
stack's is `gluetun_client.api_key`. Swapping them produces two pollers that
each authenticate against the wrong control server, which is a 401 on both and
a warning that reads as one problem rather than two.

`docker compose exec sharerr sharerr doctor` names a missing key outright, and
Diagnostics shows each poller's last success separately.

## What this changes about addresses

In the single-stack layout `tracker.advertised_host` and qBittorrent's
inbound peer port move together, because one gluetun forwards both. Here they
don't: `media/.env`'s forwarded port is qBittorrent's BitTorrent listening
port, and `sharerr/.env`'s is where friends reach the tracker and feed over
HTTP. Either tunnel can reconnect and get handed a new exit or a new forwarded
port without the other noticing, and the two exits are never the same address.

sharerr tracks the two independently, with two separate pollers, both enabled in
the shipped `sharerr/config/sharerr.toml`:

- **`[gluetun]`** → `http://localhost:8000`, this stack's own control server.
  It is on loopback because sharerr shares its gluetun's namespace. This is the
  tracker/feed address, and it drives the announce URL written into every
  `.torrent`.
- **`[gluetun_client]`** → `http://media-gluetun:8000`, across `sharerr-shared`.
  That control server listens on `:8000` rather than loopback precisely so this
  poll can arrive, which is also why `8000` is in its `FIREWALL_INPUT_PORTS`.
  Nothing rewrites a torrent from this one — sharerr's gossip self-record reads
  it, so friends can see where the swarm traffic actually comes from.

Both control servers authenticate with the API key from their own `.env`, set
through `HTTP_CONTROL_SERVER_AUTH_DEFAULT_ROLE`. For per-route roles instead of
one key per server — worth it for `media/`'s, which is reachable by anything
else on `sharerr-shared` — see `../gluetun-auth.example.toml`.

## Provider port forwarding

Both stacks ship the `VPN_PORT_FORWARDING` block commented out, because gluetun
accepts it only for private internet access, perfect privacy, privatevpn and
protonvpn — not for the `custom` provider the `.env.example` files default to.

If your providers do support it, the up/down commands are worth uncommenting:
they nudge the right poller the moment a reconnect changes something, instead of
leaving every torrent announcing to a dead port until `poll_secs` comes round.
The two stacks nudge differently, and the difference matters:

- `sharerr/` hits `http://localhost:8477/gluetun/refresh` — same namespace, and
  no `?target=`, so it wakes the default `tracker` poller.
- `media/` hits `http://sharerr-gluetun:8477/gluetun/refresh?target=client` —
  across `sharerr-shared`, addressed to the *other* stack's gluetun because
  sharerr rides in its namespace and has no name of its own, and with
  `?target=client` so it wakes the second poller rather than the first.

sharerr rejects both endpoints from non-private source addresses;
`172.31.90.0/24` qualifies. Neither carries a value — they only ask sharerr to
re-poll, and the control server stays the single source of truth.
