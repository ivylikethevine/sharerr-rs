#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Smoke test for a built image: does the binary run, and does the container
# report healthy by its own HEALTHCHECK?
#
# A build that compiles can still ship an image that does not start: a
# missing shared library in the slim runtime stage, a binary for the wrong
# architecture, an ENTRYPOINT typo, a HEALTHCHECK pointing at a route that
# moved. None of those fail `docker build`; each fails here.
#
#   1. `docker run <image> --version`   the entrypoint runs; with an expected
#                                       version, it reports that one
#   2. the image declares a HEALTHCHECK  a dropped one is a regression too
#   3. start it, poll .State.Health     healthy within SMOKE_TIMEOUT seconds
#
# Step 3 runs the image's own HEALTHCHECK command, only with a 2s interval
# instead of the Dockerfile's 60s, so the first probe does not wait a minute.
# Nothing is published and no config is mounted: both images answer their
# health route before anything is configured.
#
# Usage: docker/smoke_test.sh <image-ref> [expected-version]
#   <image-ref>         anything `docker run` accepts: name:tag, name@digest,
#                       or an image id
#   [expected-version]  a string `--version` must contain
#
# Environment:
#   SMOKE_TIMEOUT  seconds to wait for "healthy" (default 60)
#
# Exits 0 when all three pass, 1 when one fails, 2 on bad usage.
set -euo pipefail

if [ "$#" -lt 1 ] || [ "$#" -gt 2 ] || [ -z "$1" ]; then
  echo "usage: $0 <image-ref> [expected-version]" >&2
  exit 2
fi
ref="$1"
want="${2:-}"
timeout="${SMOKE_TIMEOUT:-60}"

# 1. the binary runs
if ! version="$(docker run --rm "$ref" --version 2>&1)"; then
  echo "::error title=smoke test::$ref --version failed: $version"
  exit 1
fi
echo "--version: $version"
if [ -n "$want" ] && [[ "$version" != *"$want"* ]]; then
  echo "::error title=smoke test::$ref --version reported '$version', expected it to contain '$want'"
  exit 1
fi

# 2. the image declares a healthcheck
if [ "$(docker image inspect --format '{{if .Config.Healthcheck}}yes{{end}}' "$ref")" != yes ]; then
  echo "::error title=smoke test::$ref declares no HEALTHCHECK"
  exit 1
fi

# 3. it goes healthy
cid="$(docker run --detach --health-interval 2s --health-start-period 30s "$ref")"
trap 'docker rm --force "$cid" >/dev/null 2>&1 || true' EXIT

deadline=$((SECONDS + timeout))
status=""
while [ "$SECONDS" -lt "$deadline" ]; do
  state="$(docker inspect --format '{{.State.Status}} {{if .State.Health}}{{.State.Health.Status}}{{end}}' "$cid")"
  status="${state#* }"
  case "$state" in
  "running healthy")
    echo "healthy after $((SECONDS - deadline + timeout))s"
    exit 0
    ;;
  running*) ;;
  *)
    echo "::error title=smoke test::$ref exited before going healthy (state: $state)"
    docker logs "$cid" 2>&1 | tail -n 50
    exit 1
    ;;
  esac
  [ "$status" != unhealthy ] || break
  sleep 1
done

echo "::error title=smoke test::$ref did not go healthy within ${timeout}s (health: ${status:-none})"
docker inspect --format '{{json .State.Health}}' "$cid" || true
docker logs "$cid" 2>&1 | tail -n 50
exit 1
