# Testing

The runner, the two tiers, the compose stacks tier 2 drives, fixtures, and
coverage. [`docs/CONTRIBUTING.md`](CONTRIBUTING.md#the-verification-loop) has
the loop to run before opening a pull request; this page is the reference
behind it.

## Table of contents

- [Tier 1: hermetic](#tier-1-hermetic)
- [Tier 2: the compose stacks](#tier-2-the-compose-stacks)
- [Fixtures](#fixtures)
- [Coverage](#coverage)
- [Benchmarks and fuzzing](#benchmarks-and-fuzzing)

## Tier 1: hermetic

```bash
cargo test --workspace --all-features
```

This is what `ci.yml`'s `clippy + tests` job runs on every push and pull
request (with `--locked` there). Nothing in it needs network access, a
container, or a database: service clients (Sonarr, Radarr, qBittorrent,
Transmission, rTorrent) run against `wiremock` on loopback, `sqlx` runs
against `sqlite::memory:`, and migrations are embedded at compile time by
`sqlx::migrate!`. Keeping this tier hermetic is what makes CI fast and
reliable. `--all-features` compiles the tier-2 suite too, so it cannot
silently rot, without running it.

**No tier-1 fixture opens a real vault.** `state::fixtures::unconfigured()`
has no master key, so vault-backed accessors resolve to "unavailable" there.
Prefer testing store-backed logic directly, with the store and the resolved
secret as plain parameters. When a test genuinely needs the vault open, or
needs the master key _absent_, wrap it in `figment::Jail::expect_with`; see
[`CLAUDE.md`](https://github.com/ivylikethevine/sharerr-rs/blob/main/CLAUDE.md#testing-tiers)
for why a bare `std::env::set_var` races the parallel runner, and the fixture
names worth knowing.

## Tier 2: the compose stacks

```bash
./scripts/run_docker_tests.sh                 # qBittorrent, the plain stack
./scripts/run_docker_tests.sh --vpn           # qBittorrent behind gluetun
./scripts/run_docker_tests.sh --transmission
./scripts/run_docker_tests.sh --rtorrent
./scripts/run_docker_tests_two_instance.sh    # two friend-to-friend instances
```

Opt-in and local only; CI never runs it. One flag at a time. The script
brings up `docker/compose.test.yml` (or the `--vpn`, `--transmission`,
`--rtorrent` sibling), seeds Sonarr, Radarr and Lidarr with tagged synthetic
content via `sharerr-testkit`'s `seed-arr` binary, runs the suite behind the
`e2e` feature and `#[ignore]` (`cargo test -p sharerr --features e2e --
--ignored --test-threads=1`), and tears the stack down on exit whether the
run passed or not. Every step is idempotent, so re-running after a failure is
safe.

The assertion that justifies this tier existing, and that no mock can prove:
after a real sync through a real torrent client, every media file has the
same inode, mtime, and length it started with.

The two-instance script drives a real gossip/request/grab exchange between
two independent sharerr stacks, the one scenario a single-instance stack
cannot exercise. See
[`docker/README.md`](https://github.com/ivylikethevine/sharerr-rs/blob/main/docker/README.md)
for each stack's services, ports, and how to exercise the feed and tracker by
hand.

## Fixtures

All test content is synthetic: invented titles, seeded pseudo-random bytes,
`FAKEGRP` release names, so torrent info hashes are stable across machines.
No real media, ever. `sharerr-testkit`'s `gen-fixtures` binary generates the
files tier 2 seeds; `seed-arr` pushes them into the *arr apps. Neither runs
as part of tier 1. `scripts/screenshot_pages.sh` screenshots every page
`sharerr preview` serves, for refreshing the README's screenshots after a
layout change.

## Coverage

```bash
cargo llvm-cov --workspace --all-features --locked --no-report
cargo llvm-cov report --html --output-dir coverage-html
```

`coverage.yml` runs this over tier 1 only, on every push to `main`, and
publishes the figure as a shields.io endpoint badge through `pages.yml`
rather than a Codecov upload: no third-party account, no token. There is no
threshold and no gate; a number that measures only the hermetic suite is
worth publishing, not worth failing a PR over. The
[badge on the README](../README.md) is the current figure. See the
workflow's own comments for the column it reads out of `cargo-llvm-cov`'s
summary (Lines, not Regions).

## Benchmarks and fuzzing

Neither exists. `unsafe_code = "forbid"` at the workspace level narrows what
fuzzing would catch, but the surfaces that parse externally influenced input
(`sharerr-torrent`'s bencoding, `sharerr-rtorrent`'s XML-RPC responses,
`sharerr-probe`'s media-file parsing) are real candidates for `cargo-fuzz`.
`.scorecard.yml`'s header records the same gap against Scorecard's fuzzing
check, deliberately left unannotated rather than marked not-applicable.
