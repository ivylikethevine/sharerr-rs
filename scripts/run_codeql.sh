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

WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT

STATUS=0
for lang in "${LANGUAGES[@]}"; do
    echo "==> building the $lang database (no-build mode, matching codeql.yml)"
    "$CODEQL" database create "$WORKDIR/$lang-db" \
        --language="$lang" \
        --build-mode=none \
        --source-root=. \
        --quiet

    echo "==> analyzing $lang against the default code-scanning suite"
    "$CODEQL" database analyze "$WORKDIR/$lang-db" \
        "codeql/$lang-queries:codeql-suites/$lang-code-scanning.qls" \
        --format=sarifv2.1.0 \
        --output="$WORKDIR/$lang.sarif" \
        --sarif-category="/language:$lang" \
        --quiet

    FINDINGS="$(
        python3 - "$WORKDIR/$lang.sarif" "$lang" <<'PYEOF'
import json, sys

path, lang = sys.argv[1], sys.argv[2]
with open(path, encoding="utf-8") as f:
    sarif = json.load(f)

for run in sarif.get("runs", []):
    rules = {
        rule["id"]: rule.get("shortDescription", {}).get("text", rule["id"])
        for rule in run.get("tool", {}).get("driver", {}).get("rules", [])
    }
    for result in run.get("results", []):
        rule = rules.get(result.get("ruleId"), result.get("ruleId", "?"))
        message = result.get("message", {}).get("text", "").split("\n")[0]
        for loc in result.get("locations", []):
            phys = loc.get("physicalLocation", {})
            uri = phys.get("artifactLocation", {}).get("uri", "?")
            line = phys.get("region", {}).get("startLine", "?")
            print(f"{uri}:{line}: [{lang}] {rule} — {message}")
PYEOF
    )"
    if [ -n "$FINDINGS" ]; then
        echo "$FINDINGS"
        echo "==> $lang: $(echo "$FINDINGS" | wc -l) finding(s) — see above"
        STATUS=1
    else
        echo "==> $lang: clean"
    fi
done

exit "$STATUS"
