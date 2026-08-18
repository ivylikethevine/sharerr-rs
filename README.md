# sharerr

Share your media library with friends, over the tools you already run.

sharerr connects to your *arr apps — Sonarr, Radarr, Lidarr, Readarr, Whisparr —
finds everything tagged `sharerr`, builds a
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
| Discovery by tag: Sonarr, Radarr, **Lidarr, Readarr, Whisparr** | ✅ |
| Torrent construction, files never moved | ✅ |
| Seeding through qBittorrent **or Transmission** | ✅ |
| Builtin BitTorrent tracker, served by sharerr itself | ✅ |
| Torznab feed for Prowlarr, with magnet links | ✅ |
| Jackett compatibility: URLs, indexer list, JSON results | ✅ |
| Web UI: setup, settings, connection tests | ✅ |
| Path-mapping diagnostics in the browser | ✅ |
| Friend/peer management: per-friend keys, revoke, last-seen | ✅ |
| Per-friend scoping: this friend sees TV, that one films | ✅ |
| Plain directory sharing, no *arr app at all | ✅ |
| Jellyfin / Emby as a library source | ✅ |
| Dynamic endpoint from gluetun: rotating exit IP and forwarded port | ✅ |
| Peer endpoint memory and signed endpoint gossip between friends | ✅ |
| Plex as a library source | ❌ |

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
speaks. Open **Friends**, add your friend by name, and sharerr generates a key just
for them — shown once, alongside the feed URL. They add a *Generic Torznab* indexer
in their Prowlarr using those two values.

Because each friend has their own key, the Friends page can tell you when each of
them last used the feed — "never" means they have the key but have not finished
setting up — and revoking one person leaves everybody else working.

You can also scope what each friend sees: everything, or only TV, films, music or
books. That
applies to the feed itself, not just the display — content outside a friend's scope
is never listed and never offered, and they cannot search their way around it.

> A single shared `torznab.api_key` still works, for setups made before per-friend
> keys existed. While one is set, revoking a friend does **not** cut them off,
> because the shared key still opens the feed; the Friends page says so. Clear it
> under Settings → Indexer once everyone has their own.

If your friend has a client set up for **Jackett** rather than Prowlarr, it works
unmodified. sharerr answers Jackett's URL shape
(`/api/v2.0/indexers/<anything>/results/torznab/api`) with the same feed, plus its
read-only admin endpoints — the indexer list, the server config, and the JSON
results some clients prefer to Torznab. The indexer id in the path is ignored, so
whatever id was in the old Jackett config keeps working.

Jackett's *write* endpoints — adding, configuring or deleting indexers — are not
implemented, because sharerr has exactly one indexer and it is not configurable
over HTTP. A client that calls one gets a `501` and sharerr logs the exact method
and path, so a gap that actually matters says so instead of failing silently.

**Tag something before your friend adds the indexer.** Sonarr and Radarr treat an
empty feed as a failed test — "no results in the configured categories" is an
error, not a warning — so an indexer added before anything is shared will not
validate, even though nothing is wrong.

The feed lists only what is actually seeding, and the `.torrent` files it links to
are served from the same instance. Both the feed and the downloads require the API
key — without one, the endpoint stays closed rather than open, because the feed is
a list of everything you share.

The feed URL is built from `tracker.advertised_host`, so that has to be an address
your friend can reach. Everything here is a single HTTP port; whatever you do to
make port 8477 reachable also makes the tracker and the feed reachable.

### The tracker

sharerr serves `/announce` and `/scrape` from its own process, whichever torrent
client seeds, and it answers only for torrents sharerr made — it will not act as
a tracker for anything else, whoever asks. Optionally generate an announce token
under Settings → Tracker: it is embedded in the announce URL of every torrent
built afterwards, so holding the `.torrent` is what grants the right to announce.
Note that changing the token invalidates torrents already published.

