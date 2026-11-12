#!/usr/bin/env bash
# Diffs the HTML two sharerr binaries render through `sharerr preview`, with
# whitespace normalised and tags split per line, so a template refactor can be
# shown to change nothing a browser sees.
#
# Usage: scripts/scratch/preview_html_diff.sh BASE_BINARY NEW_BINARY
# Output: local/preview-diff/{base,new}/<page>.html and a diff on stdout.
set -u

base_bin=$1
new_bin=$2
out=local/preview-diff
port=4877
pages=("/" "/settings" "/peers" "/items" "/topology" "/debug")

dump() {
  local binary=$1 dest=$2
  mkdir -p "$dest"
  "$binary" preview --bind "127.0.0.1:$port" >/dev/null 2>&1 &
  local pid=$!
  for _ in $(seq 1 50); do
    curl -fs -o /dev/null "http://127.0.0.1:$port/" && break
    sleep 0.1
  done
  for page in "${pages[@]}"; do
    local name=${page#/}
    curl -fs "http://127.0.0.1:$port$page" |
      tr -s ' \n\t' ' ' |
      sed -e 's/> *</>\n</g' -e 's/ >/>/g' \
        >"$dest/${name:-status}.html"
  done
  kill "$pid"
  wait "$pid" 2>/dev/null
}

dump "$base_bin" "$out/base"
dump "$new_bin" "$out/new"
diff -ru "$out/base" "$out/new"
