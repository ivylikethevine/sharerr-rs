#!/usr/bin/env bash
# Reading ./tools.txt, shared by this action's install.sh and by
# ../../scripts/check_tool_versions.sh - the two consumers of the roster,
# so neither keeps a copy of it. Ported from say-hi's identically-named file.

# tools.txt sits beside this file, so both consumers find it the same way:
# install.sh is sourced from $GITHUB_ACTION_PATH, check_tool_versions.sh from
# the repo root, and neither needs to know which.
_SR_TOOLS_TXT="$(cd -P "$(dirname "${BASH_SOURCE[0]}")" && pwd)/tools.txt"

# _sr_tool_rows - every data row, comments and blank lines dropped
function _sr_tool_rows() {
  grep -v '^[[:space:]]*\(#\|$\)' "$_SR_TOOLS_TXT"
}

# _sr_tool_row <tool> - that tool's row, or a message and non-zero
function _sr_tool_row() {
  local row
  row="$(_sr_tool_rows | grep "^$1|" | head -1)"
  [ -n "$row" ] || {
    echo "setup-tool: no row for '$1' in $_SR_TOOLS_TXT" >&2
    return 1
  }
  printf '%s\n' "$row"
}
