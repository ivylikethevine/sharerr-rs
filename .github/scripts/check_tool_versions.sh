#!/bin/bash
# The gap .github/dependabot.yml documents: dependabot moves the SHA-pinned
# `uses:` in every workflow, but it has no ecosystem for a tool CI installs
# from a release asset by curl.
#
# This prints each pinned version next to the upstream's latest release and
# exits with the number that differ. The tool-versions workflow runs it on a
# schedule; it runs standalone from a checkout too:
#
#   .github/scripts/check_tool_versions.sh
#
# The roster is ../actions/setup-tool/tools.txt, read here through
# ../actions/setup-tool/lib.sh rather than copied, so a tool cannot be pinned
# and go unchecked. This used to extract each pin by regex straight out of the
# `run:` line that installed it — ci.yml no longer has one of those for
# zizmor/actionlint/cargo-llvm-cov/lychee, now that ./setup-tool installs them
# from a cached, versioned pin instead of a fresh `pip`/`go install`/`cargo
# install` every run. The roster moved with it: tools.txt is now the one place
# a pin lives, and this script and setup-tool both read it rather than either
# copying the other.
#
# Deliberately absent from tools.txt, and so from here: hadolint and trivy.
# Both are installed from `releases/latest` on purpose (ci.yml and
# image-scan.yml each say why), so there is no pin to drift.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."

# SC1091: see install.sh's identical comment - this repo's shellcheck
# invocation runs without -x, so a bare `source=` directive doesn't resolve
# it.
# shellcheck disable=SC1091
source .github/actions/setup-tool/lib.sh

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
while IFS='|' read -r tool pinned _ _ _ check; do
  [ -n "$tool" ] || continue

  # "-" opts a row out of the drift report - lychee's tag scheme
  # ("lychee-v<version>", not "v<version>") does not fit the comparison below,
  # and tools.txt says why rather than this script special-casing one row.
  if [ "$check" = "-" ]; then
    printf '%-16s %-12s (not drift-checked, see tools.txt)\n' "$tool" "$pinned"
    continue
  fi

  case "$check" in
  github:*) repo="${check#github:}" ;;
  *)
    printf '%-16s %-12s ERROR (unknown check kind: %s)\n' "$tool" "-" "$check"
    bad=$((bad + 1))
    continue
    ;;
  esac

  latest="$(_latest "$repo")"
  # A missing upstream answer is a rate limit or an outage, not a stale pin;
  # counting it as outdated would open an issue about GitHub being slow.
  if [ -z "$latest" ]; then
    printf '%-16s %-12s (could not read the upstream release)\n' "$tool" "$pinned"
    continue
  fi

  # a leading v is stripped from both sides, so it does not matter which
  # convention the pin or the upstream uses
  if [ "${pinned#v}" = "${latest#v}" ]; then
    printf '%-16s %-12s current\n' "$tool" "$pinned"
  else
    printf '%-16s %-12s OUTDATED (latest: %s)\n' "$tool" "$pinned" "$latest"
    # surfaces in the workflow run's summary and annotations when run by CI
    [ -n "${GITHUB_ACTIONS:-}" ] &&
      printf '::warning title=%s outdated::pinned %s, latest %s - bump it in .github/actions/setup-tool/tools.txt\n' \
        "$tool" "$pinned" "$latest"
    bad=$((bad + 1))
  fi
done < <(_sr_tool_rows)

exit "$bad"
