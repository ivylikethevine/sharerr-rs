# sharerr beside a stack you already run

One container, sharerr, joining the docker network your qBittorrent and \*arr
apps are already on. What every layout needs (the master key, the two
volumes, path mappings) is in [the deploy README](../README.md#what-every-layout-needs);
this page is the walkthrough for this one. `compose.yaml`'s header comments
are the long form of everything here.

## Contents

- [When to pick it](#when-to-pick-it)
- [Bring it up](#bring-it-up)
- [Gotchas](#gotchas)

## When to pick it

Pick it when qBittorrent and the \*arr apps already run under some other
compose project and you want to add exactly one container. The other
layouts bring those services up themselves. If your qBittorrent sits inside
a VPN container's namespace, read the last gotcha first: you may be better
served by [`vpn/`](../vpn/README.md) or [`dual-vpn/`](../dual-vpn/README.md).
The full comparison is [the "Which one" table](../README.md#which-one).

## Bring it up

1. Find the network your stack is on:

   ```bash
   docker network ls
   docker inspect -f '{{range $k,$v := .NetworkSettings.Networks}}{{$k}}{{"\n"}}{{end}}' qbittorrent
   ```

   Compose names a project's default network `<project>_default`, so a stack
   brought up from a `media/` directory is usually on `media_default`.

2. Copy the environment file and fill it in:

   ```bash
   cd docker/deploy/sidecar
   cp .env.example .env
   ```

   - `EXTERNAL_NETWORK`: the network name from step 1.
   - `SHARERR_MASTER_KEY`: generate once with `openssl rand -base64 32` and
     keep it; losing it loses every stored credential.
   - `MEDIA_PATH`: the library on the host, mounted read-only at `/media`. It
     must be the same bytes your torrent client seeds.
   - `TZ` and `RUST_LOG` have working defaults.

3. Edit `config/sharerr.toml`. Every service URL in it is a guess: replace
   each with the service name from _your_ compose file
   (`docker network inspect <your-network>` lists what is attached). Set
   `tracker.advertised_host` (it ships as `CHANGE-ME.example.com`) and write
   one `[[path_map]]` per library root.

4. Start sharerr:

   ```bash
   docker compose up -d
   ```

5. Store the credentials in sharerr's vault through Settings in the web UI,
   or pipe them in, e.g.:

   ```bash
   printf %s "$KEY" | docker compose exec -T sharerr sharerr vault set qbittorrent.api_key
   ```

6. Check the result:

   ```bash
   docker compose exec sharerr sharerr doctor
   ```

   It checks credentials, reachability, the tag, and each path mapping
   against the files that are actually there.

## Gotchas

- **The network is joined, never owned.** It is declared `external: true`,
  so compose errors if it cannot find it rather than quietly creating a
  second network that reaches nothing, and `docker compose down` here leaves
  your stack's network alone. Services spread across several networks mean
  adding each to the `networks:` lists.
- **Use the other project's service name.** On a shared bridge, containers
  resolve each other by service name, which is not always the container
  name.
- **8477 carries the web UI too.** Friends need to reach it, so forwarding it
  puts the login page on the internet, and sharerr serves no TLS. Put a TLS
  reverse proxy in front and set `tracker.advertised_url` to the `https://`
  address, or bind a tracker-only listener with `tracker.bind` and forward
  that instead. [One port, several
  audiences](../README.md#one-port-several-audiences) weighs the options.
- **The library mount is not optional.** sharerr hashes the files to build
  torrents; a network share is a bad idea for the first pass.
- **If your qBittorrent runs inside a VPN container** (`network_mode:
"service:gluetun"` in its compose file), it has no network or name of its
  own. Either join that namespace (`network_mode: "container:gluetun"` in
  place of `networks:`, drop `ports:`, add 8477 to their gluetun's published
  ports and firewall lists, and reach qBittorrent on `localhost:8080`), or
  reach it through their gluetun by name (`http://gluetun:8080`), which
  leaves sharerr announcing your real address while qBittorrent announces
  the tunnel's. The last section of `compose.yaml` spells out both.
- **Nothing else goes in `environment:`.** Any `SHARERR_*` variable pins its
  setting and the Settings page discards a save to it.
