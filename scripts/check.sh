#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Run locally what CI's blocking jobs run, by the same commands. Every check
# step in ci.yml (and coverage.yml's floor) calls `scripts/check.sh <check>`,
# so this file is the one place a command lives and the two cannot drift.
#
#   scripts/check.sh                  same as --all
#   scripts/check.sh --fast           seconds; nothing compiles
#   scripts/check.sh --lint           --fast, plus every check that compiles
#                                     or scans without running the tests
#   scripts/check.sh --all            --lint, plus tests, MSRV and coverage
#   scripts/check.sh <check>...       just those checks, in that order
#   scripts/check.sh --install [...]  first fetch the pinned tools, then run
#   scripts/check.sh --list           print every check and its group
#   scripts/check.sh --msrv           print Cargo.toml's rust-version
#
# --install downloads every tools.txt row this script uses into
# target/ci-tools/ through .github/actions/setup-tool/install.sh (sha256
# verified, no sudo) and runs `npm ci` in .github/ for the Markdown tools.
# target/ci-tools/ goes first on PATH whether or not --install ran, so once
# fetched the pins win over whatever else is installed.
#
# A tool that is missing locally is a skip with a note, not a failure, so a
# partial toolbox still gets an answer for the rest. Under GitHub Actions a
# skip is a failure: there, a missing tool means a broken install step, and a
# gate that skips itself would pass silently.
#
# Checks, by the ci.yml job that runs them (a job name here is a required
# check name there; see ci.yml's header):
#   rustfmt                                  fmt
#   clippy + tests                           clippy, test, doc (rustdoc -D warnings)
#   msrv                                     msrv (needs the rust-version toolchain)
#   cargo-deny                               deny
#   shell + compose                          shellcheck, compose (needs docker)
#   terraform (fmt + validate)               terraform
#   workflow lint (zizmor + actionlint)      zizmor, actionlint, workflows
#   markdown links (lychee offline)          links
#   docs (markdownlint + prettier + typos)   typos, markdownlint, prettier
#   secrets (gitleaks)                       gitleaks
#   coverage.yml, after merge                coverage = coverage-run + coverage-floor
#
# Not here, because they have no local equivalent: CI's dependency review (a
# PR-diff API) and the advisory hadolint job.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

# shellcheck source=../.github/actions/setup-tool/lib.sh
source .github/actions/setup-tool/lib.sh

# The line-coverage floor, in percent, for the tier-1 suite. A few points
# under the measured figure (97.2% when this was set), so an ordinary change
# does not trip it and a real drop does. Raise it as coverage rises; lowering
# it is a decision to write down in the PR that does it.
COVERAGE_FLOOR_LINES=94

# Markdown tools come from .github/package-lock.json, not a global install.
node_bin=.github/node_modules/.bin

tool_dir="$root/target/ci-tools"
export PATH="$tool_dir:$PATH"

fast=(fmt shellcheck compose zizmor actionlint workflows links typos markdownlint prettier)
lint=("${fast[@]}" clippy doc deny terraform gitleaks)
all=("${lint[@]}" test msrv coverage)

# Every tools.txt row some check below runs, for --install.
pinned=(shellcheck lychee typos zizmor actionlint yq cargo-deny gitleaks terraform cargo-llvm-cov)

in_ci() { [ "${GITHUB_ACTIONS:-}" = true ]; }

# _files <pathspec>... - NUL-separated files matching the pathspecs that exist
# on disk: tracked, plus untracked-but-not-ignored so a file about to be
# committed is checked too. In a CI checkout the second set is empty.
_files() {
  local f
  while IFS= read -r -d '' f; do
    [ -f "$f" ] && printf '%s\0' "$f"
  done < <(git ls-files -z --cached --others --exclude-standard -- "$@")
}

# _skip <reason> - exit status 3, which the loop at the bottom reports as a skip
_skip() {
  echo "skip: $1"
  return 3
}

# _reports_pin <binary> <pin> <verify flags> - the binary's version output
# (the first three lines of running it with tools.txt's verify flags)
# mentions the pin
_reports_pin() {
  local out
  # shellcheck disable=SC2086 # the verify column is flags, word-split on purpose
  out="$("$1" $3 2>&1 | head -3)" || true
  case "$out" in
  *"$2"*) return 0 ;;
  *) return 1 ;;
  esac
}

