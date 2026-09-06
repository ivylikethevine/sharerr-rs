#!/bin/bash
# The gap .github/dependabot.yml documents, widened to cover every pin in the
# tree that dependabot cannot see.
#
# dependabot moves three kinds of pin on its own: the SHA in a `uses:` line
# (github-actions ecosystem), a Cargo dependency, and a compose `image:` (the
# docker-compose ecosystem). What is left over, and what each section below
# checks:
#
#   1. tools.txt        - CI installs zizmor/actionlint/cargo-llvm-cov/lychee/
#                          typos from a cached, versioned release asset by
#                          curl, not `pip install`/`go install`/`cargo
#                          install` on every run - no ecosystem covers that.
#   2. Action SHAs       - dependabot DOES move these... but only once it
#                          notices the repo; this section is the one place
#                          that lists every distinct action+pin actually used
#                          across every workflow and composite action, so a
#                          dependabot PR sitting unmerged for a while shows up
#                          here as drift too, not just as an ecosystem gap.
#   3. Docker base images - `rust:1.98-bookworm` and `debian:bookworm-slim` in
#                          docker/Dockerfile. dependabot's docker ecosystem
#                          does move these (a digest refresh lands even with
#                          `rust`'s semver bumps ignored, per dependabot.yml),
#                          same reasoning as section 2: this is what lets a
#                          slow-to-merge bump show up as drift in the
#                          meantime, and it is the one place that reads
#                          straight from docker/Dockerfile rather than
#                          trusting dependabot noticed.
#
# This prints each pinned version next to the upstream's latest release and
# exits with the number that differ. The tool-versions workflow runs it on a
# schedule; it runs standalone from a checkout too, given curl and jq on PATH:
#
#   .github/scripts/check_tool_versions.sh
#
# tools.txt's roster is read through ../actions/setup-tool/lib.sh rather than
# copied, so a tool cannot be pinned there and go unchecked here. Sections 2
# and 3 read straight out of the workflow/Dockerfile files for the identical
# reason - one place to edit a pin, one place that notices it drifted.
#
# Deliberately absent everywhere below: hadolint and trivy. Both are installed
# from `releases/latest` on purpose (ci.yml and image-scan.yml each say why),
# so there is no pin to drift.
#
# Timing follows .github/dependabot.yml. A release (or a re-pushed Docker Hub
# tag) younger than COOLDOWN_DAYS is reported as pending, not as drift:
# dependabot's `cooldown: default-days: 7` exists so a compromised release can
# be caught upstream before this repo's CI ever runs it, and it would be odd
# for this script to open an issue asking for the very bump dependabot is
# deliberately still waiting on. TOOL_COOLDOWN_DAYS=0 shows everything
# upstream has published.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."

# SC1091: see install.sh's identical comment - this repo's shellcheck
# invocation runs without -x, so a bare `source=` directive doesn't resolve
# it.
# shellcheck disable=SC1091
source .github/actions/setup-tool/lib.sh

# Mirrors `cooldown: default-days` in .github/dependabot.yml; change both.
COOLDOWN_DAYS="${TOOL_COOLDOWN_DAYS:-7}"

for _ctv_need in curl jq; do
  command -v "$_ctv_need" >/dev/null 2>&1 || {
    echo "check_tool_versions: no $_ctv_need on PATH" >&2
    exit 127
  }
done

