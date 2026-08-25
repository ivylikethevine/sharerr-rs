# Security policy

sharerr is **experimental** — see [the README](../README.md) — and has not had a
tagged release, and the GHCR image is only published on a `v*` tag, so there
is no image to patch yet either. There is exactly one line of support: the
`main` branch and `dev`, the branch active development happens on. There are
no older versions to patch.

## Reporting a vulnerability

Please report it privately through
[GitHub's security advisories](https://github.com/ivylikethevine/sharerr-rs/security/advisories/new)
for this repository, rather than a public issue — that gives us a private
channel to work out a fix before any detail is public.

Include what you'd include for any bug report: what you found, how to
reproduce it, and what you think the impact is. There is no bug bounty; this
is a personal project maintained by one person, and turnaround depends on
that person's spare time.

## What is in scope

sharerr handles real credentials — *arr API keys, torrent client
credentials, the tracker's announce token (current and rotated-out), gluetun
API keys, a notification webhook URL, and this instance's Ed25519 gossip
signing key, the identity every friend has pinned — in its encrypted vault
(`sharerr-store`), and exposes a web UI, a Torznab feed, gossip, and a
BitTorrent tracker over HTTP. Anything that could read, write, or bypass
authentication for any of those is in scope: vault or session handling, the
auth guard in front of every UI page except `/setup`, `/login`, `/logout`
and `/assets`, the per-peer key model behind the feed, gossip and tracker,
and the lighthouse rendezvous service's privacy properties (see
[the roadmap](ROADMAP.md#the-lighthouse) for what those are supposed to
guarantee).

What is already there, so a report can say which layer it gets past:

- Login passwords are Argon2id hashes with a per-user salt; an unknown
  username still pays a full verification against a decoy hash, and the
  form gives one message for both cases, so accounts cannot be enumerated.
- Sessions are 256-bit tokens held only in memory (a restart revokes them
  all) with a 14-day idle expiry; a password change re-checks the current
  password and revokes every other session.
- The cookie is `HttpOnly` and `SameSite=Strict`, and a middleware over the
  whole router refuses any non-GET request whose `Origin` does not match
  `Host` — belt and braces against CSRF, including on `/login` and
  `/setup`.
- The vault is XChaCha20-Poly1305 under a key derived from
  `SHARERR_MASTER_KEY` with Argon2id and a per-vault salt.
- Peer keys are stored only as SHA-256; revocation is enforced in the query
  itself, and a friend's key hash doubles as their announce token, so
  revoking a friend cuts off their announces too. Token comparisons are
  constant-time.
- The tracker fails closed: an unreadable database or a locked vault refuses
  every announce, a bad token is refused before the instance reveals whether
  it holds the info hash, and a scrape must name an info hash.
- Gossip records are Ed25519-signed by the peer they describe, with a
  `signed_at` that blocks replaying an older record, and a pull only returns
  the intersection with keys the caller already proved it knows.
- The gluetun hook endpoints are unauthenticated by design (gluetun's hooks
  are bare `wget`s) but answer only private source addresses.

A few things are **by design**, not a vulnerability report waiting to
happen — see [the README](../README.md#quickstart) before reporting:

- **The session cookie is not marked `Secure`**, so it travels over plain
  HTTP. sharerr is meant to run on a LAN; put it behind a TLS-terminating
  proxy if that does not describe your network.
- **There is no login rate limit or lockout, and no security response
  headers** (CSP, `X-Frame-Options`, and so on). Argon2's cost per attempt
  is the only brake on guessing, which is the trade a LAN tool with one
  operator account makes.
- **Losing `SHARERR_MASTER_KEY` loses every stored credential.** There is no
  recovery path — the vault is encrypted with it and nothing else.
- **The lighthouse answers an invalid key with a plausible fabricated
  record rather than an error.** That is the anti-scraping property the
  design is built around, not information leakage.
- **The lighthouse's `report` endpoint answers honestly**, so posting a
  record of your own under a guessed key hash reveals whether that key hash
  is in use — something a lookup would never tell you. The report side is
  deliberately not covered by the fabrication property: a peer whose reports
  are being refused has to be able to find out, or it sits believing it is
  reachable when it is not.
- **The first keypair to report under a key hash keeps it** until that
  record ages out. Someone who learns a key hash before the legitimate peer
  has ever reported can claim the slot and deny that pair the rendezvous.
  They cannot impersonate anyone — a friend compares the record's `pubkey`
  against the identity they already hold — and the remedy is to issue that
  friend a new key. Trust-on-first-use has no better answer.

## What is out of scope

Vulnerabilities in a service sharerr _talks to_ (Sonarr, Radarr, qBittorrent,
Transmission, rTorrent, Prowlarr, gluetun) belong to those projects, not this
one — unless sharerr is the thing misusing their API in a way that creates
the exposure.
