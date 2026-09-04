#!/bin/bash
# The severity half of digest-pinning, and the gate dependabot cannot offer.
#
# Every Dockerfile and compose file in this repo pins its base image by digest,
# and dependabot opens the repin PR on release. Neither of those knows whether
# the bump *fixes* anything: dependabot's docker ecosystem has no
# security-update mode at all (GHSA has no container ecosystem, so there are no
# alerts to drive one), and a digest is just a digest. This script supplies the
# missing question — would repinning reduce the number of fixable
# CRITICAL/HIGH findings? — and answers it by measuring both sides rather than
# assuming.
#
# Per pinned image:
#
#   1. scan the pinned digest              -> A, the fixable CVE ids today
#   2. A empty                             -> clean, say nothing
#   3. scan the tag                        -> B, and the digest it resolves to
#   4. that digest == the pinned one       -> upstream has not rebuilt yet
#   5. |B| >= |A|                          -> rebuilt, but the hole survives
#   6. |B| < |A|                           -> ACTIONABLE: the repin closes A-B
#
# Only 6 is worth waking anyone for, which is the point: the job speaks when a
# bump is known to help, and stays quiet when it is churn. A seventh outcome
# sits before 2 — trivy identifying an OS it has no data for — and is kept
# distinct from "clean" for the reason the code that detects it gives.
#
# --ignore-unfixed is load-bearing, not a flag. debian:bookworm-slim alone
# reports a double-digit pile of permanent won't-fix findings; a job that counts
# those is red forever and teaches everyone to skip it. It is also what makes
# the comparison above mean anything: two piles of permanent won't-fix findings
# differ by noise rather than by severity.
#
# This is separate from the `trivy` job in image-scan.yml, which scans the
# image *this repo publishes* rather than the images it builds on. That one asks
# "has what we shipped gone stale"; this one asks "is there a better base to
# build the next one from".
#
# Writes a markdown report (the workflow uses it for both the step summary and
# the issue body) and exits:
#
#   0  clean - nothing pinned has a fixable CRITICAL/HIGH finding
#   1  a finding exists, but no repin would improve on it
#   2  at least one repin is verified to reduce the count
#
# Runs standalone from a checkout, given trivy and jq on PATH:
#
#   .github/scripts/scan_pinned_images.sh
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."

# comm below diffs two id lists that jq sorted by codepoint; a locale whose
# collation ignores the hyphen in a CVE id makes comm call them unsorted and
# quietly emit the wrong diff. It also keeps the `sort -u` over the pins stable.
export LC_ALL=C

_HI_SEVERITY="HIGH,CRITICAL"
# the same set as prose - trivy wants it comma-separated, a sentence does not
_HI_SEVERITIES="$(echo "$_HI_SEVERITY" | tr ',' '/')"

_HI_REPORT="${SHARERR_SCAN_REPORT:-${RUNNER_TEMP:-${TMPDIR:-/tmp}}/image-scan-report.md}"

# How many images to scan concurrently. Each one is two independent `trivy
# image` invocations (network + a local vuln DB lookup), so this is
# I/O-and-network bound rather than CPU bound - 4 is comfortably under a
# hosted runner's core count without hammering a registry.
_HI_JOBS="${SHARERR_SCAN_JOBS:-4}"

for _hi_need in trivy jq; do
  command -v "$_hi_need" >/dev/null 2>&1 || {
    echo "scan_pinned_images: no $_hi_need on PATH" >&2
    exit 127
  }
done

_HI_WORK="$(mktemp -d)"
trap 'rm -rf "$_HI_WORK"' EXIT

# _hi_trivy <ref> <out.json> - one scan, as JSON so the ids can be diffed
# rather than merely counted
function _hi_trivy() {
  trivy image \
    --scanners vuln \
    --severity "$_HI_SEVERITY" \
    --ignore-unfixed \
    --format json \
    --no-progress \
    --output "$2" \
    "$1" </dev/null
}

# _hi_ids <out.json> - the fixable CVE ids, sorted and unique so comm can diff
function _hi_ids() {
  jq -r '[.Results[]?.Vulnerabilities[]?.VulnerabilityID] | unique | .[]' "$1"
}

