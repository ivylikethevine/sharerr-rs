# Security policy

sharerr is **experimental** — see [the README](../README.md) — and has not had a
tagged release, so there is no `:latest` or versioned GHCR image to patch yet.
Every push to `main` does publish a `sha-<commit>`-tagged image unattended
(see [`CLAUDE.md`](https://github.com/ivylikethevine/sharerr-rs/blob/main/CLAUDE.md#repository)'s
Repository section) — those are commit-pinned CI artifacts
rather than a supported release series, found only by someone who already has
the commit sha, and nothing here treats them as something to patch in place.
There is exactly one line of support: the `main` branch and `dev`, the branch
active development happens on. There are no older versions to patch.

## Table of contents

- [Reporting a vulnerability](#reporting-a-vulnerability)
- [What happens after a report](#what-happens-after-a-report)
- [Supported versions](#supported-versions)
- [What is in scope](#what-is-in-scope)
- [Why the existing controls are enough](#why-the-existing-controls-are-enough)
- [What is out of scope](#what-is-out-of-scope)

## Reporting a vulnerability

Please report it privately through
[GitHub's security advisories](https://github.com/ivylikethevine/sharerr-rs/security/advisories/new)
for this repository, rather than a public issue — that gives us a private
channel to work out a fix before any detail is public. Private vulnerability
reporting is enabled on this repository, so that link works without needing
to be a collaborator first.

Include what you'd include for any bug report: what you found, how to
reproduce it, and what you think the impact is. There is no bug bounty; this
is a personal project maintained by one person, and turnaround depends on
that person's spare time.

## What happens after a report

- **Acknowledgement**: within 14 days of the advisory being filed. That is a
  target, not a contractual SLA — see "maintained by one person" above — but
  it is the number a report should be able to expect a response by.
- **Triage**: the report is read, reproduced if possible, and given a
  severity. You'll hear which of "confirmed, working on a fix", "confirmed,
  won't fix" (with the reason — see [What is out of scope](#what-is-out-of-scope)
  for what that already looks like), or "not reproducible, need more detail"
  applies.
- **Fix and disclosure**: a confirmed vulnerability is fixed in the private
  advisory's own fork first, not in a public PR, so the fix doesn't itself
  announce the bug before a release carries it. The advisory stays private
  until a patched `sha-<commit>` image (or, once one exists, a tagged
  release) is available, at which point it's published and, where the
  severity warrants it, a CVE is requested through GitHub's own advisory
  flow.
- **Credit**: reporters are credited by name (or handle) in the published
  advisory and in the release notes that ship the fix, unless you ask to
  stay anonymous — say so in the report if that's what you want.

## Supported versions

There is exactly one supported line: the `main` branch, and by extension the
`sha-<commit>` image built from whatever commit is newest on it. There is no
older version to patch, because there has been no tagged release yet — see
the note at the top of this file. Once `v0.1.0` ships, this section will name
which tagged line(s) get fixes and for how long; until then, "upgrade" means
"pull the newest `sha-<commit>` image", which is also the only upgrade path
that exists.

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
[`LIGHTHOUSE.md`](LIGHTHOUSE.md) for what those are supposed to
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
  `/setup`. It also carries `Secure` when sharerr can tell the request
  arrived over HTTPS — see below for exactly how that is decided.
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

- **The session cookie's `Secure` flag is decided per request, not fixed
  at compile time.** sharerr terminates no TLS of its own, so it has no
  first-hand way to know whether a connection is encrypted; it infers the
  answer from `X-Forwarded-Proto` (or RFC 7239 `Forwarded`), checked
  first-hop-only, when either header is present, and treats the connection
  as plain HTTP otherwise. On the plain-HTTP LAN sharerr is meant to run
  on, the cookie travels without `Secure` — a `Secure` cookie on a
  plain-HTTP origin is silently dropped by the browser, which presents as
  "login does nothing". Behind a TLS-terminating proxy that sets one of
  those headers, the cookie carries `Secure` automatically, no
  configuration required. These headers are attacker-controllable by
  anyone who can already reach the port, and that is deliberately
  tolerated here: claiming `https` on a plain connection only costs the
  spoofer their own sign-in (the browser discards the cookie it is
  handed), and claiming `http` on a real TLS connection only drops
  `Secure` from a cookie on the response to the spoofer's own request.
  Nothing else in sharerr trusts
  either header for anything — see `arrived_over_https` in
  `crates/sharerr/src/web/auth.rs` for where that line is drawn. If your
  network is not a trusted LAN and you are not behind a TLS-terminating
  proxy, put one in front.
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
- **A `[[peers]]` block in `sharerr.toml` can hold a real credential — a
  friend's gossip key — in plaintext, deliberately, contradicting every
  other rule in this file about secrets never sitting outside the vault.**
  This is a one-time, self-deleting restore path (see
  [`SETTINGS.md`](SETTINGS.md#restoring-friends-after-a-full-data-directory-loss)):
  an operator hand-writes it after a full data-directory loss, and it is
  drained into the real store and vault — and stripped from the file, and
  from the running instance's own in-memory configuration — the moment
  anything adopts it: the next `sharerr serve` start, or immediately if it
  is instead pasted into a running instance through Settings → Backup and
  restore. Nothing else in sharerr ever writes a secret to `sharerr.toml`,
  and the field is `skip_serializing` on `Config` itself, so it can never be
  written back out through it once populated, including by a debug log of
  the running configuration. The exposure window is exactly "on disk,
  unencrypted, until the next thing reads this instance's config" — treat a
  `sharerr.toml` carrying an unconsumed `[[peers]]` block the same as you
  would a raw vault secret sitting in a text file, because that is what it
  is.
- **The Friends page's "export as backup block" reads a gossip key back
  out of the vault and puts it in a downloadable file — the one place in
  sharerr's web UI that shows a _previously stored_ secret again**, rather
  than only ever revealing one once at creation (every other secret in this
  instance, including a friend's own key into it, is write-only from the
  moment it is first set). This exists to produce the `[[peers]]` block
  above from a live instance rather than requiring an operator to have
  separately saved every gossip key by hand. The download is behind the
  same signed-in session guard as every other page here; nothing about it
  is reachable without already being able to read the Friends page.

## Why the existing controls are enough

sharerr is designed to run on a trusted LAN, for one operator and the friends
they explicitly grant a key to — not as a service exposed to the open
internet. The assurance case follows from that threat model, not in spite of
it:

- **Secrets at rest** are behind Argon2id-derived XChaCha20-Poly1305
  (`sharerr-store`'s vault) keyed by an operator-supplied `SHARERR_MASTER_KEY`
  that never touches disk in plaintext form. Losing that key loses the vault
  — deliberately; there is no secondary key an attacker could instead target.
- **Every credential class uses a hash or cipher shaped for what it protects**,
  not one uniform choice: Argon2id (iterated, salted) for the one class of
  secret a human chose — login passwords — where offline guessing is the
  real risk; SHA-256 for machine-generated 160-bit peer tokens, where the
  attack that iteration defends against (offline dictionary guessing of a
  human-chosen secret) does not apply, and an indexed equality lookup on
  every Torznab request does. Both choices are stated, not implicit — see
  the two bullets above and `sharerr-store/src/peers.rs`'s own header
  comment.
- **Every network-facing entry point either authenticates or narrows to a
  private-address allowlist** — the web UI and Torznab feed require a
  session or a peer key, the tracker fails closed on any vault or database
  failure rather than admitting, and the gluetun webhook (which by
  construction cannot itself authenticate, since gluetun's hooks are bare
  `wget`s) is reachable only from private source addresses. There is no
  endpoint that trusts unauthenticated input from the public internet.
- **The gossip layer's integrity does not depend on transport security**:
  every record is Ed25519-signed by the peer it describes and carries a
  signed timestamp, so a compromised or malicious relay can at worst refuse
  to forward a record, never forge or replay an older one.
- **Zero `unsafe` code** (`unsafe_code = "forbid"` at the workspace level,
  enforced by every crate, verified by `cargo clippy -D warnings` in CI)
  removes the memory-safety class of bug entirely, and static analysis
  (CodeQL, clippy, cargo-deny) runs on every push and weekly against the
  dependency graph, so a newly disclosed advisory in a dependency is caught
  without a maintainer having to go looking for it.
- **What this does not cover, by design**: no login rate limit, no security
  response headers, no recovery from a lost master key — all three are
  named explicitly above rather than left as silent gaps, because the
  threat model this project accepts is a trusted LAN with one operator, not
  a multi-tenant service on the open internet. Anyone deploying outside that
  model (a public-facing instance, an untrusted network) should treat this
  section as the boundary of what sharerr itself defends against, and add a
  reverse proxy with its own rate limiting and headers in front.

## What is out of scope

Vulnerabilities in a service sharerr _talks to_ (Sonarr, Radarr, qBittorrent,
Transmission, rTorrent, Prowlarr, gluetun) belong to those projects, not this
one — unless sharerr is the thing misusing their API in a way that creates
the exposure.

**The `sharerr.toml` path CodeQL's `rust/path-injection` query flags in
`config_io.rs`.** The path a `ConfigFile` opens or writes always traces back to
`ServeState::config_path()`, which is set once at process start from the `-c` /
`--config` CLI flag or `SHARERR_CONFIG` env var (`cli.rs`) and never
reassigned. No HTTP handler passes a path in — the settings forms submit TOML
text and field values, never a path (`web/settings.rs`'s `import_config` and
`prepare_config`). Whoever controls that flag or that env var already controls
the process, so there is no privilege boundary here to enforce; these alerts
are dismissed as won't-fix rather than coded around, since a containment check
would only break legitimate `--config` values pointing outside a fixed
directory. See [`CLAUDE.md`](https://github.com/ivylikethevine/sharerr-rs/blob/main/CLAUDE.md#codeql)'s
CodeQL note for how alert dismissal is tracked.

**`RUSTSEC-2023-0071` (the `rsa` crate's Marvin Attack timing side-channel)
is not in this list**, though a stale OpenSSF Scorecard report may claim it
should be. `rsa` reaches `Cargo.lock` only via `sqlx-mysql` — a lock entry
`sqlx`'s manifest hands out for every optional backend regardless of
activation — and `sqlx` 0.9 does not depend on it, so `rsa` is absent from
the lockfile (`cargo tree -i rsa` matches nothing, and neither does a grep of
`Cargo.lock`) and there is nothing for a scanner to misread. Kept here as the
worked example of how a lockfile-only finding gets documented; see
`deny.toml`'s matching note for the `cargo deny` side of the same answer.
