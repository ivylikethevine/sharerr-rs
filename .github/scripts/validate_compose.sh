#!/usr/bin/env bash
# `docker compose config -q` over every tracked compose file under docker/:
# schema, `${VAR:?}` interpolation and `extends` resolution, with no daemon
# and no image pulls. ci.yml's `scripts` job runs it; so can you, from
# anywhere in the checkout.
#
# The list comes from `git ls-files`, not a hand-kept list, so a new stack is
# checked the day it lands. The glob mirrors the filenames dependabot's
# docker-compose ecosystem picks up (see dependabot.yml's header). A file
# that is not a standalone project needs a case below saying why; anything
# else is validated on its own and fails loudly until it gets one.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

# Every deployment stack guards its required inputs with `${VAR:?}`, which
# `config` evaluates. These are shaped like the real thing and are not
# secrets; the point is to reach the schema check, not describe a deployment.
env_file="$(mktemp)"
trap 'rm -f "$env_file"' EXIT
cat >"$env_file" <<'PLACEHOLDERS'
WIREGUARD_PRIVATE_KEY=placeholder
WIREGUARD_ADDRESSES=10.13.13.2/32
GLUETUN_API_KEY=placeholder
SHARERR_MASTER_KEY=placeholder
MEDIA_PATH=/srv/media
EXTERNAL_NETWORK=media_default
DOMAIN=lighthouse.example.net
PLACEHOLDERS

files=()
while IFS= read -r -d '' f; do
  files+=("$f")
done < <(git ls-files -z ':(glob)docker/**/compose*.y*ml' ':(glob)docker/**/docker-compose*.y*ml')
[ "${#files[@]}" -gt 0 ] || {
  echo "validate_compose: no compose files found under docker/" >&2
  exit 1
}

failed=0
for f in "${files[@]}"; do
  case "$f" in
  # A service fragment, not a project: it references volumes each consumer
  # declares. vpn/ and dual-vpn/*/ `extends` it, so it is validated
  # through them below.
  docker/deploy/compose.gluetun.reference.yaml)
    echo "skip $f (fragment; checked via the stacks that extend it)"
    continue
    ;;
  # An override layered on the sibling compose.yaml, never run alone.
  docker/deploy/lighthouse/compose.tls.yaml)
    args=(-f "${f%/*}/compose.yaml" -f "$f")
    ;;
  *)
    args=(-f "$f")
    ;;
  esac
  if [ -n "${GITHUB_ACTIONS:-}" ]; then echo "::group::$f"; else echo "== $f"; fi
  docker compose "${args[@]}" --env-file "$env_file" config -q || failed=1
  if [ -n "${GITHUB_ACTIONS:-}" ]; then echo "::endgroup::"; fi
done

exit "$failed"