# _hi_digest <out.json> - the digest trivy resolved the reference to. Comes off
# the scan we already needed, so resolving a tag costs no second tool (no
# crane/skopeo, nothing new to install).
function _hi_digest() {
  jq -r '.Metadata.RepoDigests[0] // ""' "$1" | sed 's/.*@//'
}

# _hi_lines <file> - a plain integer, without the padding macOS wc emits
function _hi_lines() {
  echo $(($(wc -l <"$1")))
}

# The pins, read out of the files rather than repeated here, so an image added,
# dropped or repinned is covered without anyone remembering this script. Two
# shapes carry a pin: a Dockerfile `FROM` (with an optional --platform= before
# the reference) and a compose `image:`. Naming a tag literally anywhere in
# this file would create a second place to edit on every bump.
#
# One Dockerfile, not two: it used to build both docker/Dockerfile and
# docker/Dockerfile.lighthouse, before the two merged into one file with two
# runtime targets (see docker/Dockerfile's own header). A `FROM <stage> AS`
# internal reference carries neither tag nor digest and drops out on its own,
# so the multi-stage file's several `FROM` lines still resolve to just the two
# real base-image pins.
_hi_pins="$_HI_WORK/pins"
{
  sed -n 's/^FROM \(--platform=[^ ]* \)\?\([^:@ ]*\):\([^@ ]*\)@\(sha256:[0-9a-f]*\).*/\2|\3|\4/p' \
    docker/Dockerfile 2>/dev/null
  find docker \( -name '*.yml' -o -name '*.yaml' \) -exec \
    sed -n 's/^[[:space:]]*image:[[:space:]]*\([^:@ ]*\):\([^@ ]*\)@\(sha256:[0-9a-f]*\).*/\1|\2|\3/p' {} +
} | sort -u >"$_hi_pins"

_hi_total="$(_hi_lines "$_hi_pins")"
if [ "$_hi_total" -eq 0 ]; then
  echo "scan_pinned_images: no digest-pinned images found - is the checkout complete?" >&2
  exit 127
fi
echo "scanning $_hi_total pinned base image(s), $_HI_JOBS at a time:"
sed 's/^/  /' "$_hi_pins"
echo

# Indexed so each worker below gets its own scratch directory (no filename
# collisions from a `/`-bearing image ref) and so the console log and the
# report can both be replayed back in the same, stable order the pins were
# read in, no matter which worker happened to finish first.
_hi_indexed="$_HI_WORK/pins.indexed"
nl -ba -w1 -s'|' "$_hi_pins" >"$_hi_indexed"

