#!/usr/bin/env bash
#
# Screenshot every page `sharerr preview` serves — fully populated with
# invented data, no vault/database/sharerr.toml needed — and save each as a
# WebP under docs/screenshots/, so a README or docs page can show real UI
# and refreshing the images after a layout change is one command instead of
# a manual screenshot-and-crop pass.
#
#   ./screenshot_pages.sh                 headless Chrome, 1440px wide
#   ./screenshot_pages.sh --width 1920    a different width
#   ./screenshot_pages.sh --out some/dir  a different output directory
#
# Requires google-chrome-stable (or CHROME=/path/to/chrome) and ImageMagick's
# `magick`. Full-page: each page is rendered in a window far taller than any
# real content, then cropped to the actual content with `magick -trim` —
# headless Chrome's own --screenshot only captures the given window size, and
# guessing a per-page height in advance would be more fragile than trimming.
#
# Safe to re-run: every screenshot is independent, and the trap kills the
# preview server and removes the scratch directory however the script exits.

set -euo pipefail

# `readlink -f` rather than a bare `dirname $0`: everything below is relative
# to the repo root, so an invocation through a symlink would otherwise land
# in the symlink's directory and fail on paths that look fine in the source.
cd "$(dirname "$(readlink -f "$0")")"

WIDTH=1440
OUT_DIR="docs/screenshots"
CHROME="${CHROME:-google-chrome-stable}"
BIND="127.0.0.1:4877"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --width) WIDTH="$2"; shift 2 ;;
        --out) OUT_DIR="$2"; shift 2 ;;
        *)
            echo "usage: $0 [--width PX] [--out DIR]" >&2
            exit 2
            ;;
    esac
done

command -v "$CHROME" >/dev/null || {
    echo "error: $CHROME not found — set CHROME=/path/to/chrome-or-chromium" >&2
    exit 1
}
command -v magick >/dev/null || {
    echo "error: ImageMagick's magick not found (needed for WebP conversion and the full-page trim)" >&2
    exit 1
}

echo "building sharerr..."
cargo build --quiet -p sharerr

WORKDIR="$(mktemp -d)"
SERVER_LOG="$WORKDIR/preview.log"
./target/debug/sharerr preview --bind "$BIND" >"$SERVER_LOG" 2>&1 &
SERVER_PID=$!
trap 'kill "$SERVER_PID" 2>/dev/null || true; rm -rf "$WORKDIR"' EXIT

echo "waiting for the preview server on $BIND..."
ready=0
for _ in $(seq 1 50); do
    if curl -sf "http://$BIND/" >/dev/null 2>&1; then
        ready=1
        break
    fi
    sleep 0.1
done
if ((! ready)); then
    echo "error: sharerr preview never came up — see $SERVER_LOG" >&2
    cat "$SERVER_LOG" >&2
    exit 1
fi

mkdir -p "$OUT_DIR"

# name:route, in the order `sharerr preview` itself lists them.
PAGES=(
    "status:/"
    "settings:/settings"
    "peers:/peers"
    "items:/items"
    "topology:/topology"
    "topology-networking:/topology?view=networking"
    "debug:/debug"
)

for entry in "${PAGES[@]}"; do
    name="${entry%%:*}"
    route="${entry#*:}"
    png="$WORKDIR/$name.png"
    echo "shooting $name ($route)..."
    "$CHROME" --headless=new --disable-gpu --no-sandbox --hide-scrollbars \
        --window-size="${WIDTH},15000" --screenshot="$png" \
        "http://$BIND$route" >/dev/null 2>&1
    magick "$png" -fuzz 2% -trim +repage "$OUT_DIR/$name.webp"
done

echo "done — screenshots in $OUT_DIR/"