# _need <tool> - the tool is on PATH, or skip. A tools.txt tool whose version
# does not mention the pin gets a note, since its findings may differ from CI's.
_need() {
  local tool="$1" row pin verify
  if ! command -v "$tool" >/dev/null 2>&1; then
    _skip "$tool is not installed (scripts/check.sh --install fetches the pinned one)"
    return
  fi
  row="$(_ci_tool_row "$tool" 2>/dev/null)" || return 0
  IFS='|' read -r _ pin _ _ verify _ <<<"$row"
  _reports_pin "$tool" "$pin" "$verify" ||
    echo "note: $(command -v "$tool") is not the tools.txt pin ($pin); results may differ from CI"
}

# _need_node <tool> - a Markdown tool from .github/node_modules, or skip
_need_node() {
  [ -x "$node_bin/$1" ] || _skip "$node_bin/$1 is missing (scripts/check.sh --install runs npm ci in .github/)"
}

check_fmt() {
  cargo fmt --all --check
}

check_clippy() {
  cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
}

# Broken intra-doc links and the like; rustdoc only warns by default.
check_doc() {
  RUSTDOCFLAGS="${RUSTDOCFLAGS:+$RUSTDOCFLAGS }-D warnings" \
    cargo doc --workspace --no-deps --all-features --locked
}

# --all-features compiles the tier-2 suite so it cannot rot; its #[ignore]
# keeps it from running. Never add --include-ignored (see docs/TESTING.md).
check_test() {
  cargo test --workspace --all-features --locked
}

# _msrv - Cargo.toml's rust-version, the one place it is parsed: check_msrv
# here, and through --msrv ci.yml's msrv job and check_tool_versions.local.sh
_msrv() {
  local msrv
  msrv="$(sed -n 's/^rust-version[[:space:]]*=[[:space:]]*"\([0-9.]*\)".*/\1/p' Cargo.toml | head -1)"
  [ -n "$msrv" ] || {
    echo "msrv: no rust-version in Cargo.toml" >&2
    return 1
  }
  echo "$msrv"
}

# `check`, not `test`: the claim is that the code compiles on the declared
# minimum. The toolchain comes from Cargo.toml's rust-version, so this has no
# version of its own to drift; ci.yml's msrv job installs it first.
check_msrv() {
  local msrv tc
  msrv="$(_msrv)" || return 1
  for tc in "$msrv" "$msrv.0"; do
    if rustup run "$tc" cargo --version >/dev/null 2>&1; then
      cargo "+$tc" check --workspace --all-targets --all-features --locked
      return
    fi
  done
  _skip "the $msrv toolchain is not installed (rustup toolchain install $msrv --profile minimal)"
}

check_deny() {
  _need cargo-deny || return
  cargo deny --all-features check advisories licenses bans sources
}

# Every tracked shell script, composite-action helpers included, rather than
# a hand-kept list.
check_shellcheck() {
  _need shellcheck || return
  _files '*.sh' | xargs -0 --no-run-if-empty shellcheck
}

# `compose config` reads files only, but it is still docker's CLI plugin.
check_compose() {
  docker compose version >/dev/null 2>&1 || {
    _skip "docker compose is not available"
    return
  }
  .github/scripts/validate_compose.sh
}

# fmt over the whole tree; init + validate in every directory holding *.tf.
# -backend=false: validation needs the providers' schemas, never state.
# Provider plugins go under target/ rather than a .terraform/ in the tree.
check_terraform() {
  _need terraform || return
  local base=docker/deploy/lighthouse/terraform dir data rc=0
  export CHECKPOINT_DISABLE=1
  terraform fmt -check -recursive -diff "$base" || rc=1
  while IFS= read -r dir; do
    echo "terraform validate: $dir"
    data="$root/target/terraform/${dir//\//_}"
    TF_DATA_DIR="$data" terraform -chdir="$dir" init -backend=false -input=false -no-color >/dev/null || {
      rc=1
      continue
    }
    TF_DATA_DIR="$data" terraform -chdir="$dir" validate -no-color || rc=1
  done < <(_files "$base/**/*.tf" | xargs -0 -n1 dirname | sort -u)
  return "$rc"
}

check_zizmor() {
  _need zizmor || return
  # The repo root rather than .github/: zizmor honours .gitignore only when
  # it walks from the root, and .github/node_modules holds vendored workflows.
  zizmor --no-progress .
}

check_actionlint() {
  _need actionlint || return
  if in_ci; then actionlint -color; else actionlint; fi
}

check_workflows() {
  _need yq || return
  .github/scripts/lint_workflows.sh
}