# _latest <owner/repo> <tag-prefix> - prints `due|newest|age`. `due` is the
# highest-versioned non-draft, non-prerelease release at least COOLDOWN_DAYS
# old (what dependabot would propose today, and what a pin is measured
# against); `newest` is the highest-versioned such release regardless of age;
# `age` is its age in whole days. All three empty if the API declines.
#
# Ordered by version, not by date, and only over tags shaped
# `<prefix><digits>[.<digits>...]`. Both matter: actions/checkout re-releases
# every old major on the same day (v2.8.0 next to v7.0.1, all published
# within the hour), so "newest by date" is whichever backport landed last;
# and github/codeql-action's `codeql-bundle-vX.Y.Z` CLI bundles share the
# repo with the action's own `vX.Y.Z` series, so they must not compete with
# it. `releases/latest`, which this used to read, orders by created_at and
# gets the first case wrong the same way.
#
# Unauthenticated this is rate-limited to 60/hour per IP; in Actions the
# workflow passes GH_TOKEN, which raises that far above the size of the
# roster.
function _latest() {
  local auth=()
  [ -n "${GH_TOKEN:-}" ] && auth=(-H "Authorization: Bearer $GH_TOKEN")
  curl -sSf "${auth[@]}" "https://api.github.com/repos/$1/releases?per_page=30" 2>/dev/null |
    jq -r --argjson days "$COOLDOWN_DAYS" --arg prefix "$2" '
      [ .[] | select(.draft or .prerelease | not)
            | select(.tag_name | startswith($prefix))
            | (.tag_name | ltrimstr($prefix)) as $v
            | select($v | test("^[0-9]+(\\.[0-9]+)*$"))
            | . + { v: ($v | split(".") | map(tonumber)) } ]
      | sort_by(.v) | reverse
      | (now - $days * 86400) as $cutoff
      | (map(select((.published_at | fromdateiso8601) <= $cutoff)) | first) as $due
      | first as $newest
      | [ ($due.tag_name // ""),
          ($newest.tag_name // ""),
          (if $newest then ((now - ($newest.published_at | fromdateiso8601)) / 86400 | floor | tostring) else "" end)
        ] | join("|")' 2>/dev/null
}

# _pending_note <due> <newest> <age> - the parenthetical for a row whose
# upstream moved inside the cooldown; empty when there is nothing pending.
function _pending_note() {
  if [ -n "$2" ] && [ "$2" != "$1" ]; then
    printf ' (%s released %s day(s) ago, inside the %s-day cooldown dependabot.yml also applies)' \
      "$2" "$3" "$COOLDOWN_DAYS"
  fi
}

# _at_least <pinned> <due> - true when the pinned version is at or above
# `due`, compared as dotted numerics (sort -V), never as strings. The
# comparison is against `due`, not `newest`, because `due` is what dependabot
# would propose today: a pin at or ahead of it is current. That includes a pin
# sitting *between* the two - v2.87.6 pinned while v2.87.2 was the cooled-down
# release and v2.87.7 had shipped that morning - which an equality test read
# as "behind v2.87.2" and opened the tracking issue over a pin that was in
# fact five releases ahead of what it was being measured against.
function _at_least() {
  [ "$(printf '%s\n%s\n' "$2" "$1" | sort -V | tail -n1)" = "$1" ]
}

bad=0

echo "## tools.txt (release-asset installs)"
echo
# Process substitution, not a pipe: a piped `while` runs in a subshell, so
# $bad would be lost and this would always exit 0.
while IFS='|' read -r tool pinned _ _ _ check tag_prefix _; do
  [ -n "$tool" ] || continue

  # "-" opts a row out of the drift report entirely - for a tool whose
  # upstream release feed this script simply cannot read, tools.txt says why.
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

  # The tag prefix is per-row (7th tools.txt column, defaulting to "v") - what
  # lets lychee's own "lychee-v0.24.2" upstream tags compare correctly against
  # a pin that (like every row's) carries no prefix at all.
  prefix="${tag_prefix:-v}"
  IFS='|' read -r latest newest age <<<"$(_latest "$repo" "$prefix")"
  # A missing upstream answer is a rate limit or an outage, not a stale pin;
  # counting it as outdated would open an issue about GitHub being slow.
  if [ -z "$latest" ] && [ -z "$newest" ]; then
    printf '%-16s %-12s (could not read the upstream release)\n' "$tool" "$pinned"
    continue
  fi

  # A pin at or ahead of `due` (someone bumped by hand inside the cooldown,
  # to `newest` or to anything between) is current; an empty `due` with a
  # `newest` means the only release is still inside the cooldown, which is
  # also not drift.
  note=""
  [ "${newest#"$prefix"}" != "$pinned" ] && note="$(_pending_note "$latest" "$newest" "$age")"
  if [ -z "$latest" ] || _at_least "$pinned" "${latest#"$prefix"}"; then
    printf '%-16s %-12s current%s\n' "$tool" "$pinned" "$note"
  else
    printf '%-16s %-12s OUTDATED (latest: %s)%s\n' "$tool" "$pinned" "$latest" "$note"
    # surfaces in the workflow run's summary and annotations when run by CI
    [ -n "${GITHUB_ACTIONS:-}" ] &&
      printf '::warning title=%s outdated::pinned %s, latest %s - bump it in .github/actions/setup-tool/tools.txt\n' \
        "$tool" "$pinned" "$latest"
    bad=$((bad + 1))
  fi
done < <(_sr_tool_rows)

echo
echo "## GitHub Actions (uses: SHAs)"
echo

# Every `uses: <path>@<40-hex-sha> # <comment>` across every workflow and
# composite action, reduced to (repo, pin) and deduplicated: the same action
# can appear a dozen times at the same pin (actions/checkout does), and a
# subpath action (`github/codeql-action/init`, `/analyze`, `/upload-sarif`)
# names three different entry points into one repo, not three things to pin
# separately - `cut -d/ -f1-2` folds all of them down to the repo dependabot
# and this script both actually track releases against, and the dedup runs
# *after* that fold so those three collapse into the one row that matters.
#
# A comment that is not `v<digits>...` names a moving alias, not a version -
# there is nothing to compare it against, so it is skipped rather than
# misreported. Every `uses:` pin in this tree currently carries a `v<semver>`
# comment (dtolnay/rust-toolchain's `# stable` was the one exception, and it
# is gone - see ci.yml's `fmt` job for why), so this guard has nothing to
# filter today; it stays as defence for the next tool pinned by a moving ref.
while IFS='|' read -r repo comment pin; do
  [ -n "$repo" ] || continue

  # Every `uses:` comment in this tree is `v<semver>` (the awk filter below
  # guarantees it), so the prefix is always "v" here. github/codeql-action's
  # `codeql-bundle-*` releases, which used to need a check-by-hand branch at
  # this point, are filtered out inside _latest by shape.
  IFS='|' read -r latest newest age <<<"$(_latest "$repo" v)"
  if [ -z "$latest" ] && [ -z "$newest" ]; then
    printf '%-45s %-10s (could not read the upstream release)\n' "$repo" "$comment"
    continue
  fi

  # Same cooldown rule as the tools.txt section above.
  note=""
  [ "${newest#v}" != "${comment#v}" ] && note="$(_pending_note "$latest" "$newest" "$age")"
  if [ -z "$latest" ] || _at_least "${comment#v}" "${latest#v}"; then
    printf '%-45s %-10s current%s\n' "$repo" "$comment" "$note"
  else
    printf '%-45s %-10s OUTDATED (latest: %s, pinned %s)%s\n' "$repo" "$comment" "$latest" "$pin" "$note"
    [ -n "${GITHUB_ACTIONS:-}" ] &&
      printf '::warning title=%s outdated::pinned %s, latest %s - bump the SHA and the trailing comment everywhere %s is used\n' \
        "$repo" "$comment" "$latest" "$repo"
    bad=$((bad + 1))
  fi
done < <(
  grep -rhoP 'uses:\s*\K[^\s@]+@[0-9a-f]{40}\s*#\s*\S+' .github/ |
    sed -E 's/^([^@]+)@([0-9a-f]{40}) *# *(\S+)$/\1|\2|\3/' |
    awk -F'|' '$3 ~ /^v[0-9]/ {
      n = split($1, parts, "/")
      print parts[1] "/" parts[2] "|" $3 "|" $2
    }' |
    sort -t'|' -k1,1 -u
)

echo
echo "## Docker base images (docker/Dockerfile)"
echo

# The same two shapes scan_pinned_images.sh reads, narrowed to the one file
# whose pin is also this build's MSRV claim - a `FROM` here is a toolchain
# decision, not a CVE surface scan_pinned_images.sh already covers on its own
# schedule. Docker Hub's tag API resolves a bare tag to the manifest-list
# digest it points at *right now*, the same field `scan_pinned_images.sh`
# reads off a `trivy image` scan - this just skips the scan and asks Docker
# Hub directly, since a plain "has this moved" question does not need trivy's
# vulnerability data at all.
while IFS='|' read -r image tag pinned; do
  [ -n "$image" ] || continue
  ref="$image:$tag"

  # Docker Hub's API path for an official image (`rust`, `debian`, ...) is
  # `library/<name>`, not `<name>` - the only two rows this section reads
  # today are both official images, so this is not generalised further.
  case "$image" in
  */*) hub_path="$image" ;;
  *) hub_path="library/$image" ;;
  esac

  # `pinned` below is the bare hex digest (the sed capture drops the
  # `sha256:` prefix, matching scan_pinned_images.sh's own convention) - strip
  # the same prefix off the API's answer so the two are comparable.
  #
  # `age` is how many whole days ago the tag was last pushed (Docker Hub's
  # timestamps carry fractional seconds jq's parser rejects, hence the sub).
  IFS='|' read -r current age <<<"$(curl -sSf "https://hub.docker.com/v2/repositories/$hub_path/tags/$tag" 2>/dev/null |
    jq -r '[ (.digest // "" | sub("^sha256:"; "")),
             (if .tag_last_pushed then ((now - (.tag_last_pushed | sub("\\.[0-9]+"; "") | fromdateiso8601)) / 86400 | floor | tostring) else "" end)
           ] | join("|")' 2>/dev/null)"
  if [ -z "$current" ]; then
    printf '%-45s %-24s (could not read the current tag digest)\n' "$ref" "${pinned:0:19}..."
    continue
  fi

  if [ "$current" = "$pinned" ]; then
    printf '%-45s %-24s current\n' "$ref" "${pinned:0:19}..."
  elif [ -n "$age" ] && [ "$age" -lt "$COOLDOWN_DAYS" ]; then
    # dependabot's docker ecosystem waits out the same cooldown before it
    # proposes the new digest; so does this row.
    printf '%-45s %-24s current (tag re-pushed %s day(s) ago, inside the %s-day cooldown dependabot.yml also applies)\n' \
      "$ref" "${pinned:0:19}..." "$age" "$COOLDOWN_DAYS"
  else
    printf '%-45s %-24s OUTDATED (tag now resolves to %s)\n' "$ref" "${pinned:0:19}..." "$current"
    [ -n "${GITHUB_ACTIONS:-}" ] &&
      printf '::warning title=%s outdated::%s now resolves to %s - repin docker/Dockerfile (scan_pinned_images.sh says whether it fixes anything)\n' \
        "$ref" "$ref" "$current"
    bad=$((bad + 1))
  fi
done < <(
  sed -n 's/^FROM \(--platform=[^ ]* \)\?\([^:@ ]*\):\([^@ ]*\)@sha256:\([0-9a-f]*\).*/\2|\3|\4/p' \
    docker/Dockerfile
)

