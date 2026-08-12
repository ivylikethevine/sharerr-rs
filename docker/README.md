# The compose test stack

A disposable Sonarr + Radarr + qBittorrent + Prowlarr stack for exercising sharerr
against the real services. **Entirely optional** — the default `cargo test` suite is
hermetic and needs none of this.

Everything in `tests/fixtures/media` is synthetic: invented titles, seeded
pseudo-random bytes, `FAKEGRP` release names. No real content is involved anywhere.

## Running it

```bash
# 1. Generate the synthetic library (idempotent — same bytes every time).
cargo run -p sharerr-testkit --bin gen-fixtures -- tests/fixtures/media

# 2. Bring the stack up.
docker compose -f docker/compose.test.yml up -d --build

# 3. Collect API keys from each app's config.
SONARR_KEY=$(docker compose -f docker/compose.test.yml exec -T sonarr \
    sed -n 's:.*<ApiKey>\(.*\)</ApiKey>.*:\1:p' /config/config.xml)

# qBittorrent prints a temporary admin password to its log on first start.
docker compose -f docker/compose.test.yml logs qbittorrent | grep -i password

# 4. Load them into sharerr. Either open http://127.0.0.1:18477/ and paste them
#    into Settings, or pipe them in — the two write to the same vault.
docker compose -f docker/compose.test.yml exec -T sharerr \
    sh -c "printf %s '$SONARR_KEY' | sharerr vault set sonarr.api_key"

# 5. Check the wiring before trying to sync. The UI's per-service "Test
#    connection" buttons cover the same ground for the services themselves;
#    `doctor` additionally resolves the path mappings, which is what the
#    deliberately-disagreeing mounts in this stack exist to exercise.
docker compose -f docker/compose.test.yml exec sharerr sharerr doctor
```

The stack sets `SHARERR_MASTER_KEY`, without which the vault cannot be opened and
neither the UI nor the CLI can store a credential. A real deployment must set it
too — see the root `README.md`.

Then tag something `sharerr` in Sonarr and:

```bash
docker compose -f docker/compose.test.yml exec sharerr sharerr sync
```

## The opt-in test suite

```bash
cargo test -p sharerr --features e2e -- --ignored --test-threads=1
```

The assertion that justifies the whole tier is in `crates/sharerr/tests/e2e.rs`:
after a sync through a real qBittorrent, every media file has the same **inode,
mtime, and length** it started with. A mock cannot prove that. Only a client that
genuinely tried to manage the files can.

Serialised with `--test-threads=1` because the tests share one stack.

## The network is internal, and that has a consequence

`internal: true` on the compose network means the containers have no route off the
host. That is how the project's no-egress requirement is *enforced* rather than
merely documented. Image pulls still work — they happen on the host, before the
network is joined.

One thing genuinely does not work offline, stated plainly rather than papered over:

> **Adding a series or movie through the *arr API triggers a metadata lookup**
> against `services.sonarr.tv` / `api.radarr.video`, which cannot resolve on an
> internal network. The add fails.

Everything sharerr itself does works offline: tag lookup, file discovery, path
resolution, torrent creation, and seeding all stay inside the stack.

Two ways to get content into the *arr apps anyway:

1. **Pre-seed the database** (keeps the stack fully offline). Write rows directly
   into Sonarr's `/config/sonarr.db`. This couples the fixture to Sonarr's schema,
   which is why the image tags in `compose.test.yml` are pinned rather than
   `:latest`.
2. **Allow the lookup once.** Temporarily drop `internal: true`, add the content,
   then restore it. Simpler, and fine for local exploration, but it means the
   no-egress guarantee is off for the duration.

Neither is automated here. Whichever you choose, sharerr's own behaviour is
unaffected — it only ever reads what the *arr apps already have.

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
| Prowlarr | 19696 |
| sharerr | 18477 |

The tracker port is the one most easily forgotten in a real deployment: friends
announce to it directly, so it has to be reachable from outside the container, not
just on the docker network.

## Tearing down

```bash
docker compose -f docker/compose.test.yml down -v
```

`-v` drops the config volumes too, which is what you want between runs — the API
keys are regenerated on every fresh start.