(There used to be a second option here — qBittorrent's embedded tracker — and it
was removed: two tracker backends meant two independently built announce URLs,
and every improvement to endpoint handling had to be made twice. A `sharerr.toml`
still naming `tracker.backend` fails to load with an error saying exactly this;
delete the line.)

One caveat: the announce endpoint is part of `sharerr serve`, so a one-shot
`sharerr sync` produces correct torrents whose announces fail until `serve` is
running.

### A dynamic endpoint (gluetun)

Behind a VPN with provider port forwarding there is no stable address to type
into `tracker.advertised_host` — the exit IP and the granted port both change on
reconnect. Point sharerr at gluetun's control server instead:

```toml
[gluetun]
control_url = "http://localhost:8000"   # sharerr inside gluetun's namespace
poll_secs = 60
```

sharerr polls `/v1/publicip/ip` and `/v1/openvpn/portforwarded` as the source of
truth, and torrents carry an announce *list* spanning the recently held
endpoints, so a friend's client falls back through older tiers after a rotation.
When the endpoint changes, sharerr rewrites every cached `.torrent` (the info
hash is untouched — announce lives outside the info dictionary) and repoints the
tracker lists inside the torrent client, immediately rather than at the next
scheduled sync. For reconnects to be picked up in seconds, set gluetun's
`VPN_PORT_FORWARDING_UP_COMMAND` to `wget -qO- http://localhost:8477/gluetun/refresh`
— the push only nudges sharerr to re-ask the control server, so nothing pushed is
trusted. Where the provider grants no port, sharerr says so and degrades to the
statically configured endpoint. `docker/deploy/` wires all of this up.

Two related settings for constrained setups: `tracker.advertised_url` takes a
full base URL (scheme, path prefix, bracketed IPv6) for reverse-proxied
instances, and `tracker.bind` opens a second listener carrying only the tracker
— for the topology where exactly one forwarded port exists and it has to be the
tracker's, while the web UI stays on the LAN side.

## Sharing music, books, and more

Each *arr app is its own optional section, and any combination works:

```toml
[lidarr]
url = "http://localhost:8686"

[readarr]
url = "http://localhost:8787"

[whisparr]
url = "http://localhost:6969"
```

Then store each key: `printf %s "$KEY" | sharerr vault set lidarr.api_key`.

Notes that are easy to trip over:

- **Tags live on the artist and the author**, not the album or the book — so
  tagging one shares their whole discography or catalogue, the same way tagging a
  Sonarr series shares every episode.
- **Lidarr and Readarr are on API v1**, Sonarr/Radarr/Whisparr on v3. sharerr picks
  the right one per app; you only supply the base URL.
- **Whisparr content is categorised as XXX**, not TV, and a friend scoped to "TV
  only" does **not** receive it. Only an unscoped friend does, which has to be
  chosen deliberately.

## Sharing from Jellyfin or Emby

Not everyone runs the *arr apps. Point sharerr at a Jellyfin (or Emby — same
API) server and tag content there instead:

```toml
[jellyfin]
url = "http://localhost:8096"
```

Then store an API key (Dashboard → API Keys in Jellyfin):
`printf %s "$KEY" | sharerr vault set jellyfin.api_key`.

Tag the **container**: a movie, a series, an album, or a book. A tagged series
shares every episode with a file; a tagged album shares every track. Tags on
individual episodes or tracks are deliberately not discovered — "tag the
series" is an easier rule to act on than a partial one.

Two honest caveats: releases from this source match less reliably on a friend's
end than Sonarr-sourced ones (Jellyfin's external ids are only as good as its
metadata match, and there is no scene name), and a friend scoped to "TV only"
sees Jellyfin's episodes but not its films — items are scoped by what they are,
since one server holds every kind at once.

## Friends finding each other

A peer used to be only a credential; sharerr now also remembers *where* each
friend was recently seen — the last few addresses, timestamped, with their feed
traffic and their torrent client recorded separately (a dual-VPN friend has the
two behind different exits). Sightings come from authenticated feed pulls and
from **gossip**: when a friend also runs sharerr, the two instances exchange
signed endpoint records over the same per-friend key the feed uses, so one
friend noticing a moved address is enough for everyone who already knows them.

