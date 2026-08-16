# sharerr

Share your media library with friends, over the tools you already run.

sharerr connects to Sonarr and Radarr, finds everything tagged `sharerr`, builds a
torrent for each file **where it already sits**, seeds it through your qBittorrent,
and publishes the lot as a Torznab feed. Your friend adds that feed to their
Prowlarr; their Sonarr and Radarr then find your releases with the TVDB/TMDb/IMDb
ids attached, so a release matches a known series or film rather than being guessed
from its filename.

Nothing is copied, renamed, re-linked, or moved. That is the constraint the whole
design is built around.

> **Status: experimental.** This is a personal project and has not had a tagged
> release. See [the roadmap](docs/roadmap.md) for what works and what does not.
> Large parts were written with generative AI — see [AI usage](#ai-usage).

## What works today

| | |
|---|---|
| Sonarr / Radarr discovery by tag | ✅ |
| Torrent construction, files never moved | ✅ |
| Seeding through qBittorrent | ✅ |
| qBittorrent embedded tracker, or sharerr's own | ✅ |
| Torznab feed for Prowlarr | ✅ |
| Web UI: setup, settings, connection tests | ✅ |
| Path-mapping diagnostics in the browser | ✅ |
| Friend/peer management | ❌ — one shared API key; see [roadmap](docs/roadmap.md) |
| Lidarr / Readarr, non-qBittorrent clients | ❌ |

## Quickstart

```bash
docker run -d --name sharerr \
  -p 8477:8477 \
  -e SHARERR_MASTER_KEY="$(openssl rand -base64 32)" \
  -v sharerr-config:/config \
  -v sharerr-data:/data \
  -v /path/to/library:/media:ro \
  ghcr.io/ivyduggan/sharerr-rs:main
```

> The `:latest` tag is only published from a version tag, and there are none yet.
> Until the first release, `:main` is the image to use.

Then open `http://localhost:8477/`. The first visit asks you to create an account —
whoever gets there first claims the instance, so do it now rather than leaving it
reachable and unclaimed. After that, **Settings** takes the Sonarr and Radarr URLs
and API keys, the qBittorrent URL, username and password, the path mappings, and
the tracker's advertised host. Each service has a *Test connection* button, and
saving takes effect within a second or two — no restart.

`SHARERR_MASTER_KEY` is the one thing that cannot come from the UI, because it is
what encrypts the vault the UI writes into. Set it (or `SHARERR_MASTER_KEY_FILE`,
pointing at a docker secret) and keep it: **losing it means losing every stored
credential.** Without it sharerr still starts and the UI still loads — it will just
tell you the credential fields are unavailable until you set it, rather than
quietly storing your API keys in plaintext.

Two volumes matter. `/data` holds the vault, the database, and the generated
`.torrent` files; `/config` holds `sharerr.toml`, which the UI rewrites in place
(comments and all) when you save. Both must persist across restarts.

Anyone on the network who can reach port 8477 can reach the login page, and the
session cookie is not sent over TLS, because sharerr is normally run on a LAN. If
that is not true of your network, put it behind a TLS-terminating proxy.

## Sharing with a friend

sharerr publishes what it shares as a **Torznab** feed, which is what Prowlarr
speaks. In **Settings → Indexer**, generate an API key and copy it together with
the feed URL. Your friend adds a *Generic Torznab* indexer in their Prowlarr using
those two values.

The feed lists only what is actually seeding, and the `.torrent` files it links to
are served from the same instance. Both the feed and the downloads require the API
key — without one, the endpoint stays closed rather than open, because the feed is
a list of everything you share.

> **One shared key, for now.** Every friend uses the same API key, so they cannot
> be told apart and revoking one revokes all of them. Per-peer keys are the next
> milestone — see [the roadmap](docs/roadmap.md).

The feed URL is built from `tracker.advertised_host`, so that has to be an address
your friend can reach. Everything here is a single HTTP port; whatever you do to
make port 8477 reachable also makes the tracker and the feed reachable.

### Which tracker

**qBittorrent's embedded tracker** is the default and needs nothing from you.

**sharerr's builtin tracker** is the alternative, selected under Settings →
Tracker. It serves `/announce` and `/scrape` from the sharerr process itself, and
it answers only for torrents sharerr made — it will not act as a tracker for
anything else, whoever asks. Optionally generate an announce token: it is embedded
in the announce URL of every torrent built afterwards, so holding the `.torrent` is
what grants the right to announce. Note that changing the token invalidates
torrents already published.

One caveat with the builtin tracker: the announce endpoint is part of
`sharerr serve`, so a one-shot `sharerr sync` produces correct torrents whose
announces fail until `serve` is running.

## The CLI

The UI covers everything, but each verb has a headless equivalent, which is what a
scripted deployment or a secrets manager wants:

| Command | What it does |
|---|---|
| `sharerr serve` | The long-running mode: HTTP, the tracker, the feed, and the reconciliation loop. What the container runs. |
| `sharerr sync` | One reconciliation pass, then exit. |
| `sharerr doctor` | Checks credentials, service reachability, the tag, and **path mapping resolution** — the check most likely to explain "nothing is shared". The same checks back the UI's **Diagnostics** page, so the two cannot disagree. |
| `sharerr vault set <key>` | Reads a secret from stdin into the encrypted vault. |

```bash
printf %s "$SONARR_API_KEY" | docker exec -i sharerr sharerr vault set sonarr.api_key
docker exec sharerr sharerr doctor
```

Settings can also come from the environment — `SHARERR_QBITTORRENT__URL` sets
`qbittorrent.url`, and so on for any field. Be aware that these take precedence
over the config file, so a field pinned by a variable cannot be changed from the
UI; sharerr renders those inputs disabled and names the variable rather than
accepting a save that would be silently discarded.

## Building and testing

Rust **1.88** or newer (the workspace sets `rust-version`; `docker build .` is the
de-facto MSRV check, since a local toolchain is invariably newer).

```bash
cargo build
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Clippy must stay at zero warnings. The workspace sets `unwrap_used` and
`expect_used` to `warn` because the vault and service clients handle secrets, and
CI promotes them with `-D warnings`. Test modules opt out with an inner
`#![allow(clippy::unwrap_used, clippy::expect_used)]` rather than weakening the
workspace lint.

The default suite is **hermetic** — no network, no containers, no database: the
service clients run against wiremock on loopback and sqlx against
`sqlite::memory:`. There is a second, opt-in tier that drives a real
Sonarr + Radarr + qBittorrent stack:

```bash
./run_docker_tests.sh
```

See [docker/README.md](docker/README.md) for what it does and how to drive it by
hand. Everything it touches is synthetic — invented titles, seeded pseudo-random
bytes. No real content is involved anywhere.

## Layout

| Crate | |
|---|---|
| `sharerr` | The binary: CLI, web UI, Torznab, tracker, reconciliation |
| `sharerr-core` | Domain types, layered config, path mapping. No I/O |
| `sharerr-arr` | Sonarr/Radarr clients and tagged-content discovery |
| `sharerr-qbit` | qBittorrent WebUI client |
| `sharerr-store` | Encrypted vault + SQLite store |
| `sharerr-torrent` | Torrent construction and tracker resolution |
| `sharerr-testkit` | Synthetic fixtures. Never in a release build |

The original design brief, and the two corrections the implementation forced on
it, are in [docs/design.md](docs/design.md).

## AI usage

Heavily inspired by: https://v2.dictionarry.dev/ai-transparency

I have used generative AI to write large parts of this project. Regardless, all of
the code in this repository is my _responsibility_. AI is a tool, not an owner of a
project. I have personally understood, reviewed, and approved all of the AI
generated code in this repository. _Mainline releases_ have the same level of
accountability to me as any code I write and publish.

## Licence

MIT — see [LICENSE](LICENSE).
