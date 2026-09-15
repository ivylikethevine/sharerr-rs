# One tunnel for everything

gluetun owns one network namespace, and qBittorrent and sharerr both ride in
it, so the swarm traffic, the tracker and the feed all leave from one VPN
exit. What every layout needs (the master key, the two volumes, path
mappings) is in [the deploy README](../README.md#what-every-layout-needs);
this page is the walkthrough for this one. `compose.yaml`'s header comments
are the long form of everything here.

## Contents

- [When to pick it](#when-to-pick-it)
- [Bring it up](#bring-it-up)
- [Gotchas](#gotchas)

## When to pick it

Pick it when you want one tunnel in front of everything and one public
address for both the swarm traffic and the tracker, so a friend sees one
host rather than two. It is drawn for a **WireGuard endpoint you control**
(a VPS, a seedbox, a box at another site), because that is the only shape
where all its inbound ports forward; see the port-forwarding gotcha below
for a commercial subscription. Want qBittorrent and sharerr on two separate
tunnels? [`dual-vpn/`](../dual-vpn/README.md). No tunnel at all?
[`direct/`](../direct/README.md). The full comparison is
[the "Which one" table](../README.md#which-one).

## Bring it up

1. Copy the environment file and fill it in:

   ```bash
   cd docker/deploy/vpn
   cp .env.example .env
   ```

   - The tunnel: `WIREGUARD_PRIVATE_KEY`, `WIREGUARD_PUBLIC_KEY`,
     `WIREGUARD_ENDPOINT_IP` and friends for your own endpoint. For a
     commercial provider, set `VPN_SERVICE_PROVIDER` to its name (e.g.
     `mullvad`, `protonvpn`) and leave the `WIREGUARD_ENDPOINT_*` pair blank.
   - `LAN_SUBNETS`: your LAN and docker's ranges; must not overlap
     `WIREGUARD_ADDRESSES`.
   - `GLUETUN_API_KEY`: generate with
     `docker run --rm qmcgaw/gluetun:v3.41.3 genkey`.
   - `SHARERR_MASTER_KEY`: generate once with `openssl rand -base64 32` and
     keep it; losing it loses every stored credential.
   - `MEDIA_PATH`: the library on the host, mounted read-only at
     `/downloads` for qBittorrent and `/media` for sharerr.

2. Edit `config/sharerr.toml`: set `tracker.advertised_host` to the VPN
   exit's public address (it ships as `CHANGE-ME.example.com`; once the
   `[gluetun]` poller runs, the live exit address wins and this becomes the
   fallback) and check the `[[path_map]]` entries match your library.

3. Start the stack:

   ```bash
   docker compose up -d
   ```

4. Put the **same** gluetun key into sharerr's vault, or sharerr never asks
   gluetun anything:

   ```bash
   printf %s "$GLUETUN_API_KEY" | \
     docker compose exec -T sharerr sharerr vault set gluetun.api_key
   ```

   or paste it into Settings → gluetun. Store qBittorrent's API key (5.2 or
   newer: Options → Web UI → API key) the same way, as
   `qbittorrent.api_key`.

5. Check the result:

   ```bash
   docker compose exec sharerr sharerr doctor
   ```

   It names a missing gluetun key outright, and checks credentials,
   reachability, the tag, and each path mapping.

## Gotchas

- **The key has two halves.** `GLUETUN_API_KEY` in `.env` alone leaves
  `[gluetun]` inert: sharerr skips the poll rather than send a request that
  can only come back `401`, and `advertised_host` quietly stays in force.
  Half configured looks exactly like fully configured until you notice the
  announce URLs never changed. [The key that has two
  halves](../README.md#gluetun-and-the-key-that-has-two-halves) has the
  detail.
- **`localhost`, not service names.** The three containers share one
  namespace, so sharerr reaches qBittorrent at `http://localhost:8080`;
  `http://qbittorrent:8080` does not resolve. Config copied from
  [`direct/`](../direct/README.md) does not work here.
- **`ports:` belongs to gluetun.** Declaring `ports:` on a service with
  `network_mode: service:gluetun` is a compose error. Both WebUIs
  (`127.0.0.1:8080` and `127.0.0.1:8477`) are published on gluetun.
- **One port space.** 8080, 8477, 6881 and gluetun's 8000 control server
  must all be distinct; a collision looks like a service that starts and
  then refuses connections.
- **A commercial provider forwards one port, not three.** Most give you one,
  of their choosing, often changing on reconnect. Give it to qBittorrent's
  listener (uncomment the `VPN_PORT_FORWARDING` block in `compose.yaml`), and
  publish the tracker and feed some other way: a reverse proxy on the host,
  or a tunnel that does not depend on the VPN.
- **A closed inbound port is invisible from inside.** `doctor` cannot tell
  it from a quiet swarm; use
  [the reachability check](../../../README.md#checking-that-you-are-actually-reachable)
  from outside.
- **The gluetun service is extended, not defined here.** Its image pin,
  tunnel credentials, capabilities and healthcheck live in
  [`../compose.gluetun.reference.yaml`](../compose.gluetun.reference.yaml);
  this file adds only the ports, firewall lists and control-server address.
- **Nothing else goes in sharerr's `environment:`.** Any `SHARERR_*` variable
  pins its setting and the Settings page discards a save to it.
