#!/usr/bin/env bash
# Installs whatever ./resolve.sh's cache step missed, one tool at a time via
# ../setup-tool/install.sh. In a real file for the same shellcheck reason
# ./resolve.sh and ../setup-tool/install.sh are.
#
# Never sets SR_TOOL_VERSION: every call here installs the tools.txt pin, so
# install.sh's sha256 check stays on its normal pin-verifying path rather
# than the unverified override path a `version:` input would take.
set -euo pipefail

: "${SR_TOOLS:?set by action.yml}"

for _sr_t in $SR_TOOLS; do
  [ -x "/usr/local/bin/$_sr_t" ] && continue
  SR_TOOL="$_sr_t" "$(dirname "${BASH_SOURCE[0]}")/../setup-tool/install.sh" install
done
