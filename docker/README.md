# The compose test stacks

Disposable Sonarr + Radarr + Lidarr + torrent-client + Prowlarr stacks for
exercising sharerr against the real services. **Entirely optional**: the
default `cargo test` suite is hermetic and needs none of this.

> **Not for deploying.** These stacks build the image from source, seed
> synthetic fixtures, publish on throwaway ports and are torn down with `-v`.
> To run sharerr, use [`deploy/`](deploy/README.md).

Everything in `tests/fixtures/media` is synthetic (see
[`docs/TESTING.md`](../docs/TESTING.md#fixtures)).

## Table of contents

- [Running it](#running-it)
- [Exercising the indexer and the tracker](#exercising-the-indexer-and-the-tracker)
- [The opt-in test suite](#the-opt-in-test-suite)
- [Seeding tagged content](#seeding-tagged-content)
- [Views of one library](#views-of-one-library)
- [The six stacks](#the-six-stacks)
- [Ports](#ports)
- [The two-instance stack](#the-two-instance-stack)
  - [Why Prowlarr sits in front of instance B's Radarr](#why-prowlarr-sits-in-front-of-instance-bs-radarr)
  - [Why `tracker.advertised_host` is a service name here, not `localhost`](#why-trackeradvertised_host-is-a-service-name-here-not-localhost)
- [The mesh stack](#the-mesh-stack)
- [Tearing down](#tearing-down)

## Running it

```bash
./scripts/run_docker_tests.sh                 # plain stack, qBittorrent
./scripts/run_docker_tests.sh --vpn
./scripts/run_docker_tests.sh --transmission
./scripts/run_docker_tests.sh --rtorrent
./scripts/run_docker_tests_two_instance.sh
```

That is the whole runbook and the supported path: generate the fixtures,
bring the stack up, seed tagged content, load the credentials each app
generated, run `doctor`, run the opt-in suite, tear down. Every step is
idempotent and the teardown runs however the script exits. Read the script
for the by-hand equivalent of any step; the two that are not obvious from
the compose file are below (seeding, and minting qBittorrent's API key).

qBittorrent prints a temporary admin password to its log on first start.
That password is only good for logging in to mint the WebUI _API key_
sharerr's client authenticates with (Options → Web UI → API key at
`http://127.0.0.1:18080/`, or `rotateAPIKey` over the API, which is what
`qbittorrent_api_key` in the script does). Load keys with
`printf %s "$KEY" | docker compose -f docker/compose.test.yml exec -T sharerr sharerr vault set <key>`,
or paste them into Settings at `http://127.0.0.1:18477/`; both write the same
vault. The stack sets `SHARERR_MASTER_KEY` in the compose file, which is fine
here and nowhere else.

## Exercising the indexer and the tracker

The Torznab endpoint authenticates against a _friend's_ key, not a shared
instance secret. Create the operator account on first visit (`/setup`), then
add a friend on `/peers`; the key is shown once. The script does both over
HTTP, since neither has a CLI shortcut.

```bash
curl -s "http://127.0.0.1:18477/api?t=caps&apikey=$FRIEND_KEY"
curl -s "http://127.0.0.1:18477/api?t=tvsearch&apikey=$FRIEND_KEY"
```

A normal run asserts `t=caps` on both the native and the Jackett-shaped path,
that Jackett's `server/config` never echoes a key back, and, after a sync,
that a real Sonarr (and Lidarr on the plain stack) accepts the instance as a
Torznab indexer. That last one is the check that matters: Sonarr once refused
the whole feed over a missing `pubDate`, and only a real client caught it.

Prowlarr is behind an opt-in profile, since nothing automated uses it:

```bash
docker compose -f docker/compose.test.yml --profile indexer up -d prowlarr
```

Then add a _Generic Torznab_ indexer pointing at `http://sharerr:8477/api`.
`/announce` refuses any info hash the instance is not sharing, so a
`d14:failure reason...` response to a made-up hash is expected, not a fault.

## The opt-in test suite

```bash
cargo test -p sharerr --features e2e -- --ignored --test-threads=1
```

The assertion that justifies the whole tier is in
`crates/sharerr/tests/e2e.rs`: after a sync through a real client, every
media file has the same **inode, mtime, and length** it started with.
Serialised with `--test-threads=1` because the tests share one stack.

## Seeding tagged content

sharerr shares what carries the `sharerr` tag and nothing else, so a stack
with no tagged content exercises nothing: `doctor` fails with `TagNotFound`
and `sync` bails. Getting content in cannot go through the *arr APIs:
**adding a series, movie, or artist through the API triggers a metadata
lookup** against `services.sonarr.tv` / `api.radarr.video` / MusicBrainz, and
every fixture title is invented, so the add fails. `seed-arr` therefore
writes the rows straight into the apps' own SQLite databases:

```bash
docker compose -f docker/compose.test.yml stop sonarr radarr lidarr
cargo run -p sharerr-testkit --bin seed-arr -- \
    --sonarr docker/state/sonarr/sonarr.db \
    --radarr docker/state/radarr/radarr.db \
    --lidarr docker/state/lidarr/lidarr.db
docker compose -f docker/compose.test.yml start sonarr radarr lidarr
```

`--lidarr` is optional; only the plain stack carries a Lidarr container.

- **Every named app must be stopped.** Each holds its database open and will
  not observe an external write while running.
- **They keep their config in `./docker/state*`**, bind-mounted rather than
  in a named volume, so a host-side seeder can open the files. `down -v` does
  not clear a bind mount, so teardown removes the directory too.
- **It is coupled to their schemas**, which is why the image tags in the
  compose files are pinned. The rows are minimal: enough for the endpoints
  sharerr reads, not enough for a metadata refresh. The schema notes (Lidarr
  and Radarr 5 split metadata into their own tables; Lidarr's `TrackFiles`
  stores full paths) live with `seed-arr` in `crates/sharerr-testkit`.

The tag id is left to SQLite; sharerr resolves the _label_,
case-insensitively. The network is deliberately not `internal: true`: an
internal bridge would sever the host→container path the published ports,
readiness probes and `seed-arr` all travel. So the stack does not _prove_
sharerr makes no outbound requests; that rests on the code and the hermetic
suite. `compose.test.yml`'s header has the full trade-off.

## Views of one library

The mounts deliberately disagree, because in a real deployment they almost
always do, and identical mounts would hide every path-mapping bug:

| Who         | Sees the library at |
| ----------- | ------------------- |
| Sonarr      | `/tv`               |
| Radarr      | `/movies`           |
| Lidarr      | `/music`            |
| qBittorrent | `/downloads`        |
| sharerr     | `/media`            |

`docker/config/sharerr.toml` maps between them. Every media mount is `:ro`,
which turns "sharerr never writes to what it shares" into something the
kernel enforces.

## The six stacks

| Stack | File | What it adds |
| --- | --- | --- |
| Plain | `compose.test.yml` | Sonarr, Radarr, Lidarr, qBittorrent, Prowlarr (opt-in). The baseline. |
| VPN | `compose.vpn.yml` | qBittorrent inside a gluetun namespace. A genuinely different topology: qBittorrent has no network or DNS name of its own, so its address is `http://gluetun:8080` and its ports are published by gluetun. The run asserts `qbittorrent:8080` does _not_ resolve. A WireGuard server inside the stack terminates a real tunnel that goes nowhere, so the suite needs no subscription and no egress; keys in `docker/wireguard/wg0.conf` are committed on purpose. The compose file's header and `wg0.conf` explain the rest, including the in-tunnel listener gluetun's health check needs. |
| Transmission | `compose.transmission.yml` | Seeds through Transmission. Credentials given up front by compose; it always verifies on add. |
| rTorrent | `compose.rtorrent.yml` | Seeds through `crazymax/rtorrent-rutorrent`, which bundles rTorrent, ruTorrent, and an nginx proxy answering HTTP XML-RPC over rTorrent's SCGI socket. No `.htpasswd`, so Basic Auth is off and the configured credentials are placeholders. Exists because `sharerr-rtorrent`'s unit tests run against a hand-mocked server, which proves the crate parses what it _expects_, not what a real rTorrent sends. |
| Two-instance | `compose.two-instance.yml` | Two sharerr + Radarr + qBittorrent stacks wired together as friends, plus a Prowlarr. [Below](#the-two-instance-stack). |
| Mesh | `compose.mesh.yml` | Three independent sharerr nodes and one independent lighthouse — no *arr app, no torrent client. Proves the gossip/reconnection mesh instead of the media path. [Below](#the-mesh-stack). |

Every stack asserts the same thing about the tracker: built torrents carry
an announce URL from sharerr's own tracker, regardless of `torrent_backend`.
How the three clients differ is tabulated in
[`docs/SUPPORT.md`](../docs/SUPPORT.md#torrent-clients-what-actually-seeds).

## Ports

All bound to `127.0.0.1`. Each stack takes its own leading digit so any
combination can run at once.

| Service | Plain | VPN | Transmission | rTorrent | Two-instance A | Two-instance B |
| --- | --- | --- | --- | --- | --- | --- |
| Sonarr | 18989 | 28989 | 38989 | 48989 | | |
| Radarr | 17878 | 27878 | 37878 | 47878 | 58878 | 59878 |
| Lidarr | 18686 | | | | | |
| Torrent client WebUI | 18080 | 28080 (via gluetun) | 39091 | 48080 (ruTorrent), 48000 (XML-RPC) | 58080 | 59080 |
| Prowlarr | 19696 (opt-in, [above](#exercising-the-indexer-and-the-tracker)) | | | | | 59696 |
| sharerr | 18477 | 28477 | 38477 | 48477 | 58477 | 59477 |

sharerr's port doubles as the tracker: friends announce to it directly, so
in a real deployment it has to be reachable from outside the container.

The mesh stack does not fit this table — three sharerr nodes and one
lighthouse, not two clients and an *arr app — so its ports are listed with
[the stack itself](#the-mesh-stack) instead.

## The two-instance stack

Every other stack is one sharerr against one *arr stack and proves a local
add is safe; none proves the friend-to-friend loop. This one does:

| | Every other stack | Two-instance stack |
| --- | --- | --- |
| What it proves | a local add never moves or rewrites a file | a friend's Radarr can index, grab, and correctly download one |
| Torrent transport | never actually downloaded by anyone | a real BitTorrent handshake between two containers |
| Grab trigger | `sharerr sync`, driven by the test | Radarr's own automatic search, the same command its UI sends |

Instance A is seeded like the plain stack's Radarr. Instance B's Radarr gets
the _same_ movie by TMDB id via `seed-arr --radarr-wanted`, untagged and with
no file, so its own automatic search can match instance A's release. The
script then does by API what an operator would do by hand: creates a peer
on A, registers A as a Torznab indexer on a Prowlarr between the two Radarrs,
registers B's qBittorrent as its download client, and triggers
`MoviesSearch`. The one assertion:
[`e2e_two_instance.rs`](../crates/sharerr/tests/e2e_two_instance.rs)
compares the bytes B's qBittorrent saved, byte for byte, against A's
original.

### Why Prowlarr sits in front of instance B's Radarr

Radarr's direct Torznab indexer has no setting to prefer a `.torrent`
enclosure over a magnet when a feed offers both, and the first live run
(before `feed.magnet_links` defaulted off) showed it choosing the magnet
anyway (qBittorrent-B's record: `has_metadata: false`, a `magnet_uri` whose
`dn=` was the release title). Every torrent sharerr builds is private by
default, so a magnet can never complete against it anywhere. Prowlarr's
per-indexer "Prefer Magnet URL" is the one place this is configurable, so the
script still pins it to `false` and confirms the pin took — belt-and-braces
now that the feed advertises no magnet under the default config at all, and
the setup an operator who does turn `feed.magnet_links` on should still
copy. A real friend should use the same topology: Radarr through Prowlarr,
not directly. See
[`docs/SUPPORT.md`](../docs/SUPPORT.md#the-feeds-magnet-link) for the
settled decision this leaves in place.

### Why `tracker.advertised_host` is a service name here, not `localhost`

Every other stack sets it to `localhost`, which works only because nothing
in them ever needs a different container to dial it back. Here
`qbittorrent-b` really does have to reach `sharerr-a`'s tracker, so
`docker/config-two-instance-a/sharerr.toml` advertises `sharerr-a`, resolved
by Docker's embedded DNS on the shared network, on port 8477, sharerr's
internal listen port rather than the host-published one.

## The mesh stack

```bash
./scripts/run_docker_tests_mesh.sh
```

| Service | Port |
| --- | --- |
| Lighthouse | 127.0.0.1:63878 |
| sharerr-a | 127.0.0.1:63481 |
| sharerr-b | 127.0.0.1:63482 |
| sharerr-c | 127.0.0.1:63483 |

Tier 3: three independent sharerr nodes and one independent lighthouse, no
*arr app and no torrent client. See
[`docs/TESTING.md`](../docs/TESTING.md#tier-3-the-mesh-stack) for what it
proves and why the gossip/lighthouse intervals run in seconds here rather
than the production default, and `docker/compose.mesh.yml`'s own header for
the full design — the topology it meshes, why the config mounts point at a
gitignored `state-mesh/` scratch copy rather than the checked-in
`config-mesh-*` templates, and why the lighthouse is built from the working
tree rather than pulled.

## Tearing down

Two halves are needed: `-v` drops the named volumes (API keys are regenerated
on every fresh start, and qBittorrent only logs its temporary password when
it has no stored one), and the bind-mounted `state` directory goes
separately. The scripts do both from a trap.

```bash
docker compose -f docker/compose.test.yml --profile indexer down -v && rm -rf docker/state
docker compose -f docker/compose.vpn.yml down -v          && rm -rf docker/state-vpn
docker compose -f docker/compose.transmission.yml down -v && rm -rf docker/state-tm
docker compose -f docker/compose.rtorrent.yml down -v     && rm -rf docker/state-rt
docker compose -f docker/compose.two-instance.yml down -v && rm -rf docker/state-two-instance
docker compose -f docker/compose.mesh.yml down -v         && rm -rf docker/state-mesh
```

If `rm -rf` fails with a permission error, Docker auto-created the directory
as root before the script could:

```bash
docker run --rm -v "$PWD/docker:/w" alpine:3 rm -rf /w/state
```
