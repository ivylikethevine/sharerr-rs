#!/usr/bin/env bash
# Print every uncovered source line, with context, from the profile the last
# `scripts/check.sh coverage-run` left behind. Same ignore regex as the
# script's report commands, so the files match the floor's.
#
#   scratch/uncovered.sh                     # every file, worst first
#   scratch/uncovered.sh sync/mod.rs auth    # files whose path matches any argument
#   CONTEXT=5 scratch/uncovered.sh gossip    # lines of context around each gap
#
# Output goes to stdout; redirect under local/ to keep it.
set -euo pipefail

root=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
ignore='(^|/)(crates/sharerr-testkit/|crates/sharerr/src/test_support\.rs$)'
context=${CONTEXT:-2}

listing=$(cargo llvm-cov report --summary-only --show-missing-lines \
  --manifest-path "$root/Cargo.toml" --ignore-filename-regex "$ignore" 2>/dev/null \
  | sed -n '/^Uncovered Lines:/,$p' | tail -n +2)

# Whether $file matches any argument as a substring; no arguments match all.
matches() {
  [ $# -eq 0 ] && return 0
  for pat in "$@"; do
    [[ $file == *"$pat"* ]] && return 0
  done
  return 1
}

while IFS= read -r line; do
  [ -n "$line" ] || continue
  file=${line%%: *}
  ranges=${line#*: }
  matches "$@" || continue
  rel=${file#"$root/"}
  echo "=== $rel ($ranges)"
  IFS=',' read -ra parts <<<"$ranges"
  for part in "${parts[@]}"; do
    part=${part// /}
    start=${part%-*}
    end=${part#*-}
    from=$((start - context)); [ "$from" -lt 1 ] && from=1
    to=$((end + context))
    awk -v s="$start" -v e="$end" -v f="$from" -v t="$to" \
      'NR>=f && NR<=t { mark = (NR>=s && NR<=e) ? ">>" : "  "; printf "%s %5d  %s\n", mark, NR, $0 } NR>t { exit }' "$file"
    echo
  done
done <<<"$listing"
