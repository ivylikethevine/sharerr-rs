# Security policy

How to report a vulnerability, what happens next, what is in and out of
scope, and why the existing controls fit the threat model. sharerr is
**experimental** and has had no tagged release; see
[Supported versions](#supported-versions).

## Table of contents

- [Reporting a vulnerability](#reporting-a-vulnerability)
- [What happens after a report](#what-happens-after-a-report)
- [Supported versions](#supported-versions)
- [What is in scope](#what-is-in-scope)
- [Why the existing controls are enough](#why-the-existing-controls-are-enough)
- [What is out of scope](#what-is-out-of-scope)

## Reporting a vulnerability

Report it privately through
[GitHub's security advisories](https://github.com/ivylikethevine/sharerr-rs/security/advisories/new)
for this repository, not a public issue. Private vulnerability reporting is
enabled, so that link works without being a collaborator. Include what you
found, how to reproduce it, and what you think the impact is. There is no bug
bounty; this is a personal project maintained by one person, and turnaround
depends on their spare time.

## What happens after a report

- **Acknowledgement** within 14 days of the advisory being filed. A target,
  not a contractual SLA.
- **Triage**: reproduced if possible and given a severity. You will hear
  which of "confirmed, working on a fix", "confirmed, won't fix" (with the
  reason; see [What is out of scope](#what-is-out-of-scope)), or "not
  reproducible, need more detail" applies.
- **Fix and disclosure**: a confirmed vulnerability is fixed in the private
  advisory's own fork first, so the fix does not announce the bug before an
  image carries it. The advisory is published once a patched `sha-<commit>`
  image (or, once one exists, a tagged release) is available, and a CVE is
  requested through GitHub's advisory flow where severity warrants it.
- **Credit**: reporters are credited by name or handle in the advisory and
  the release notes unless they ask to stay anonymous.

## Supported versions

Exactly one supported line: the `main` branch, and the `sha-<commit>` image
built from whatever commit is newest on it (see
[`docs/RELEASING.md`](RELEASING.md#between-releases-the-sha-tag)). There is no
older version to patch, because there has been no tagged release. Once
`v0.1.0` ships, this section will name which tagged lines get fixes and for
how long; until then, "upgrade" means "pull the newest `sha-<commit>`
image".

## What is in scope

sharerr handles real credentials (*arr API keys, torrent-client credentials,
the tracker's announce token, gluetun API keys, a notification webhook URL,
and this instance's Ed25519 gossip signing key) in its encrypted vault, and
exposes a web UI, a Torznab feed, gossip, and a BitTorrent tracker over HTTP.
Anything that could read, write, or bypass authentication for any of those
is in scope: vault or session handling, the auth guard in front of every UI
page except `/setup`, `/login`, `/logout` and `/assets`, the per-peer key
model behind the feed, gossip and tracker, and the lighthouse's privacy
properties (see [`LIGHTHOUSE.md`](LIGHTHOUSE.md)).

What is already there, so a report can say which layer it gets past:

- Login passwords are Argon2id hashes with a per-user salt; an unknown
  username still pays a full verification against a decoy hash, and the form
  gives one message for both cases, so accounts cannot be enumerated.
- Sessions are 256-bit tokens held only in memory (a restart revokes them
  all) with a 14-day idle expiry; a password change re-checks the current
  password and revokes every other session.
- The cookie is `HttpOnly` and `SameSite=Strict`, and a middleware over the
  whole router refuses any non-GET request whose `Origin` does not match
  `Host`, including on `/login` and `/setup`.
- The vault is XChaCha20-Poly1305 under a key derived from
  `SHARERR_MASTER_KEY` with Argon2id and a per-vault salt.
- Peer keys are stored only as SHA-256; revocation is enforced in the query
  itself, and a friend's key hash doubles as their announce token, so
  revoking a friend cuts off their announces too. Comparisons are
  constant-time.
- The tracker fails closed: an unreadable database or a locked vault refuses
  every announce, a bad token is refused before the instance reveals whether
  it holds the info hash, and a scrape must name an info hash.
- Gossip records are Ed25519-signed by the peer they describe, with a
  `signed_at` that blocks replaying an older record, and a pull only returns
  the intersection with keys the caller already proved it knows.
- The gluetun hook endpoints are unauthenticated by design (gluetun's hooks
  are bare `wget`s) but answer only private source addresses.

A few things are **by design**, not a vulnerability report waiting to happen:

- **The session cookie's `Secure` flag is decided per request.** sharerr
  terminates no TLS, so it infers HTTPS from `X-Forwarded-Proto` or RFC 7239
  `Forwarded` (first hop only) and treats the connection as plain HTTP
  otherwise. On the LAN it is meant for, the cookie travels without `Secure`;
  behind a TLS-terminating proxy it carries it automatically. Those headers
  are spoofable by anyone who can reach the port, and that is tolerated:
  claiming `https` on a plain connection only costs the spoofer their own
  sign-in, and claiming `http` on a TLS connection only drops `Secure` from
  the response to their own request. Nothing else trusts either header; see
  `arrived_over_https` in `crates/sharerr/src/web/auth.rs`. If your network
  is not a trusted LAN, put a TLS-terminating proxy in front.
- **No login rate limit or lockout, and no security response headers** (CSP,
  `X-Frame-Options`, and so on). Argon2's cost per attempt is the only brake
  on guessing, which is the trade a LAN tool with one operator account makes.
- **The feed API key and the `.torrent` download token travel as query-string
  parameters**, and the tracker's announce and scrape tokens as path
  segments. Consistent with the threat model, but query strings and paths
  commonly end up in access logs if a reverse proxy sits in front.
- **Losing `SHARERR_MASTER_KEY` loses every stored credential.** There is no
  recovery path; the vault is encrypted with it and nothing else.
- **The lighthouse answers an invalid key with a plausible fabricated
  record**, not an error. That is the anti-scraping property, not leakage.
  Its `report` endpoint answers honestly, so posting under a guessed key hash
  reveals whether that hash is in use; a peer whose reports are refused has
  to be able to find out. And **the first keypair to report under a key hash
  keeps it** until the record ages out: someone who learns a key hash before
  the legitimate peer ever reports can claim the slot and deny that pair the
  rendezvous, but cannot impersonate anyone, and the remedy is a new key.
- **A `[[peers]]` block in `sharerr.toml` can hold a friend's gossip key in
  plaintext, deliberately**, as a one-time, self-deleting restore path after
  a full data-directory loss. It is drained into the vault and stripped from
  the file the moment anything reads it, and the field is `skip_serializing`
  so it can never be written back out. Treat a `sharerr.toml` carrying an
  unconsumed block the same as a raw vault secret in a text file. See
  [`SETTINGS.md`](SETTINGS.md#restoring-friends-after-a-full-data-directory-loss).
  The Friends page's "export as backup block" is the one place the web UI
  shows a _previously stored_ secret again, to produce that block from a
  live instance; it sits behind the same session guard as every other page.

## Why the existing controls are enough

sharerr is designed to run on a trusted LAN, for one operator and the friends
they explicitly grant a key to, not as a service exposed to the open
internet. The assurance case follows from that threat model:

- **Every credential class uses a hash or cipher shaped for what it
  protects**: Argon2id for the one class a human chose (login passwords),
  where offline guessing is the real risk; SHA-256 for machine-generated
  160-bit peer tokens, where iteration defends against nothing and an
  indexed lookup on every feed request matters. Both choices are stated in
  `sharerr-store/src/peers.rs`'s header comment.
- **Every network-facing entry point either authenticates or narrows to a
  private-address allowlist.** There is no endpoint that trusts
  unauthenticated input from the public internet.
- **The gossip layer's integrity does not depend on transport security**: a
  compromised relay can at worst refuse to forward a record.
- **Zero `unsafe` code** (`unsafe_code = "forbid"` at the workspace level)
  removes the memory-safety class of bug, and static analysis (CodeQL,
  clippy, cargo-deny) runs on every push and weekly against the dependency
  graph, so a new advisory in a dependency is caught without anyone looking.
- **What this does not cover, by design**: no login rate limit, no security
  response headers, no recovery from a lost master key. Anyone deploying
  outside the model (a public-facing instance, an untrusted network) should
  treat this section as the boundary of what sharerr defends against and put
  a reverse proxy with its own rate limiting and headers in front.

## What is out of scope

Vulnerabilities in a service sharerr _talks to_ (Sonarr, Radarr, qBittorrent,
Transmission, rTorrent, Prowlarr, gluetun) belong to those projects, unless
sharerr is misusing their API in a way that creates the exposure.

**The `sharerr.toml` path CodeQL's `rust/path-injection` query flags in
`config_io.rs`.** That path always traces back to `ServeState::config_path()`,
set once at process start from `--config` or `SHARERR_CONFIG` and never
reassigned; no HTTP handler passes a path in. Whoever controls that flag
already controls the process, so there is no privilege boundary to enforce.
These alerts are dismissed as won't-fix in the Security tab rather than coded
around, since a containment check would only break legitimate `--config`
values. This paragraph is the record of that dismissal; a dismissal is
fingerprint-bound to the flagged line, so moving the code resets it and the
finding reappears as new.

**The vault key _names_ `rust/cleartext-logging` flags in `commands/doctor.rs`.**
`TorrentClientSettings::api_key_key` and `::password_key` are
`Option<&'static str>`, and the only values they ever hold are the
`secret_keys` constants — `"qbittorrent.api_key"`, `"transmission.password"`
and friends. They name the vault slot a secret is stored _under_; they cannot
carry the secret itself, because the type is a compile-time literal and no
runtime value is ever assigned to one. `doctor` prints them so an operator can
see which slot to fill (`qbittorrent.api_key is set`), which is most of what
the command is for. CodeQL matches them on the field name alone — anything
containing `api_key` or `password` is sensitive to its heuristic, regardless of
what flows through it — and there is no rename that both drops those substrings
and still says what the field is. Dismissed rather than renamed.

**The operator's own username in `doctor`'s client summary.** The same query
flags `println!("  client: {} {} (user {username})")`. That line exists so an
operator confirms _which account_ the configured client authenticates as; a
`doctor` run that hid it would send someone to debug the wrong service, the
same failure the surrounding comment describes for the URL. It writes to the
operator's terminal about their own instance, not to a shared log, and the
username is not a credential.

**`RUSTSEC-2023-0071` (the `rsa` crate's Marvin Attack) is not in this
list**, though a stale Scorecard report may claim it should be. `rsa` would
ride in only via `sqlx-mysql`, and `sqlx` 0.9's mysql backend does not depend
on it, so it is absent from the lockfile (`cargo tree -i rsa` matches
nothing). Kept as the worked example of documenting a lockfile-only finding;
`deny.toml` carries the matching note.
