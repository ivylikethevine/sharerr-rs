# Settings reference

Every `sharerr.toml` field, environment variable, and vault secret sharerr
reads, in one place. [The README](../README.md) is where to go for
walkthroughs and _why_; this page is where to go for the exact key, its
default, and what sets it.

## Table of contents

- [How configuration is layered](#how-configuration-is-layered)
- [Top-level settings](#top-level-settings)
- [`[server]`](#server)
- [\*arr apps](#arr-apps)
- [`[[library]]` — plain directories](#library--plain-directories)
- [Torrent client](#torrent-client)
  - [`[qbittorrent]`](#qbittorrent)
  - [`[transmission]`](#transmission)
  - [`[rtorrent]`](#rtorrent)
- [`[seeding]`](#seeding)
- [`[tracker]`](#tracker)
- [`[[path_map]]`](#path_map)
- [`[lighthouse]`](#lighthouse)
- [`[gluetun]` and `[gluetun_client]`](#gluetun-and-gluetun_client)
- [`[sync]`](#sync)
- [`[checks]`](#checks)
- [`[notifications]`](#notifications)
- [`[metrics]`](#metrics)
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

| TOML key      | Type        | Default        | Notes                                                                                                                                |
| ------------- | ----------- | -------------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| `server.bind` | socket addr | `0.0.0.0:8477` | Carries the web UI, the tracker, and the Torznab feed on one port, unless [`[tracker]`](#tracker) opens a second dedicated listener. |

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

Zero-dependency sharing with no *arr app at all — see the README's
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

`torrent_backend` selects which of the three sections below actually seeds.
All three sections' settings are kept regardless of which is selected, so a
client can be filled in and tested before switching to it.

| TOML key          | Type                                          | Default       |
| ----------------- | --------------------------------------------- | ------------- |
| `torrent_backend` | `qbittorrent` \| `transmission` \| `rtorrent` | `qbittorrent` |

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
[`SUPPORT.md`](SUPPORT.md#torrent-clients-what-actually-seeds).

## `[seeding]`

Applied once, at the moment sharerr hands a torrent to whichever client is
selected — never re-applied or enforced afterward. See the README's
["Seeding limits"](../README.md#seeding-limits).

| TOML key                   | Type  | Default | Notes                                                  |
| -------------------------- | ----- | ------- | ------------------------------------------------------ |
| `seeding.upload_limit_kib` | int   | unset   | Per-torrent upload cap, KiB/s.                         |
| `seeding.ratio_limit`      | float | unset   | Seed-ratio goal. Not honoured by rTorrent — see above. |

## `[tracker]`

| TOML key                  | Type        | Default              | Notes                                                                                                                                                           |
| ------------------------- | ----------- | -------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `tracker.advertised_host` | string      | unset (required*)    | Hostname/IP friends reach the tracker on.                                                                                                                       |
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

## `[lighthouse]`

See [`LIGHTHOUSE.md`](LIGHTHOUSE.md) for the design and for running one. `enabled`
controls _hosting_ one on this instance's own listener; `lighthouse.urls`
(below the _client_ half) is independent — consuming a friend's lighthouse
needs nothing here, and hosting one for friends needs nothing set there.

| TOML key             | Type                    | Default    | Notes                                                                                                              |
| -------------------- | ----------------------- | ---------- | ------------------------------------------------------------------------------------------------------------------ |
| `lighthouse.enabled` | bool                    | `false`    | Run the lighthouse as extra routes on one of sharerr's own listeners.                                              |
| `lighthouse.mount`   | `frontend` \| `tracker` | `frontend` | Which listener, when `enabled`.                                                                                    |
| `lighthouse.urls`    | list of urls            | `[]`       | Lighthouse(s) this instance reports its own endpoint to and queries for a quiet friend — independent of `enabled`. |

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
which wires all of this up for four deployment shapes.

## `[sync]`

| TOML key             | Type | Default | Notes                                                      |
| -------------------- | ---- | ------- | ---------------------------------------------------------- |
| `sync.enabled`       | bool | `true`  | Run the reconciliation loop on a timer while `serve` runs. |
| `sync.interval_secs` | int  | `900`   | Floor of 60s.                                              |

## `[checks]`

| TOML key              | Type | Default | Notes                                                                                                                                                                                                                       |
| --------------------- | ---- | ------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `checks.reachability` | bool | `false` | Dial this instance's own advertised tracker and feed addresses and report whether they accept a TCP connection. Opt-in because many NAT setups refuse hairpinning, which would show a scary failure on a healthy instance.  |

See the README's
["Checking that you are actually reachable"](../README.md#checking-that-you-are-actually-reachable).

## `[notifications]`

A webhook fired on whichever triggers below are enabled — see the README's
mentions under ["Friends finding each other"](../README.md#friends-finding-each-other).

| TOML key                        | Type                                | Default           | Notes                                                                                                                                                          |
| ------------------------------- | ----------------------------------- | ----------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `notifications.kind`            | `generic` \| `discord` \| `apprise` | `generic`         | Which payload shape to send.                                                                                                                                   |
| `notifications.peer_quiet_secs` | int                                 | `604800` (7 days) | `0` turns the peer-quiet check off, independent of `triggers` below.                                                                                           |
| `notifications.triggers`        | array of strings                    | all six, below    | Which triggers actually fire. A webhook being configured is necessary but not sufficient — a trigger not listed here stays silent regardless of what fires it. |

The six triggers, by their TOML spelling: `sync_failed`, `peer_quiet`,
`endpoint_rotated` (the gluetun-resolved advertised address changing —
usually the one most likely to silently break a friend's downloads),
`items_shared` and `item_failed` (each digested into one notification per
sync pass rather than one per item), and `peer_revoked`.

Vault secret: `notifications.webhook_url` — in the vault rather than
`sharerr.toml` even though it is not credential-shaped at a glance, because a
Discord webhook URL embeds its own bearer token in the path.

## `[metrics]`

`/metrics` (OpenMetrics, for Prometheus) and `/dashboard` (JSON, for
Homepage, Homarr, or Glance). Both off by default and both require the bearer
token below once enabled: unlike `/health` and `/ready`, they reveal how much
this instance is sharing and to how many friends.

| TOML key           | Type | Default | Notes                                           |
| ------------------ | ---- | ------- | ----------------------------------------------- |
| `metrics.enabled`  | bool | `false` | Serve `/metrics` and the dashboard-widget JSON. |

Vault secret: `metrics.token` — the bearer token both endpoints require,
sent as `Authorization: Bearer <token>`. There is no unauthenticated form of
either endpoint; a request with a missing or wrong token gets the same `404`
as when `metrics.enabled` is `false`, so a probe cannot tell "off" from
"wrong token" from "instance does not exist".

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

| Vault key                   | What it is                                                                             |
| --------------------------- | -------------------------------------------------------------------------------------- |
| `sonarr.api_key`            | Sonarr API key                                                                         |
| `radarr.api_key`            | Radarr API key                                                                         |
| `lidarr.api_key`            | Lidarr API key                                                                         |
| `readarr.api_key`           | Readarr API key                                                                        |
| `whisparr.api_key`          | Whisparr API key                                                                       |
| `qbittorrent.api_key`       | qBittorrent 5.2+ WebUI API key                                                         |
| `transmission.password`     | Transmission RPC password                                                              |
| `rtorrent.password`         | rTorrent Basic Auth password (see [`[rtorrent]`](#rtorrent))                           |
| `tracker.token`             | Built-in tracker announce token                                                        |
| `gluetun.api_key`           | Tracker-facing gluetun control server API key                                          |
| `gluetun_client.api_key`    | Torrent-client-facing gluetun control server API key                                   |
| `notifications.webhook_url` | Where a sync-failure/peer-quiet notification is POSTed                                 |
| `metrics.token`             | Bearer token `/metrics` and the dashboard widget require (see [`[metrics]`](#metrics)) |

Those thirteen are the keys `sharerr vault set` accepts. `sharerr vault list`
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
`SHARERR_MASTER_KEY` (and `SHARERR_MASTER_KEY_FILE`, pointing at a Docker
secret) is the one setting that has no `sharerr.toml` equivalent at all,
since it is what encrypts the vault the file's own secrets would otherwise
need to unlock. It, `SHARERR_CONFIG`, and the tier-2 test suite's
`SHARERR_E2E_*` variables are the only `SHARERR_*` names that are not config
fields; any other unrecognised `SHARERR_*` variable is a startup error, same
as an unknown key in the file.

The `sharerr-lighthouse` binary reads no `sharerr.toml` and has two
variables of its own: `LIGHTHOUSE_BIND` (default `0.0.0.0:7878`) and
`LIGHTHOUSE_SECRET_FILE` (default `/data/lighthouse.secret`). See
[`LIGHTHOUSE.md`](LIGHTHOUSE.md#running-one).
