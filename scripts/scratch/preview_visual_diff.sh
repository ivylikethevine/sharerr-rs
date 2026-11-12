#!/usr/bin/env bash
# Screenshots every `sharerr preview` page from two binaries at desktop and phone
# width, in both themes' system default, and prints how many pixels differ, so a
# stylesheet refactor can be shown to change nothing visible.
#
# Usage: scripts/scratch/preview_visual_diff.sh BASE_BINARY NEW_BINARY
# Needs headless Chrome or Chromium and ImageMagick; Chrome's singleton socket
# means this cannot run inside a sandbox that denies unix sockets.
# Output: local/preview-diff/shots/{base,new}/<page>-<width>.png.
set -u

base_bin=$1
new_bin=$2
out=local/preview-diff/shots
port=4877
pages=("/" "/settings" "/peers" "/items" "/topology" "/debug")
widths=(1280 400)
chrome=$(command -v google-chrome-stable || command -v chromium)
profile=$(mktemp -d "${TMPDIR:-/tmp}/preview-chrome.XXXXXX")

shoot() {
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
    name=${name:-status}
    for width in "${widths[@]}"; do
      "$chrome" --headless=new --disable-gpu --hide-scrollbars \
        --user-data-dir="$profile" \
        --window-size="$width,15000" \
        --screenshot="$PWD/$dest/$name-$width.png" \
        "http://127.0.0.1:$port$page" >/dev/null 2>&1
    done
  done
  kill "$pid"
  wait "$pid" 2>/dev/null
}

shoot "$base_bin" "$out/base"
shoot "$new_bin" "$out/new"

for shot in "$out"/base/*.png; do
  name=$(basename "$shot")
  printf '%-24s ' "$name"
  magick compare -metric AE "$shot" "$out/new/$name" null: 2>&1
  echo
done
