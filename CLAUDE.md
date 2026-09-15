# Working on sharerr

Only what an agent cannot derive from the code, and traps that have each cost
real debugging time. The human-facing contract (the verification loop, the
lint policy, the test policy, MSRV, what CI runs, which docs change with what)
is [`docs/CONTRIBUTING.md`](docs/CONTRIBUTING.md); this file links to it
rather than copying it, and adds only what an agent needs beyond it.

## Contents

- [Verification loop](#verification-loop)
  - [Testing tiers](#testing-tiers)
  - [CodeQL](#codeql)
- [Hard constraints](#hard-constraints)
- [Traps](#traps)
- [Docs rules](#docs-rules)
- [Repository mechanics](#repository-mechanics)
  - [CI tiers and scheduled workflows](#ci-tiers-and-scheduled-workflows)
  - [Publishing and images](#publishing-and-images)
  - [Digest pins](#digest-pins)
  - [cargo-chef](#cargo-chef)
  - [Compose file names](#compose-file-names)
  - [Pinned tools](#pinned-tools)
  - [Generated artifacts on main](#generated-artifacts-on-main)
- [Machine-specific notes](#machine-specific-notes)

## Verification loop

```bash
scripts/check.sh            # everything CI's blocking jobs run, plus MSRV and the coverage floor
scripts/check.sh --fast     # seconds, nothing compiles: for a docs- or workflow-only change
```

Run it before declaring anything done; `scripts/check.sh --install` fetches
the pinned tools first. What each group holds is
[`docs/CONTRIBUTING.md`](docs/CONTRIBUTING.md#the-verification-loop); the
agent-relevant half: every `ci.yml` check step calls this same script, so
add or change a command there, never inline in a workflow. **A missing tool
is reported as `skipped`, not failed**, locally only (under Actions it
fails), so read the summary's `skipped:` line before calling a run green. A
plain `cargo test --workspace` misses a tier-2 compile break, `fmt` alone has
failed more than one otherwise-green PR, and neither `cargo test` nor the
script builds `target/debug/sharerr`, so a CLI smoke test without an
explicit `cargo build` first runs a stale binary and produces a confidently
wrong conclusion.

### Testing tiers

Tier 1 is hermetic `cargo test`; tier 2 is `./scripts/run_docker_tests.sh`
(and `run_docker_tests_two_instance.sh`) behind the `e2e` feature and
`#[ignore]`, compiled by `ci.yml` but never run there; tier 3 is
`./scripts/run_docker_tests_mesh.sh`, the gossip/lighthouse mesh test bed,
same feature and gate. `integration.yml` runs tiers 2 and 3 weekly,
advisory. Never add `--include-ignored` to `ci.yml`. The tiers, stacks and
fixtures are in [`docs/TESTING.md`](docs/TESTING.md). What is not derivable
from there:

**`gossip.exchange_secs`, `lighthouse.interval_secs`, and
`lighthouse.quiet_secs` are config keys, not constants, specifically so tier
3 does not have to wait out the production defaults (900/900/3600 seconds)
to see a mesh converge.** They deliberately have no settings-page field and
no `config_paths` entry, unlike every other interval in `Config` — a web
field with no floor (there is one on purpose; see `GossipConfig`'s own doc
in `crates/sharerr-core/src/config.rs`) would be a standing invitation to
set it to a few seconds in production and hammer every friend's instance.
`sharerr.toml` or a `SHARERR_GOSSIP__*` / `SHARERR_LIGHTHOUSE__*` override
still reaches them, same as any other config key.

Fixtures worth knowing by name: `state::fixtures::{unconfigured, unloadable}`
(a fresh container, with or without a loadable `sharerr.toml`; keep the
returned `TempDir` alive), `web::web_state(serve)` for a `WebState` around
one of those, and `sharerr_testkit::mock::{base_url, mount_json, mount_ok,
mount_text, multipart_field, QBIT_API_KEY, ARR_API_KEY}` for wiremock-backed
service clients.

**No tier-1 fixture opens a real vault.** `fixtures::unconfigured()` has no
master key, so `ServeState::open_vault`/`tracker_token`/`gossip_identity`
resolve to "unavailable" there. Prefer testing store-backed logic directly
(pass `Store` and the resolved secret as plain parameters, the way
`tracker::authenticate_token` does). When a test genuinely needs the vault
open, the one sanctioned way is `figment::Jail::expect_with` with
`jail.set_env("SHARERR_MASTER_KEY", ..)`; see `state.rs`'s
`vault_backed_accessors_succeed_and_cache_once_the_vault_opens`. `Jail`
scopes the variable and serialises against every other Jail test in the
binary; a bare `std::env::set_var` does neither and races the parallel
runner.

**A test that needs the master key _absent_ is just as exposed.**
`master_key_from_env` reads the real process environment, and this binary's
Jail tests mutate it. `Jail` only serialises against other Jail closures, so
a bare `#[tokio::test]` asserting a rejection that depends on the vault
failing to open can flip to success when a Jail test is live on another
thread. It passes indefinitely until enough Jail tests exist to widen the
window, then reproduces deterministically, so a green suite is not evidence
the race is absent. Wrap this class in `Jail` too, with `jail.clear_env()`
and nothing else; see `secrets.rs`'s
`opening_a_vault_without_a_master_key_fails_with_no_side_effects` or
`web/settings.rs`'s
`save_arr_rejects_when_the_vault_will_not_open_rather_than_write_a_partial_config`.

### CodeQL

`./scripts/run_codeql.sh` runs the same analysis `codeql.yml` runs in CI,
entirely locally (one-time CLI download; the script's header says where
from). Not part of `scripts/check.sh`, since it is slow; run it before
pushing anything touching crypto, secret handling, or a workflow file.

**A local run needs the toolchain's `rust-src` component, and the script now
refuses without it.** The extractor expands macros through an embedded
rust-analyzer that reads `core`'s sources out of `rust-src`; nothing installs
it by default. Without it every `format!`, `assert!`, `println!` and `panic!`
fails to expand and the report says "clean" whether the tree is or not - one
run here failed 4274 expansions across 91 of 106 files and found two things.
That is why the preflight exists; `SKIP_RUST_SRC_CHECK=1` bypasses it, and a
clean result then means nothing. Note also that even a healthy run resolves
very few of the query's logging sinks under `build-mode: none` - `tracing::`
macros are not modelled as sinks at all (only `log::`, `println!` and
`panic!` are), so a local zero is not evidence the Security tab is empty.

**A PR's CodeQL check only shows alerts on lines the diff touched.** A full
local run scans the whole tree and surfaces findings no PR ever showed; a
first full run here found 52, chiefly in two auth-adjacent test suites. Most
were test literals or redaction-proving `Debug` prints that CodeQL's Rust
queries (new since 2.23.3) read as a secret reaching a sink. Before assuming
a `Cleartext logging` or `Hard-coded cryptographic value` finding is new
behaviour, check whether the flagged line is years-old code a diff simply
never included.

**There is no in-source suppression.** A `// codeql[...]` or `// lgtm[...]`
comment does nothing. A finding is either fixed so the flagged value no
longer exists (`sharerr_testkit::mock::rpc_credentials` replaced hard-coded
Basic Auth literals across two test suites) or dismissed in the Security tab
with a reason recorded somewhere durable;
[`docs/CODEQL.md`](docs/CODEQL.md) is that record, and its
[`rust/path-injection` entry](docs/CODEQL.md#path-injection-on-the-config-path)
is the model. A dismissal is fingerprint-bound to the flagged line: moving a
literal to a new file resets it and the finding reappears as new.

## Hard constraints

**Never move, rename, or re-link media.** This is the project's central
constraint; `skip_checking` under [Traps](#traps) is where it bites.

**Clippy must stay at zero warnings.** `unwrap_used` and `expect_used` are
`warn` at the workspace level and CI promotes them. Test modules opt out with
an inner `#![allow(clippy::unwrap_used, clippy::expect_used)]`; never weaken
the workspace lint. See
[`docs/CONTRIBUTING.md`](docs/CONTRIBUTING.md#clippy-stays-at-zero-warnings).

**The MSRV must hold, and a local toolchain will not tell you.** Build the
image (`docker build -f docker/Dockerfile .`) before claiming it does; the
version, CI's `msrv` job, and the two breaches that shipped are in
[`docs/CONTRIBUTING.md`](docs/CONTRIBUTING.md#msrv).

**Secrets never go in `sharerr.toml`.** They live in the encrypted vault,
keyed by the constants in `sharerr_core::config::secret_keys`. Adding a
secret means adding it to `secret_keys::ALL` (or `commands/vault.rs` silently
rejects it) _and_ wiring the constant into the relevant handler in
`web/settings.rs`, which does not consult `ALL`: each save handler passes its
own secret key to `write_config_and_secret` (Transmission and rTorrent share
`save_rpc_client`). The one documented exception is the `[[peers]]`
bootstrap block, drained into the vault by `commands/serve::import_peers` on
the next start; see
[`docs/SETTINGS.md`](docs/SETTINGS.md#restoring-friends-after-a-full-data-directory-loss).
Not a precedent.

**Dependency versions live in `[workspace.dependencies]`** in the root
`Cargo.toml`, one pin for the whole tree. Crates write
`foo = { workspace = true }` and add only their own `features`. Do not pin a
version in a crate manifest.

**The version is the tag; never bump `[workspace.package].version`.** See
[Publishing and images](#publishing-and-images) for where the real one comes
from.

## Traps

**Torrent name vs release title are two different strings.** Conflating them
stalls seeding at 0%. The torrent's name must describe the file where it
already sits; the release title is what the feed advertises. See
`sharerr-torrent`.

**`skip_checking = true` seeds a wrong mapping instead of refusing it.** It
is the default, so qBittorrent seeds a newly added torrent immediately rather
than re-verifying it. Set it `false` while still confirming a path mapping:
with checking skipped, a wrong mapping seeds mismatched data instead of
qBittorrent refusing it.

**The config file is rewritten in place by the web UI**, comments and all,
via `toml_edit`. A settings path is a hand-typed string in more than one
place; check `web/settings.rs` and `web/templates/settings.html` agree.

**`config_io::env_overrides()` is memoised in a `OnceLock`.** A test that
sets a `SHARERR_*` variable (even inside a `Jail`) and then renders the
settings or wizard page sees whatever the _first_ caller in the binary saw.
Test lock detection through `collect_overrides(vars)` with an explicit
iterator, as `config_io`'s tests do.

**Every `web/settings.rs` form field must be `#[serde(default)]`.** An
`<input>` can render `disabled` (no master key yet, or pinned by a
`SHARERR_*` variable), and a disabled input submits nothing. A `Form` field
with no default then fails to deserialize _before_ the handler runs, so
`reject()`'s styled error page never renders and everything else typed on
that form is discarded. Apply it at the struct level (`#[serde(default)]`
plus `#[derive(Default)]`) so a field added later inherits it.

## Docs rules

Which doc a change updates is
[`docs/CONTRIBUTING.md`'s table](docs/CONTRIBUTING.md#which-doc-changes-with-what),
and the markdown shape and local checks are
[its docs section](docs/CONTRIBUTING.md#working-on-the-docs). Every markdown
fact has one owning doc, listed in `docs/README.md`; link to it rather than
restating it (this file included), and keep every heading that
`crates/sharerr/src/web/docs.rs` links to verbatim.

The roadmap is `README.md`'s own "Roadmap" section, and it holds candidates
that have been weighed but not committed to as well as firm intentions. An
idea belongs there or in `docs/COMPATIBILITY.md`'s "Not supported" section,
never in both. The design brief and the two premises the implementation
disproved are in `docs/DESIGN.md`.

## Repository mechanics

### CI tiers and scheduled workflows

Which checks block a merge and which only report is
[`docs/CONTRIBUTING.md`](docs/CONTRIBUTING.md#what-ci-runs). What matters when
editing the workflows:

**Standing findings notify by keeping one issue current** rather than by
turning a run red: `image-scan.yml`'s `pins` job, `tool-versions.yml`,
`advisories.yml`, `link-check.yml` and `integration.yml`. Each opens a
single labelled issue when its finding appears, rewrites the body on every
later run, and **closes it** on the run that finds things clean. Every one
of these is a standing state of the repo that nobody caused with a commit
(or, for `integration.yml`, a flake nobody should be blocked on), and a
scheduled run that goes red on a Monday is not a notification if nobody
opens it. If you add another, call `.github/actions/upsert-tracking-issue`
rather than hand-rolling `gh issue` calls (it owns the label, the footer,
the `--body-file` and the close comment), and give the workflow a
non-ref-scoped `concurrency:` group with `cancel-in-progress: false`, the
same as the others. Without one, two runs racing (a push and its cron, or two
quick merges) can both find no open issue and both create one.

**The post-merge workflows chain off a green run of `ci.yml` on `main`**,
via a `workflow_run` trigger, rather than firing on every push to `main`
regardless of outcome: `advisories.yml`, `image-scan.yml`,
`tool-versions.yml`, `link-check.yml`, `scorecard.yml` and `coverage.yml`'s
`main` leg (grep `workflows: \[CI\]` for the current list rather than
trusting a count). A tree `cargo test` just rejected isn't worth scanning,
coverage-measuring, or scoring. Each keeps its own weekly cron too, as a
backstop for drift no commit caused; the `workflow_run` leg is what makes a
merge that _does_ cause one (a `Cargo.lock` bump reintroducing an advisory, a
docs change linking somewhere dead) surface immediately, just gated on
green. `pages.yml` chains off `coverage.yml` in turn. **`codeql.yml` is not
one of them**: it fires on the push to `main` itself, because Scorecard's
SAST check counts the commits that got a CodeQL run, and a commit whose CI
went red (or was still running when the next merge landed) would otherwise
count as unanalysed. `docker.yml` is push-triggered too: it publishes the
`sha-<7-char-sha>` image, not a scan, and that should ship whether or not an
advisory job would have been red. None of the chained workflows blocks a
merge; `ci.yml` is the gate. Every `workflow_run` trigger here carries an
inline `# zizmor: ignore[dangerous-triggers]`, with the shared justification
living in `pages.yml`'s header — the original of this pattern — rather than
repeated per file, and `lint_workflows.sh` (rule 4) fails a `workflow_run`
job that does not check the triggering run's conclusion.

**`ci.yml`'s `concurrency:` group only cancels a superseded run on
`pull_request`.** Cancelling a superseded push to `main` would silently
cancel the run every chained workflow is waiting on. Outside a PR that group
is also keyed per commit (`github.sha`), not per ref: a group holds one
running and one pending run, so a quick second push would otherwise replace
the first commit's queued run and leave that commit with no CI result for
`docker.yml`'s `gate` to find.

**A push to `main` reuses a green PR run on the same tree** (the
`reuse` / `mark` / `carry-forward` jobs in `ci.yml`; the mechanism is in
[`docs/CONTRIBUTING.md`](docs/CONTRIBUTING.md#what-ci-runs)). Two things to
know when editing it. Every job added to `ci.yml` needs
`needs: [..., reuse]` and `needs.reuse.outputs.run == ''` (with
`!cancelled()`, since `reuse` skips on a PR) in its `if:`, and a new blocking
job belongs in `mark`'s `needs:` too, or a PR run that skipped or failed it
can still vouch for the push. And the cost: `test` and `msrv` save their
rust-cache only from `main`, so a reused push saves no rust-cache on `main`;
after a run of reused merges the cache is whatever the last full push (or a
manual dispatch on `main`, which never reuses) wrote.

**Every job starts with `step-security/harden-runner`**, with the
`egress-policy` spelled out: `audit` everywhere except `docker.yml`'s
`publish` and `release`, which hold write tokens and `block` to an allowlist.
`lint_workflows.sh` rule 6 fails a job without it (the action's own default
is `block`, which with no allowlist would cut a job off).

**`image-scan.yml` runs two scans that answer different questions.** `trivy`
scans both images the repo _publishes_ ("has what we shipped gone stale");
`pins` scans the images it _builds on_ ("is there a better base"). The
second only speaks when a repin is verified to reduce the fixable CVE count,
and `--ignore-unfixed` is load-bearing there: without it the permanent
won't-fix pile makes every comparison noise.

### Publishing and images

The tag scheme, the two images, and the release gates are in
[`docs/RELEASING.md`](docs/RELEASING.md). What matters when editing the
workflows: `docker.yml` builds both images — its `docker` and `lighthouse`
jobs are thin callers of `docker-image.yml`, passing
`target: runtime-sharerr` / `runtime-lighthouse` into the one
`docker/Dockerfile`, so there is one MSRV pin. That used to be two pins that
had to move together; merging the file is what removed the trap, not a
comment. `docker-image.yml` only builds, attests (provenance plus an SPDX
SBOM per architecture) and smoke-tests (`docker/smoke_test.sh`: `--version`
and the image's own `HEALTHCHECK`); `docker.yml`'s own `publish` job is the
one `environment: release` job that promotes both images together, by
digest. That environment has no required reviewer, on purpose: releases run
unattended, and what stands in for a reviewer is `docker.yml`'s `gate` job
(tag shape, a tag signed by a key in `main`'s `.github/allowed_signers`, the
commit on `main`, a green `ci.yml` push run) and its `scan` job (trivy,
fixable HIGH/CRITICAL, `docker/.trivyignore` the only escape hatch). See
`docs/RELEASING.md` for the chain and for why that used to be two workflow
files.

**A job added between the builds and `publish` must be in `publish`'s
`needs:`**, or a failure there no longer holds the release back.
`smoke_test.sh` runs the image with no config mounted, so a change that makes
either binary refuse to start (or answer its health route) without one fails
every build, PRs included.

**The version is the tag.** `[workspace.package].version` is a fixed
`0.0.0-dev` placeholder. Three places carry the real one:
`crates/sharerr/build.rs` and its twin in `crates/sharerr-lighthouse`
(read `SHARERR_VERSION`, fall back to the placeholder), `docker/Dockerfile`'s
`ARG SHARERR_VERSION` in each builder stage (declared _after_ the cook step,
so the chef layer never depends on it), and `docker-image.yml`'s `version`
step, which strips the `v` from the tag or stamps `0.0.0-dev+g<sha7>` on a
dev build. That step refuses a malformed tag before anything is pushed, and
`gate` repeats the same regex so its verdict stands on its own; change both
together. `docs/openapi.json` is generated with the
placeholder and needs no regeneration at release time. One trap: cargo
exports a build script's `rustc-env` variables into `cargo test` and
`cargo run` too, so `SHARERR_VERSION` sits in every test's environment inside
the config loader's `SHARERR_` prefix; `settings::NON_CONFIG_ENV` lists it,
and any future build-time variable with that prefix needs the same entry.

### Digest pins

**Every container image is pinned by digest, not just by tag**, in
`docker/Dockerfile` and in every compose file under `docker/`
(`name:tag@sha256:...`). The deliberate exception is sharerr's own images:
the deploy files that reference `sharerr-rs:latest` or
`sharerr-lighthouse:latest` leave that one reference unpinned, and
`compose.mesh.yml` builds from source rather than pulling. Adding an image
means pinning it the same way:
`docker buildx imagetools inspect <ref> --format '{{.Manifest.Digest}}'`.
Two things read those pins back. `.github/scripts/scan_pinned_images.sh`
extracts them by regex from the files its `git ls-files` globs match (a file
those globs miss is never read), and an image referenced by tag alone has no
digest to compare, so it is listed in its own report section rather than
scanned. Dependabot's `docker` / `docker-compose` entries rewrite tag and
digest together.

### cargo-chef

**cargo-chef caches the dependency compile as a real image layer, not a
`--mount=type=cache`.** `docker/Dockerfile` cooks the ~400 third-party crates
once per package (`cargo chef cook --package <sharerr | sharerr-lighthouse>`)
before `COPY . .` runs, so CI gets a warm dependency layer. The one way to
break this silently: give `cook` and the `cargo build` after it different
flags, or put a cache mount back on either step. Either produces a correct
image and zero speedup with the cook step still reporting `CACHED`. Read the
Dockerfile's comments at the two builder stages before touching either.

### Compose file names

**A compose file's name decides whether dependabot manages it.** The
`docker-compose` ecosystem matches
`(docker-)?compose(-\w+)?(\.[\w-]+)?\.ya?ml`. The gluetun fragment is named
`compose.gluetun.reference.yaml` precisely so it matches. A new file holding
an image pin must satisfy that regex and sit in a directory `dependabot.yml`
lists; `/docker/deploy/*` matches the directories _under_ `deploy`, not
`deploy` itself, which is why `/docker/deploy` is listed separately. Every
ecosystem feeds one multi-ecosystem group, and `target-branch: dev` sits on
that group, since an entry inside a group may not set its own.

### Pinned tools

**Pinned tool versions that dependabot cannot see** are zizmor, actionlint,
cargo-llvm-cov, lychee, typos, shellcheck, hadolint, trivy, cargo-deny,
gitleaks, terraform and yq, each installed from a cached, pinned,
sha256-verified release asset by `.github/actions/setup-tool`.
`.github/actions/setup-tool/tools.txt` is the one roster (version, URL,
archive kind, verify flag, sha256, per platform where the asset differs),
read by that action, by `check_tool_versions.sh`, and by
`scripts/check.sh --install` (into `target/ci-tools/`); bumping one is a hand
edit to that file and nothing else. Its `kind` column is `raw`, `tar.gz`,
`tar.xz`, `zip` (terraform, which HashiCorp ships zipped from
releases.hashicorp.com rather than as a GitHub asset), or a source build;
the file's header documents each. `.github/actions/setup-tools` (plural) is
the batched sibling for a job that needs more than one row at once —
`ci.yml`'s `workflow-lint` is the only current caller — resolving every pin
and paying one shared `actions/cache` round trip instead of N; a job needing
exactly one tool still goes through `setup-tool` (singular). A new tool a
check uses also needs adding to `scripts/check.sh`'s `pinned` list, or
`--install` never fetches it.

**The Markdown tools are npm packages, not `tools.txt` rows.**
markdownlint-cli2 and prettier are pinned in `.github/package.json` and
locked, integrity hashes and transitive dependencies included, in
`.github/package-lock.json`; install them with `npm ci --prefix .github`
(`scripts/check.sh --install` does) and run them from
`.github/node_modules/.bin/`: a bare `npx` from the repo root does not see
`.github/node_modules` and fetches whatever version is current. Dependabot's npm entry moves them.

**A few pins live inline in other files**, and
`.github/scripts/check_tool_versions.local.sh` rosters them for the same
drift check: cargo-chef (`docker/Dockerfile`'s `ARG CARGO_CHEF_VERSION`),
chrome-devtools-mcp (the `npx` pin in `.mcp.json`) and the just-the-docs
theme (`_config.yml`'s `remote_theme:`). The same file holds the MSRV
consistency check, which fails when `Cargo.toml`'s `rust-version` and the
Dockerfile's `FROM rust:<x>` disagree; `ci.yml`'s `msrv` job reads its
toolchain from `Cargo.toml`, so it has no copy to drift.

**Local actions are `uses: ./.github/...`, never zizmor's `$/...`
self-repository syntax.** The tree was rewritten to `$/` once and reverted:
OpenSSF Scorecard's Pinned-Dependencies check only treats a `./` prefix as a
local action, so every `$/` line became a "third-party GitHubAction not
pinned by hash" code-scanning alert (37 of them) and capped that check at
6/10. `.github/zizmor.yml` disables the `self-repository` audit for this
reason; don't re-apply `zizmor --fix` for it. `actionlint` still points at
`kjanat/actionlint`, the maintained fork adopted for `$/` support; the
`tools.txt` comment on that row has the trade and when to go back upstream.

### Generated artifacts on main

**`main` carries a ruleset (PR required, protected ref, verified signatures)
that no Actions `git push` can satisfy.** A workflow that generates something
meant to be published (the coverage badge) cannot commit it to `main`. The
pattern is `coverage.yml` uploading the figure as a build artifact, and
`pages.yml`, on a `workflow_run` trigger, looking up the newest successful
run via `gh api .../actions/workflows/coverage.yml/runs`, downloading that
artifact (the newest green run that still holds it, via
`.github/actions/fetch-latest-artifact`), and writing it into `_site/` after
Jekyll has built, so it ships
with the same Pages deploy. Copy that shape rather than reaching for a bot
commit; `pages.yml`'s own header comment says why the `workflow_run` trigger
is safe here (no attacker-controlled ref is ever checked out or read) — the
same comment every other `workflow_run`-triggered workflow in the repo
points back to rather than repeating.

## Machine-specific notes

Machine-specific notes (absolute paths, a particular host's runner or local
tool setup) live in an untracked `CLAUDE.local.md` beside this file;
`.gitignore` keeps it out of the repository. Nothing here depends on one.