echo
echo "## MSRV (rust-version claimed in three places)"
echo

# Not a drift-against-upstream check like the three sections above - this one
# compares the repo against itself. `docker/Dockerfile`'s `FROM` used to be the
# only place a moving `dtolnay/rust-toolchain@<SHA>` pin's `toolchain: "1.98"`
# input had a twin to fall out of sync with; now that ci.yml's `msrv` job
# installs "1.98" via a bare `rustup` call instead (see that job's own
# comment for why dtolnay/rust-toolchain doesn't stay pinnable), the same
# claim lives in three places with nothing but this section comparing them.
cargo_msrv="$(grep -oP 'rust-version\s*=\s*"\K[^"]+' Cargo.toml | head -1)"
docker_msrv="$(sed -n 's/^FROM \(--platform=[^ ]* \)\?rust:\([0-9.]*\)-.*/\2/p' docker/Dockerfile | head -1)"
ci_msrv="$(grep -oP 'rustup toolchain install \K[0-9.]+' .github/workflows/ci.yml | head -1)"

if [ -z "$cargo_msrv" ] || [ -z "$docker_msrv" ] || [ -z "$ci_msrv" ]; then
  printf 'Cargo.toml=%s docker/Dockerfile=%s ci.yml(msrv)=%s (could not read one of the three)\n' \
    "${cargo_msrv:-?}" "${docker_msrv:-?}" "${ci_msrv:-?}"
  bad=$((bad + 1))
elif [ "$cargo_msrv" = "$docker_msrv" ] && [ "$cargo_msrv" = "$ci_msrv" ]; then
  printf '%-45s current (%s)\n' "Cargo.toml / Dockerfile / ci.yml" "$cargo_msrv"
else
  printf 'MISMATCH: Cargo.toml=%s docker/Dockerfile=%s ci.yml(msrv)=%s\n' \
    "$cargo_msrv" "$docker_msrv" "$ci_msrv"
  [ -n "${GITHUB_ACTIONS:-}" ] &&
    printf '::warning title=MSRV mismatch::Cargo.toml=%s docker/Dockerfile=%s ci.yml(msrv)=%s - these three must agree\n' \
      "$cargo_msrv" "$docker_msrv" "$ci_msrv"
  bad=$((bad + 1))
fi

exit "$bad"
