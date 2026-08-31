# Working on sharerr

Conventions that are not derivable from the code, and traps that have each cost
real debugging time.

## Table of contents

- [The verification loop](#the-verification-loop)
- [MSRV](#msrv)
- [CodeQL](#codeql)
- [Dependencies](#dependencies)
- [Testing tiers](#testing-tiers)
- [Traps](#traps)
- [Repository](#repository)

## The verification loop

```bash
cargo test --workspace && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo build && cargo fmt --all --check
```

Run it before declaring anything done. `cargo fmt --all --check` is what `ci.yml`'s
`fmt` job actually runs — a change that compiles and passes clippy but was never
run through `cargo fmt` still fails CI on this step alone, which has cost more
than one otherwise-green PR. Run plain `cargo fmt --all` (no `--check`) first if
this fails, then re-run the loop.

**Clippy must stay at zero warnings.** The workspace sets `unwrap_used` and
`expect_used` to `warn` because the vault and the service clients handle secrets;
CI promotes them with `-D warnings`. When a test needs to panic on failure, opt out
with an inner attribute at the top of the module:

```rust
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
```

Never weaken the workspace lint to make a test compile. A non-test `expect()` that
genuinely cannot fail takes a targeted `#[allow]` **plus a comment saying why** —
`QbitConfig::default`'s `Url::parse` on a literal (and its Transmission and
rTorrent twins) are the only examples in the tree.

**`cargo test` does not build `target/debug/sharerr`.** It builds the test harness.
Any CLI smoke test needs an explicit `cargo build` first; running a stale binary has
already produced one confidently wrong conclusion here.

## MSRV

`rust-version` is 1.98, and the Dockerfile pins the same toolchain, which makes
`docker build -f docker/Dockerfile .` the **de-facto MSRV check** — a local
toolchain is invariably newer and will not catch a breach. Two have shipped
unnoticed this way (`str::split_at_checked` in a const context, then
let-chains). Build the image before claiming the declared MSRV holds.

## CodeQL

`./scripts/run_codeql.sh` runs the same analysis `.github/workflows/codeql.yml`
runs in CI — `rust` and `actions`, the default code-scanning suite for each,
`build-mode: none` for both — entirely locally, so a finding surfaces before a
push rather than after one. It needs the CodeQL CLI + bundled query packs
installed once (the script's own header comment says where from and where to);
after that it makes no network calls. Not part of the always-run verification
loop above — the CLI is a large one-time download this repo does not manage,
and a full database build/analyze is much slower than `cargo test`. Run it
before pushing anything that touches crypto/secret handling or a workflow
file, or whenever CI's CodeQL check disagrees with what shipped locally.

**A PR's CodeQL check only ever shows alerts on lines that PR's diff touched** —
GitHub's code-scanning UI is diff-scoped by design. Running this script with no
argument scans the whole tree instead, which is a materially bigger picture: a
first full run here surfaced 52 findings across files no recent PR had touched
at all (chiefly two auth-adjacent test suites), none visible in any PR check
before. Most were the same shape as the CodeQL section under Repository below
— a test literal or a redaction-proving `Debug` print CodeQL's Rust queries
(new since 2.23.3, and still prone to this) read as a real secret reaching a
sink. Reading a `Cleartext logging of sensitive information` or
`Hard-coded cryptographic value` finding therefore takes an extra step this
repo's own history did not immediately suggest: before assuming it is new
behaviour, check whether the flagged line is actually years-old code a diff
simply never happened to include.

## Dependencies

Versions live in `[workspace.dependencies]` in the root `Cargo.toml`, one pin for
the whole tree. Crates write `foo = { workspace = true }` and add only their own
`features`, which cargo unions. Do not pin a version in a crate manifest.

## Testing tiers

**Tier 1** is the default `cargo test` and is hermetic: no network, no containers,
no database. Service clients run against wiremock on loopback, sqlx against
`sqlite::memory:`, migrations are embedded by `sqlx::migrate!`. Keep it that way —
it is what makes CI fast and reliable.

**Tier 2** is `./scripts/run_docker_tests.sh`, opt-in and local only, behind the `e2e`
feature and `#[ignore]`. It drives a real Sonarr + Radarr + qBittorrent stack. CI
compiles it with `--all-features` so it cannot rot, but never runs it — do not add
`--include-ignored`.

The assertion that justifies tier 2 existing: after a real sync through a real
qBittorrent, every media file has the same inode, mtime, and length it started
with. Mocks cannot prove that.

All fixtures are synthetic — invented titles, seeded pseudo-random bytes, so
torrent info hashes are stable across machines. No real content, ever.

Fixtures worth knowing by name: `state::fixtures::{unconfigured, unloadable}`
(a fresh container, with or without a loadable `sharerr.toml`; keep the returned
`TempDir` alive), `web::web_state(serve)` for a `WebState` around one of those,
and `sharerr_testkit::mock::{base_url, mount_json, mount_ok, mount_text,
multipart_field, QBIT_API_KEY, ARR_API_KEY}` for wiremock-backed service clients.

**No tier-1 fixture opens a real vault.** `state::fixtures::unconfigured()` has no
master key, so `ServeState::open_vault`/`tracker_token`/`gossip_identity` all
resolve to "unavailable" there — fine for testing the not-yet-configured path, a
real gap for anything that only behaves differently once a vault-backed secret
*is* set. Prefer testing the store-backed logic directly (pass `Store` and the
resolved secret as plain parameters, the way `tracker::authenticate_token` does).
When a test genuinely needs the vault open, the one sanctioned way is
`figment::Jail::expect_with` with `jail.set_env("SHARERR_MASTER_KEY", ..)` — see
`state.rs`'s `vault_backed_accessors_succeed_and_cache_once_the_vault_opens` —
because `Jail` scopes the variable and serialises against every other Jail test
in the binary. A bare `std::env::set_var` does neither and races the parallel
runner.

**A test that needs the master key _absent_ is just as exposed as one that needs
it present, and just as easy to reach for `fixtures::unconfigured()` alone and
call it done.** `master_key_from_env` reads the real process environment with no
injection point, and this binary's `Jail` tests genuinely mutate it — `Jail`
only serialises against _other Jail closures_, so a bare `#[tokio::test]`
asserting a rejection that depends on the vault failing to open (`BAD_REQUEST`
from a secret-writing handler, say) can flip to success mid-run if one of those
Jail tests happens to be live on another thread at that moment. Seven tests
shipped this way before the race was ever noticed, and it reproduced
deterministically once enough Jail tests existed to make the window wide
enough — it did not show up as one-off flakiness. Wrap this class of test in
`Jail` too, with `jail.clear_env()` and nothing else, exactly as if it needed a
var _set_; see `secrets.rs`'s `opening_a_vault_without_a_master_key_fails_with_no_side_effects`
or `web/settings.rs`'s `save_arr_rejects_when_the_vault_will_not_open_rather_than_write_a_partial_config`.

## Traps

**Torrent name vs release title are two different strings.** Conflating them
stalls seeding at 0%. The torrent's name must describe the file where it already
sits; the release title is what the feed advertises. See `sharerr-torrent`.

**Never move, rename, or re-link media.** This is the project's central constraint,
not a preference. `skip_checking = true` is the default, so qBittorrent seeds a
newly-added torrent immediately rather than re-verifying it — sharerr never wrote
anything qBittorrent's own hash check would need to catch. Set it `false` while
still confirming a path mapping is correct: with checking on, a wrong mapping
seeds mismatched data instead of qBittorrent refusing it.

**Secrets never go in `sharerr.toml`.** They live in the encrypted vault, keyed by
the constants in `sharerr_core::config::secret_keys`. Adding a secret means adding
it to `secret_keys::ALL` (or `commands/vault.rs`'s `vault set`/`vault list` silently
reject it) *and* wiring the constant into the relevant handler in `web/settings.rs`
(which does not consult `ALL` at all — each save handler passes its own secret key
to the shared `write_config_and_secret` helper; Transmission and rTorrent share
one, `save_rpc_client`), or the web UI silently will not manage it.

**One deliberate, documented exception**: `Config::peers` (`sharerr_core::config::PeerImport`),
a one-time `[[peers]]` bootstrap block for restoring friends after a full
data-directory loss. `gossip_key` genuinely is a credential sitting in
`sharerr.toml`, briefly — `commands/serve::import_peers` drains it into the
vault on the next `sharerr serve` start and strips the block from the file
in the same write, so it exists on disk only until that first start. Not a
precedent for anything else; see the struct's own doc comment,
`docs/SETTINGS.md`'s "Restoring friends" section, and `SECURITY.md`'s
by-design list before reaching for this pattern a second time.

**The config file is rewritten in place by the web UI**, comments and all, via
`toml_edit`. A settings path is a hand-typed string in more than one place; check
`web/settings.rs` and `web/templates/settings.html` agree.

**`config_io::env_overrides()` is memoised in a `OnceLock`.** The process env
cannot change after start, so it is scanned once. A test that sets a `SHARERR_*`
variable (even inside a `Jail`) and then renders the settings or wizard page will
see whatever the *first* caller in the binary saw, not its own variable. Test the
lock detection through `collect_overrides(vars)` with an explicit iterator, as
`config_io`'s tests do.

**Every `web/settings.rs` form field must be `#[serde(default)]`**, even ones the
handler goes on to treat as required. An `<input>` can render `disabled` — no
master key yet, or its config path pinned by a `SHARERR_*` env var
(`lock_attr`/`locks` in `settings.html`) — and a disabled input submits nothing at
all. A `Form` field with no default then fails to deserialize *before* the handler
runs, so `reject()`'s own styled error page never renders; the caller gets a bare
`Failed to deserialize form body: missing field` instead, and whatever they typed
in every other field on that form is discarded. Apply it once, at the struct
level (`#[serde(default)]` above the struct, `#[derive(Default)]` on it) rather
than per field — see any struct in `web/settings.rs`'s Forms section — so a field
added later inherits the tolerance instead of needing to remember it.

## Repository

Publishing to GHCR happens two ways. Every push to `main` builds both
architectures — that build is load-bearing as the MSRV check — and ships
unattended under a `sha-<7-char-sha>` tag and nothing else: no `latest`, no
`:main`, no branch tag of any kind, so an image only turns up for someone who
already has the commit sha that produced it. A `v*` tag goes through the
slower, approval-gated path (`build` then `publish`, see each workflow's own
comments) and is what actually moves `latest` and the semver tags.

**Two images ship, from two workflows.** `docker.yml` builds `docker/Dockerfile`
into `ghcr.io/<repo>`; `docker-lighthouse.yml` builds
`docker/Dockerfile.lighthouse` into `ghcr.io/<owner>/sharerr-lighthouse`. They
are separate so a break in one cannot
hold the other's release, and so each is approved on its own. Both Dockerfiles
pin the toolchain to `rust-version`, and **both pins have to move together** —
the lighthouse one silently sat three minors behind for a while, which meant that
image had no working MSRV check at all.

**Every container image is pinned by digest, not just by tag.** Both Dockerfiles
and all nine compose files under `docker/` carry `name:tag@sha256:...`. The tag
stays for legibility; the digest is what actually resolves. Adding an image
means pinning it the same way — `docker buildx imagetools inspect <ref>
--format '{{.Manifest.Digest}}'` prints the digest to paste.

Two things read those pins back, and neither tolerates a bare tag:
`.github/scripts/scan_pinned_images.sh` extracts them by regex (a bare tag is
invisible to it, so an unpinned image is silently unscanned), and dependabot's
`docker` / `docker-compose` entries rewrite tag and digest together.

**A compose file's _name_ decides whether dependabot manages it.** The
`docker-compose` ecosystem matches filenames against
`(docker-)?compose(-\w+)?(\.[\w-]+)?\.ya?ml`. The gluetun service fragment was
`gluetun.reference.yaml` — no "compose" in it, so dependabot never fetched it
and its pin moved by hand; it is `compose.gluetun.reference.yaml` now, and the
three tunnelled stacks `extends` it by that path. A new file holding an image
pin must satisfy that regex, and must sit in a directory `dependabot.yml` lists
— `/docker/deploy/*` matches the directories _under_ `deploy`, not `deploy`
itself, which is why `/docker/deploy` is listed separately.

**CodeQL (`.github/workflows/codeql.yml`) has no in-source suppression
mechanism.** A `// codeql[rule-id]` or `// lgtm[rule-id]` comment above a flagged
line does nothing — GitHub code scanning only recognizes a dismissal made in the
Security tab (or the equivalent REST/`gh api` call), never a source comment. Nine
such dead comments sat in the tree for a while before this was noticed, none of
them suppressing anything. There are exactly two real options for a finding: fix
the code so the value it flags no longer exists (see `sharerr_testkit::mock::rpc_credentials`,
which replaced hard-coded Basic Auth literals across the rTorrent and
Transmission test suites), or dismiss it in the Security tab with a reason and
record *why* somewhere durable — `docs/SECURITY.md`'s "What is out of scope" for
the `rust/path-injection` alerts is the model. A dismissal is also
fingerprint-bound to the flagged line, not the underlying pattern: moving a
literal to a new file (a crate split, a rename) resets the fingerprint and the
same finding reappears as "new" even though nothing about it changed.

**Three scheduled workflows notify by keeping one issue current**, rather than by
turning a run red: `image-scan.yml`'s `pins` job, `tool-versions.yml`, and
`advisories.yml`. Each opens a single labelled issue when its finding appears,
rewrites that issue's body on every later run, and **closes it** on the run that
finds things clean. The shape is deliberate — every one of these findings is a
standing state of the repo that nobody caused with a commit, and a scheduled run
that goes red on a Monday morning is not a notification if nobody opens it. If
you add a fifth, follow the same shape: `gh label create --force`, reuse the
first open issue with that label, `--body-file` (never `--body`, which arrives
YAML-indented into the code block).

None of the three blocks a merge. `ci.yml` is the gate — it fails on anything a
diff introduces; these four report on what upstream did while nobody was
looking.

**`image-scan.yml` runs two scans that answer different questions.** `trivy`
scans the image the repo _publishes_ ("has what we shipped gone stale");
`pins` scans the images it _builds on_ ("is there a better base for the next
one"). The second only speaks when a repin is verified to reduce the fixable
CVE count — it scans the pinned digest, scans what the tag points at now, and
stays quiet unless the second is strictly better. `--ignore-unfixed` is
load-bearing there, not a flag: without it the permanent won't-fix pile makes
every comparison noise against noise.

**Pinned tool versions that dependabot cannot see** are zizmor, actionlint,
cargo-llvm-cov, lychee and typos, each installed from a cached, pinned GitHub
release asset by `.github/actions/setup-tool` (ported from say-hi's
identically-named action, which solved this the same way there first) rather
than a fresh `pip install`/`go install`/`cargo install` on every run.
`.github/actions/setup-tool/tools.txt` is the one roster — version, download
URL, archive shape, verify flag — read both by that action and by
`check_tool_versions.sh`, so a pin cannot go undrift-checked; bumping one is a
hand edit to that file and nothing else. hadolint and trivy are deliberately
unpinned (newest release, always), so neither has a row.

The roadmap is `docs/ROADMAP.md`, and it holds candidates that have been
weighed but not all committed to as well as firm intentions — an idea belongs
there or in `docs/SUPPORT.md`'s "Not supported" section, never in both. The original design brief and
the two premises the implementation disproved are in `docs/DESIGN.md`.

**`main` carries a ruleset (PR required, protected ref, verified signatures) that
no Actions `git push` can satisfy** — a `GITHUB_TOKEN`-authored commit has no
signature and can't open a PR against itself. A workflow that generates
something meant to be published (the coverage badge is the current example)
therefore cannot commit it to `main` at all; it has to reach the world some
other way. The pattern here is `coverage.yml` uploading the figure as a build
artifact, and `pages.yml` — on a `workflow_run` trigger watching for
`coverage.yml` to finish — looking up the newest successful run via `gh api
.../actions/workflows/coverage.yml/runs`, downloading that run's artifact with
`actions/download-artifact`, and writing it straight into `_site/` after Jekyll
has already built it, so it ships as part of the same Pages deploy. Copy that
shape for the next generated-artifact-on-`main` case rather than reaching for a
bot commit; see `.github/zizmor.yml`'s `dangerous-triggers` entry for why the
`workflow_run` trigger this depends on is safe here despite zizmor's default
suspicion of it (no attacker-controlled ref is ever checked out or read).
