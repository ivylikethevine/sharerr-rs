#!/usr/bin/env bash
# The install half of ./action.yml, in a real file so the repo's own
# `shellcheck` step (ci.yml's `scripts` job) reads it - a `run:` block inside
# a composite action is code nothing here would otherwise lint, and this one
# curls, untars and sudo mvs. Ported from say-hi's identically-named script.
#
# Two subcommands, because actions/cache needs the version as an expression
# before the install step runs:
#   resolve   write the effective version to $GITHUB_OUTPUT
#   install   download it and put it on PATH
set -euo pipefail

# SC1091: this repo's shellcheck step (ci.yml's `scripts` job) runs without
# -x, so `source=` alone (say-hi's own install.sh uses it, under a shellcheck
# invocation that does pass -x) does not resolve this - disabled explicitly
# instead of adding -x repo-wide for one file.
# shellcheck disable=SC1091
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

: "${SR_TOOL:?set by action.yml}"

# the row is captured first rather than read straight from a here-string: a
# failing command substitution inside `read <<<` is not the read's status, so
# an unknown tool would print its error and still exit 0
_sr_row="$(_sr_tool_row "$SR_TOOL")" || exit 1
IFS='|' read -r _ _sr_pin _sr_kind _sr_url _sr_verify _ <<<"$_sr_row"

# an explicit `version:` input wins over the manifest's pin; empty means "use
# the pin", which is what every call site passes
_sr_version="${SR_TOOL_VERSION:-}"
[ -n "$_sr_version" ] || _sr_version="$_sr_pin"

case "${1:-}" in
resolve)
  : "${GITHUB_OUTPUT:?set by the runner}"
  printf 'version=%s\n' "$_sr_version" >>"$GITHUB_OUTPUT"
  exit 0
  ;;
install) ;;
*)
  echo "setup-tool: expected 'resolve' or 'install', got '${1:-}'" >&2
  exit 1
  ;;
esac

# %a: no current row needs more than one asset (this repo's jobs are all
# Linux), but the substitution is kept so a future macOS job costs a tools.txt
# row, not a rewrite of this script - see say-hi's setup-tool, which does need
# it, for the shellcheck row this was copied from. Resolved before %v so a
# version can never look like an asset slug.
case "$_sr_url" in
*%a*)
  case "$(uname -s).$(uname -m)" in
  Linux.x86_64) _sr_asset="linux.x86_64" ;;
  Darwin.arm64) _sr_asset="darwin.aarch64" ;;
  Darwin.x86_64) _sr_asset="darwin.x86_64" ;;
  *)
    echo "setup-tool: no $SR_TOOL asset mapping for $(uname -s).$(uname -m)" >&2
    exit 1
    ;;
  esac
  _sr_url="${_sr_url//%a/$_sr_asset}"
  ;;
esac
_sr_url="${_sr_url//%v/$_sr_version}"

_sr_tmp="$(mktemp -d)"
trap 'rm -rf "$_sr_tmp"' EXIT

case "$_sr_kind" in
raw)
  curl -sSfL -o "$_sr_tmp/$SR_TOOL" "$_sr_url"
  chmod +x "$_sr_tmp/$SR_TOOL"
  _sr_bin="$_sr_tmp/$SR_TOOL"
  ;;
tar.gz | tar.xz)
  curl -sSfL -o "$_sr_tmp/archive" "$_sr_url"
  # extract whole and then look, rather than naming a member: the layouts here
  # are flat, versioned-dir and arch-subdir, and a stale member path fails hard
  # on a version bump where a search does not
  case "$_sr_kind" in
  tar.gz) tar -xzf "$_sr_tmp/archive" -C "$_sr_tmp" ;;
  tar.xz) tar -xJf "$_sr_tmp/archive" -C "$_sr_tmp" ;;
  esac
  _sr_bin="$(find "$_sr_tmp" -type f -name "$SR_TOOL" -perm -u+x | head -1)"
  [ -n "$_sr_bin" ] || {
    echo "setup-tool: no executable named $SR_TOOL inside $_sr_url" >&2
    exit 1
  }
  ;;
*)
  echo "setup-tool: unknown kind '$_sr_kind' for $SR_TOOL" >&2
  exit 1
  ;;
esac

sudo mkdir -p /usr/local/bin
sudo mv "$_sr_bin" "/usr/local/bin/$SR_TOOL"

# the verify column is a flag, word-split on purpose
# shellcheck disable=SC2086
"$SR_TOOL" $_sr_verify
