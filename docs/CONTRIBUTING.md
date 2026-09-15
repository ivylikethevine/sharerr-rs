# Contributing

How to build, test, and submit a change. [The README](../README.md) covers
_using_ sharerr; this page is the contract for changing it, and the one place
the verification loop, the lint policy, and the MSRV rule are written down.
[`CLAUDE.md`](https://github.com/ivylikethevine/sharerr-rs/blob/main/CLAUDE.md)
is the agent-facing companion: it links here for all of that and adds only
the traps and repository mechanics a person would not need spelled out.

## Contents

- [Before you start](#before-you-start)
- [Getting set up](#getting-set-up)
- [The verification loop](#the-verification-loop)
- [Clippy stays at zero warnings](#clippy-stays-at-zero-warnings)
- [Test policy](#test-policy)
- [Testing tiers](#testing-tiers)
- [MSRV](#msrv)
- [What CI runs](#what-ci-runs)
- [Working on the docs](#working-on-the-docs)
- [Which doc changes with what](#which-doc-changes-with-what)
- [Commits and pull requests](#commits-and-pull-requests)
- [AI-assisted contributions](#ai-assisted-contributions)
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
scripts/check.sh --install   # once: the pinned tools, and the Markdown tools
scripts/check.sh --fast      # seconds; nothing compiles
scripts/check.sh             # everything below, before calling anything done
```

`scripts/check.sh` is the one local entry point, and every check step in
`ci.yml` (and `coverage.yml`'s floor) calls it too, so a command lives in one
place. `--fast` is formatting, shell, compose, workflow, link, spelling and
Markdown checks; `--lint` adds clippy, rustdoc, cargo-deny, Terraform and
gitleaks; the default (`--all`) adds the tests, the MSRV check and the
coverage floor. `scripts/check.sh <check>...` runs just those, and `--list`
names them all. `--install` fetches every tool the script uses from its
sha256-pinned row in `.github/actions/setup-tool/tools.txt` into
`target/ci-tools/` (no sudo), and runs `npm ci --prefix .github` for
markdownlint and prettier.

Four things about it are easy to get wrong:

- **A missing tool is a skip locally, not a pass.** The summary line lists
  what was skipped; under GitHub Actions the same skip is a failure. Run
  `--install` rather than reading a partial run as green.
- `--all-features` is not optional. The `test` check uses it, as CI does, and
  it compiles the tier-2 suite behind the `e2e` feature so that suite cannot
  silently rot. A plain `cargo test --workspace` passes locally and fails CI
  on a tier-2 compile break.
- The `fmt` check is what CI's `rustfmt` job runs. A change that compiles and
  passes clippy but was never formatted fails CI on that step alone. If it
  fails, run `cargo fmt --all` and re-run.
- `cargo test` builds the test harness, not `target/debug/sharerr`. A CLI
  smoke test needs an explicit `cargo build` first; the script does not run
  one.

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
behind the `e2e` feature and `#[ignore]`; it drives a real *arr +
torrent-client stack and proves the one thing mocks cannot, that a real sync
leaves every media file's inode, mtime, and length untouched. **Tier 3** is
the mesh stack, same feature and gate. `ci.yml` compiles tiers 2 and 3 on
every PR but never runs them; `integration.yml` runs them on a weekly
schedule, advisory only. Never add `--include-ignored` to a `ci.yml`
command. [`docs/TESTING.md`](TESTING.md) has every flag, stack, fixture, and
the coverage floor.

## MSRV

`rust-version` is 1.98. CI's `msrv` job runs `cargo check --workspace
--all-targets --all-features --locked` on that pinned toolchain. Locally,
`docker build -f docker/Dockerfile .` is the equivalent check, because the
Dockerfile pins the same toolchain and a local toolchain is invariably newer
and will not catch a breach on its own. Two breaches have shipped unnoticed
this way (`str::split_at_checked` in a const context, then let-chains), so
build the image before claiming the MSRV holds.

## What CI runs

The jobs in `ci.yml` that compile, test or lint code wait on `prepare`,
which decides whether anything besides workflow YAML and Markdown changed; a
PR touching only those shows few checks, by design. The checks that read
exactly the files that filter ignores (workflow lint, links, docs, secrets)
run regardless. Everything runs on every push to `main` and every PR, except
a draft PR: `ci.yml`, `codeql.yml`, `docker.yml`, `coverage.yml` and
`release-note.yml` skip every job until the PR is marked ready for review,
which starts a full run.

The check names below are matched verbatim by `main`'s ruleset, so a job
rename is also a ruleset edit; that is why `msrv` carries no version in its
name and the two image builds carry the image's name rather than sharing
one. `ci.yml`'s header comment keeps the same list.

| Check                                                                  | Workflow           | Blocks a merge?                                     |
| ---------------------------------------------------------------------- | ------------------ | --------------------------------------------------- |
| `rustfmt`                                                              | `ci.yml`           | Yes                                                 |
| `clippy + tests` (also rustdoc with `-D warnings`)                     | `ci.yml`           | Yes                                                 |
| `msrv`                                                                 | `ci.yml`           | Yes                                                 |
| `cargo-deny`                                                           | `ci.yml`           | Yes                                                 |
| `shell + compose`                                                      | `ci.yml`           | Yes                                                 |
| `workflow lint (zizmor + actionlint)` (also `lint_workflows.sh`)       | `ci.yml`           | Yes                                                 |
| `markdown links (lychee offline)`                                      | `ci.yml`           | Yes                                                 |
| `docs (markdownlint + prettier + typos)`                               | `ci.yml`           | Yes, once added to the ruleset                      |
| `secrets (gitleaks)`                                                   | `ci.yml`           | Yes, once added to the ruleset                      |
| `terraform (fmt + validate)`                                           | `ci.yml`           | Yes, once added to the ruleset                      |
| `dependency review` (pull requests only)                               | `ci.yml`           | Yes, once added to the ruleset                      |
| `advisory (hadolint)`                                                  | `ci.yml`           | No, reports only in its step summary                |
| `release note (pr body)`                                               | `release-note.yml` | No; warns on a missing note, never fails            |
| CodeQL (`rust`, `actions`)                                             | `codeql.yml`       | Yes, as code scanning; alerts are diff-scoped       |
| `docker (sharerr) / build`, `docker (lighthouse) / build` (amd64 only) | `docker.yml`       | Yes; also the de-facto MSRV check and a smoke test  |
| `coverage (pull request)`                                              | `coverage.yml`     | No; keeps one coverage comment on the PR up to date |

The rows marked "once added to the ruleset" already fail red; they block a
merge only once `main`'s ruleset lists them as required.

`gitleaks` scans the whole history, not the diff, with the known false
positives in `.gitleaks.toml`. `dependency review` fails a PR that adds a
dependency (a crate, an action, an npm package under `.github/`) with a known
high or critical advisory; `cargo-deny` is the whole-tree check. The
coverage comment compares the PR's line coverage with `main`'s and with the
floor in `scripts/check.sh` ([`TESTING.md`](TESTING.md#coverage)); same-repo
PRs only, since a fork's token cannot write the comment.

**A push to `main` whose tree a green PR run already tested is not tested
twice.** When a same-repo PR run passes every blocking job, `ci.yml`'s `mark`
job records the tree it tested. The push that lands that same tree on
`main` (merging an up-to-date PR) finds the record in its `reuse` job, skips
every other job, and `carry-forward` copies the PR run's passing checks onto
the pushed commit, linked back to that run. The push run still concludes
`success`. A tree the PR run never saw (`main` moved under the PR, or the PR
run skipped jobs) runs in full, as does a manual dispatch. `docker.yml` does
not dedupe: every push to `main` publishes its own image.

Six more workflows (`advisories.yml`, `image-scan.yml`, `tool-versions.yml`,
`link-check.yml`, `coverage.yml`'s `main` leg, `scorecard.yml`) run weekly
and after every green run of `ci.yml` on `main`, not on every push
regardless of outcome, so a tree CI just rejected isn't also scanned,
coverage-measured, or scored. `coverage.yml`'s `main` leg fails below the
coverage floor; the others never block anything. `advisories.yml`,
`image-scan.yml`'s `pins` job, `tool-versions.yml` and `link-check.yml`
report by keeping one tracking issue current rather than by failing.
`integration.yml` runs the docker-backed tiers weekly (or one tier on manual
dispatch), advisory, and keeps an `integration` tracking issue current the
same way. `codeql.yml` runs on every push to `main` and weekly as well as on
PRs.

The third-party tools these jobs run that dependabot cannot see (zizmor,
actionlint, cargo-llvm-cov, lychee, typos, shellcheck, hadolint, trivy,
cargo-deny, gitleaks, terraform and yq) each install from a sha256-pinned row
in `.github/actions/setup-tool/tools.txt`, and `tool-versions.yml` reports
when one falls behind. markdownlint and prettier come from
`.github/package-lock.json`, which dependabot's npm entry moves.

Every job in every workflow starts with `step-security/harden-runner`, in
`audit` mode except the two release jobs that hold write tokens (`publish`
and `release` in `docker.yml`), which `block` egress to an allowlist.
`.github/scripts/lint_workflows.sh` (rule 6) fails a job that does not start
with it.

`./scripts/run_codeql.sh` runs CodeQL's analysis entirely locally, worth
doing before pushing anything that touches crypto, secret handling, or a
workflow file. A full-tree run shows findings a PR's diff-scoped check never
will; read a "cleartext logging" or "hard-coded cryptographic value" finding
with that in mind, since most are test literals or redaction-proving `Debug`
prints. There is no in-source suppression: a finding is either fixed or
dismissed in the Security tab with a recorded reason (see
[`docs/CODEQL.md`](CODEQL.md) for the record).

## Working on the docs

Every markdown file follows the same shape: one `#` title, `## Contents` as
the first `##` heading, sentence-case ATX headings down to
`###`, `_underscore_` for italics and `**asterisks**` for bold (pinned by
`.markdownlint.yaml`'s `MD049`).

**One fact, one home.** Each fact lives in the doc that owns its topic
([`docs/README.md`](README.md) says which); everywhere else links to it
rather than restating it. A doc's opening paragraph says what it covers
versus the README. When adding something, find the owner first.

Formatting, style, spelling and link checks, the same ones CI runs:

```bash
scripts/check.sh markdownlint prettier typos links
.github/node_modules/.bin/prettier --write <file>...   # fix what prettier names
```

`.prettierrc.yaml` pins the formatting (hand-wrapped prose, aligned tables)
and `.markdownlint.yaml` the style; both tools come from
`.github/package.json` (`npm ci --prefix .github`, which
`scripts/check.sh --install` runs). markdownlint, prettier and typos make up
CI's blocking `docs (markdownlint + prettier + typos)` check; a false
positive from typos goes in `.typos.toml` with a comment saying what the
word is. The offline lychee run is CI's blocking
`markdown links (lychee offline)` check: it catches a relative link to a
moved file or a `#fragment` whose heading was renamed, without touching the
network. `link-check.yml` covers external URLs weekly and after each green
CI run on `main`, keeping a `link-check` tracking issue open while any link
is broken.
`crates/sharerr/src/web/docs.rs` hard-codes the documentation links the web
UI shows, as absolute URLs with deep anchors into `SETTINGS.md`,
`COMPATIBILITY.md`, `SECURITY.md`, `API.md`, `LIGHTHOUSE.md` and the README,
and has a test that resolves each against a real heading, so renaming one of
those headings also fails `cargo test`. Files under `docker/`, `crates/`, and `CLAUDE.md` are excluded from
the published docs site (`_config.yml`), so a link to them from `README.md`
or `docs/*.md` must be an absolute GitHub URL.

## Which doc changes with what

[`docs/README.md`](README.md) maps each doc to the topic it owns; this is
the same map read the other way, as a checklist — what _kind_ of change
should make you go check a doc. The PR template's checklist just points
here rather than repeating it.

| If your change...                                                                                                                                                                                    | Update                                                                                                                                                       |
| ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| adds, renames, or changes the default of a `sharerr.toml` field, environment variable, or vault secret                                                                                               | [Settings reference](SETTINGS.md)                                                                                                                            |
| adds or drops a supported torrent client, *arr app, or indexer behaviour                                                                                                                             | [Compatibility](COMPATIBILITY.md)                                                                                                                            |
| adds, removes, or changes an HTTP route (Torznab, Jackett, gossip, tracker, lighthouse, ops)                                                                                                         | [The API](API.md) (regenerate `docs/openapi.json`; see that doc for how)                                                                                     |
| moves a trust boundary, changes where state lives, or adds/removes a crate                                                                                                                           | [Architecture](ARCHITECTURE.md)                                                                                                                              |
| changes the tag scheme, an image, the release gates (`docker.yml`'s `gate` and `scan` jobs; the `release` environment has no required reviewer), or anything `docker.yml`/`docker-image.yml` publish | [Releasing](RELEASING.md)                                                                                                                                    |
| adds a test fixture, a compose stack, or a testing tier                                                                                                                                              | [Testing](TESTING.md), and [the compose stacks doc](https://github.com/ivylikethevine/sharerr-rs/blob/main/docker/README.md) if it touches tier 2            |
| changes what data crosses a trust boundary, or a class of vulnerability the threat model should name                                                                                                 | [Security policy](SECURITY.md)                                                                                                                               |
| changes a deploy compose layout under `docker/deploy/`                                                                                                                                               | [Deploying](https://github.com/ivylikethevine/sharerr-rs/blob/main/docker/deploy/README.md)                                                                  |
| changes anything a user would notice (a feature, a fix, a default, a removed option)                                                                                                                 | the PR template's `## Release note` section, which the release workflow collects into the GitHub Release body ([Releasing](RELEASING.md#the-github-release)) |
| fixes or dismisses a CodeQL finding for a reason a later reader could not reconstruct                                                                                                                | [CodeQL findings](CODEQL.md)                                                                                                                                 |
| changes a convention, trap, or repository mechanic an agent would need but a person wouldn't                                                                                                         | [`CLAUDE.md`](https://github.com/ivylikethevine/sharerr-rs/blob/main/CLAUDE.md)                                                                              |

A change that fits none of these rows updates no doc beyond its own code
comments — most PRs are in this category, and the checklist item exists for
the minority that aren't.

## Commits and pull requests

Branch from `dev`, where active development happens. `main` carries a
ruleset requiring a pull request, a protected ref, and verified commit
signatures.

**Every PR body should have a `## Release note` section**: one or two
sentences a user would care about, or `none`. It is optional:
`release-note.yml`'s `release note (pr body)` check warns on a PR with no
section or an empty one but stays green, and re-runs when the description is
edited, so fixing it re-runs nothing else. A PR without one is left out of
the release page's "What changed". The release workflow collects those sections into the GitHub Release
body (see [`RELEASING.md`](RELEASING.md#the-github-release)). A `dev` →
`main` PR is checked the same way, since those are the PRs a release reads:
its note aggregates the notes of the PRs that went into `dev`. Dependabot's
PRs are exempt.

## AI-assisted contributions

Generative AI is allowed here; the README's
[AI usage](../README.md#ai-usage) section says how the maintainer uses it.
The same accountability applies to a contribution: disclose AI use in the PR
template's "AI disclosure" checklist, and only submit what you have reviewed,
understood, and would stand behind as if you had written it by hand. "The
agent wrote it" does not explain away a bug.

## Licence

MIT. By contributing, you agree your contribution is licensed under the same
terms; see [`LICENSE.md`](../LICENSE.md).
