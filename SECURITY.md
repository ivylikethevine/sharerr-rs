# Security policy

sharerr is **experimental** — see [the README](README.md) — and has not had a
tagged release. There is exactly one line of support: the `main` branch
(mirrored to `:main` on GHCR) and `dev`, the branch active development
happens on. There are no older versions to patch.

## Reporting a vulnerability

Please report it privately through
[GitHub's security advisories](https://github.com/ivyduggan/sharerr-rs/security/advisories/new)
for this repository, rather than a public issue — that gives us a private
channel to work out a fix before any detail is public.

Include what you'd include for any bug report: what you found, how to
reproduce it, and what you think the impact is. There is no bug bounty; this
is a personal project maintained by one person, and turnaround depends on
that person's spare time.

## What is in scope

sharerr handles real credentials — *arr API keys, torrent client
credentials, the tracker's announce token — in its encrypted vault
(`sharerr-store`), and exposes a web UI, a Torznab feed, and a BitTorrent
tracker over HTTP. Anything that could read, write, or bypass authentication
for any of those is in scope: vault or session handling, the auth guard
around the settings routes, the per-peer key model behind the feed and
tracker, and the lighthouse rendezvous service's privacy properties (see
[the roadmap](docs/ROADMAP.md#the-lighthouse) for what those are supposed to
guarantee).

A few things are **by design**, not a vulnerability report waiting to
happen — see [the README](README.md#quickstart) before reporting:

- **The session cookie is not sent over TLS.** sharerr is meant to run on a
  LAN; put it behind a TLS-terminating proxy if that does not describe your
  network.
- **Losing `SHARERR_MASTER_KEY` loses every stored credential.** There is no
  recovery path — the vault is encrypted with it and nothing else.
- **The lighthouse answers an invalid key with a plausible fabricated
  record rather than an error.** That is the anti-scraping property the
  design is built around, not information leakage.

## What is out of scope

Vulnerabilities in a service sharerr _talks to_ (Sonarr, Radarr, qBittorrent,
Transmission, rTorrent, Prowlarr, gluetun) belong to those projects, not this
one — unless sharerr is the thing misusing their API in a way that creates
the exposure.
