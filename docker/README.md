# The compose test stack

A disposable Sonarr + Radarr + Lidarr + qBittorrent + Prowlarr stack for exercising
sharerr against the real services. **Entirely optional** — the default `cargo test`
suite is hermetic and needs none of this.

> **Not for deploying.** The stacks in this directory build the image from
> source, seed synthetic fixtures, publish on throwaway ports and are torn down
> with `-v`. To actually run sharerr, use [`deploy/`](deploy/), which has four
> layouts and a chooser for picking between them.

Everything in `tests/fixtures/media` is synthetic: invented titles, seeded
pseudo-random bytes, `FAKEGRP` release names. No real content is involved anywhere.

## Table of contents

- [Running it](#running-it)
- [Exercising the indexer and the tracker](#exercising-the-indexer-and-the-tracker)
- [The opt-in test suite](#the-opt-in-test-suite)
- [Seeding tagged content](#seeding-tagged-content)
  - [The network used to be `internal: true`](#the-network-used-to-be-internal-true)
- [Views of one library](#views-of-one-library)
- [Ports](#ports)
- [The Transmission stack](#the-transmission-stack)
- [The rTorrent stack](#the-rtorrent-stack)
- [The VPN stack](#the-vpn-stack)
  - [There is a WireGuard server in the stack](#there-is-a-wireguard-server-in-the-stack)
  - [Ports](#ports-1)
- [Tearing down](#tearing-down)

## Running it

```bash
./run_docker_tests.sh
```

That is the whole runbook, and it is the supported path: generate the fixtures,
bring the stack up, seed tagged content, load the credentials each app generated,
run `doctor`, run the opt-in suite, tear down. Every step is idempotent and the
teardown runs however the script exits.

The steps, if you want to drive them by hand:

```bash
# 1. Generate the synthetic library (idempotent — same bytes every time).
cargo run -p sharerr-testkit --bin gen-fixtures -- tests/fixtures/media

# 2. Bring the stack up. Sonarr, Radarr, and Lidarr keep their config in
#    ./docker/state, which has to exist first — see "Seeding tagged content" below.
mkdir -p docker/state/sonarr docker/state/radarr docker/state/lidarr
docker compose -f docker/compose.test.yml up -d --build

# 3. Give the *arr apps something tagged. They must be stopped for this.
docker compose -f docker/compose.test.yml stop sonarr radarr lidarr
cargo run -p sharerr-testkit --bin seed-arr -- \
    --sonarr docker/state/sonarr/sonarr.db \
    --radarr docker/state/radarr/radarr.db \
    --lidarr docker/state/lidarr/lidarr.db
docker compose -f docker/compose.test.yml start sonarr radarr lidarr

# 4. Collect API keys from each app's config.
SONARR_KEY=$(sed -n 's:.*<ApiKey>\(.*\)</ApiKey>.*:\1:p' docker/state/sonarr/config.xml)
RADARR_KEY=$(sed -n 's:.*<ApiKey>\(.*\)</ApiKey>.*:\1:p' docker/state/radarr/config.xml)
LIDARR_KEY=$(sed -n 's:.*<ApiKey>\(.*\)</ApiKey>.*:\1:p' docker/state/lidarr/config.xml)

# qBittorrent prints a temporary admin password to its log on first start. That
# password is only good for logging in to mint the WebUI *API key* sharerr's
# client actually authenticates with (Options → Web UI → API key in the WebUI at
# http://127.0.0.1:18080/, or `rotateAPIKey` over the API the way the script
# does it — see `qbittorrent_api_key` in run_docker_tests.sh).
docker compose -f docker/compose.test.yml logs qbittorrent | grep -i password
QBIT_KEY=qbt_...   # the key the WebUI (or rotateAPIKey) handed back

# 5. Load them into sharerr. Either open http://127.0.0.1:18477/ and paste them
#    into Settings, or pipe them in — the two write to the same vault. Piped on
#    stdin rather than interpolated into `sh -c`, so a value containing a quote
#    cannot break out of the command.
for pair in "sonarr.api_key=$SONARR_KEY" "radarr.api_key=$RADARR_KEY" \
            "lidarr.api_key=$LIDARR_KEY" "qbittorrent.api_key=$QBIT_KEY"; do
    printf %s "${pair#*=}" | docker compose -f docker/compose.test.yml \
        exec -T sharerr sharerr vault set "${pair%%=*}"
done

# 6. Check the wiring before trying to sync. The UI's per-service "Test
#    connection" buttons cover the same ground for the services themselves;
#    `doctor` additionally resolves the path mappings, which is what the
#    deliberately-disagreeing mounts in this stack exist to exercise.
docker compose -f docker/compose.test.yml exec sharerr sharerr doctor

# 7. And a real sync.
docker compose -f docker/compose.test.yml exec sharerr sharerr sync
```

The stack sets `SHARERR_MASTER_KEY`, without which the vault cannot be opened and
neither the UI nor the CLI can store a credential. A real deployment must set it
too — see the root `README.md`.

## Exercising the indexer and the tracker

With the stack up and something tagged and synced:

The Torznab endpoint authenticates against a *friend's* key, not a shared
instance secret — there is no vault entry for it. Create the operator account on
first visit (`/setup`), then add a friend on the Friends page (`/peers`); the key
is shown exactly once. `run_docker_tests.sh` does both over HTTP, since neither
has a CLI shortcut and `sharerr-data` is a named volume `seed-arr` cannot reach.

```bash
# What a friend's Prowlarr would fetch.
curl -s "http://127.0.0.1:18477/api?t=caps&apikey=$FRIEND_KEY"
curl -s "http://127.0.0.1:18477/api?t=tvsearch&apikey=$FRIEND_KEY"
```

A normal run covers the feed further than that: it asserts `t=caps` on both the
native and the Jackett-shaped path (`/api/v2.0/indexers/sharerr/results/torznab/api`),
that Jackett's `server/config` never echoes a key back, and — after a sync — that a
real Sonarr (and, on the plain stack, Lidarr) accepts the instance as a Torznab
indexer. The last one is the check that matters: Sonarr once refused the whole feed
over a missing `pubDate`, and only a real client caught it.

Prowlarr is in the stack behind an opt-in profile, since nothing automated uses it:

```bash
docker compose -f docker/compose.test.yml --profile indexer up -d prowlarr
```

Then add a *Generic Torznab* indexer pointing at `http://sharerr:8477/api` with
that key.

sharerr's own tracker is the only one — there used to be a `[tracker] backend`
choice between it and qBittorrent's embedded tracker, and it was removed; a
`sharerr.toml` still naming `backend` now fails to load. `/announce` refuses
any info hash the instance is not sharing, so a `d14:failure reason...`
response to a made-up hash is the expected result, not a fault.

## The opt-in test suite

```bash
cargo test -p sharerr --features e2e -- --ignored --test-threads=1
```

The assertion that justifies the whole tier is in `crates/sharerr/tests/e2e.rs`:
after a sync through a real qBittorrent, every media file has the same **inode,
mtime, and length** it started with. A mock cannot prove that. Only a client that
genuinely tried to manage the files can.

Serialised with `--test-threads=1` because the tests share one stack.

## Seeding tagged content

sharerr shares what carries the `sharerr` tag and nothing else, so a stack with no
tagged content exercises nothing: `doctor` fails with `TagNotFound`, and `sync`
bails with "no library source could be scanned". Getting content in is the one part of
this stack that cannot go through the *arr APIs, and it is worth knowing why.

> **Adding a series, movie, or artist through the *arr API triggers a metadata
> lookup** against `services.sonarr.tv` / `api.radarr.video` / MusicBrainz. Every
> fixture title is invented, so the lookup finds nothing and the add fails.

That is a property of the fixtures, not of the network, so it holds however the
stack is wired. `seed-arr` therefore writes the rows straight into Sonarr's,
Radarr's, and Lidarr's own SQLite databases:

```bash
cargo run -p sharerr-testkit --bin seed-arr -- \
    --sonarr docker/state/sonarr/sonarr.db \
    --radarr docker/state/radarr/radarr.db \
    --lidarr docker/state/lidarr/lidarr.db
```

`--lidarr` is optional — the VPN and Transmission stacks carry no Lidarr
container, so their invocations omit it and seed only Sonarr and Radarr.

Three things about it:

- **Every named app must be stopped.** Each holds its database open and will not
  observe an external write while running. `run_docker_tests.sh` stops and starts
  them around the seed.
- **They keep their config in `./docker/state`**, bind-mounted rather than in a
  named volume, so a host-side seeder can open those files at all. `down -v` does
  not clear a bind mount, so teardown removes the directory too.
- **It is coupled to their schemas.** That is the cost of seeding this way, and it
  is why the image tags in `compose.test.yml` are pinned rather than `:latest`. The
  rows are minimal — enough for the endpoints sharerr reads (`tag`, `series`,
  `episodefile`+`episode`, `movie`, `artist`, `album`, `trackfile`+`track`), not
  enough for a metadata refresh. Lidarr's schema splits the descriptive half of
  both an artist and an album into their own tables (`ArtistMetadata`,
  `AlbumReleases`), the same way Radarr 5 splits `MovieMetadata` from `Movies` —
  and unlike Sonarr/Radarr's `EpisodeFiles`/`MovieFiles`, Lidarr's `TrackFiles`
  stores the file's *full* path rather than a path relative to the artist's own
  folder. Confirmed against a live container rather than assumed, the same way the
  original Sonarr/Radarr schema was.

### The network used to be `internal: true`

It is not any more. An internal bridge severs the host→container path that
*published ports* travel, and this stack's whole control plane runs over those
ports: the readiness probes curl `127.0.0.1`, the API keys are scraped from a bind
mount, `seed-arr` runs on the host, and the browser URLs above are host-side. With
the network isolated, those probes hung for their full timeout against containers
that were perfectly healthy.

The trade is explicit: the stack no longer *proves* sharerr makes no outbound
requests. That property now rests on the code and the hermetic test suite rather
than on the kernel refusing to route.

The tag id is left to SQLite. sharerr resolves the *label*, case-insensitively, so
`sharerr_testkit::TAG_ID` — a mock-server detail — deliberately does not apply here.

Everything else sharerr does stays inside the stack anyway: tag lookup, file
discovery, path resolution, torrent creation, and seeding.

## Views of one library

The mounts deliberately disagree, because in a real deployment they almost always
do, and identical mounts would hide every path-mapping bug:

| Who         | Sees the library at |
| ----------- | ------------------- |
| Sonarr      | `/tv`               |
| Radarr      | `/movies`           |
| Lidarr      | `/music`            |
| qBittorrent | `/downloads`        |
| sharerr     | `/media`            |

`docker/config/sharerr.toml` maps between them. Every media mount is `:ro` — sharerr
never needs to write to the content it shares, and the read-only flag turns that
from a promise into something the kernel enforces.

## Ports

All bound to `127.0.0.1` so the stack is not exposed on the network.

| Service           | Host port                  |
| ----------------- | -------------------------- |
| Sonarr            | 18989                      |
| Radarr            | 17878                      |
| Lidarr            | 18686                      |
| qBittorrent WebUI | 18080                      |
| Prowlarr          | 19696 (opt-in — see below) |
| sharerr           | 18477                      |

sharerr's port doubles as the tracker: friends announce to it directly, so in a
real deployment it has to be reachable from outside the container, not just on
the docker network.

## The Transmission stack

```bash
./run_docker_tests.sh --transmission
```

The same services and the same assertions, seeding through Transmission instead of
qBittorrent. It exists because the two clients differ in ways that no amount of
mocking establishes:

|                 | qBittorrent                                                                                    | Transmission                                   |
| --------------- | ---------------------------------------------------------------------------------------------- | ---------------------------------------------- |
| Tracker         | sharerr's own, same as every other client — see "Exercising the indexer and the tracker" above | same                                           |
| Categories      | a category plus tags                                                                           | one flat list of labels; both collapse into it |
| Skip hash check | supported                                                                                      | not supported; it always verifies              |
| Credentials     | a temporary password printed to the log on first start                                         | given up front by compose                      |

Neither client has a tracker of its own to fall back on any more — sharerr's
builtin tracker is the only one wired up, regardless of `torrent_backend` —
so this run asserts the same thing the plain stack does: built torrents carry
an announce URL from sharerr's own tracker.

Ports are offset again (38989, 37878, 39091, 38477) so all three stacks can run at
once.

## The rTorrent stack

```bash
./run_docker_tests.sh --rtorrent
```

The same services and the same assertions, seeding through rTorrent instead of
qBittorrent. Unlike the Transmission stack, the point here is not a difference in
behaviour an operator would notice — it is that `sharerr-rtorrent`'s unit tests run
against a hand-mocked XML-RPC server, which proves the crate parses the requests
and responses it *expects*, not the ones a real rTorrent sends. The XML-RPC parser
and throttle-method bugs fixed on 2026-08-24 were exactly the kind that server
could not have caught.

The image is `crazymax/rtorrent-rutorrent`, which bundles rTorrent, ruTorrent, and
an nginx proxy that answers plain HTTP XML-RPC POSTs over rTorrent's SCGI socket —
the "some HTTP proxy in front of the RPC endpoint" shape `sharerr-rtorrent`'s module
docs describe, since rTorrent itself speaks nothing but SCGI. No `.htpasswd` is
populated, so the proxy's Basic Auth is off; `[rtorrent]` in `docker/config-rt/`
carries a placeholder username and the vault a placeholder password, both ignored.

|                 | qBittorrent                                                | rTorrent                                          |
| --------------- | ----------------------------------------------------------- | -------------------------------------------------- |
| Tracker         | sharerr's own — see "Exercising the indexer and the tracker" above | same                                         |
| Categories      | a category plus tags                                        | one free-text `d.custom1` slot; both collapse into it |
| Skip hash check | supported                                                    | not supported; it always verifies                  |
| Credentials     | a temporary password printed to the log on first start      | none — the RPC proxy has no auth configured         |

Ports are offset again (48989, 47878, 48477, plus 48000 for the XML-RPC endpoint and
48080 for ruTorrent's own web UI) so all four stacks can run at once.

## The VPN stack

```bash
./run_docker_tests.sh --vpn
```

The same services and the same assertions, with qBittorrent inside a VPN
container's network namespace — which is close to standard practice in this
ecosystem and which nothing exercised until now. `docker/compose.vpn.yml` has the
full reasoning; the short version is that this is a genuinely different topology,
not a variation:

|                       | Plain stack               | VPN stack                                                              |
| --------------------- | ------------------------- | ---------------------------------------------------------------------- |
| qBittorrent's address | `http://qbittorrent:8080` | `http://gluetun:8080` — it has no network, and no DNS name, of its own |
| Its published ports   | on `qbittorrent`          | on `gluetun`; declaring them on qBittorrent is a compose error         |
| Announce address      | the machine               | the tunnel's exit                                                      |

The first row is the one that bites. `qbittorrent:8080` does not merely refuse the
connection — **the name does not resolve at all** — and the run asserts exactly that
rather than only asserting the right name works.

### There is a WireGuard server in the stack

So the suite needs no VPN subscription, no credentials, and no egress. `vpn`
terminates a real tunnel that gluetun really connects to, with both ends inside the
stack, so the namespace, routing, and firewall behaviour are real. The tunnel
simply does not go anywhere. Keys live in `docker/wireguard/wg0.conf` and are
committed on purpose — they are as secret as the `SHARERR_MASTER_KEY` literal in
`compose.test.yml`, which is to say not at all.

One non-obvious piece: gluetun health-checks its tunnel by reaching something on
the far side, and its default target is on the internet. A listener is parked
inside the tunnel for it to find (`PostUp` in `wg0.conf`). Without it gluetun
decides the tunnel is dead and restarts it every twenty seconds.

### Ports

Offset from the plain stack so both can run at once.

| Service           | Host port                    |
| ----------------- | ---------------------------- |
| Sonarr            | 28989                        |
| Radarr            | 27878                        |
| qBittorrent WebUI | 28080 (published by gluetun) |
| sharerr           | 28477                        |

Tear it down with the same two halves as the plain stack, against the other file:

```bash
docker compose -f docker/compose.vpn.yml down -v && rm -rf docker/state-vpn
```

## Tearing down

```bash
docker compose -f docker/compose.test.yml --profile indexer down -v && rm -rf docker/state
```

Both halves are needed. `-v` drops the named volumes, which is what you want
between runs — the API keys are regenerated on every fresh start, and qBittorrent
only logs its temporary password when it has no stored one. It does *not* touch
`docker/state`, where Sonarr, Radarr, and Lidarr keep theirs, so that goes separately.
`run_docker_tests.sh` does both from a trap.

If `rm -rf docker/state` fails with a permission error, Docker auto-created that
directory as root before the script could. Delete it as root instead:

```bash
docker run --rm -v "$PWD/docker:/w" alpine:3 rm -rf /w/state
```

The script pre-creates the directory to avoid this, and falls back to the same
command when it finds a tree an older run left behind.
