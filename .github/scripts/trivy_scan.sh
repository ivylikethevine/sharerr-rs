#!/usr/bin/env bash
# One scan of a pushed image for fixable HIGH/CRITICAL vulnerabilities, shared by the release gate and
# the scheduled scan of the published image; lib.sh holds the flags, so a sparse checkout lists it too.
# One JSON scan, converted to the table and the SARIF so they can't disagree: <out-dir>/trivy.json,
# trivy.txt (also printed) and trivy.sarif.
# The accepted-risk list is trivy's own default, .trivyignore in the working directory, unless the
# caller points TRIVY_IGNOREFILE elsewhere.
#
# trivy exits 1 on a finding and on a failed scan alike; only a scan that ran writes the report, so a
# scan that didn't run fails this script. A finding doesn't: it writes `found=<trivy's exit>` (0: clean)
# to $GITHUB_OUTPUT (stdout outside Actions), so the caller can upload the SARIF before blocking or
# warning on it.
#
# Usage: .github/scripts/trivy_scan.sh <image-ref> <out-dir>
set -uo pipefail

image="${1:?usage: $0 <image-ref> <out-dir>}"
out="${2:?usage: $0 <image-ref> <out-dir>}"

# shellcheck source=./lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

rc=0
trivy image "${_CI_TRIVY_FLAGS[@]}" --exit-code 1 --output "$out/trivy.json" "$image" || rc=$?
if ! jq -e .SchemaVersion "$out/trivy.json" >/dev/null 2>&1; then
  echo "::error title=trivy::the scan of $image did not run (exit $rc)"
  exit 1
fi
set -e
trivy convert --format table --table-mode detailed --output "$out/trivy.txt" "$out/trivy.json"
trivy convert --format sarif --output "$out/trivy.sarif" "$out/trivy.json"
cat "$out/trivy.txt"
echo "found=$rc" >>"${GITHUB_OUTPUT:-/dev/stdout}"
