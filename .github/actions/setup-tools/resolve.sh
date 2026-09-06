#!/usr/bin/env bash
# The batched form of ../setup-tool/install.sh's `resolve` subcommand:
# resolves every pin named in $SR_TOOLS at once, so action.yml can build one
# shared actions/cache key and path list instead of paying N round trips for
# N tools.
#
# In a real file, not an inline `run:` block, for the same reason
# ../setup-tool/install.sh is: this repo's own shellcheck step (ci.yml's
# `scripts` job) reads it, and a `run:` block inside a composite action is
# code nothing here would otherwise lint.
set -euo pipefail

# SC1091: this repo's shellcheck step (ci.yml's `scripts` job) runs without
# -x, so `source=` alone does not resolve this - disabled explicitly instead
# of adding -x repo-wide for one file.
# shellcheck disable=SC1091
source "$(dirname "${BASH_SOURCE[0]}")/../setup-tool/lib.sh"

: "${SR_TOOLS:?set by action.yml}"
: "${GITHUB_OUTPUT:?set by the runner}"

_sr_key=""
_sr_paths=""
for _sr_t in $SR_TOOLS; do
  _sr_row="$(_sr_tool_row "$_sr_t")" || exit 1
  IFS='|' read -r _ _sr_pin _ _ _ _ _ _ <<<"$_sr_row"
  _sr_key="$_sr_key $_sr_t=$_sr_pin"
  _sr_paths="$_sr_paths/usr/local/bin/$_sr_t"$'\n'
done

{
  printf 'key=%s\n' "$_sr_key"
  echo "paths<<SR_EOF"
  printf '%s' "$_sr_paths"
  echo "SR_EOF"
} >>"$GITHUB_OUTPUT"
