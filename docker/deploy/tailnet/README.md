# A tailnet, no forwarded port

sharerr and qBittorrent inside a Tailscale container's network namespace,
reachable only over the tailnet: no port forwarding, no VPN provider, no
gluetun. What every layout needs (the master key, the two volumes, path
mappings) is in [the deploy README](../README.md#what-every-layout-needs);
this page is the walkthrough for this one. `compose.yaml`'s header comments
are the long form of everything here.

## Contents

- [When to pick it](#when-to-pick-it)
- [Bring it up](#bring-it-up)
- [Letting friends in](#letting-friends-in)
- [Gotchas](#gotchas)

## When to pick it

Pick it when you cannot open a port: CGNAT, a router you do not control, a
mobile or campus connection. The friend group joins one tailnet instead, on
Tailscale's coordination server or a [Headscale](https://headscale.net/)
server of your own. The cost is that every friend's torrent client runs on a
tailnet device. Pick [`direct/`](../direct/README.md) if you can forward a
port, and [`vpn/`](../vpn/README.md) if you want the swarm to see a VPN exit
rather than a private network.

## Bring it up

1. Copy the environment file and fill it in:

   ```bash
   cd docker/deploy/tailnet
   cp .env.example .env
   ```

   - `SHARERR_MASTER_KEY`: generate once with `openssl rand -base64 32` and
     keep it.
   - `TS_AUTHKEY`: an auth key from the Tailscale admin console, or
     `headscale preauthkeys create`. Used on the first start only.
   - `TS_LOGIN_SERVER`: your Headscale server's URL; empty for Tailscale.
   - `MEDIA_PATH`: the library on the host.

2. Start the stack and read the machine's tailnet address:

   ```bash
   docker compose up -d
   docker compose exec tailscale tailscale ip -4
   ```

3. Put that address in `config/sharerr.toml` as `tracker.advertised_host`
   (it ships as `100.64.0.1`) and check the `[[path_map]]` entries, then
   `docker compose restart sharerr`.

4. In qBittorrent (5.2 or newer, on `127.0.0.1:8080`), generate an API key
   under Options → Web UI → API key and store it through sharerr's Settings,
   with the Sonarr and Radarr keys.

5. Check the result with `docker compose exec sharerr sharerr doctor`.

## Letting friends in

A friend needs two things on the tailnet: their Prowlarr, to read your feed
on port 8477, and their torrent client, to reach qBittorrent on 6881 and
announce to 8477. Either invite them to the tailnet, or share just this
machine with their account (Tailscale's machine sharing). A shared machine
cannot open connections back to theirs, so for gossip, which runs both ways,
each instance has to be shared with the other.

Then add them as a friend in sharerr as usual; the feed URL their Prowlarr
uses is `http://<tailnet address>:8477`.

## Gotchas

- **`localhost`, not service names, inside the namespace.** sharerr reaches
  qBittorrent at `http://localhost:8080`. Sonarr and Radarr stay outside it
  and are reached by name; their Webhook connections reach sharerr at
  `http://tailscale:8477/hooks/arr`.
- **The address, not the MagicDNS name.** qBittorrent announces from inside
  the namespace, which does not use MagicDNS, so `advertised_host` is the
  `100.x` address. It stays fixed for the life of the node.
- **An ACL that limits ports must allow 6881 and 8477** to this machine for
  every friend's devices.
- **Kernel networking.** The container runs with `TS_USERSPACE=false`,
  `/dev/net/tun` and `NET_ADMIN`, so qBittorrent's peer port answers TCP and
  UDP like any interface.
- **Plain WireGuard** has the same shape: a WireGuard container as the
  namespace owner in place of tailscale.
