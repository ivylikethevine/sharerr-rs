# Settings reference

Every `sharerr.toml` field, environment variable, and vault secret sharerr
reads, in one place. [The README](../README.md) is where to go for
walkthroughs and _why_; this page is where to go for the exact key, its
default, and what sets it.

## Contents

- [How configuration is layered](#how-configuration-is-layered)
- [Top-level settings](#top-level-settings)
- [`[server]`](#server)
- [\*arr apps](#arr-apps)
- [`[[library]]` — plain directories](#library--plain-directories)
- [Torrent client](#torrent-client)
  - [`[qbittorrent]`](#qbittorrent)
  - [`[transmission]`](#transmission)
  - [`[rtorrent]`](#rtorrent)
  - [`[deluge]`](#deluge)
- [`[seeding]`](#seeding)
- [`[feed]`](#feed)
- [`[tracker]`](#tracker)
- [`[[path_map]]`](#path_map)
- [`[gossip]`](#gossip)
- [`[retention]`](#retention)
- [`[lighthouse]`](#lighthouse)
- [`[gluetun]` and `[gluetun_client]`](#gluetun-and-gluetun_client)
- [`[sync]`](#sync)
  - [Syncing on import](#syncing-on-import)
- [`[checks]`](#checks)
- [`[notifications]`](#notifications)
- [`[metrics]`](#metrics)
- [`[api]`](#api)
- [Backup and restore](#backup-and-restore)
  - [Backing up `/data`](#backing-up-data)
  - [Restoring friends after a full data-directory loss](#restoring-friends-after-a-full-data-directory-loss)
- [Vault secrets](#vault-secrets)
- [Environment variable overrides](#environment-variable-overrides)

## How configuration is layered

Three layers, applied in order, each overriding the last:

1. **Compiled defaults** — what every field below is when `sharerr.toml`
   does not mention it.
2. **`sharerr.toml`** — read from `/config/sharerr.toml` by default
   (`--config <path>`, or `SHARERR_CONFIG`, points elsewhere). This is the
   file the web UI rewrites in place (comments and all) when you save a
   settings form. A missing file is not an error; a deployment can be
   configured entirely through the environment.
3. **`SHARERR_*` environment variables** — the top layer, and the only one a
   restart is needed to change. See
   [Environment variable overrides](#environment-variable-overrides).

Unknown keys are rejected, not ignored — a typo like `taag = "x"` is a
startup error naming the key, at every layer. If the file fails to load,
`serve` still starts on a fallback config that salvages `data_dir` and
`server.bind` from the broken file (and the environment) so the web UI is
reachable to fix it, and says so on every page.

**Secrets are never in `sharerr.toml`.** Every API key, password, and token
lives in the encrypted vault instead, keyed by the constants listed in
[Vault secrets](#vault-secrets) — set with `sharerr vault set <key>`, or from
the corresponding settings-page field once `SHARERR_MASTER_KEY` is set. A
field's _presence_ is still readable from `sharerr.toml` or the environment
(a URL, a username), just never the credential itself.

## Top-level settings

| TOML key   | Type   | Default   | Notes                                                                           |
| ---------- | ------ | --------- | ------------------------------------------------------------------------------- |
| `data_dir` | path   | `/data`   | Holds the SQLite database, the encrypted vault, and generated `.torrent` files. |
| `tag`      | string | `sharerr` | The Sonarr/Radarr/etc. tag that marks content for sharing.                      |

## `[server]`

| TOML key                  | Type            | Default        | Notes                                                                                                                                                                                                              |
| ------------------------- | --------------- | -------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `server.bind`             | socket addr     | `0.0.0.0:8477` | Carries the web UI, the tracker, and the Torznab feed on one port, unless [`[tracker]`](#tracker) opens a second dedicated listener.                                                                               |
| `server.security_headers` | bool            | `false`        | Sends a strict CSP, `X-Frame-Options: DENY`, `Referrer-Policy: no-referrer` and `nosniff` with every web UI response, for an instance reachable outside a trusted LAN. Applies on the next request. File/env only. |
| `server.trusted_proxies`  | list of strings | `[]`           | Addresses or CIDR blocks (`172.18.0.5`, `10.0.0.0/8`) of reverse proxies whose `X-Forwarded-For` the login throttle believes, so each client behind the proxy gets its own limit. File/env only.                   |
| `server.auth_header`      | string          | `""` (off)     | The header a forward-auth proxy names the signed-in user in, such as `Remote-User` (Authelia) or `X-authentik-username` (Authentik). Believed only on a connection from `server.trusted_proxies`. File/env only.   |

### Signing in through a forward-auth proxy

With Authelia, Authentik or another forward-auth proxy in front, sharerr can
take the proxy's sign-in instead of asking for its own password:

```toml
[server]
trusted_proxies = ["172.18.0.5"]
auth_header = "Remote-User"
```

A request from a listed proxy that carries a non-empty `auth_header` is
signed in as the user it names; the proxy's own access policy decides who
gets that far. Any other request still meets the login page, so a direct
connection to port 8477 keeps working with the sharerr password, and
**Sign out** ends only a sharerr session, not the proxy's.

Two things make this safe, and both are yours to check:

- **List the proxy's own address, not its network.** Every address listed
  can name any user, so a Docker subnet trusts every container on it.
- **The proxy must replace the header a client sent, not pass it through.**
  A proxy that forwards a client's own `Remote-User` signs in anyone who
  types one.

## \*arr apps

Each app is its own optional section — any combination works, and Lidarr,
Readarr, and Whisparr are all independent of Sonarr/Radarr. See the
README's ["Sharing music, books, and more"](../README.md#sharing-music-books-and-more)
for the per-app caveats (API version, category quirks).

| App      | TOML key       | Vault secret       |
| -------- | -------------- | ------------------ |
| Sonarr   | `sonarr.url`   | `sonarr.api_key`   |
| Radarr   | `radarr.url`   | `radarr.api_key`   |
| Lidarr   | `lidarr.url`   | `lidarr.api_key`   |
| Readarr  | `readarr.url`  | `readarr.api_key`  |
| Whisparr | `whisparr.url` | `whisparr.api_key` |

```toml
[sonarr]
url = "http://localhost:8989"
```

## `[[library]]` — plain directories

Zero-dependency sharing with no \*arr app at all — see the README's
["Sharing a plain directory"](../README.md#sharing-a-plain-directory-no-arr-app-at-all).

| Field  | Type                                 | Notes                                               |
| ------ | ------------------------------------ | --------------------------------------------------- |
| `path` | path                                 | Scanned recursively; everything under it is shared. |
| `kind` | `tv` \| `movie` \| `music` \| `book` | Decides the feed category and peer-scope bucket.    |

```toml
[[library]]
path = "/media/extras"
kind = "movie"
```

## Torrent client

`torrent_backend` selects which of the sections below actually seeds. Every
section's settings are kept regardless of which is selected, so a client can
be filled in and tested before switching to it.

| TOML key          | Type                                                      | Default       |
| ----------------- | --------------------------------------------------------- | ------------- |
| `torrent_backend` | `qbittorrent` \| `transmission` \| `rtorrent` \| `deluge` | `qbittorrent` |

### `[qbittorrent]`

| TOML key                    | Type   | Default                 | Notes                                                                                                                                                                                                                                                                       |
| --------------------------- | ------ | ----------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `qbittorrent.url`           | url    | `http://localhost:8080` |                                                                                                                                                                                                                                                                             |
| `qbittorrent.category`      | string | `sharerr`               | Category applied to torrents sharerr creates.                                                                                                                                                                                                                               |
| `qbittorrent.tag`           | string | `sharerr`               | Tag applied alongside the category.                                                                                                                                                                                                                                         |
| `qbittorrent.skip_checking` | bool   | `true`                  | Skip qBittorrent's hash check on add — see [the README](../README.md#authenticating-to-qbittorrent) and the trap in [`CLAUDE.md`](https://github.com/ivylikethevine/sharerr-rs/blob/main/CLAUDE.md#traps) before turning this off partway through setting up path mappings. |

Vault secret: `qbittorrent.api_key` (a qBittorrent 5.2+ WebUI API key — the
sole credential; there is no username/password fallback).

### `[transmission]`

| TOML key                | Type   | Default                 | Notes                                                                                |
| ----------------------- | ------ | ----------------------- | ------------------------------------------------------------------------------------ |
| `transmission.url`      | url    | `http://localhost:9091` |                                                                                      |
| `transmission.username` | string | `transmission`          |                                                                                      |
| `transmission.label`    | string | `sharerr`               | Stands in for both category and tag — Transmission has only one flat list of labels. |

Vault secret: `transmission.password`.

### `[rtorrent]`

| TOML key            | Type   | Default                 | Notes                                                                                                                                                                                                                                    |
| ------------------- | ------ | ----------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `rtorrent.url`      | url    | `http://localhost/RPC2` | The **exact** XML-RPC endpoint, not a base — rTorrent has no HTTP server or standard path of its own. See [`sharerr-rtorrent`](https://github.com/ivylikethevine/sharerr-rs/blob/main/crates/sharerr-rtorrent/src/lib.rs)'s module docs. |
| `rtorrent.username` | string | `rtorrent`              | Sent as HTTP Basic Auth — see below.                                                                                                                                                                                                     |
| `rtorrent.label`    | string | `sharerr`               | Stands in for both category and tag, stored in rTorrent's `d.custom1`.                                                                                                                                                                   |

Vault secret: `rtorrent.password`. rTorrent's own XML-RPC has no credential
of its own; username/password authenticate against whatever reverse proxy
fronts the RPC endpoint (the standard way ruTorrent's `httprpc` plugin is
secured). Any placeholder values work if your proxy has no such gate.

For what rTorrent cannot do (skip the hash check, honour a per-torrent ratio
limit, remove a stale tracker) see
[`COMPATIBILITY.md`](COMPATIBILITY.md#torrent-clients-what-actually-seeds).

### `[deluge]`

| TOML key       | Type   | Default                 | Notes                                                                                                           |
| -------------- | ------ | ----------------------- | --------------------------------------------------------------------------------------------------------------- |
| `deluge.url`   | url    | `http://localhost:8112` | The web UI's root, not the `/json` endpoint. Deluge 2 only.                                                     |
| `deluge.label` | string | `sharerr`               | Stands in for both category and tag, through Deluge's Label plugin. Without the plugin, torrents go unlabelled. |

Vault secret: `deluge.password`, the web UI's password. It has no accounts, so
there is no username.

## `[seeding]`

Applied when sharerr hands a torrent to whichever client is selected, and
restated on every torrent sharerr created on the first sync pass after a
value changes. An unset key is no opinion: nothing is sent, and whatever the
client holds for a torrent stays, so clearing a limit you no longer want is
done in the client. Torrents sharerr adopted rather than created are never
touched. The client's own engine does the enforcing. See the README's
["Seeding limits"](../README.md#seeding-limits).

| TOML key                         | Type   | Default | Notes                                                                                                                                                                                                                                                                                                                                                                 |
| -------------------------------- | ------ | ------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `seeding.upload_limit_kib`       | int    | unset   | Per-torrent upload cap, KiB/s.                                                                                                                                                                                                                                                                                                                                        |
| `seeding.ratio_limit`            | float  | unset   | Seed-ratio goal. Not honoured by rTorrent — see above.                                                                                                                                                                                                                                                                                                                |
| `seeding.quiet_hours`            | string | unset   | A daily window, `HH:MM-HH:MM` in the container's time zone (`TZ`); `22:00-06:00` wraps past midnight.                                                                                                                                                                                                                                                                 |
| `seeding.quiet_upload_limit_kib` | int    | unset   | The upload cap inside `quiet_hours`, KiB/s.                                                                                                                                                                                                                                                                                                                           |
| `seeding.sidecars`               | bool   | `false` | Build a tv or movie `[[library]]` file's torrent over it and the subtitle files beside it named after it (`Show.S01E01.en.srt`). Applies only to future builds, like `seeding.private`; a file with no subtitles builds the same single-file torrent either way. Only `srt`, `ass`, `ssa`, `sub`, `idx`, `vtt` and `sup`, in the same directory.                      |
| `seeding.private`                | bool   | `true`  | BEP 27's private flag on torrents built from then on. Applies only to future builds — flipping it changes the info hash, so existing shares keep whatever they were built with. Off lets a client also use DHT/PEX, so **revoking a friend no longer removes them from that torrent's swarm**. See [`docs/COMPATIBILITY.md`](COMPATIBILITY.md#the-feeds-magnet-link). |

Quiet hours take effect only with `upload_limit_kib`, `quiet_hours` and
`quiet_upload_limit_kib` all set: inside the window sharerr's torrents take
the quiet cap, and after it the normal one again, so without a normal cap
there would be nothing to go back to. Each edge lands on the first sync pass
past it, so within `sync.interval_secs`.

A share can also end on a date: **Share until** on an item's page. From the
start of that day (container time) each pass unshares the item and leaves it
unshared while the date stands, even though the file is still tagged;
clearing the date shares it again on the next pass.

## `[feed]`

What the Torznab and Jackett feeds advertise alongside the `.torrent` link
every item already carries. Settings page: the same "Seeding limits" panel
as `[seeding]` above, since the two are meant to be read together.

| TOML key            | Type | Default | Notes                                                                                                                                                                                                                                                                                                                                 |
| ------------------- | ---- | ------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `feed.magnet_links` | bool | `false` | Emit a `magneturl` (Torznab) / `MagnetUri` (Jackett) attribute per item. Only ever produces one for an item that is _also_ not private (`seeding.private = false`); a private item's magnet can never resolve, so it is omitted rather than advertised broken. See [`docs/COMPATIBILITY.md`](COMPATIBILITY.md#the-feeds-magnet-link). |

## `[tracker]`

| TOML key                  | Type        | Default              | Notes                                                                                                                                                           |
| ------------------------- | ----------- | -------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `tracker.advertised_host` | string      | unset (required\*)   | Hostname/IP friends reach the tracker on.                                                                                                                       |
| `tracker.port`            | int         | `server.bind`'s port | Override when a published docker port differs from the internal one.                                                                                            |
| `tracker.advertised_url`  | url         | unset                | Full base URL (scheme, path prefix, bracketed IPv6) — wins over `advertised_host`/`port`.                                                                       |
| `tracker.bind`            | socket addr | unset                | A second listener carrying only the tracker and `.torrent` downloads, for a one-forwarded-port topology. File/env only — the settings page has no field for it. |

\* Required unless `advertised_url` is set, or [`[gluetun]`](#gluetun-and-gluetun_client)
resolves an endpoint dynamically.

Vault secret: `tracker.token` — an announce token embedded in every torrent
built after it is set. It becomes one unencoded segment of every announce
URL, so `vault set` and the settings page both refuse anything outside
letters, digits, `-`, `_`, `.` and `~`. Rotating it from the settings page
(retyping or "Generate") keeps the old value as `tracker.token_previous`, and
the tracker keeps accepting that one, unattributed, until you finalise the
rotation or clear the token — clearing removes both. See the README's
["The tracker"](../README.md#the-tracker).

## `[[path_map]]`

Rewrite rules between the three views of the library — how Sonarr/Radarr see
a path, how sharerr sees it, and how the torrent client sees it. Empty means
all three agree.

| Field     | Type | Notes                                                                                                                                       |
| --------- | ---- | ------------------------------------------------------------------------------------------------------------------------------------------- |
| `arr`     | path | Prefix as Sonarr/Radarr report it.                                                                                                          |
| `sharerr` | path | Prefix as the sharerr process sees it.                                                                                                      |
| `qbit`    | path | Prefix as the torrent client sees it. Defaults to `sharerr`'s value when omitted, despite the name predating rTorrent/Transmission support. |

```toml
[[path_map]]
arr = "/tv"
sharerr = "/media/tv"
qbit = "/downloads/tv"
```

## `[gossip]`

Endpoint gossip with friends — see `sharerr::gossip`'s module docs for the
protocol. One key, config-file only: there is no settings-page field for
it, unlike every other interval in this file (`sync.interval_secs`,
`gluetun.poll_secs`). Those two clamp to a floor before a form ever renders
them; this one deliberately has none, since the only reason to set it below
the default is a test bed of several real sharerr nodes converging in
seconds rather than the production default's fifteen minutes — see
`docs/TESTING.md`'s "Tier 3" section. A web field with no floor would be a
standing invitation to set it to a few seconds in production and hammer
every friend's instance; `sharerr.toml` or a `SHARERR_GOSSIP__*` override
still reaches it.

| TOML key               | Type | Default | Notes                                                                               |
| ---------------------- | ---- | ------- | ----------------------------------------------------------------------------------- |
| `gossip.exchange_secs` | int  | `900`   | How often the outbound exchange runs against friends with a `gossip_url`. No floor. |

## `[retention]`

How much history the database keeps of the three tables that grow on their
own. Each is pruned on write to its limit, so a lowered value takes effect at
the next write to that table, a saved config included, with no restart.
Config-file only, like `[gossip]`: no settings-page field, since the
defaults suit every ordinary deployment and the one reason to change them is
a host short of disk or a Diagnostics chart that should span longer than a
fortnight. `SHARERR_RETENTION__*` reaches them too.

| TOML key                     | Type | Default | Notes                                                                                                       |
| ---------------------------- | ---- | ------- | ----------------------------------------------------------------------------------------------------------- |
| `retention.swarm_samples`    | int  | `336`   | Hourly swarm samples kept in total, behind the Diagnostics chart. 336 is a fortnight.                       |
| `retention.endpoint_history` | int  | `5`     | Addresses kept per friend and endpoint kind. Enough to span a few reconnects; more would be a location log. |
| `retention.sync_runs`        | int  | `0`     | Sync runs kept, newest first. `0` keeps every run.                                                          |

## `[lighthouse]`

See [`LIGHTHOUSE.md`](LIGHTHOUSE.md) for the design and for running one. `enabled`
controls _hosting_ one on this instance's own listener; `lighthouse.urls`
(below the _client_ half) is independent — consuming a friend's lighthouse
needs nothing here, and hosting one for friends needs nothing set there.
`interval_secs` and `quiet_secs` are config-file only for the same reason
`gossip.exchange_secs` is — see `[gossip]` above.

| TOML key                   | Type                    | Default    | Notes                                                                                                              |
| -------------------------- | ----------------------- | ---------- | ------------------------------------------------------------------------------------------------------------------ |
| `lighthouse.enabled`       | bool                    | `false`    | Run the lighthouse as extra routes on one of sharerr's own listeners.                                              |
| `lighthouse.mount`         | `frontend` \| `tracker` | `frontend` | Which listener, when `enabled`.                                                                                    |
| `lighthouse.urls`          | list of urls            | `[]`       | Lighthouse(s) this instance reports its own endpoint to and queries for a quiet friend — independent of `enabled`. |
| `lighthouse.interval_secs` | int                     | `900`      | How often the report-and-lookup pass runs. No floor; matches `gossip.exchange_secs` by default on purpose.         |
| `lighthouse.quiet_secs`    | int                     | `3600`     | How long a peer must go unseen before a lighthouse lookup is worth trying. No floor.                               |

## `[gluetun]` and `[gluetun_client]`

For a dynamic endpoint behind a VPN with provider port forwarding — see the
README's ["A dynamic endpoint (gluetun)"](../README.md#a-dynamic-endpoint-gluetun).
`[gluetun]` resolves the _tracker's_ endpoint; `[gluetun_client]` is an
independent second poller for the torrent client's own tunnel, when it is a
separate one — see [`docker/deploy/dual-vpn/`](https://github.com/ivylikethevine/sharerr-rs/tree/main/docker/deploy/dual-vpn).

| TOML key                     | Type | Default | Notes                                                                                                       |
| ---------------------------- | ---- | ------- | ----------------------------------------------------------------------------------------------------------- |
| `gluetun.enabled`            | bool | `true`  | Independent of whether `control_url` is set, so pausing is a checkbox rather than blanking a saved address. |
| `gluetun.control_url`        | url  | unset   | gluetun's control server. `None` disables resolution entirely, same as `enabled = false`.                   |
| `gluetun.poll_secs`          | int  | `60`    | Floor of 10s.                                                                                               |
| `gluetun_client.enabled`     | bool | `true`  | Same shape as above, for the client's own tunnel.                                                           |
| `gluetun_client.control_url` | url  | unset   |                                                                                                             |
| `gluetun_client.poll_secs`   | int  | `60`    |                                                                                                             |

Vault secrets: `gluetun.api_key`, `gluetun_client.api_key`. **Not optional.**
gluetun's control server has required a credential on every route since
v3.39.1, and sharerr skips the poll rather than send a request that can only
come back `401`, so a `control_url` with no matching key is inert and looks
identical to one that is working. `sharerr doctor` names the missing key. How
to mint the key on gluetun's side, and the three-route role a per-route
config needs, is in
[`docker/deploy/README.md`](https://github.com/ivylikethevine/sharerr-rs/blob/main/docker/deploy/README.md),
which wires all of this up for five deployment shapes.

## `[sync]`

| TOML key             | Type | Default | Notes                                                      |
| -------------------- | ---- | ------- | ---------------------------------------------------------- |
| `sync.enabled`       | bool | `true`  | Run the reconciliation loop on a timer while `serve` runs. |
| `sync.interval_secs` | int  | `900`   | Floor of 60s.                                              |

### Syncing on import

A pass also runs at once, with or without the timer, from Status's **Sync
now** button or from an \*arr Webhook connection. In Sonarr, Radarr, Lidarr,
Readarr or Whisparr, add Settings → Connect → Webhook with:

- **URL**: `http://<sharerr>:8477/hooks/arr`
- **Method**: `POST`
- **Triggers**: On Import (or On Release Import), and On Series, Movie,
  Artist or Book Add; any other trigger runs a pass too

The connection's Test sends an event sharerr answers without a pass. The
payload is never trusted, so the route has no credential; it answers only
private addresses (the same host, Docker network or LAN), like gluetun's
hooks. Webhooks that arrive during a pass coalesce into one more pass after
it.

## `[checks]`

| TOML key              | Type | Default | Notes                                                                                                                                                                                                                      |
| --------------------- | ---- | ------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `checks.reachability` | bool | `false` | Dial this instance's own advertised tracker and feed addresses and report whether they accept a TCP connection. Opt-in because many NAT setups refuse hairpinning, which would show a scary failure on a healthy instance. |

See the README's
["Checking that you are actually reachable"](../README.md#checking-that-you-are-actually-reachable).

## `[notifications]`

A webhook fired on whichever triggers below are enabled — see the README's
mentions under ["Friends finding each other"](../README.md#friends-finding-each-other).
Settings → Notifications has a _Send a test notification_ button that posts
one `test` event through the stored URL and shape, whatever the triggers
say, and shows what the service answered.

| TOML key                        | Type             | Default           | Notes                                                                                                                                                          |
| ------------------------------- | ---------------- | ----------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `notifications.kind`            | string           | `generic`         | Which payload shape to send: `generic`, `discord`, `apprise`, `ntfy`, `gotify`, `telegram` or `pushover`. Each one's webhook URL is below.                     |
| `notifications.peer_quiet_secs` | int              | `604800` (7 days) | `0` turns the peer-quiet check off, independent of `triggers` below.                                                                                           |
| `notifications.heartbeat_secs`  | int              | `60`              | How often the heartbeat push fires. `0` turns it off without clearing the URL. Match the monitor's own interval.                                               |
| `notifications.triggers`        | array of strings | all ten, below    | Which triggers actually fire. A webhook being configured is necessary but not sufficient — a trigger not listed here stays silent regardless of what fires it. |

The twelve triggers, by their TOML spelling: `sync_failed`, `peer_quiet`,
`endpoint_rotated` (the gluetun-resolved advertised address changing —
usually the one most likely to silently break a friend's downloads),
`items_shared` and `item_failed` (each digested into one notification per
sync pass rather than one per item), `peer_revoked`, `peer_first_contact`
(a friend's first authenticated feed request or attributed tracker
announce — the moment a key you handed out was actually used),
`tracker_unreachable` (your own advertised tracker address stopped
accepting connections after having been confirmed reachable; needs
[`checks.reachability`](#checks), dials every fifteen minutes, and never
fires on an address that was never confirmed, so a router that refuses
hairpinning stays silent), `library_unreadable` (a `[[library]]` path that
could not be read or only partly listed this sync pass, once per distinct
reason rather than once per pass), `torrent_interrupted` (a torrent sharerr
built is stopped in the client, or was removed from it and the pass added it
back), `file_changed` (a shared file's inode or modification time differs
from when sharerr built its torrent, so friends' downloads fail the piece
checks; Force rebuild on the item page builds a new one), and `heartbeat`.
Those two fire once per torrent, not once per pass, and only for torrents
sharerr built; nothing is resumed or rebuilt for you. A mount whose inode
numbers change on every remount (CIFS with `noserverino`) reads as a change
after each remount.

`heartbeat` is the odd one out: it never posts to the webhook. It is an
Uptime-Kuma-style push — a plain GET to `notifications.heartbeat_url`, every
`heartbeat_secs`, only while the instance would answer `/ready` with 200 — so
the monitor on the other end notices when the pushes _stop_. Paste the
monitor's push URL whole, query string included.

The webhook URL carries everything a kind needs:

- `generic`: any URL that takes `{"event": ..., "message": ...}`.
- `discord`: the channel's webhook URL.
- `apprise`: the API server's `/notify` (or `/notify/<key>`) URL.
- `ntfy`: the topic's own URL, `https://ntfy.sh/<topic>`, with `?auth=` for a
  protected topic. sharerr publishes to the server root with the topic in the
  body.
- `gotify`: `https://<server>/message?token=<app token>`.
- `telegram`: `https://api.telegram.org/bot<token>/sendMessage?chat_id=<chat>`.
- `pushover`:
  `https://api.pushover.net/1/messages.json?token=<app token>&user=<user key>`.

For `telegram` and `pushover` those parameters move from the query into the
request body, where the service reads them. A URL missing one sends nothing
and logs which.

Vault secrets: `notifications.webhook_url` and `notifications.heartbeat_url`
— in the vault rather than `sharerr.toml` even though neither is
credential-shaped at a glance, because a Discord webhook URL embeds its own
bearer token in the path, as most kinds above carry one somewhere, and a
push URL embeds the monitor's.

## `[metrics]`

`/metrics` (OpenMetrics, for Prometheus) and `/dashboard` (JSON, for
Homepage, Homarr, or Glance). Both off by default and both require the bearer
token below once enabled: unlike `/health` and `/ready`, they reveal how much
this instance is sharing and to how many friends.

| TOML key          | Type | Default | Notes                                           |
| ----------------- | ---- | ------- | ----------------------------------------------- |
| `metrics.enabled` | bool | `false` | Serve `/metrics` and the dashboard-widget JSON. |

Vault secret: `metrics.token` — the bearer token both endpoints require,
sent as `Authorization: Bearer <token>`. There is no unauthenticated form of
either endpoint; a request with a missing or wrong token gets the same `404`
as when `metrics.enabled` is `false`, so a probe cannot tell "off" from
"wrong token" from "instance does not exist".

## `[api]`

The read-only management API: JSON for scripts and dashboards that need more
than `/dashboard`'s counts. Off by default.

| TOML key      | Type | Default | Notes                                   |
| ------------- | ---- | ------- | --------------------------------------- |
| `api.enabled` | bool | `false` | Serve the management API at `/api/v1/`. |

| Route                 | Returns                                                                 |
| --------------------- | ----------------------------------------------------------------------- |
| `GET /api/v1/items`   | Every item: source, kind, titles, state, last error, size, info hash    |
| `GET /api/v1/friends` | Every friend: label, scope, when created, last seen and revoked, totals |
| `GET /api/v1/sync`    | Whether sync runs on a timer, what blocks it, the last 20 runs          |
| `GET /api/v1/doctor`  | The `sharerr doctor --json` document, run on request, never `--fix`     |

Vault secret: `api.token`, sent as `Authorization: Bearer <token>`. Its own
token, never `metrics.token`: the API names every shared file and friend, the
metrics none, so a Prometheus scrape token must not read it. A missing or
wrong token gets the same `404` as `api.enabled = false`. Nothing here
writes, and no friend's key or key hash is ever in a response. The schemas
are in [the API document](API.md).

## Backup and restore

The settings page's "Backup and restore" section downloads the _effective_
config — defaults, `sharerr.toml`, and any `SHARERR_*` overrides, merged —
rather than the raw file, since an env-overridden field is never written to
the file at all (see "How configuration is layered" above). Restoring
replaces `sharerr.toml` wholesale after validating the pasted document the
same way a normal save does, moving the previous file aside as
`sharerr.toml.invalid` rather than overwriting it.

Deliberately narrower than "everything": nothing in the vault (API keys, the
tracker token, this instance's Ed25519 signing key) or the peers table
(friends and their scopes) is exported or touched by this section's
import/export — both live outside `sharerr.toml` entirely, and neither
survives losing the data directory on their own. The peers table has its own,
separate restore path — see below.

### Backing up `/data`

The section above only covers `sharerr.toml`, in `/config`. The vault
(`vault.bin`), the database (`sharerr.db`), and the generated `.torrent`
files — all of `/data` — have no equivalent button, because there is
nothing web-safe to build: this is a volume-level backup, the same as any
other stateful container.

`vault.bin` is written tmp-then-rename, so a plain filesystem copy is
always either the old file or the new one, never a torn write. `sharerr.db`
runs in WAL mode, so it is **not** safe to copy live with a plain `cp` —
either stop the container first and copy `/data`, or take a consistent copy
of the database while sharerr keeps running with
`sqlite3 /data/sharerr.db ".backup /somewhere/sharerr.db.bak"`. A
volume-level snapshot (LVM, ZFS, Btrfs, or your storage driver's own
snapshot feature) covers the whole directory atomically and is the simplest
option where it's available.

**Losing `SHARERR_MASTER_KEY` loses every credential in the vault, with no
recovery path** — a `/data` backup restores nothing without the same key
that encrypted it, so back the key up too, the same way you would any other
credential (a password manager, not a note left beside the backup). See
[`SECURITY.md`](SECURITY.md#what-is-in-scope) for why there is deliberately
no recovery path.

### Restoring friends after a full data-directory loss

A friend's own key _into_ this instance is stored as a one-way hash and can
never be recovered — that part always means reissuing a fresh key and
re-sending it, no matter what. Everything else about a friendship — its
label, scope, last-known address, and the gossip key _they_ issued to _this_
instance (a credential this instance genuinely does hold, in the vault) —
can be restored.

**Before losing the data directory**, the Friends page has its own
"export as backup block" link, next to the friends list. It downloads a
`sharerr-peers-export.toml` carrying every active (non-revoked) friend as a
`[[peers]]` array — save that file somewhere outside sharerr, the same way
you would any other credential (a password manager, an offline backup). A
revoked friend is deliberately left out: importing this block always creates
an _active_ peer, and a revoked friend flowing back through it would silently
un-revoke them. If the vault cannot be opened at export time, the file still
downloads with everything except gossip keys, and says so in a leading
comment.

**To restore**, hand-add that file's `[[peers]]` array into the new
instance's `sharerr.toml` — or write one from scratch, if nothing was ever
exported:

```toml
[[peers]]
label = "sam"
scope = "all"                                 # "all", "tv", "movies", "music", or "books"
max_size_bytes = 5368709120                   # optional: the largest file offered
max_height = 1080                             # optional: the highest resolution offered
last_addr = "203.0.113.5:51413"               # optional
gossip_url = "https://sam.example/sharerr"    # optional
gossip_key = "the key sam issued this instance"  # optional
```

This is read exactly once — on the next `sharerr serve` start, or immediately
if it is instead pasted into a running instance through Settings → Backup and
restore — and the block is removed from `sharerr.toml`, and from the running
instance's own configuration, the moment it has been applied: successfully
imported entries and skipped ones (a duplicate label, most likely) alike, so
the same block is never replayed. Until that first read, `[[peers]]` is the
one place a secret is allowed to sit in `sharerr.toml` at all; if the vault
cannot be opened yet (no `SHARERR_MASTER_KEY`) and any entry carries a
`gossip_key`, nothing is imported and the block is left in place to retry,
rather than importing everything except the one credential
that had nowhere to go.

## Vault secrets

Every secret sharerr reads, all set the same way:

```bash
printf %s "$VALUE" | sharerr vault set <key>
sharerr vault list      # which keys are set, never their values
sharerr vault remove <key>
```

| Vault key                     | What it is                                                                                          |
| ----------------------------- | --------------------------------------------------------------------------------------------------- |
| `sonarr.api_key`              | Sonarr API key                                                                                      |
| `radarr.api_key`              | Radarr API key                                                                                      |
| `lidarr.api_key`              | Lidarr API key                                                                                      |
| `readarr.api_key`             | Readarr API key                                                                                     |
| `whisparr.api_key`            | Whisparr API key                                                                                    |
| `qbittorrent.api_key`         | qBittorrent 5.2+ WebUI API key                                                                      |
| `transmission.password`       | Transmission RPC password                                                                           |
| `rtorrent.password`           | rTorrent Basic Auth password (see [`[rtorrent]`](#rtorrent))                                        |
| `deluge.password`             | Deluge web UI password                                                                              |
| `tracker.token`               | Built-in tracker announce token                                                                     |
| `gluetun.api_key`             | Tracker-facing gluetun control server API key                                                       |
| `gluetun_client.api_key`      | Torrent-client-facing gluetun control server API key                                                |
| `notifications.webhook_url`   | Where every enabled trigger except `heartbeat` is POSTed (see [`[notifications]`](#notifications))  |
| `notifications.heartbeat_url` | Uptime-Kuma-style push URL fetched on a timer while ready (see [`[notifications]`](#notifications)) |
| `metrics.token`               | Bearer token `/metrics` and the dashboard widget require (see [`[metrics]`](#metrics))              |
| `api.token`                   | Bearer token the management API requires (see [`[api]`](#api))                                      |

Those sixteen are the keys `sharerr vault set` accepts. `sharerr vault list`
shows every key the vault holds, which includes a few sharerr manages
itself and `vault set` refuses: `tracker.token_previous` (the rotated-out
announce token, see [`[tracker]`](#tracker)), `identity.signing_key` (this
instance's gossip signing key, generated on first use),
`lighthouse.decoy_seed` (the embedded lighthouse's decoy-answer seed, same),
and one `peer.gossip.<id>` per friend, managed from the Friends page instead
of Settings.

## Environment variable overrides

Any `sharerr.toml` field can also be set as a `SHARERR_*` environment
variable — the dotted path, uppercased, with `.` replaced by `__`:

```text
qbittorrent.url        -> SHARERR_QBITTORRENT__URL
rtorrent.username       -> SHARERR_RTORRENT__USERNAME
tracker.advertised_host -> SHARERR_TRACKER__ADVERTISED_HOST
```

These take precedence over `sharerr.toml`, so a field pinned by a variable
cannot be changed from the web UI — its input renders disabled, naming the
variable, rather than accepting a save that would be silently discarded.

Every variable also takes a `_FILE` suffix, for a value mounted as a Docker
or Swarm secret: `SHARERR_QBITTORRENT__URL_FILE=/run/secrets/qbit_url` reads
the value from that file, surrounding whitespace trimmed, so a file written
with `echo` works. A file that cannot be read is a startup error naming the
variable. When both forms are set the plain variable wins, and a `_FILE`
form pins its field in the UI just as the plain one does.

`SHARERR_MASTER_KEY` (and `SHARERR_MASTER_KEY_FILE`, pointing at a Docker
secret) is the one setting that has no `sharerr.toml` equivalent at all,
since it is what encrypts the vault the file's own secrets would otherwise
need to unlock. It, `SHARERR_CONFIG`, `SHARERR_LOG_FORMAT` (the CLI's
`--log-format`, read before the config loads), and the opt-in test suites'
own `SHARERR_E2E_*` variables (tiers 2 and 3) are the only `SHARERR_*` names
that are not config fields; any other unrecognised `SHARERR_*` variable is
a startup error, same as an unknown key in the file.

The `sharerr-lighthouse` binary reads no `sharerr.toml` and has three
variables of its own: `LIGHTHOUSE_BIND` (default `0.0.0.0:7878`),
`LIGHTHOUSE_SECRET_FILE` (default `/data/lighthouse.secret`) and
`LIGHTHOUSE_LOG_FORMAT` (`text` or `json`). See
[`LIGHTHOUSE.md`](LIGHTHOUSE.md#running-one).
