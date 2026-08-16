# The compose test stack

A disposable Sonarr + Radarr + qBittorrent + Prowlarr stack for exercising sharerr
against the real services. **Entirely optional** — the default `cargo test` suite is
hermetic and needs none of this.

Everything in `tests/fixtures/media` is synthetic: invented titles, seeded
pseudo-random bytes, `FAKEGRP` release names. No real content is involved anywhere.

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

# 2. Bring the stack up. Sonarr and Radarr keep their config in ./docker/state,
#    which has to exist first — see "Seeding tagged content" below.
mkdir -p docker/state/sonarr docker/state/radarr
docker compose -f docker/compose.test.yml up -d --build

# 3. Give the *arr apps something tagged. They must be stopped for this.
docker compose -f docker/compose.test.yml stop sonarr radarr
cargo run -p sharerr-testkit --bin seed-arr -- \
    --sonarr docker/state/sonarr/sonarr.db \
    --radarr docker/state/radarr/radarr.db
docker compose -f docker/compose.test.yml start sonarr radarr

# 4. Collect API keys from each app's config.
SONARR_KEY=$(sed -n 's:.*<ApiKey>\(.*\)</ApiKey>.*:\1:p' docker/state/sonarr/config.xml)

# qBittorrent prints a temporary admin password to its log on first start.
docker compose -f docker/compose.test.yml logs qbittorrent | grep -i password

# 5. Load them into sharerr. Either open http://127.0.0.1:18477/ and paste them
#    into Settings, or pipe them in — the two write to the same vault.
docker compose -f docker/compose.test.yml exec -T sharerr \
    sh -c "printf %s '$SONARR_KEY' | sharerr vault set sonarr.api_key"

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

```bash
# The Torznab endpoint needs an API key; generate one in the UI (Settings →
# Indexer) or pipe one in. Piped on stdin rather than interpolated into `sh -c`,
# so a value containing a quote cannot break out of the command.
printf %s "a-test-key" | docker compose -f docker/compose.test.yml exec -T sharerr \
    sharerr vault set torznab.api_key

# What a friend's Prowlarr would fetch.
curl -s "http://127.0.0.1:18477/api?t=caps&apikey=a-test-key"
curl -s "http://127.0.0.1:18477/api?t=tvsearch&apikey=a-test-key"
```

`run_docker_tests.sh` sets a key of its own and asserts `t=caps` returns a document,
so the feed is covered by a normal run — but only that far.

Prowlarr is in the stack behind an opt-in profile, since nothing automated uses it:

```bash
docker compose -f docker/compose.test.yml --profile indexer up -d prowlarr
```

Then add a *Generic Torznab* indexer pointing at `http://sharerr:8477/api` with
that key.

To exercise sharerr's own tracker rather than qBittorrent's, set
`backend = "builtin"` under `[tracker]` in `docker/config/sharerr.toml` and
re-sync. `/announce` refuses any info hash the instance is not sharing, so a
`d14:failure reason...` response to a made-up hash is the expected result, not a
fault.

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
bails with "no *arr app could be scanned". Getting content in is the one part of
this stack that cannot go through the *arr APIs, and it is worth knowing why.

> **Adding a series or movie through the *arr API triggers a metadata lookup**
> against `services.sonarr.tv` / `api.radarr.video`. Every fixture title is
> invented, so the lookup finds nothing and the add fails.

That is a property of the fixtures, not of the network, so it holds however the
stack is wired. `seed-arr` therefore writes the rows straight into Sonarr's and
Radarr's own SQLite databases:

```bash
cargo run -p sharerr-testkit --bin seed-arr -- \
    --sonarr docker/state/sonarr/sonarr.db \
    --radarr docker/state/radarr/radarr.db
```

Three things about it:

- **Both apps must be stopped.** They hold their databases open and will not
  observe an external write while running. `run_docker_tests.sh` stops and starts
  them around the seed.
- **Sonarr and Radarr keep their config in `./docker/state`**, bind-mounted rather
  than in a named volume, so a host-side seeder can open those files at all. `down
  -v` does not clear a bind mount, so teardown removes the directory too.
- **It is coupled to their schemas.** That is the cost of seeding this way, and it
  is why the image tags in `compose.test.yml` are pinned rather than `:latest`. The
  rows are minimal — enough for the four endpoints sharerr reads (`tag`, `series`,
  `episodefile`+`episode`, `movie`), not enough for a metadata refresh.

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

## Four views of one library

The mounts deliberately disagree, because in a real deployment they almost always
do, and identical mounts would hide every path-mapping bug:

| Who | Sees the library at |
|---|---|
| Sonarr | `/tv` |
| Radarr | `/movies` |
| qBittorrent | `/downloads` |
| sharerr | `/media` |

`docker/config/sharerr.toml` maps between them. Every media mount is `:ro` — sharerr
never needs to write to the content it shares, and the read-only flag turns that
from a promise into something the kernel enforces.

## Ports

All bound to `127.0.0.1` so the stack is not exposed on the network.

| Service | Host port |
|---|---|
| Sonarr | 18989 |
| Radarr | 17878 |
| qBittorrent WebUI | 18080 |
| qBittorrent embedded tracker | 19000 |
| Prowlarr | 19696 (opt-in — see below) |
| sharerr | 18477 |

The tracker port is the one most easily forgotten in a real deployment: friends
announce to it directly, so it has to be reachable from outside the container, not
just on the docker network.

## Tearing down

```bash
docker compose -f docker/compose.test.yml --profile indexer down -v && rm -rf docker/state
```

Both halves are needed. `-v` drops the named volumes, which is what you want
between runs — the API keys are regenerated on every fresh start, and qBittorrent
only logs its temporary password when it has no stored one. It does *not* touch
`docker/state`, where Sonarr and Radarr keep theirs, so that goes separately.
`run_docker_tests.sh` does both from a trap.

If `rm -rf docker/state` fails with a permission error, Docker auto-created that
directory as root before the script could. Delete it as root instead:

```bash
docker run --rm -v "$PWD/docker:/w" alpine:3 rm -rf /w/state
```

The script pre-creates the directory to avoid this, and falls back to the same
command when it finds a tree an older run left behind.
