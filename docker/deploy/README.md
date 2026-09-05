# Deploying sharerr

Four layouts for sharerr itself. They differ in one thing, where sharerr's
traffic comes out, and everything else follows from that. A fifth directory,
[`lighthouse/`](lighthouse/), is not sharerr at all; see
[A lighthouse of your own](#a-lighthouse-of-your-own).

These are deployment recipes. `docker/*.yml` one directory up are the
**tier-2 test stacks**; do not deploy from those.

> These files pull `:latest`, which tracks the newest tagged release. To pin
> a specific version, point `image:` at `ghcr.io/…:vX.Y.Z` instead, or track
> `main` between releases via the `sha-<commit>` image every push publishes.
> See [the tag scheme](../../docs/RELEASING.md#the-tag-scheme).

## Table of contents

- [Which one](#which-one)
- [What every layout needs](#what-every-layout-needs)
- [One port, several audiences](#one-port-several-audiences)
- [gluetun, and the key that has two halves](#gluetun-and-the-key-that-has-two-halves)
- [The thing that is invisible from inside](#the-thing-that-is-invisible-from-inside)
- [A lighthouse of your own](#a-lighthouse-of-your-own)

## Which one

| | Layout | Use it when |
| --- | --- | --- |
| **[`direct/`](direct/)** | One bridge network. sharerr, qBittorrent, Sonarr, Radarr. | The exit address is already yours: a VPS, a seedbox, a static IP or dynamic-DNS name at home. Start here if unsure. |
| **[`vpn/`](vpn/)** | gluetun owns a namespace; qBittorrent and sharerr ride in it. | You want one commercial tunnel in front of everything, and one public address for both the swarm traffic and the tracker. |
| **[`dual-vpn/`](dual-vpn/README.md)** | Two gluetuns, two compose projects. | qBittorrent on one subscription, sharerr's tracker/feed on another, each rotating independently. |
| **[`sidecar/`](sidecar/)** | sharerr alone, joining a network you already have. | You already run qBittorrent and the *arr apps and want to add exactly one container. |

Each layout's compose file carries its own header explaining the choices it
makes; read that before editing it. Two shared files:

- **[`compose.gluetun.reference.yaml`](compose.gluetun.reference.yaml)**: the
  gluetun service the three tunnelled layouts `extends`. Every gluetun
  v3.41.3 variable that is not stack-specific is in it with its real
  default, so turning something on is uncommenting a line; the per-stack
  ones (firewall ports, control-server auth, DoT, health timings) live in
  each layout's own file. Not a stack; it does not come up on its own.
- **[`gluetun-auth.example.toml`](gluetun-auth.example.toml)**: per-route
  control-server roles, for when one key for everything is too much key.

## What every layout needs

**A master key.** `SHARERR_MASTER_KEY`, from `openssl rand -base64 32`, set
in `.env`. It encrypts the vault that holds every other credential. Losing it
loses all of them; there is no recovery and sharerr will not fall back to
plaintext. `SHARERR_MASTER_KEY_FILE` takes a path instead, for a docker
secret.

**Two volumes that must persist.** `/config` holds `sharerr.toml` and is
rewritten in place by the web UI, so it has to be writable by uid 1000, which
is what the image runs as. `/data` holds the vault, the database and the
generated `.torrent` files. Backing them up is in
[`docs/SETTINGS.md`](../../docs/SETTINGS.md#backup-and-restore).

**Nothing else in `environment:`.** Every sharerr setting is reachable as a
`SHARERR_*` variable, and setting one _pins_ it: the Settings page renders
that field disabled and a save through the UI is discarded. A `SHARERR_*`
name that is not a real config path is a startup error. Configure in
`config/sharerr.toml` or the UI.

**The library, read-only.** sharerr hashes the files to build torrents and
never moves, renames or re-links anything, so `:ro` is accurate rather than
cautious. Each layout mounts it at `/media` for sharerr and somewhere else
for the torrent client, deliberately: the `[[path_map]]` entries translate
between the two views, and identical mounts would hide a mapping bug.

**Path mappings that are actually right.** A wrong one is the most common
reason nothing gets shared. `docker compose exec sharerr sharerr doctor`
resolves each mapping against the files that are there;
`sharerr doctor --suggest-paths` proposes them by searching `/media`. Worth
running before the first sync, and worth setting `skip_checking = false` for
the first run so qBittorrent verifies the mapping instead of trusting it.

## One port, several audiences

sharerr serves the web UI, the tracker's announce/scrape, the Torznab feed
and the gossip endpoints from a single listener on **8477**. Friends need to
reach it and so does your browser, which means opening it to the internet
opens the login page to the internet, and without a TLS proxy in front the
session cookie travels in the clear (sharerr detects a proxy and marks the
cookie `Secure` automatically; see
[the security policy](../../docs/SECURITY.md#what-is-in-scope)).

Three ways to handle that, in rough order of how much they buy you:

1. **A TLS reverse proxy** in front, with `tracker.advertised_url` set to the
   `https://` address so announce URLs match what friends can reach, and
   `X-Forwarded-Proto` set so sharerr marks the session cookie `Secure`.
2. **A second, tracker-only listener** via `tracker.bind`. It carries the
   tracker routes and `.torrent` downloads and not the UI, so you can forward
   that port and leave 8477 on loopback.
3. **Forward 8477 as it is**, which works, and is a login page on the open
   internet with no rate limit in front of it.

sharerr opens no BitTorrent peer port of its own. The `6881` in these files
is always qBittorrent's.

## gluetun, and the key that has two halves

The tunnelled layouts point sharerr at gluetun's control server so it can
learn its own exit address and forwarded port and rewrite every torrent's
announce URL when either changes. gluetun has required a key on every
control-server route since v3.39.1, and sharerr skips the poll entirely
rather than send a request that can only come back `401`. So the key has to
be in two places:

1. `GLUETUN_API_KEY` in the stack's `.env`, which reaches gluetun through
   `HTTP_CONTROL_SERVER_AUTH_DEFAULT_ROLE`. Generate it with
   `docker run --rm qmcgaw/gluetun:v3.41.3 genkey`.
2. The same value in sharerr's vault:

   ```bash
   printf %s "$GLUETUN_API_KEY" | \
     docker compose exec -T sharerr sharerr vault set gluetun.api_key
   ```

With only the first half done, `[gluetun]` is inert and `advertised_host`
quietly stays in force. `sharerr doctor` names the missing key. `dual-vpn/`
has two tunnels and so two keys; see [its README](dual-vpn/README.md). To
scope the key to the three routes sharerr calls (three, not two, because
gluetun answers one of them with a redirect that is authorised separately),
use `gluetun-auth.example.toml`.

For reconnects to be picked up in seconds rather than at the next poll, set
gluetun's `VPN_PORT_FORWARDING_UP_COMMAND` to
`wget -qO- http://localhost:8477/gluetun/refresh` and
`VPN_PORT_FORWARDING_DOWN_COMMAND` to
`wget -qO- http://localhost:8477/gluetun/down`. Both only nudge sharerr to
re-ask the control server; nothing pushed is trusted, and both are refused
from non-private source addresses.

## The thing that is invisible from inside

Inbound reachability. A friend's announce arriving is the only proof that
`FIREWALL_VPN_INPUT_PORTS`, your provider's forwarded port and your router
agree. From inside, a closed port and a quiet swarm are the same picture, and
`doctor` cannot tell them apart. The Debug page's script, run from outside,
can; see [the README](../../README.md#checking-that-you-are-actually-reachable).

Commercial providers that forward ports give you **one**, of their choosing,
often changing on reconnect. The tunnelled layouts as written want more than
that, which is why they are drawn for a WireGuard endpoint you control. Each
stack's header covers what to give up if yours is a subscription instead.

## A lighthouse of your own

[`lighthouse/`](lighthouse/) runs `sharerr-lighthouse`, the rendezvous
service. It is not one of the four layouts above and shares none of their
concerns: no library mount, no torrent client, no *arr apps, no path
mappings, and no master key. It persists exactly one file (a decoy secret)
and has no UI or config file, only the two environment variables its compose
file sets. Why it exists, how a peer points at one, and the embedded
alternative are in [`docs/LIGHTHOUSE.md`](../../docs/LIGHTHOUSE.md).
