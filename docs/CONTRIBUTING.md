# Contributing

How to build, test, and submit a change. [The README](../README.md) covers
_using_ sharerr; [`CLAUDE.md`](../CLAUDE.md) is the same ground written for AI
agents working in this tree, with more conventions and traps than a human needs
spelled out — this page links to it rather than duplicating it, and stays the
one written for a person.

## Table of contents

- [Before you start](#before-you-start)
- [Getting set up](#getting-set-up)
- [The verification loop](#the-verification-loop)
- [Clippy stays at zero warnings](#clippy-stays-at-zero-warnings)
- [Testing tiers](#testing-tiers)
- [MSRV](#msrv)
- [What CI runs](#what-ci-runs)
- [Working on the docs](#working-on-the-docs)
- [Commits and pull requests](#commits-and-pull-requests)
- [Licence](#licence)

## Before you start

sharerr is **experimental**, pre-1.0, and maintained by one person in their
spare time — see [the README](../README.md#experimental-until-v100-stable-releases).
For anything feature-sized, open an issue first and say what you have in mind;
it saves both of us the cost of a PR built on a misread of the project's
direction. A small, obviously-correct fix (a typo, a stale doc link, an
off-by-one) doesn't need that step.

Found a security issue? Do not open a public issue — use
[the private advisory route](SECURITY.md#reporting-a-vulnerability) instead.

## Getting set up

Rust **1.98** or newer (`rust-version` in the root `Cargo.toml`; see
[MSRV](#msrv) below for what "newer" actually means here):

```bash
cargo build
```

`SHARERR_MASTER_KEY` — the vault's encryption key — is not needed to build or to
run the default test suite; nothing in tier 1 opens a real vault.

## The verification loop

```bash
cargo test --workspace \
  && cargo clippy --workspace --all-targets --all-features -- -D warnings \
  && cargo build \
  && cargo fmt --all --check
```

Run it before calling anything done. `cargo fmt --all --check` is what CI's
`rustfmt` job actually runs — a change that compiles and passes clippy but was
never run through `cargo fmt` still fails CI on that step alone. If it fails, run
plain `cargo fmt --all` (no `--check`) and re-run the loop.

Note that `cargo test` builds the test harness, not `target/debug/sharerr` — a
CLI smoke test needs an explicit `cargo build` first.

## Clippy stays at zero warnings

The workspace sets `unwrap_used` and `expect_used` to `warn` because the vault
and the service clients handle secrets, and CI promotes every warning to an
error with `-D warnings`. When a test needs to panic on failure, opt out with an
inner attribute at the top of the module:

```rust
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
```

Never weaken the workspace lint to make a test compile. A non-test `expect()`
that genuinely cannot fail takes a targeted `#[allow]` plus a comment saying
why.

## Testing tiers

**Tier 1** is the default `cargo test` and is hermetic: no network, no
containers, no database. Service clients run against wiremock on loopback,
sqlx against `sqlite::memory:`, migrations are embedded. This is what CI runs on
every push, and it stays fast because nothing in it reaches outside the process.

**Tier 2** is `./scripts/run_docker_tests.sh`, opt-in and local only, behind the
`e2e` feature and `#[ignore]`. It drives a real Sonarr + Radarr + qBittorrent
stack and proves the one thing mocks cannot: after a real sync through a real
qBittorrent, every media file has the same inode, mtime, and length it started
with. CI compiles it with `--all-features` so it cannot silently rot, but never
runs it.

Two related scripts worth knowing about: `scripts/run_docker_tests_two_instance.sh`
brings up two independent friend-to-friend sharerr stacks and drives a real
request/grab between them; `scripts/screenshot_pages.sh` screenshots every page
`sharerr preview` serves, for refreshing the README's screenshots after a layout
change.

All fixtures are synthetic — invented titles, seeded pseudo-random bytes. No
real content is involved anywhere.

See [`docs/TESTING.md`](TESTING.md) for the fuller reference: every
`run_docker_tests.sh` flag, the compose stacks each one drives, and the
coverage caveat.

## MSRV

CI's `msrv (1.98)` job runs `cargo check --workspace --all-targets
--all-features --locked` on a pinned 1.98 toolchain. Locally, `docker build -f
docker/Dockerfile .` is the equivalent check — a local toolchain is invariably
newer and will not catch a breach on its own. Both `docker/Dockerfile` and
`docker/Dockerfile.lighthouse` pin the same toolchain as `rust-version`, and the
two pins have to move together if either changes.

## What CI runs

| Job | Blocks a merge? |
| --- | --- |
| `rustfmt` | Yes |
| `clippy + tests` | Yes |
| `msrv (1.98)` | Yes |
| `cargo-deny` | Yes |
| `shell + compose` | Yes |
| `zizmor` | Yes |
| `actionlint` | Yes |
| `hadolint (advisory)` | No — reports only |
| `markdownlint (advisory)` | No — reports only |

`./scripts/run_codeql.sh` runs the same analysis CI's CodeQL workflow runs,
entirely locally — worth doing before pushing anything that touches
crypto/secret handling or a workflow file, since it needs a one-time local
install and is slower than the loop above.

## Working on the docs

Every markdown file in this repo follows the same shape: one `#` title, then a
`## Table of contents` as the first `##` heading, ATX headings in sentence case
down to `###`, `_underscore_` for italics and `**asterisks**` for bold (pinned by
`.markdownlint.yaml`'s `MD049`). Each doc's opening paragraph says what it covers
versus [the README](../README.md), which is the project's anti-duplication
convention — keep it when adding to one.

Two advisory linters run in CI and can be run the same way locally:

```bash
markdownlint-cli2 '**/*.md'
lychee '**/*.md' crates/sharerr/src/web/docs.rs
```

`crates/sharerr/src/web/docs.rs` hard-codes every documentation link the web UI
shows, including deep anchors into these files, and has a test that resolves
each one against a real heading in the working tree. Renaming a heading that
file links to fails `cargo test`, not just the advisory lint jobs — that test is
what actually catches it.

## Commits and pull requests

Branch from `dev`, where active development happens; `main` carries a ruleset
requiring a pull request, a protected ref, and verified commit signatures, so a
signed commit is required to land there.

## Licence

MIT. By contributing, you agree your contribution is licensed under the same
terms — see [`LICENSE.md`](../LICENSE.md).