# _hi_scan_one <idx> <image> <tag> <pinned> - one image, start to finish,
# entirely self-contained: everything it learns goes into files under its own
# $_HI_WORK/w<idx>/ rather than a shared variable, because this runs as a
# background job and nothing it sets in its own subshell is visible to the
# parent once it exits. The console narration (the numbered comments below,
# unchanged from the sequential version) is buffered into w<idx>/log instead
# of printed directly, so N of these running at once do not interleave their
# ::group:: blocks into unreadable output - the parent replays every log in
# pin order after the whole batch finishes.
#
# Writes w<idx>/verdict: three space-separated 0/1 flags, "<hit> <win>
# <unscanned>" - the same three counters the sequential version accumulated
# directly, now read back by the parent instead. An image this function
# cannot scan at all (step 1 failing outright) writes no verdict file, which
# the parent treats as "0 0 0": silently excluded from every count, exactly
# as the sequential version's own `continue` there did.
function _hi_scan_one() {
  local idx="$1" image="$2" tag="$3" pinned="$4"
  local ref="$image:$tag"
  local wd="$_HI_WORK/w$idx"
  mkdir -p "$wd"
  local log="$wd/log"

  {
    echo "::group::$ref"

    # 1. what the pin has today
    if ! _hi_trivy "$image@$pinned" "$wd/a.json"; then
      echo "  could not scan the pinned digest - skipping" >&2
      echo "::endgroup::"
      exit 0
    fi
    # trivy has no vulnerability source for every distro it can identify, and
    # that report comes back with a null .Results rather than an empty finding
    # list. Zero findings and zero data are not the same answer, and reporting
    # the second as the first is how an unscanned image sits inside a green job
    # forever.
    if [ "$(jq -r '.Results | type' "$wd/a.json")" = null ]; then
      echo "  not scanned: trivy has no vulnerability data for this image"
      # shellcheck disable=SC2016
      printf -- '- `%s` - trivy has no vulnerability data for this OS, so its zero means "not scanned", not "clean".\n' \
        "$ref" >"$wd/stale.md"
      echo "0 0 1" >"$wd/verdict"
      echo "::endgroup::"
      exit 0
    fi

    _hi_ids "$wd/a.json" >"$wd/a.ids"
    a_n="$(_hi_lines "$wd/a.ids")"

    # 2. the quiet path, and the one this stays on almost every week
    if [ "$a_n" -eq 0 ]; then
      echo "  clean: no fixable $_HI_SEVERITIES findings in the pinned digest"
      echo "::endgroup::"
      exit 0
    fi
    echo "  pinned digest has $a_n fixable finding(s)"

    # 3. what the tag points at now. Scanned rather than merely resolved because
    # the resolve is free either way and the counts are the whole question.
    if ! _hi_trivy "$ref" "$wd/b.json"; then
      echo "  could not scan the current tag - reporting the pin as-is" >&2
      # shellcheck disable=SC2016
      printf -- '- `%s` - %d fixable finding(s); the current tag could not be scanned\n' \
        "$ref" "$a_n" >"$wd/stale.md"
      echo "1 0 0" >"$wd/verdict"
      echo "::endgroup::"
      exit 0
    fi
    _hi_ids "$wd/b.json" >"$wd/b.ids"
    b_n="$(_hi_lines "$wd/b.ids")"
    candidate="$(_hi_digest "$wd/b.json")"

    # 4. upstream has not rebuilt - there is nothing to repin to
    if [ -n "$candidate" ] && [ "$candidate" = "$pinned" ]; then
      echo "  the tag still resolves to the pinned digest - no rebuild to take"
      # shellcheck disable=SC2016
      printf -- '- `%s` - %d fixable finding(s), and the tag still resolves to the pinned digest. Nothing to repin to yet.\n' \
        "$ref" "$a_n" >"$wd/stale.md"
      echo "1 0 0" >"$wd/verdict"
      echo "::endgroup::"
      exit 0
    fi

    # 5. rebuilt, but no better - repinning would be churn, so do not ask for it
    if [ "$b_n" -ge "$a_n" ]; then
      echo "  the tag has been rebuilt but still has $b_n finding(s) - no gain"
      # shellcheck disable=SC2016
      printf -- '- `%s` - %d fixable finding(s); the current tag has %d, so a repin would not reduce the count.\n' \
        "$ref" "$a_n" "$b_n" >"$wd/stale.md"
      echo "1 0 0" >"$wd/verdict"
      echo "::endgroup::"
      exit 0
    fi

    # 6. the one case worth acting on
    closed="$(comm -23 "$wd/a.ids" "$wd/b.ids" | tr '\n' ' ')"
    echo "  ACTIONABLE: repinning drops $a_n finding(s) to $b_n"
    # Every hit below is a markdown code span in report text, which is a
    # backtick shellcheck reads as a command substitution and no expansion
    # anyone wants.
    # shellcheck disable=SC2016
    {
      printf -- '#### `%s`: %d fixable finding(s) -> %d\n\n' "$ref" "$a_n" "$b_n"
      printf -- 'Closed by the repin: %s\n\n' "$(echo "$closed" | sed 's/ *$//;s/ /, /g')"
      printf -- '```diff\n'
      printf -- '-%s@%s\n' "$ref" "$pinned"
      printf -- '+%s@%s\n' "$ref" "${candidate:-<resolve the tag>}"
      printf -- '```\n\n'
      printf -- 'Pinned in:\n\n'
      grep -rl "@$pinned" docker 2>/dev/null | sed 's|^|- `|;s|$|`|' || true
      printf -- '\n'
    } >"$wd/actionable.md"
    echo "1 1 0" >"$wd/verdict"
    echo "::endgroup::"
  } >"$log" 2>&1
}

