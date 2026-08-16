# Task runner for sharerr. `just --list` to see everything.
#
# Nothing here is required — every recipe is a one-line wrapper around the command
# it names, and the commands are documented in README.md and CLAUDE.md. This exists
# so the flags that matter (--all-features, -D warnings) are not retyped from
# memory and quietly dropped.

default:
    @just --list

# The full local gate: what CI runs, in the order CI runs it.
check: fmt-check lint test

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all --check

# `-D warnings` is the point: unwrap_used and expect_used are `warn` in Cargo.toml
# precisely so this promotes them.
lint:
    cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

# Tier 1 — hermetic. No network, no containers, no database.
test:
    cargo test --workspace --all-features --locked

# Tier 2 — the real *arr stack. Opt-in, local only, several minutes.
test-e2e:
    ./run_docker_tests.sh

# Advisories, licences, and banned crates. Config in deny.toml.
audit:
    cargo deny check advisories licenses bans sources

# Doubles as the MSRV check: the Dockerfile pins the workspace's rust-version,
# and a local toolchain is always newer.
docker-build:
    docker build . -t sharerr-rs:dev

# Shell and compose linting — neither is covered by any cargo check.
lint-scripts:
    shellcheck run_docker_tests.sh
    docker compose -f docker/compose.test.yml config -q

# Drop the test stack and everything it left behind.
clean-stack:
    docker compose -f docker/compose.test.yml --profile indexer down -v --remove-orphans
    rm -rf docker/state || docker run --rm -v "$PWD/docker:/w" alpine:3 rm -rf /w/state
