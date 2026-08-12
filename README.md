This is a project that aims to help users with content share that content over
already existing tools, as a friend-to-friend system. It aims to be as friction-less
as possible, and slim on resource and configuration requirements. It will be
run typically as a docker image, accessable via a web interface.

I want the tech stack to use Rust as the base layer. There are existing APIs
for the services I would like to integrate with. The idea is as follows:

1. Existing selfhosted deployments have libraries of content, as well as all
   the required metadata to accurately request and serve that content.
2. qbittorrent has a built-in rss feed & an optional embedded tracker
3. Prowlarr can index torrents from qbittorrent's rss feed
4. I would like sharerr to connect to sonarr and radarr to find any content tagged
   "sharerr", and serve it over the existing tools, making the new torrent as easily
   searchable/indexable by those existing tools as possible.
5. One would then be able to connect to another friend's sharerr instance to
   share content.
6. Eventually, I would like a web interface to manage sharerr instances and content,
   and the ability for a sonarr or radarr instance to directly request content from
   a sharerr instance, again with as much metadata as possible, using the sonarr/radarr
   data as well as metadata found with the file. I want to also preserve any existing
   torrents if possible, instead of moving the file & causing errors.

The services:

1. Sonarr - library and request system for tv shows
2. Radarr - library and request system for movies
3. Qbittorrent - torrent client
4. Prowlarr - torrent indexer
5. Docker - container runtime

Requirements:

1. Security - use API tokens for the services, and store them securely. Only send
   them with requests as necessary. Do not store them in plaintext, and maintain
   them between service restarts.
2. Network usage - do not make any requests outside of the configured services
   unless absolutely necessary.
3. Testing - use docker ro run the various services with test configurations.
   Do not use any real files or filenames.

Assumptions:

1. The user has a system with currently deployed sonarr (and/or) radarr.
   This is typically via docker.
2. The user has a torrent client (qbittorrent) and indexer (prowlarr) running.
3. The user may or may not be using a VPN, or a VPN container such as gluetun.
4. The user has their media library accessable to the container(s).

### Getting started

sharerr is configured from its web interface. No CLI command is required.

```bash
docker run -d --name sharerr \
  -p 8477:8477 \
  -e SHARERR_MASTER_KEY="$(openssl rand -base64 32)" \
  -v sharerr-config:/config \
  -v sharerr-data:/data \
  -v /path/to/library:/media:ro \
  ghcr.io/ivyduggan/sharerr-rs:latest
```

Then open `http://localhost:8477/`. The first visit asks you to create an account —
whoever gets there first claims the instance, so do it now rather than leaving it
reachable and unclaimed. After that, **Settings** takes the Sonarr and Radarr URLs
and API keys, the qBittorrent URL, username and password, the path mappings, and
the tracker's advertised host. Each service has a *Test connection* button, and
changes take effect within about fifteen seconds — no restart.

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

### Sharing with a friend

sharerr publishes what it shares as a **Torznab** feed, which is what Prowlarr
speaks. In **Settings → Indexer**, generate an API key and copy it together with
the feed URL. Your friend adds a *Generic Torznab* indexer in their Prowlarr using
those two values; their Sonarr and Radarr then find your releases through it, with
the TVDB/TMDb/IMDb ids attached so a release matches a known series or film rather
than being parsed from its name.

The feed lists only what is actually seeding, and the `.torrent` files it links to
are served from the same instance. Both the feed and the downloads require the API
key — without one, the endpoint stays closed rather than open, because the feed is
a list of everything you share.

The feed URL is built from `tracker.advertised_host`, so that has to be an address
your friend can reach. Everything here is a single HTTP port; whatever you do to
make port 8477 reachable also makes the tracker and the feed reachable.

#### Which tracker

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

#### Configuring it without the UI

Everything above has a headless equivalent, which is what a scripted deployment or
a secrets manager wants:

```bash
printf %s "$SONARR_API_KEY" | docker exec -i sharerr sharerr vault set sonarr.api_key
docker exec sharerr sharerr doctor
```

Settings can also come from the environment — `SHARERR_QBITTORRENT__URL` sets
`qbittorrent.url`, and so on for any field. Be aware that these take precedence
over the config file, so a field pinned by a variable cannot be changed from the
UI; sharerr renders those inputs disabled and names the variable rather than
accepting a save that would be silently discarded.

###### AI Usage

Heavily inspired by: https://v2.dictionarry.dev/ai-transparency

I have used generative AI to write large parts of this project. Regardless, all of the code in this repository is my _responsibility_. AI is a tool, not an owner of a project. I have personally understood, reviewed, and approved all of the AI generated code in this repository. _Mainline releases_ have the same level of accountability to me as any code I write and publish.

###### The MIT License (MIT)

From: https://mit-license.org/

Copyright © 2026 Ivy Duggan ivylikethevine.com

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the “Software”), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED “AS IS”, WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
