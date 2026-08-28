#!/usr/bin/env bash
#
# Screenshot every page `sharerr preview` serves — fully populated with
# invented data, no vault/database/sharerr.toml needed — and save each as a
# WebP under docs/screenshots/, so a README or docs page can show real UI
# and refreshing the images after a layout change is one command instead of
# a manual screenshot-and-crop pass.
#
#   ./scripts/screenshot_pages.sh                 headless Chrome, 1920px wide
#   ./scripts/screenshot_pages.sh --width 1440    a different width
#   ./scripts/screenshot_pages.sh --out some/dir  a different output directory
#
# Requires google-chrome-stable (or CHROME=/path/to/chrome) and ImageMagick's
# `magick`. Full-page: each page is rendered in a window far taller than any
# real content, then cropped to the actual content with `magick -trim` —
# headless Chrome's own --screenshot only captures the given window size, and
# guessing a per-page height in advance would be more fragile than trimming.
#
# A page can also declare a split (`name:route:N`), which cuts its full-page
# render into N images. Settings needs it: at 1920px it is one 11,000px strip
# that no README can show. The obvious alternative — shoot each section at its
# own `#anchor` — does not survive headless capture. The fragment scroll is
# correct in a real browser, but the screenshot fires before the scrolled frame
# is drawn and lands back at the top; --virtual-time-budget,
# --run-all-compositor-stages-before-draw and --force-prefers-reduced-motion
# were all tried, and the last one renders a blank page. Cutting the full-page
# render has no timing to lose.
#
# Cuts snap to a row of pure page background so they land in the gap between
# two sections instead of through a panel: the render is averaged to one pixel
# per row, and the nearest all-background row to each even division wins.
#
# Safe to re-run: every screenshot is independent, and the trap kills the
# preview server and removes the scratch directory however the script exits.

set -euo pipefail

# `readlink -f` rather than a bare `dirname $0`: everything below is relative
# to the repo root, one level up from this script's own `scripts/` directory,
# so an invocation through a symlink would otherwise land somewhere else
# entirely and fail on paths that look fine in the source.
cd "$(dirname "$(readlink -f "$0")")/.."

WIDTH=1920
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

# The colour of `--bg` in the dark theme the screenshots are taken in. A row
# of the render averaging to exactly this is a gap between two blocks, which
# is what makes it a safe place to cut.
PAGE_BG="131417"

# Cuts $1 (a trimmed full-page render) into $3 images named "$2-1..N.webp",
# snapping each boundary into the gap between two sections.
#
# Not simply the nearest background row: the gap between a heading and the
# panel it labels is background too, and cutting there orphans the heading at
# the foot of one image from its form at the head of the next. The gap above a
# section heading is the tallest run of background on the page, so the widest
# run near the target wins and the cut lands in its middle.
SNAP=900
split_render() {
    local src="$1" name="$2" parts="$3"
    local height
    height="$(magick identify -format '%h' "$src")"

    # One pixel per row: each is the average colour of that row of the page.
    magick "$src" -resize "1x${height}!" txt:- \
        | tail -n +2 \
        | sed 's/.*#\([0-9A-F]\{6\}\).*/\1/' >"$WORKDIR/rows.txt"

    local cuts
    cuts="$(awk -v n="$parts" -v h="$height" -v bg="$PAGE_BG" -v snap="$SNAP" '
        { row[NR - 1] = $0 }
        END {
            print 0
            for (i = 1; i < n; i++) {
                target = int(h * i / n)
                lo = target - snap; if (lo < 1) lo = 1
                hi = target + snap; if (hi > h - 1) hi = h - 1
                best = target; bestrun = 0; start = -1
                for (y = lo; y <= hi + 1; y++) {
                    if (y <= hi && row[y] == bg) {
                        if (start < 0) start = y
                        continue
                    }
                    if (start >= 0) {
                        run = y - start
                        # Ties go to the run nearest the even division, so the
                        # images stay close to equal height.
                        mid = int((start + y) / 2)
                        if (run > bestrun ||
                            (run == bestrun && (mid - target) ^ 2 < (best - target) ^ 2)) {
                            bestrun = run; best = mid
                        }
                        start = -1
                    }
                }
                print best
            }
            print h
        }' "$WORKDIR/rows.txt")"

    local prev="" i=0
    for cut in $cuts; do
        if [[ -n "$prev" ]]; then
            i=$((i + 1))
            magick "$src" -crop "0x$((cut - prev))+0+${prev}" +repage \
                "$OUT_DIR/${name}-${i}.webp"
        fi
        prev="$cut"
    done
    echo "  split into $i images"
}

# name:route[:parts], in the order `sharerr preview` itself lists them.
# A third field cuts that page's render into that many images.
PAGES=(
    "status:/"
    "settings:/settings:4"
    "peers:/peers"
    "items:/items"
    "topology:/topology"
    "topology-networking:/topology?view=networking"
    "debug:/debug"
)

for entry in "${PAGES[@]}"; do
    name="${entry%%:*}"
    rest="${entry#*:}"
    # Only a trailing all-digits field is a split count; a route may itself
    # contain a colon, and "/topology?view=networking" must not lose its tail.
    parts=""
    if [[ "$rest" == *:* && "${rest##*:}" =~ ^[0-9]+$ ]]; then
        parts="${rest##*:}"
        rest="${rest%:*}"
    fi
    route="$rest"

    png="$WORKDIR/$name.png"
    trimmed="$WORKDIR/$name-trim.png"
    echo "shooting $name ($route)..."
    "$CHROME" --headless=new --disable-gpu --no-sandbox --hide-scrollbars \
        --window-size="${WIDTH},15000" --screenshot="$png" \
        "http://$BIND$route" >/dev/null 2>&1
    magick "$png" -fuzz 2% -trim +repage "$trimmed"

    if [[ -n "$parts" ]]; then
        split_render "$trimmed" "$name" "$parts"
    else
        magick "$trimmed" "$OUT_DIR/$name.webp"
    fi
done

echo "done — screenshots in $OUT_DIR/"
