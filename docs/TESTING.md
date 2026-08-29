# Testing

The runner, the two tiers, the compose stacks tier 2 drives, and what CI does
and does not run. [`docs/CONTRIBUTING.md`](CONTRIBUTING.md) has the loop to
run before opening a pull request; this page is the fuller reference behind
it. Ported from say-hi's identically-named doc, which keeps this material out
of its own `CLAUDE.md` for the same reason: a contributor who never opens an
agent-facing file should still be able to read this.

## Contents

- [Running the tests](#running-the-tests)
- [Tier 1: hermetic](#tier-1-hermetic)
- [Tier 2: the compose stack](#tier-2-the-compose-stack)
- [The two-instance stack](#the-two-instance-stack)
- [Fixtures](#fixtures)
- [Coverage](#coverage)
- [Benchmarks and fuzzing](#benchmarks-and-fuzzing)

## Running the tests

```bash
cargo test --workspace --all-features
```

This is tier 1, and it is what `ci.yml`'s `clippy + tests` job runs on every
push and pull request (`--locked` there; a local run without it is fine and
usually faster). Nothing in it needs network access, a container, or a
database — see [Tier 1](#tier-1-hermetic) below for why, and CLAUDE.md's "The
verification loop" for the full command sequence (clippy and `cargo fmt
--check` alongside it) to run before calling anything done.

Tier 2 is opt-in and does not run under a plain `cargo test`:

```bash
./scripts/run_docker_tests.sh
```

CI compiles the tier-2 suite with `--all-features` on every push (so it
cannot silently rot) but never runs it — the compose stack it needs is not
something a hosted runner in this repo's CI has.

## Tier 1: hermetic

The default `cargo test --workspace`. Service clients (Sonarr, Radarr,
qBittorrent, Transmission, rTorrent) run against `wiremock` on loopback; `sqlx`
runs against `sqlite::memory:`; migrations are embedded at compile time by
`sqlx::migrate!`, so no suite needs a live database file. Keeping this tier
hermetic is what makes CI fast and reliable — see CLAUDE.md's "Testing tiers"
section for the fixtures and helpers worth knowing by name
(`state::fixtures::{unconfigured, unloadable}`, `sharerr_testkit::mock::*`),
and its "No tier-1 fixture opens a real vault" note for the one gap this tier
cannot cover on its own.

## Tier 2: the compose stack

```bash
./scripts/run_docker_tests.sh              # qBittorrent, the plain stack
./scripts/run_docker_tests.sh --vpn        # qBittorrent behind gluetun
./scripts/run_docker_tests.sh --transmission
./scripts/run_docker_tests.sh --rtorrent
```

One flag at a time. The script brings up `docker/compose.test.yml` (or its
`--vpn`/`--transmission`/`--rtorrent` sibling), seeds Sonarr and Radarr with
tagged, synthetic content via `sharerr-testkit`'s `seed-arr` binary, runs the
suite behind the `e2e` feature and `#[ignore]` (`cargo test --workspace
--all-features -- --ignored`, which is what the script actually invokes), and
tears the stack down on exit whether the run passed or not — every step is
idempotent, so re-running after a failure is always safe.

The assertion that justifies this tier existing, and that no mock can prove:
after a real sync through a real qBittorrent, every media file has the same
inode, mtime, and length it started with. See `docker/README.md` for the full
walkthrough of the stack, what each service is for, and how to exercise the
Torznab feed and the tracker by hand against it.

## The two-instance stack

```bash
./scripts/run_docker_tests_two_instance.sh
```

Brings up two independent friend-to-friend sharerr stacks and drives a real
gossip/request/grab exchange between them — the one scenario a single-instance
stack cannot exercise at all. See `docker/README.md`'s two-instance section.

## Fixtures

All test content is synthetic: invented titles, seeded pseudo-random bytes, so
torrent info hashes are stable across machines. No real media, ever.
`sharerr-testkit`'s `gen-fixtures` binary generates the files tier 2 seeds; its
`seed-arr` binary pushes them into Sonarr/Radarr. Neither runs as part of tier
1.

## Coverage

```bash
cargo llvm-cov --workspace --all-features --locked --no-report
cargo llvm-cov report --html --output-dir coverage-html
```

`coverage.yml` runs this over tier 1 only, on every push to `main`
(`workflow_dispatch` too), and publishes the figure as a shields.io endpoint
badge through `pages.yml` rather than a Codecov upload — no third-party
account, no token to provision. There is no threshold and no gate: a number
that measures only the hermetic suite (tier 2's real-qBittorrent path is not
in it) is a figure worth publishing, not one worth failing a PR over. See that
workflow's own comments for the exact column it reads out of
`cargo-llvm-cov`'s summary and why (Lines, not Regions — the two are easy to
transpose and only one of them is what the badge claims to show).

## Benchmarks and fuzzing

Neither exists in this tree today. `unsafe_code = "forbid"` at the workspace
level narrows what fuzzing would catch, but the surfaces that parse
externally-influenced input unsafely-adjacent code cannot protect against —
`sharerr-torrent`'s bencoding, `sharerr-rtorrent`'s XML-RPC responses,
`sharerr-probe`'s media-file parsing — are real candidates for `cargo-fuzz` if
this ever gets picked up. See `.scorecard.yml`'s header for the same gap
recorded against Scorecard's fuzzing check, deliberately left unannotated
rather than marked not-applicable.
