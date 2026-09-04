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
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."

# SC1091: see install.sh's identical comment - this repo's shellcheck
# invocation runs without -x, so a bare `source=` directive doesn't resolve
# it.
# shellcheck disable=SC1091
source .github/actions/setup-tool/lib.sh

for _ctv_need in curl jq; do
  command -v "$_ctv_need" >/dev/null 2>&1 || {
    echo "check_tool_versions: no $_ctv_need on PATH" >&2
    exit 127
  }
done

# _latest <owner/repo> - the newest release tag, or empty if the API declines.
# Unauthenticated this is rate-limited to 60/hour per IP; in Actions the
# workflow passes GH_TOKEN, which raises that far above the size of the roster.
function _latest() {
  local auth=()
  [ -n "${GH_TOKEN:-}" ] && auth=(-H "Authorization: Bearer $GH_TOKEN")
  curl -sSf "${auth[@]}" "https://api.github.com/repos/$1/releases/latest" 2>/dev/null |
    sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1
}

bad=0

echo "## tools.txt (release-asset installs)"
echo
# Process substitution, not a pipe: a piped `while` runs in a subshell, so
# $bad would be lost and this would always exit 0.
while IFS='|' read -r tool pinned _ _ _ check tag_prefix; do
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

  latest="$(_latest "$repo")"
  # A missing upstream answer is a rate limit or an outage, not a stale pin;
  # counting it as outdated would open an issue about GitHub being slow.
  if [ -z "$latest" ]; then
    printf '%-16s %-12s (could not read the upstream release)\n' "$tool" "$pinned"
    continue
  fi

  # The tag prefix stripped from `latest` is per-row (7th tools.txt column,
  # defaulting to "v"), not a symmetric strip on both sides - what lets
  # lychee's own "lychee-v0.24.2" upstream tags compare correctly against a
  # pin that (like every row's) carries no prefix at all, instead of needing
  # its own `check = "-"` opt-out the way it used to.
  prefix="${tag_prefix:-v}"
  if [ "${latest#"$prefix"}" = "$pinned" ]; then
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

  latest="$(_latest "$repo")"
  if [ -z "$latest" ]; then
    printf '%-45s %-10s (could not read the upstream release)\n' "$repo" "$comment"
    continue
  fi
  # A repo can run more than one release train off the same `releases/latest`
  # endpoint - github/codeql-action is the example in this tree: its "latest
  # release" by publish date is a `codeql-bundle-vX.Y.Z` CLI bundle, not the
  # action's own `vX.Y.Z` series this pin actually tracks. There is no way to
  # tell those apart from here except shape, so a `latest` that does not look
  # like this pin's own scheme is reported as unresolved instead of a false
  # OUTDATED - see this repo's own SUPPORT.md-style caveat: guessing wrong is
  # worse than saying nothing.
  case "$latest" in
  v[0-9]*) ;;
  *)
    printf '%-45s %-10s (upstream latest release, %s, is not this pin'"'"'s v<version> scheme - check by hand)\n' \
      "$repo" "$comment" "$latest"
    continue
    ;;
  esac

  if [ "${latest#v}" = "${comment#v}" ]; then
    printf '%-45s %-10s current\n' "$repo" "$comment"
  else
    printf '%-45s %-10s OUTDATED (latest: %s, pinned %s)\n' "$repo" "$comment" "$latest" "$pin"
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
  current="$(curl -sSf "https://hub.docker.com/v2/repositories/$hub_path/tags/$tag" 2>/dev/null |
    jq -r '.digest // empty' | sed 's/^sha256://')"
  if [ -z "$current" ]; then
    printf '%-45s %-24s (could not read the current tag digest)\n' "$ref" "${pinned:0:19}..."
    continue
  fi

  if [ "$current" = "$pinned" ]; then
    printf '%-45s %-24s current\n' "$ref" "${pinned:0:19}..."
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
