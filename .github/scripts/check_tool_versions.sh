#!/bin/bash
# The gap .github/dependabot.yml documents: dependabot moves the SHA-pinned
# `uses:` in every workflow, but it has no ecosystem for a tool that CI installs
# by curl, pip or `go install`. Those pins move only when somebody edits the
# version by hand, and nothing else in the repo would ever say they are behind.
#
# This prints each pinned version next to the upstream's latest release and
# exits with the number that differ. The tool-versions workflow runs it on a
# schedule; it runs standalone from a checkout too:
#
#   .github/scripts/check_tool_versions.sh
#
# The roster below carries no version numbers. Each row names the file and the
# regex that *extracts* the pin from the workflow that installs it, so the pin
# has exactly one home — the `run:` line that uses it — and this file cannot
# drift out of agreement with what CI actually runs. A tool whose row stops
# matching is reported as an error rather than silently skipped, because a
# roster that quietly covers nothing is worse than no roster.
#
#   name|file|extract-regex|github-owner/repo
#
# Deliberately absent: hadolint and trivy. Both are installed from
# `releases/latest` on purpose (ci.yml and image-scan.yml each say why), so
# there is no pin to drift. Everything under `uses:` belongs to dependabot.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."

_ROSTER=$(
  cat <<'ROWS'
zizmor|.github/workflows/ci.yml|zizmor==\([0-9][0-9.]*\)|zizmorcore/zizmor
actionlint|.github/workflows/ci.yml|actionlint@v\([0-9][0-9.]*\)|rhysd/actionlint
ROWS
)

# _latest <owner/repo> - the newest release tag, or empty if the API declines.
# Unauthenticated this is rate-limited to 60/hour per IP; in Actions the
# workflow passes GH_TOKEN, which raises that far above the size of the roster.
function _latest() {
  local auth=()
  [ -n "${GH_TOKEN:-}" ] && auth=(-H "Authorization: Bearer $GH_TOKEN")
  curl -sSf "${auth[@]}" "https://api.github.com/repos/$1/releases/latest" 2>/dev/null |
    sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1
}

# Process substitution, not a pipe: a piped `while` runs in a subshell, so
# $bad would be lost and this would always exit 0.
bad=0
while IFS='|' read -r tool file regex repo; do
  [ -n "$tool" ] || continue

  if [ ! -f "$file" ]; then
    printf '%-14s %-12s ERROR (no such file: %s)\n' "$tool" "-" "$file"
    bad=$((bad + 1))
    continue
  fi

  pinned="$(sed -n "s/.*$regex.*/\1/p" "$file" | head -1)"
  if [ -z "$pinned" ]; then
    printf '%-14s %-12s ERROR (no pin matched in %s - fix this row)\n' "$tool" "-" "$file"
    [ -n "${GITHUB_ACTIONS:-}" ] &&
      printf '::error title=%s::the roster regex matched nothing in %s - the pin moved or the row is stale\n' \
        "$tool" "$file"
    bad=$((bad + 1))
    continue
  fi

  latest="$(_latest "$repo")"
  # A missing upstream answer is a rate limit or an outage, not a stale pin;
  # counting it as outdated would open an issue about GitHub being slow.
  if [ -z "$latest" ]; then
    printf '%-14s %-12s (could not read the upstream release)\n' "$tool" "$pinned"
    continue
  fi

  # a leading v is stripped from both sides, so it does not matter which
  # convention the pin or the upstream uses
  if [ "${pinned#v}" = "${latest#v}" ]; then
    printf '%-14s %-12s current\n' "$tool" "$pinned"
  else
    printf '%-14s %-12s OUTDATED (latest: %s)\n' "$tool" "$pinned" "$latest"
    # surfaces in the workflow run's summary and annotations when run by CI
    [ -n "${GITHUB_ACTIONS:-}" ] &&
      printf '::warning title=%s outdated::pinned %s, latest %s - bump it in %s\n' \
        "$tool" "$pinned" "$latest" "$file"
    bad=$((bad + 1))
  fi
done < <(printf '%s\n' "$_ROSTER")

exit "$bad"
