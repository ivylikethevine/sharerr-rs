# Working on sharerr

Conventions that are not derivable from the code, and traps that have each cost
real debugging time. The human-facing contract (the verification loop, the lint
policy, the test policy, MSRV, what CI runs) is `docs/CONTRIBUTING.md`; this
file links to it and adds only what an agent needs beyond it.

## Table of contents

- [The verification loop](#the-verification-loop)
- [MSRV](#msrv)
- [CodeQL](#codeql)
- [Dependencies](#dependencies)
- [Testing tiers](#testing-tiers)
- [Traps](#traps)
- [Repository](#repository)
  - [Publishing and images](#publishing-and-images)
  - [Digest pins](#digest-pins)
  - [cargo-chef](#cargo-chef)
  - [Compose file names](#compose-file-names)
  - [Scheduled workflows](#scheduled-workflows)
  - [Pinned tools](#pinned-tools)
  - [Generated artifacts on main](#generated-artifacts-on-main)
  - [Where the roadmap lives](#where-the-roadmap-lives)

## The verification loop

```bash
cargo test --workspace --all-features --locked && cargo clippy --workspace --all-targets --all-features --locked -- -D warnings && cargo build && cargo fmt --all --check
```

Run it before declaring anything done. `--all-features` is what CI runs and
what compiles the tier-2 suite; a plain `cargo test --workspace` misses a
tier-2 compile break. `cargo fmt --all --check` alone has failed more than one
otherwise-green PR. `cargo test` does not build `target/debug/sharerr`; a CLI
smoke test needs an explicit `cargo build` first, or a stale binary produces a
confidently wrong conclusion.

**Clippy must stay at zero warnings.** `unwrap_used` and `expect_used` are
`warn` at the workspace level and CI promotes them. Test modules opt out with
an inner `#![allow(clippy::unwrap_used, clippy::expect_used)]`; never weaken
the workspace lint. See
[`docs/CONTRIBUTING.md`](docs/CONTRIBUTING.md#clippy-stays-at-zero-warnings).

## MSRV

`rust-version` is 1.98 and the Dockerfile pins the same toolchain, so
`docker build -f docker/Dockerfile .` is the **de-facto MSRV check**. A local
toolchain is invariably newer and will not catch a breach; two have shipped
this way. Build the image before claiming the MSRV holds. See
[`docs/CONTRIBUTING.md`](docs/CONTRIBUTING.md#msrv).

## CodeQL

`./scripts/run_codeql.sh` runs the same analysis `codeql.yml` runs in CI,
entirely locally (one-time CLI download; the script's header says where
from). Not part of the always-run loop, since it is slow; run it before
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
[`docs/SECURITY.md`](docs/SECURITY.md#what-is-out-of-scope)'s
`rust/path-injection` entry is the model. A dismissal is fingerprint-bound to
the flagged line: moving a literal to a new file resets it and the finding
reappears as new.

## Dependencies

Versions live in `[workspace.dependencies]` in the root `Cargo.toml`, one pin
for the whole tree. Crates write `foo = { workspace = true }` and add only
their own `features`. Do not pin a version in a crate manifest.

## Testing tiers

Tier 1 is hermetic `cargo test`; tier 2 is `./scripts/run_docker_tests.sh`
(and `run_docker_tests_two_instance.sh`) behind the `e2e` feature and
`#[ignore]`, compiled by CI but never run; tier 3 is
`./scripts/run_docker_tests_mesh.sh`, the gossip/lighthouse mesh test bed,
same feature and gate. Never add `--include-ignored`. The tiers, stacks and
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

## Traps

**Torrent name vs release title are two different strings.** Conflating them
stalls seeding at 0%. The torrent's name must describe the file where it
already sits; the release title is what the feed advertises. See
`sharerr-torrent`.

**Never move, rename, or re-link media.** This is the project's central
constraint. `skip_checking = true` is the default, so qBittorrent seeds a
newly added torrent immediately rather than re-verifying it. Set it `false`
while still confirming a path mapping: with checking on, a wrong mapping
seeds mismatched data instead of qBittorrent refusing it.

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

## Repository

### Publishing and images

The tag scheme, the two images, and the approval gate are in
[`docs/RELEASING.md`](docs/RELEASING.md). What matters when editing the
workflows: `docker.yml` builds both images — its `docker` and `lighthouse`
jobs are thin callers of `docker-image.yml`, passing
`target: runtime-sharerr` / `runtime-lighthouse` into the one
`docker/Dockerfile`, so there is one MSRV pin. That used to be two pins that
had to move together; merging the file is what removed the trap, not a
comment. `docker-image.yml` only builds and attests; `docker.yml`'s own
`publish` job is the one `environment: release` gate that promotes both
images together, so cutting a release is one approval, not two — see
`docs/RELEASING.md` for why that used to be two workflow files and two
gates.

**The version is the tag; never bump `[workspace.package].version`.** It is
a fixed `0.0.0-dev` placeholder. Three places carry the real one:
`crates/sharerr/build.rs` and its twin in `crates/sharerr-lighthouse`
(read `SHARERR_VERSION`, fall back to the placeholder), `docker/Dockerfile`'s
`ARG SHARERR_VERSION` in each builder stage (declared _after_ the cook step,
so the chef layer never depends on it), and `docker-image.yml`'s `version` step, which strips the
`v` from the tag or stamps `0.0.0-dev+g<sha7>` on a dev build. That step is
also the only shape check a tag gets, and it runs before anything is pushed.
`docs/openapi.json` is generated with the placeholder and needs no
regeneration at release time. One trap: cargo exports a build script's
`rustc-env` variables into `cargo test` and `cargo run` too, so
`SHARERR_VERSION` sits in every test's environment inside the config
loader's `SHARERR_` prefix; `settings::NON_CONFIG_ENV` lists it, and any
future build-time variable with that prefix needs the same entry.

### Digest pins

**Every container image is pinned by digest, not just by tag**, in
`docker/Dockerfile` and in nine of the twelve compose files under `docker/`
(`name:tag@sha256:...`); the three deploy files that reference sharerr's own
`:latest` are the deliberate exception. Adding an image means pinning it the
same way: `docker buildx imagetools inspect <ref> --format '{{.Manifest.Digest}}'`.
Two things read those pins back and neither tolerates a bare tag:
`.github/scripts/scan_pinned_images.sh` extracts them by regex (an unpinned
image is silently unscanned, and the script has to be pointed at
`docker/Dockerfile`'s actual path, not a bare filename), and dependabot's
`docker` / `docker-compose` entries rewrite tag and digest together.

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
`deploy` itself, which is why `/docker/deploy` is listed separately.

### Scheduled workflows

**Three scheduled workflows notify by keeping one issue current** rather than
by turning a run red: `image-scan.yml`'s `pins` job, `tool-versions.yml`, and
`advisories.yml`. Each opens a single labelled issue when its finding
appears, rewrites the body on every later run, and **closes it** on the run
that finds things clean. Every one of these findings is a standing state of
the repo that nobody caused with a commit, and a scheduled run that goes red
on a Monday is not a notification if nobody opens it. If you add a fifth,
follow the same shape: `gh label create --force`, reuse the first open issue
with that label, `--body-file` (never `--body`, which arrives YAML-indented
into the code block), and give it a non-ref-scoped `concurrency:` group with
`cancel-in-progress: false`, the same as the three. Without one, two runs
racing (a push and its cron, or two quick merges) can both find no open issue
and both create one.

All four (the three above plus `link-check.yml`, which is advisory the same
way but has no issue to upsert) run on every push to `main` in addition to
their weekly cron. The cron catches drift no commit caused; the push trigger
means a merge that _does_ cause one (a `Cargo.lock` bump reintroducing an
advisory, a docs change linking somewhere dead) surfaces immediately. None
of the four blocks a merge; `ci.yml` is the gate.

**`image-scan.yml` runs two scans that answer different questions.** `trivy`
scans the image the repo _publishes_ ("has what we shipped gone stale");
`pins` scans the images it _builds on_ ("is there a better base"). The
second only speaks when a repin is verified to reduce the fixable CVE count,
and `--ignore-unfixed` is load-bearing there: without it the permanent
won't-fix pile makes every comparison noise.

### Pinned tools

**Pinned tool versions that dependabot cannot see** are zizmor, actionlint,
cargo-llvm-cov, lychee and typos, each installed from a cached, pinned,
sha256-verified GitHub release asset by `.github/actions/setup-tool`.
`.github/actions/setup-tool/tools.txt` is the one roster (version, URL,
archive shape, verify flag, sha256), read both by that action and by
`check_tool_versions.sh`; bumping one is a hand edit to that file and nothing
else. hadolint and trivy are deliberately unpinned, so neither has a row.

### Generated artifacts on main

**`main` carries a ruleset (PR required, protected ref, verified signatures)
that no Actions `git push` can satisfy.** A workflow that generates something
meant to be published (the coverage badge) cannot commit it to `main`. The
pattern is `coverage.yml` uploading the figure as a build artifact, and
`pages.yml`, on a `workflow_run` trigger, looking up the newest successful
run via `gh api .../actions/workflows/coverage.yml/runs`, downloading that
artifact, and writing it into `_site/` after Jekyll has built, so it ships
with the same Pages deploy. Copy that shape rather than reaching for a bot
commit; `.github/zizmor.yml`'s `dangerous-triggers` entry says why the
`workflow_run` trigger is safe here (no attacker-controlled ref is ever
checked out or read).

### Where the roadmap lives

The roadmap is `README.md`'s own "Roadmap" section, and it holds candidates
that have been weighed but not committed to as well as firm intentions. An
idea belongs there or in `docs/SUPPORT.md`'s "Not supported" section, never
in both. The design brief and the two premises the implementation disproved
are in `docs/DESIGN.md`. Every markdown fact has one owning doc, listed in
`docs/README.md`; link to it rather than restating it, and keep every
heading that `crates/sharerr/src/web/docs.rs` links to verbatim.
