# Contributing

How to build, test, and submit a change. [The README](../README.md) covers
_using_ sharerr; this page is the contract for changing it, and the one place
the verification loop, the lint policy, and the MSRV rule are written down.
[`CLAUDE.md`](https://github.com/ivylikethevine/sharerr-rs/blob/main/CLAUDE.md)
is the agent-facing companion: it links here for all of that and adds only
the traps and repository mechanics a person would not need spelled out.

## Table of contents

- [Before you start](#before-you-start)
- [Getting set up](#getting-set-up)
- [The verification loop](#the-verification-loop)
- [Clippy stays at zero warnings](#clippy-stays-at-zero-warnings)
- [Test policy](#test-policy)
- [Testing tiers](#testing-tiers)
- [MSRV](#msrv)
- [What CI runs](#what-ci-runs)
- [Working on the docs](#working-on-the-docs)
- [Commits and pull requests](#commits-and-pull-requests)
- [Licence](#licence)

## Before you start

sharerr is **experimental**, pre-1.0, and maintained by one person in their
spare time (see [the README](../README.md)). For anything feature-sized, open
an issue first and say what you have in mind; it saves both of us the cost of
a PR built on a misread of the project's direction. A small, obviously
correct fix (a typo, a stale link, an off-by-one) does not need that step.

Found a security issue? Do not open a public issue; use
[the private advisory route](SECURITY.md#reporting-a-vulnerability).
Participating in any project space means abiding by the
[code of conduct](CODE_OF_CONDUCT.md).

## Getting set up

Rust **1.98** or newer (`rust-version` in the root `Cargo.toml`; see
[MSRV](#msrv) for what "newer" means here), then `cargo build`.
`SHARERR_MASTER_KEY` is not needed to build or to run the default test suite;
nothing in tier 1 opens a real vault.

## The verification loop

```bash
cargo test --workspace --all-features --locked \
  && cargo clippy --workspace --all-targets --all-features --locked -- -D warnings \
  && cargo build \
  && cargo fmt --all --check
```

Run it before calling anything done. Three things about it are easy to get
wrong:

- `--all-features` is not optional. It is what CI runs, and it compiles the
  tier-2 suite behind the `e2e` feature so that suite cannot silently rot. A
  plain `cargo test --workspace` passes locally and fails CI on a tier-2
  compile break.
- `cargo fmt --all --check` is what CI's `rustfmt` job runs. A change that
  compiles and passes clippy but was never formatted fails CI on that step
  alone. If it fails, run `cargo fmt --all` and re-run the loop.
- `cargo test` builds the test harness, not `target/debug/sharerr`. A CLI
  smoke test needs an explicit `cargo build` first.

## Clippy stays at zero warnings

The workspace sets `unwrap_used` and `expect_used` to `warn` because the vault
and the service clients handle secrets, and CI promotes every warning to an
error with `-D warnings`. When a test needs to panic on failure, opt out with
an inner attribute at the top of the module:

```rust
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
```

Never weaken the workspace lint to make a test compile. A non-test `expect()`
that genuinely cannot fail takes a targeted `#[allow]` plus a comment saying
why; `QbitConfig::default`'s `Url::parse` on a literal (and its Transmission
and rTorrent twins) are the only examples in the tree.

## Test policy

**A PR adding a feature or fixing a bug is expected to add or extend a tier-1
test that would have failed without the change.** A reviewer can and should
ask for one before merging. A change with no behaviour (a typo, a doc link, a
comment) does not need one. The PR template's checklist asks for this; if it
genuinely does not apply, say why rather than leaving the box unchecked.

## Testing tiers

**Tier 1** is the default `cargo test` and is hermetic: no network, no
containers, no database. **Tier 2** is `./scripts/run_docker_tests.sh`,
opt-in and local only, behind the `e2e` feature and `#[ignore]`; it drives a
real *arr + torrent-client stack and proves the one thing mocks cannot, that
a real sync leaves every media file's inode, mtime, and length untouched. CI
compiles tier 2 but never runs it. Never add `--include-ignored` to a CI
command. [`docs/TESTING.md`](TESTING.md) has every flag, stack, fixture, and
the coverage caveat.

## MSRV

`rust-version` is 1.98. CI's `msrv` job runs `cargo check --workspace
--all-targets --all-features --locked` on that pinned toolchain. Locally,
`docker build -f docker/Dockerfile .` is the equivalent check, because the
Dockerfile pins the same toolchain and a local toolchain is invariably newer
and will not catch a breach on its own. Two breaches have shipped unnoticed
this way (`str::split_at_checked` in a const context, then let-chains), so
build the image before claiming the MSRV holds.

## What CI runs

Every job in `ci.yml` waits on `prepare`, which decides whether anything
besides workflow YAML changed; a PR touching only `.github/workflows/**`
shows almost no checks, by design. Everything else runs on every push and PR.

The check names below are what `main`'s ruleset requires, verbatim, so a
job rename is also a ruleset edit; that is why `msrv` carries no version in
its name and the two image builds carry the image's name rather than sharing
one.

| Check | Workflow | Blocks a merge? |
| --- | --- | --- |
| `rustfmt` | `ci.yml` | Yes |
| `clippy + tests` | `ci.yml` | Yes |
| `msrv` | `ci.yml` | Yes |
| `cargo-deny` | `ci.yml` | Yes |
| `shell + compose` | `ci.yml` | Yes |
| `workflow lint (zizmor + actionlint)` | `ci.yml` | Yes |
| `advisory (hadolint + markdownlint + typos)` | `ci.yml` | No, reports only; each gets its own step summary |
| CodeQL (`rust`, `actions`) | `codeql.yml` | Yes, as code scanning; alerts are diff-scoped |
| `docker (sharerr) / build`, `docker (lighthouse) / build` (amd64 only) | `docker.yml`, `docker-lighthouse.yml` | Yes; also the de-facto MSRV check |

Four more workflows (`advisories.yml`, `image-scan.yml`, `tool-versions.yml`,
`link-check.yml`) run weekly and on every push to `main`, and report by
keeping an issue current rather than by failing; none blocks a merge.

`./scripts/run_codeql.sh` runs CodeQL's analysis entirely locally, worth
doing before pushing anything that touches crypto, secret handling, or a
workflow file. A full-tree run shows findings a PR's diff-scoped check never
will; read a "cleartext logging" or "hard-coded cryptographic value" finding
with that in mind, since most are test literals or redaction-proving `Debug`
prints. There is no in-source suppression: a finding is either fixed or
dismissed in the Security tab with a recorded reason (see
[`docs/SECURITY.md`](SECURITY.md#what-is-out-of-scope) for the model).

## Working on the docs

Every markdown file follows the same shape: one `#` title, `## Table of
contents` as the first `##` heading, sentence-case ATX headings down to
`###`, `_underscore_` for italics and `**asterisks**` for bold (pinned by
`.markdownlint.yaml`'s `MD049`).

**One fact, one home.** Each fact lives in the doc that owns its topic
([`docs/README.md`](README.md) says which); everywhere else links to it
rather than restating it. A doc's opening paragraph says what it covers
versus the README. When adding something, find the owner first.

Two advisory linters run in CI and locally:

```bash
markdownlint-cli2 '**/*.md'
lychee '**/*.md' crates/sharerr/src/web/docs.rs
```

lychee does not check `#anchors`. `crates/sharerr/src/web/docs.rs` hard-codes
the documentation links the web UI shows, with deep anchors into
`SETTINGS.md`, `SUPPORT.md`, `SECURITY.md`, `API.md`, `LIGHTHOUSE.md` and the
README, and has a test that resolves each against a real heading. Renaming
one of those headings fails `cargo test`, which is the only anchor check in
the repo. Files under `docker/`, `crates/`, and `CLAUDE.md` are excluded from
the published docs site (`_config.yml`), so a link to them from `README.md`
or `docs/*.md` must be an absolute GitHub URL.

## Commits and pull requests

Branch from `dev`, where active development happens. `main` carries a
ruleset requiring a pull request, a protected ref, and verified commit
signatures. The PR title becomes the release-notes line, so write it for a
user.

## Licence

MIT. By contributing, you agree your contribution is licensed under the same
terms; see [`LICENSE.md`](../LICENSE.md).
