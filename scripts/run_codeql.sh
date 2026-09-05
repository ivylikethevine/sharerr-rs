#!/usr/bin/env bash
#
# Run the same CodeQL analysis .github/workflows/codeql.yml runs in CI —
# `rust` and `actions`, the default "code-scanning" query suite for each,
# `build-mode: none` for both — entirely locally, so a finding shows up
# before a push rather than after one.
#
#   ./scripts/run_codeql.sh              both languages
#   ./scripts/run_codeql.sh rust         just the Rust source
#   ./scripts/run_codeql.sh actions      just the workflow YAML
#
# Not part of the always-run verification loop in CLAUDE.md: the CodeQL CLI
# is a ~580MB one-time download this script does not manage, and a full
# database build/analyze is much slower than cargo test+clippy+build+fmt.
# Run this before pushing anything that touches crypto/secret handling or a
# workflow file — the two query classes that have actually caught something
# in this repo — or whenever CI's CodeQL check disagrees with what shipped
# locally.
#
# One-time setup: download the CLI + query pack bundle (the "codeql bundle",
# not the bare CLI — the bundle ships the standard query packs already
# resolved, so no further network access happens at scan time) from
# https://github.com/github/codeql-action/releases, tag `codeql-bundle-vX.Y.Z`
# matching the version this repo's codeql.yml pins, asset
# `codeql-bundle-linux64.tar.zst` (or the platform equivalent), and extract it
# to `~/.codeql/` so `~/.codeql/codeql/codeql` exists. `CODEQL_HOME` overrides
# that location.

set -euo pipefail

# See run_docker_tests.sh's header for why `readlink -f` rather than a bare
# `dirname $0`.
cd "$(dirname "$(readlink -f "$0")")/.."

CODEQL_HOME="${CODEQL_HOME:-$HOME/.codeql}"
CODEQL="$CODEQL_HOME/codeql/codeql"
if ! [ -x "$CODEQL" ]; then
    if command -v codeql >/dev/null 2>&1; then
        CODEQL="$(command -v codeql)"
    else
        echo "error: no CodeQL CLI found at $CODEQL and none on PATH." >&2
        echo "See this script's header comment for how to install one." >&2
        exit 1
    fi
fi

case "${1:-both}" in
    rust) LANGUAGES=(rust) ;;
    actions) LANGUAGES=(actions) ;;
    both) LANGUAGES=(rust actions) ;;
    *)
        echo "usage: $0 [rust|actions]" >&2
        exit 2
        ;;
esac

# The Rust extractor expands macros through an embedded rust-analyzer, which
# reads `core`'s own sources out of the toolchain's `rust-src` component.
# Nothing installs that by default: rustup omits it until asked, and a distro
# rust package (Arch's among them) splits it into a separate package. Without
# it every `format!`, `assert!`, `println!` and `panic!` in the tree fails to
# expand — and that failure is silent. Extraction still succeeds, the analysis
# still runs, and this script still prints "clean", while the sinks the
# cleartext-logging and log-injection queries match on have simply stopped
# existing in the database. One run here failed 4274 expansions under
# `crates/` and reported two findings; the same tree with `rust-src` present
# is the only number worth acting on. Checked rather than assumed, because
# "clean" and "never analyzed" are indistinguishable in the output.
if [ -z "${SKIP_RUST_SRC_CHECK:-}" ] && [[ " ${LANGUAGES[*]} " == *" rust "* ]]; then
    if ! command -v rustc >/dev/null 2>&1; then
        echo "run_codeql: no rustc on PATH - the rust extractor needs one to find its sysroot." >&2
        exit 127
    fi
    RUST_SYSROOT="$(rustc --print sysroot)"
    if ! [ -f "$RUST_SYSROOT/lib/rustlib/src/rust/library/core/src/macros/mod.rs" ]; then
        cat >&2 <<EOF
run_codeql: rust-src is missing from $RUST_SYSROOT.

Without it the extractor cannot expand core's builtin macros, so every
format!/assert!/println!/panic! drops out of the database and this script
reports "clean" whether the tree is or not. Install it and re-run:

    rustup component add rust-src   # a rustup toolchain
    sudo pacman -S rust-src         # Arch's distro rust

SKIP_RUST_SRC_CHECK=1 runs anyway, understanding that a clean result then
means nothing.
EOF
        exit 1
    fi
fi

WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT

# CI analyses a fresh checkout, which has no `target/`; a working tree has one,
# and extracting it means minutes spent on generated build artefacts plus
# findings in code no PR could ever touch. Excluded so a local run answers the
# same question CI's does.
cat >"$WORKDIR/config.yml" <<'YAML'
paths-ignore:
  - target
YAML

STATUS=0
for lang in "${LANGUAGES[@]}"; do
    echo "==> building the $lang database (no-build mode, matching codeql.yml)"
    "$CODEQL" database create "$WORKDIR/$lang-db" \
        --language="$lang" \
        --build-mode=none \
        --source-root=. \
        --codescanning-config="$WORKDIR/config.yml" \
        --quiet

    echo "==> analyzing $lang against the default code-scanning suite"
    "$CODEQL" database analyze "$WORKDIR/$lang-db" \
        "codeql/$lang-queries:codeql-suites/$lang-code-scanning.qls" \
        --format=csv \
        --output="$WORKDIR/$lang.csv" \
        --quiet

    # No `--sarif-category`/SARIF here: nothing uploads this locally the way
    # CI's `upload-sarif` step does, so the CLI's own CSV (name, description,
    # severity, message, path, line, ...) is the whole finding with nothing
    # to parse back out of it — no SARIF write, no JSON walk, no extra
    # interpreter dependency for a file this script deletes seconds later.
    if [ -s "$WORKDIR/$lang.csv" ]; then
        cat "$WORKDIR/$lang.csv"
        echo "==> $lang: $(wc -l <"$WORKDIR/$lang.csv") finding(s) — see above"
        STATUS=1
    else
        echo "==> $lang: clean"
    fi
done

exit "$STATUS"
