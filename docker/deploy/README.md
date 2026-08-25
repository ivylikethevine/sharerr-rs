# Deploying sharerr

Four layouts. They differ in one thing — where sharerr's traffic comes out —
and everything else follows from that.

These are deployment recipes. `docker/*.yml` one directory up are the
**tier-2 test stacks**: they build the image from source, seed synthetic
fixtures, and are driven by `run_docker_tests.sh`. Do not deploy from those.

## Which one

| | Layout | Use it when |
|---|---|---|
| **[`direct/`](direct/)** | One bridge network. sharerr, qBittorrent, Sonarr, Radarr. | The exit address is already yours — a VPS, a seedbox, a static IP or dynamic-DNS name at home. Start here if unsure. |
| **[`vpn/`](vpn/)** | gluetun owns a namespace; qBittorrent and sharerr ride in it. | You want one commercial tunnel in front of everything, and one public address for both the swarm traffic and the tracker. |
| **[`dual-vpn/`](dual-vpn/)** | Two gluetuns, two compose projects. | qBittorrent on one subscription, sharerr's tracker/feed on another, each rotating independently. |
| **[`sidecar/`](sidecar/)** | sharerr alone, joining a network you already have. | You already run qBittorrent and the *arr apps and want to add exactly one container. |

Two shared files:

- **[`compose.gluetun.reference.yaml`](compose.gluetun.reference.yaml)** — the
  gluetun service the three tunnelled layouts `extends`. Every environment
  variable gluetun v3.41.3 accepts is in it, with its real default, so turning
  something on is uncommenting a line. Not a stack; it does not come up on its
  own.
- **[`gluetun-auth.example.toml`](gluetun-auth.example.toml)** — per-route
  control-server roles, for when one key for everything is too much key.

## What every layout needs

**A master key.** `SHARERR_MASTER_KEY`, from `openssl rand -base64 32`, set in
`.env`. It encrypts the vault that holds every other credential. Losing it loses
all of them; there is no recovery and sharerr will not fall back to plaintext.
`SHARERR_MASTER_KEY_FILE` takes a path instead, for a docker secret.

**Two volumes that must persist.** `/config` holds `sharerr.toml` and is
rewritten in place by the web UI when you save — comments and all — so it has to
be *writable* by uid 1000, which is what the image runs as. `/data` holds the
vault, the database and the generated `.torrent` files.

**Nothing else in `environment:`.** Every sharerr setting is reachable as a
`SHARERR_*` variable, and setting one *pins* it: env layers over the file, so the
Settings page renders that field disabled and a save through the UI is discarded.
Worse, a `SHARERR_*` name that is not a real config path is a startup error, not
a typo you find later. Configure in `config/sharerr.toml` or the UI.

**The library, read-only.** sharerr hashes the files to build torrents and never
moves, renames or re-links anything, so `:ro` is accurate rather than cautious.
Each layout mounts it at `/media` for sharerr and somewhere else for the torrent
client, deliberately: the `[[path_map]]` entries translate between the two views,
and identical mounts would hide a mapping bug until it mattered.

**Path mappings that are actually right.** A wrong one is the most common reason
nothing gets shared. `docker compose exec sharerr sharerr doctor` resolves each
mapping against the files that are there; `sharerr doctor --suggest-paths`
proposes them by searching `/media`. Worth running before the first sync, and
worth setting `skip_checking = false` for the first run so qBittorrent verifies
the mapping instead of trusting it.

## One port, several audiences

sharerr serves the web UI, the tracker's announce/scrape, the Torznab feed and
the gossip endpoints from a single listener on **8477**. Friends need to reach it
and so does your browser, which means opening it to the internet opens the login
page to the internet — over a connection sharerr does not encrypt, with a session
cookie it cannot mark secure.

Three ways to handle that, in rough order of how much they buy you:

1. **A TLS reverse proxy** in front, with `tracker.advertised_url` set to the
   `https://` address so announce URLs match what friends can reach.
2. **A second, tracker-only listener** via `tracker.bind`. It carries the tracker
   routes and `.torrent` downloads and not the UI, so you can forward that port
   and leave 8477 on loopback. It adds a door rather than moving one.
3. **Forward 8477 as it is** — which works, and is a login page on the open
   internet.

sharerr opens no BitTorrent peer port of its own. The `6881` in these files is
always qBittorrent's.

## gluetun, and the key that has two halves

The tunnelled layouts point sharerr at gluetun's control server so it can learn
its own exit address and forwarded port, and rewrite every torrent's announce URL
when either changes. This is the part that most often looks configured and is not.

gluetun has made every control-server route private by default since v3.39.1.
sharerr knows this, and rather than send a request whose only possible answer is
`401`, it **skips the poll entirely** and logs one warning. So the key has to be
in two places:

1. `GLUETUN_API_KEY` in the stack's `.env`, which reaches gluetun through
   `HTTP_CONTROL_SERVER_AUTH_DEFAULT_ROLE`. Generate it with
   `docker run --rm qmcgaw/gluetun:v3.41.3 genkey`.
2. The same value in sharerr's vault:

   ```bash
   printf %s "$GLUETUN_API_KEY" | \
     docker compose exec -T sharerr sharerr vault set gluetun.api_key
   ```

   (`dual-vpn/` has two tunnels and so two keys — see its README.)

With only the first half done, `[gluetun]` in `sharerr.toml` is inert and
`advertised_host` quietly stays in force. `sharerr doctor` names the missing key;
Diagnostics shows the poller's last success, which stays empty.

If you would rather scope the key to the three routes sharerr actually calls,
`gluetun-auth.example.toml` does that — and explains why it is three routes for
what looks like two calls.

## The thing that is invisible from inside

Inbound reachability. A friend's announce arriving is the only proof that
`FIREWALL_VPN_INPUT_PORTS`, your provider's forwarded port and your router agree.
From inside, a closed port and a quiet swarm are the same picture, and `doctor`
cannot tell them apart either.

Commercial providers that forward ports give you **one**, of their choosing,
often changing on reconnect. The tunnelled layouts as written want more than
that, which is why they are drawn for a WireGuard endpoint you control. Each
stack's header covers what to give up if yours is a subscription instead.