The trust model is worth stating plainly: every record is Ed25519-signed by the
peer it describes, so a friend can relay it but never rewrite it; an older
record never overwrites a newer one; a peer's identity key is pinned on first
use; and a gossip pull returns only records for peers the caller proves they
already know — nobody learns of a peer they are not already sharing with.

Set it up per friend on the Friends page: their sharerr's URL, and the key they
issued you (from *their* Friends page). Leave both empty and your instance still
answers their pulls and accepts their pushes; it just never initiates.

## Sharing a plain directory, no *arr app at all

Point sharerr at a folder and everything in it is shared — the zero-dependency
path for a library curated by hand:

```toml
[[library]]
path = "/media/extras"
kind = "movie"   # tv, movie, music, or book

[[library]]
path = "/media/tapes"
kind = "tv"
```

Each entry is scanned recursively; being in the directory is the tag, and the
declared `kind` decides the feed category and which scoped friends see it. The
trade-offs to know:

- **No external ids travel with these releases.** A friend's app matches them by
  parsing the release name alone, so name files the way releases are named —
  `Show.Name.S01E02.mkv`, `Film.Title.2019.mkv`. A `tv` file with no `SxxEyy` in
  its name is skipped (and `doctor` says so) rather than advertised as something
  it cannot be matched to.
- **Music and books lean on the directory layout**: `Artist/Album/01 - Track.flac`
  and `Author/Title.epub`.
- **One file, one torrent.** An album is shared per track file, not as a folder.
- The directory is never modified — same rule as everywhere else in sharerr.

## Authenticating to qBittorrent

By default sharerr signs in with the WebUI username and password:

```
printf %s "$PW" | sharerr vault set qbittorrent.password
```

**qBittorrent 5.2 and newer also accept an API key**, which is stateless — no
session to expire, no re-login — and is the better choice where it is available.
Generate one under Options → Web UI → API key, then:

```
printf %s "$KEY" | sharerr vault set qbittorrent.api_key
```

When a key is stored it is used *instead of* the username and password, so you can
clear the password afterwards. Rotating the key in qBittorrent invalidates the old
one immediately, so store the new one at the same time.

### If a correct password is rejected

Two causes, and neither is the password:

- **qBittorrent 5.2 changed the login response.** Success moved from `200 Ok.` to
  `204 No Content`, and a rejection from `200 Fails.` to `401`. Clients that check
  for the literal `Ok.` — sharerr included, before this was fixed — report a
  perfectly good login as a wrong password. Update sharerr.
- **qBittorrent validates the `Host` header's port** against the port it listens
  on, and answers `401` before it ever reads the credentials when they differ. A
  remapped docker port (`-p 18080:8080`) or a reverse proxy on another port trips
  this. Either point `qbittorrent.url` at the port qBittorrent itself listens on,
  or turn off Options → Web UI → *Validate Host header*.

`sharerr doctor` names both, rather than reporting "rejected the password" and
leaving you to retype a password that was never wrong.

## Using Transmission instead of qBittorrent

```toml
torrent_backend = "transmission"

[transmission]
url = "http://localhost:9091"
username = "transmission"
# Transmission has no categories, only a flat list of labels per torrent, so this
# one value stands in for qBittorrent's category and tag.
label = "sharerr"
```

Then store the password: `printf %s "$PW" | sharerr vault set transmission.password`.

One difference worth knowing, enforced rather than documented-and-hoped:

- **No skip-checking.** qBittorrent can be told to trust the data on disk;
  Transmission cannot, so it always verifies. That is slower on a large library the
  first time and is not something sharerr can fake safely — claiming completeness
  without verifying would mean seeding whatever happens to be at the path.

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
| `sharerr-jellyfin` | Jellyfin/Emby client and its tagged-content discovery |
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