# A hand-rolled job pool rather than `xargs -P`: the worker is a shell
# function with locals and early `exit`s (clean under `set -e` inside its own
# subshell), and exporting it across an xargs-spawned `bash -c` would mean
# re-quoting every one of those `printf` backtick spans through a second
# layer of shell. `wait -n` (bash 4.3+, present on every runner this targets)
# blocks for the next background job to finish rather than all of them, which
# is what keeps exactly $_HI_JOBS in flight instead of launching all of them
# at once or waiting for a full batch between rounds.
_hi_running=0
while IFS='|' read -r idx image tag pinned; do
  [ -n "$image" ] || continue
  if [ "$_hi_running" -ge "$_HI_JOBS" ]; then
    wait -n
    _hi_running=$((_hi_running - 1))
  fi
  _hi_scan_one "$idx" "$image" "$tag" "$pinned" &
  _hi_running=$((_hi_running + 1))
done <"$_hi_indexed"
wait

_hi_actionable="$_HI_WORK/actionable.md"
_hi_stale="$_HI_WORK/stale.md"
: >"$_hi_actionable"
: >"$_hi_stale"

_hi_hits=0
_hi_wins=0
_hi_unscanned=0
# Replayed in pin order, not finish order - a worker that scans a slow image
# first and a worker that scans a fast one last must not reorder either the
# console log or the report relative to the sequential version.
while IFS='|' read -r idx image tag pinned; do
  [ -n "$image" ] || continue
  wd="$_HI_WORK/w$idx"

  [ -f "$wd/log" ] && cat "$wd/log"

  if [ -f "$wd/verdict" ]; then
    read -r hit win unscanned <"$wd/verdict"
    _hi_hits=$((_hi_hits + hit))
    _hi_wins=$((_hi_wins + win))
    _hi_unscanned=$((_hi_unscanned + unscanned))
  fi
  [ -f "$wd/actionable.md" ] && cat "$wd/actionable.md" >>"$_hi_actionable"
  [ -f "$wd/stale.md" ] && cat "$wd/stale.md" >>"$_hi_stale"
done <"$_hi_indexed"

# The report is one file for both consumers: the workflow feeds it to
# $GITHUB_STEP_SUMMARY on every path, and to the issue body when it is
# actionable, so the two can never drift into saying different things.
{
  if [ "$_hi_wins" -gt 0 ]; then
    echo "### A pinned base image can be repinned to close real vulnerabilities"
    echo
    echo "trivy scanned each pinned digest and the image its tag points at now."
    echo "The repins below are **verified** to reduce the fixable"
    echo "$_HI_SEVERITIES count - dependabot's open bump PR for each is the fix."
    echo
    cat "$_hi_actionable"
  elif [ "$_hi_hits" -gt 0 ]; then
    echo "### A pinned base image has a fixable finding, but no repin helps yet"
    echo
    echo "No action: repinning would not reduce the count. Left here so the"
    echo "finding is visible without asking anyone to merge a bump that fixes"
    echo "nothing."
    echo
  else
    echo "### Image scan: clean"
    echo
    echo "No fixable $_HI_SEVERITIES vulnerabilities in the $((_hi_total - _hi_unscanned)) of"
    echo "$_hi_total pinned base image(s) trivy has vulnerability data for."
    echo
  fi
  if [ -s "$_hi_stale" ]; then
    echo "<details><summary>Findings with no better image to move to</summary>"
    echo
    cat "$_hi_stale"
    echo
    echo "</details>"
  fi
} >"$_HI_REPORT"

echo
echo "report: $_HI_REPORT"

[ "$_hi_wins" -gt 0 ] && exit 2
[ "$_hi_hits" -gt 0 ] && exit 1
exit 0