# Offline: relative links and #fragments only, so it cannot flake on someone
# else's server. link-check.yml sweeps external URLs weekly.
check_links() {
  _need lychee || return
  _files '*.md' | xargs -0 --no-run-if-empty \
    lychee --config lychee.toml --offline --include-fragments --no-progress
}

# A typo can live in Rust source as easily as in a doc: the whole tree.
check_typos() {
  _need typos || return
  typos
}

check_markdownlint() {
  _need_node markdownlint-cli2 || return
  _files '*.md' | xargs -0 --no-run-if-empty \
    "$node_bin/markdownlint-cli2" --config .markdownlint.yaml
}

check_prettier() {
  _need_node prettier || return
  _files '*.md' | xargs -0 --no-run-if-empty "$node_bin/prettier" --check
}

# Full history; CI checks out with fetch-depth: 0 for the same answer.
# .gitleaks.toml carries the known false positives, each with a reason.
check_gitleaks() {
  _need gitleaks || return
  gitleaks git --redact --no-banner --config .gitleaks.toml .
}

check_coverage-run() {
  _need cargo-llvm-cov || return
  cargo llvm-cov --workspace --all-features --locked --no-report
}

# Reads the profile coverage-run left behind; no second test run.
check_coverage-floor() {
  _need cargo-llvm-cov || return
  cargo llvm-cov report --summary-only --fail-under-lines "$COVERAGE_FLOOR_LINES"
}

check_coverage() {
  check_coverage-run && check_coverage-floor
}

install_tools() {
  local tool row pin verify
  mkdir -p "$tool_dir"
  for tool in "${pinned[@]}"; do
    row="$(_ci_tool_row "$tool")"
    IFS='|' read -r _ pin _ _ verify _ <<<"$row"
    if [ -x "$tool_dir/$tool" ] && _reports_pin "$tool_dir/$tool" "$pin" "$verify"; then
      echo "install: $tool $pin already in target/ci-tools"
      continue
    fi
    echo "install: $tool $pin"
    CI_TOOL="$tool" CI_TOOL_BIN_DIR="$tool_dir" .github/actions/setup-tool/install.sh install >/dev/null
  done
  if command -v npm >/dev/null 2>&1; then
    echo "install: npm ci in .github/ (markdownlint-cli2, prettier)"
    npm ci --prefix .github --ignore-scripts --no-audit --no-fund >/dev/null
  else
    echo "install: no npm on PATH - markdownlint and prettier will skip" >&2
  fi
}

usage() {
  sed -n '3,16s/^# \{0,1\}//p' "${BASH_SOURCE[0]}"
}

checks=()
install=0
for arg in "$@"; do
  case "$arg" in
  --fast) checks+=("${fast[@]}") ;;
  --lint) checks+=("${lint[@]}") ;;
  --all) checks+=("${all[@]}") ;;
  --install) install=1 ;;
  --list)
    printf 'fast: %s\nlint: %s\nall:  %s\n' "${fast[*]}" "${lint[*]}" "${all[*]}"
    exit 0
    ;;
  --msrv)
    _msrv
    exit
    ;;
  -h | --help)
    usage
    exit 0
    ;;
  -*)
    echo "check.sh: unknown option $arg" >&2
    usage >&2
    exit 2
    ;;
  *)
    declare -F "check_$arg" >/dev/null || {
      echo "check.sh: unknown check '$arg' (--list shows them)" >&2
      exit 2
    }
    checks+=("$arg")
    ;;
  esac
done
[ "$install" -eq 0 ] || install_tools
[ "${#checks[@]}" -gt 0 ] || checks=("${all[@]}")

passed=()
failed=()
skipped=()
for c in "${checks[@]}"; do
  if [ "${#checks[@]}" -gt 1 ]; then echo "==> $c"; fi
  rc=0
  "check_$c" || rc=$?
  if [ "$rc" -eq 0 ]; then
    passed+=("$c")
  elif [ "$rc" -eq 3 ] && ! in_ci; then
    skipped+=("$c")
  else
    [ "$rc" -ne 3 ] || echo "check.sh: '$c' cannot skip under GitHub Actions - the install step above it failed" >&2
    failed+=("$c")
  fi
done

if [ "${#checks[@]}" -gt 1 ]; then
  echo
  echo "passed:  ${passed[*]:-none}"
  [ "${#skipped[@]}" -eq 0 ] || echo "skipped: ${skipped[*]}"
  [ "${#failed[@]}" -eq 0 ] || echo "FAILED:  ${failed[*]}"
fi
[ "${#failed[@]}" -eq 0 ]
