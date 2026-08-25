# sharerr

---

## EXPERIMENTAL UNTIL v1.0.0-stable RELEASES

NOTE: Project is in active development, many things are subject to change and this current state is not a representation of final, published quality. This is a hobby project.

---

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
> release. See [the roadmap](docs/ROADMAP.md) for what works and what does not.
> Large parts were written with generative AI — see [AI usage](#ai-usage).

## Table of contents

- [What works today](#what-works-today)
- [Quickstart](#quickstart)
- [Sharing with a friend](#sharing-with-a-friend)
  - [The tracker](#the-tracker)
  - [Seeding limits](#seeding-limits)
  - [A dynamic endpoint (gluetun)](#a-dynamic-endpoint-gluetun)
  - [The lighthouse](#the-lighthouse)
- [Sharing music, books, and more](#sharing-music-books-and-more)
- [Friends finding each other](#friends-finding-each-other)
- [Topology](#topology)
  - [Checking that you are actually reachable](#checking-that-you-are-actually-reachable)
- [Sharing a plain directory, no \*arr app at all](#sharing-a-plain-directory-no-arr-app-at-all)
- [Authenticating to qBittorrent](#authenticating-to-qbittorrent)
  - [If a correct key is rejected](#if-a-correct-key-is-rejected)
- [Using Transmission instead of qBittorrent](#using-transmission-instead-of-qbittorrent)
- [Using rTorrent / ruTorrent instead of qBittorrent](#using-rtorrent--rutorrent-instead-of-qbittorrent)
- [The CLI](#the-cli)
- [Building and testing](#building-and-testing)
- [Layout](#layout)
- [AI usage](#ai-usage)
- [Licence](#licence)

See also: [the configuration reference](docs/CONFIGURATION.md), [supported
services](docs/SUPPORTED.md), [what's deliberately not
supported](docs/UNSUPPORTED.md), [the API](docs/API.md), [the
roadmap](docs/ROADMAP.md), [the original design brief](docs/DESIGN.md), and
[the security policy](docs/SECURITY.md).

## What works today

|                                                                                  |   |
|----------------------------------------------------------------------------------|---|
| Discovery by tag: Sonarr, Radarr, **Lidarr, Readarr, Whisparr**                  | ✅ |
| Torrent construction, files never moved                                          | ✅ |
| Seeding through qBittorrent, Transmission, **or rTorrent/ruTorrent**             | ✅ |
| Builtin BitTorrent tracker, served by sharerr itself                             | ✅ |
| Torznab feed for Prowlarr, with magnet links                                     | ✅ |
| Jackett compatibility: URLs, indexer list, JSON results                          | ✅ |
| Web UI: setup, settings, connection tests                                        | ✅ |
| Path-mapping diagnostics in the browser                                          | ✅ |
| Friend/peer management: per-friend keys, revoke, last-seen                       | ✅ |
| Per-friend scoping: this friend sees TV, that one films                          | ✅ |
| Per-friend announce-token attribution: revoking a friend cuts tracker access too | ✅ |
| Safe rotation of the shared fallback announce token: old and new both work       | ✅ |
| Ratio and bandwidth limits: per-torrent upload cap and seed-ratio goal           | ✅ |
| Plain directory sharing, no *arr app at all                                      | ✅ |
| Dynamic endpoint from gluetun: rotating exit IP and forwarded port               | ✅ |
| Peer endpoint memory and signed endpoint gossip between friends                  | ✅ |
| The lighthouse: rendezvous for a friend whose address rotated while unwatched    | ✅ |
| Topology diagram: sources, this instance, and friends in one picture             | ✅ |
| Live per-torrent swarm view: who is connected to each torrent right now          | ✅ |
| Reachability script for checking from outside your network (`/debug`)            | ✅ |

Full detail on which *arr apps, torrent clients, and indexers are supported —
and the trait/seam each plugs into — is [`docs/SUPPORTED.md`](docs/SUPPORTED.md).
For things tried and deliberately left out (a media-server library source,
Readarr as a direct indexer, and more), see
[`docs/UNSUPPORTED.md`](docs/UNSUPPORTED.md).

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
and API keys, the qBittorrent URL and API key, the path mappings, and the
tracker's advertised host. Each service has a _Test connection_ button, and
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
for them — shown once, alongside the feed URL. They add a _Generic Torznab_ indexer
in their Prowlarr using those two values.

Because each friend has their own key, the Friends page can tell you when each of
them last used the feed — "never" means they have the key but have not finished
setting up — and revoking one person leaves everybody else working. That key is
also what a magnet from the feed embeds as the announce token, so revoking a
friend cuts their access to sharerr's own tracker too, not just the feed —
instantly, and with no effect on anyone else, since nobody else's access ever
depended on it. The same attribution applies whether a friend's Sonarr fetches
by magnet or downloads the `.torrent` directly.

You can also scope what each friend sees: everything, or only TV, films, music or
books. That applies to the feed itself, not just the display — content outside a
friend's scope is never listed and never offered, and they cannot search their
way around it.

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

Jackett's _write_ endpoints — adding, configuring or deleting indexers — are not
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

Rotating that token — "Rotate the announce token" — does not cut off torrents
already published. The token it replaces keeps working, unattributed, alongside
the new one until you explicitly finish the rotation from Settings; the page
shows whether (and when) anything has used the old token since the rotation, so
you can wait until nothing has for a while before finishing. This is a safety
net for the *shared* token specifically, not a substitute for per-friend
revocation above — a shared token can never single out one already-connected
peer, only stop admitting it.

(There used to be a second option here — qBittorrent's embedded tracker — and it
was removed: two tracker backends meant two independently built announce URLs,
and every improvement to endpoint handling had to be made twice. A `sharerr.toml`
still naming `tracker.backend` fails to load with an error saying exactly this;
delete the line.)

One caveat: the announce endpoint is part of `sharerr serve`, so a one-shot
`sharerr sync` produces correct torrents whose announces fail until `serve` is
running.

### Seeding limits

Sharing a library with no cap on what it costs you is a real deterrent to
running this, so Settings → Seeding limits takes an upload-speed cap (KiB/s)
and a seed-ratio goal, applied once per torrent at the moment sharerr hands
it to qBittorrent or Transmission:

```toml
[seeding]
upload_limit_kib = 500
ratio_limit = 2.0
```

Neither is enforced by sharerr itself afterward — each client's own already-
running seeding engine honours the goal from then on, the same as it would
for a torrent added by hand. That also means a change here only takes effect
on torrents added _after_ the change; nothing already seeding is touched.
Leave a field blank (or the section out entirely) for no cap, today's
default. There is deliberately no time-based goal: qBittorrent's equivalent
is total time seeded, but Transmission's only comparable knob is _idle_
time, a different condition, and one field meaning two different things per
backend would be a footgun rather than a fix.

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
truth, and torrents carry an announce _list_ spanning the recently held
endpoints, so a friend's client falls back through older tiers after a rotation.
When the endpoint changes, sharerr rewrites every cached `.torrent` (the info
hash is untouched — announce lives outside the info dictionary) and repoints the
tracker lists inside the torrent client, immediately rather than at the next
scheduled sync. For reconnects to be picked up in seconds, set gluetun's
`VPN_PORT_FORWARDING_UP_COMMAND` to `wget -qO- http://localhost:8477/gluetun/refresh`
and, so a port going away is dropped immediately instead of lingering as a stale
fallback until the next poll, `VPN_PORT_FORWARDING_DOWN_COMMAND` to
`wget -qO- http://localhost:8477/gluetun/down` — both pushes only nudge sharerr to
re-ask the control server, so nothing pushed is trusted. gluetun's own control
server has required an API key (`gluetun.api_key` in Settings, or
`CONTROL_SERVER_AUTH` on gluetun's side) since v3.40; without one, sharerr skips
the poll rather than send a request that can only come back `401`. The exit
address and the forwarded port are also resolved independently — since the
routes in gluetun's own auth config can grant one without the other, a port
lookup that fails falls back to the last known port rather than blocking an exit
address change on it. Where the provider grants no port at all, sharerr says so
and degrades to the statically configured endpoint. `docker/deploy/` wires all of
this up.

Two related settings for constrained setups: `tracker.advertised_url` takes a
full base URL (scheme, path prefix, bracketed IPv6) for reverse-proxied
instances, and `tracker.bind` opens a second listener carrying only the tracker
— for the topology where exactly one forwarded port exists and it has to be the
tracker's, while the web UI stays on the LAN side.

### The lighthouse

Gossip only helps a friend who can still reach _somebody_ — two friends whose
addresses both rotated while neither was watching have no path back to each
other. The lighthouse is the rendezvous for that case: a `key hash -> latest
endpoint` service, deliberately independent of the rest of sharerr, that a
peer reports its endpoint to and a friend looks up under the API key that
peer issued them. A request without a valid key still gets a plausible
fabricated answer rather than an error, so scraping it yields only noise —
see `docs/ROADMAP.md`'s "The lighthouse" for the full design.

Using one is a Settings → Lighthouse field, `lighthouse.urls` — one or more
lighthouse base URLs, self-hosted by a friend or by you:

```toml
[lighthouse]
urls = ["https://a-friends-lighthouse.example"]
```

With at least one set, sharerr reports its own endpoint to every URL listed
— once per active friend's issued-key hash, since a lighthouse indexes by
key hash alone and never learns which reports belong to the same instance —
and queries the same list for any friend who has gone quiet. A lookup result
is only ever trusted, and folded into peer endpoint memory, once it both
verifies and names that friend's already-known identity (bound the first
time gossip ever heard from them) — a friend never gossiped with has nothing
to check a lighthouse's answer against, so is skipped rather than guessed
at. This is independent of running the embedded service below: consuming a
friend's lighthouse needs nothing checked in Settings, and running one for
friends needs nothing typed here.

Its own binary and image (`sharerr-lighthouse`, `crates/sharerr-lighthouse`,
built from `Dockerfile.lighthouse`) is meant to be self-hosted by anyone on
neutral ground:

```bash
docker build -f Dockerfile.lighthouse -t sharerr-lighthouse .
docker run -d --name sharerr-lighthouse -p 7878:7878 -v lighthouse-data:/data sharerr-lighthouse
```

`/data` holds nothing but the decoy secret — losing it just reshuffles
fabricated answers after a restart, not a credential. There is no published
image for it yet; building locally is the only way to run it today.

For a single operator who would rather not run a second container, it can
also run as extra routes on one of sharerr's own listeners — under
**Settings → Lighthouse**, or directly in `sharerr.toml`:

```toml
[lighthouse]
enabled = true
mount = "tracker"   # or "frontend" — see below
```

`mount = "tracker"` puts it on the same port a friend's torrent client
already reaches (`tracker.bind` if set, otherwise the main listener);
`mount = "frontend"` puts it on the main listener regardless. Off by
default, and unrelated to `lighthouse.urls` above — running the embedded
service and using a lighthouse as a client are two independent choices, not
a matched pair.

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

## Friends finding each other

A peer used to be only a credential; sharerr now also remembers _where_ each
friend was recently seen — the last few addresses, timestamped, with their feed
traffic and their torrent client recorded separately (a dual-VPN friend has the
two behind different exits). Sightings come from authenticated feed pulls, from
**gossip** — when a friend also runs sharerr, the two instances exchange signed
endpoint records over the same per-friend key the feed uses, so one friend
noticing a moved address is enough for everyone who already knows them — and,
when gossip alone has no path back to a quiet friend, from a lighthouse (see
"The lighthouse" above), ranked below both of the other two.

The trust model is worth stating plainly: every record is Ed25519-signed by the
peer it describes, so a friend can relay it but never rewrite it; an older
record never overwrites a newer one; a peer's identity key is pinned on first
use; and a gossip pull returns only records for peers the caller proves they
already know — nobody learns of a peer they are not already sharing with.

Set it up per friend on the Friends page: their sharerr's URL, and the key they
issued you (from _their_ Friends page). Leave both empty and your instance still
answers their pulls and accepts their pushes; it just never initiates.

## Topology

The **Topology** page is one diagram of how this instance connects to
everything around it: configured library sources on the left, this instance
and its torrent client in the middle, friends on the right. It draws nothing
new — every fact on it already lives on Settings' connection tests, Status'
networking panel and path-mapping table, or the Friends page's endpoint list —
it just puts them in one place, since "why can't Sam see this torrent" or
"which of my two gluetun tunnels is this port actually on" otherwise means
checking three pages by hand.

Each box carries an icon for what kind of thing it is and a tagged row per
detail, so an address is never left to be identified by position. A solid line
to a friend means their address was seen directly; dashed means gossip relayed
it; dotted means a lighthouse answered it; no line at all means that friend's
sharerr has not been heard from yet. Every friend gets their own colour, shared
between their box and the lines reaching it, so which lines belong to whom is
readable at a glance. Each friend's box carries three rows — the address their
feed requests arrive from, their torrent client, and their own sharerr's
announce endpoint — and each line lands on the row it describes. A legend
under the diagram spells out the icons, the border colours (health), and the
line styles. Under that, **Active swarms** lists who is connected to each
torrent right now, from the tracker's own bookkeeping.

**Networking only** hides the sources lane — the *arr apps and library
directories feed sharerr files but are not part of the network — and reframes
the diagram on this instance, its client, and friends. The choice is
remembered per browser, and `/topology?view=networking` links straight to it.

Addresses are redacted by default: an IPv4 keeps its first two octets and hides
the last two (`203.0.113.9` shows as `203.0.•••.•`), and a port keeps only its
leading half. The first half is what you recognise as your own network; the
second half is what identifies one machine on it — so the page stays readable
to you and stays safe to screenshot. A checkbox at the top reveals the real
values; that choice is remembered per browser too.

### Checking that you are actually reachable

Two separate things, because they answer different questions.

Settings → Automatic checks has an opt-in **reachability** probe. With it on,
the Topology page dials this instance's own advertised tracker and feed
addresses and reports whether they answer. It is off by default, and a failure
there says *could not confirm* rather than "your port is shut" — a host
dialling its own public address is exercising NAT hairpinning, which plenty of
perfectly working routers refuse.

The **Debug** page is the version that settles it. It shows what sharerr
believes its own addresses are, and hands you a `bash` + `curl` script with
those addresses already filled in. Run it from somewhere else — a friend's
machine, a phone off wifi, a VPS — and it reports plainly whether the tracker
and the feed are reachable from outside. Any HTTP status counts as reachable:
the feed answering `401` still proves the port is open and sharerr is behind
it.

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

sharerr signs in with a qBittorrent 5.2+ WebUI API key — stateless, no session to
expire, no re-login. Generate one under Options → Web UI → API key, then:

```
printf %s "$KEY" | sharerr vault set qbittorrent.api_key
```

Rotating the key in qBittorrent invalidates the old one immediately, so store the
new one at the same time. Older qBittorrent builds without the API key feature are
not supported — upgrade to 5.2 or newer.

### If a correct key is rejected

**qBittorrent validates the `Host` header's port** against the port it listens on,
and answers `401` before it ever reads the key when they differ. A remapped docker
port (`-p 18080:8080`) or a reverse proxy on another port trips this. Either point
`qbittorrent.url` at the port qBittorrent itself listens on, or turn off Options →
Web UI → _Validate Host header_.

`sharerr doctor` names this, rather than reporting "rejected the API key" and
leaving you to rotate a key that was never wrong.

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

## Using rTorrent / ruTorrent instead of qBittorrent

rTorrent has no HTTP server of its own, so unlike qBittorrent and Transmission
above there is no one standard path to guess — `rtorrent.url` is the exact
address your reverse proxy answers XML-RPC requests on (commonly `/RPC2`, or
ruTorrent's `/plugins/httprpc/action.php`), not a base address sharerr appends
a path to:

```toml
torrent_backend = "rtorrent"

[rtorrent]
url = "http://seedbox.example/RPC2"
username = "rtorrent"
# rTorrent has no categories either — this one value stands in for
# qBittorrent's category and tag, stored in rTorrent's d.custom1 field.
label = "sharerr"
```

Then store the password: `printf %s "$PW" | sharerr vault set rtorrent.password`.
rTorrent's own XML-RPC has no credential of its own; username and password are
sent as HTTP Basic Auth on every request, for the common case where the
reverse proxy in front of the RPC endpoint is what enforces access — if yours
does not, any placeholder values work.

Two differences worth knowing, same "enforced, not just documented" rule as
Transmission's above:

- **No skip-checking**, for the same reason as Transmission: rTorrent always
  verifies a torrent's data against its piece hashes when a download starts.
- **No per-torrent seed-ratio limit.** rTorrent's ratio enforcement is a
  `.rtorrent.rc` schedule, not a setting exposed per torrent over XML-RPC — a
  configured `ratio_limit` is accepted and silently has nothing to attach to.
  `upload_limit_kib` *is* honoured, through a per-torrent named throttle.

Replacing an already-seeding torrent's trackers — what keeps it announcing
somewhere alive after your advertised endpoint rotates — is also incomplete
for rTorrent specifically: its XML-RPC API has never grown a way to remove a
tracker, so sharerr can only add the new endpoint as a fresh tier ahead of the
stale one, not replace it outright. Harmless — the stale tier just goes on
being tried and failing — but see
[`docs/SUPPORTED.md`](docs/SUPPORTED.md)'s "Torrent clients" for the full
reasoning.

## The CLI

The UI covers everything, but each verb has a headless equivalent, which is what a
scripted deployment or a secrets manager wants:

| Command                      | What it does                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| ---------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `sharerr serve`              | The long-running mode: HTTP, the tracker, the feed, and the reconciliation loop. What the container runs.                                                                                                                                                                                                                                                                                                                                                                                                            |
| `sharerr sync`               | One reconciliation pass, then exit.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| `sharerr doctor`             | Checks credentials, service reachability, the tag, and **path mapping resolution** — the check most likely to explain "nothing is shared". The same checks back the web UI's **Status** page, so the two cannot disagree. `--fix` creates a missing tag or qBittorrent category; `--suggest-paths` proposes `[[path_map]]` rules by matching tagged files against a mounted directory (default `/media`) by name and size — a proposal to review, never written automatically. Everything else still needs a person. |
| `sharerr vault set <key>`    | Reads a secret from stdin into the encrypted vault.                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| `sharerr vault list`         | Lists which secret keys are currently set, without their values.                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| `sharerr vault remove <key>` | Deletes a secret from the vault.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |

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

Rust **1.98** or newer (the workspace sets `rust-version`; `docker build .` is the
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

| Crate                  |                                                           |
| ---------------------- | --------------------------------------------------------- |
| `sharerr`              | The binary: CLI, web UI, Torznab, tracker, reconciliation |
| `sharerr-core`         | Domain types, layered config, path mapping. No I/O        |
| `sharerr-arr`          | Sonarr/Radarr clients and tagged-content discovery        |
| `sharerr-client`       | The narrow trait a torrent client backend implements      |
| `sharerr-qbit`         | qBittorrent WebUI client                                  |
| `sharerr-transmission` | Transmission RPC client                                   |
| `sharerr-rtorrent`     | rTorrent XML-RPC client                                   |
| `sharerr-store`        | Encrypted vault + SQLite store                            |
| `sharerr-torrent`      | Torrent construction and tracker resolution               |
| `sharerr-lighthouse`   | The lighthouse rendezvous service — its own binary too    |
| `sharerr-testkit`      | Synthetic fixtures. Never in a release build              |

The original design brief, and the two corrections the implementation forced on
it, are in [docs/DESIGN.md](docs/DESIGN.md).

## AI usage

Heavily inspired by: [Dictionarry/Profilarr's AI Transparency Statement](https://v2.dictionarry.dev/ai-transparency)

I have used generative AI to write large parts of this project. All of the code here is my responsibility regardless: AI is a tool, not an owner of a project. I have personally understood, reviewed and approved all of the AI-generated code in this repository, and _mainline releases_ carry the same accountability to me as anything I write and publish myself.

## Licence

MIT — see [LICENSE](./LICENSE.md).
