#!/usr/bin/env bash
#
# Bring up the compose test stack, seed it with tagged content, and run the
# opt-in tier-2 suite against it. See docker/README.md for what each piece is.
#
# Safe to re-run: every step is idempotent, and the trap tears the stack down
# however the script exits.

set -euo pipefail

# `readlink -f` rather than a bare `dirname $0`: everything below is relative to
# the repo root, so an invocation through a symlink would otherwise land in the
# symlink's directory and fail on paths that look fine in the source.
cd "$(dirname "$(readlink -f "$0")")"

COMPOSE=(docker compose -f docker/compose.test.yml)
SONARR_DB=docker/state/sonarr/sonarr.db
RADARR_DB=docker/state/radarr/radarr.db

# Remove `docker/state`, falling back to a root-owned delete.
#
# Docker creates the parent of a bind mount as root when it does not already
# exist, which leaves `docker/state` as `root:root` and its children unremovable
# by the invoking user — `rm -rf` gets EACCES on the entries inside. The script
# pre-creates the directory precisely to avoid that, but a tree left behind by an
# older run still has to be cleanable, so fall back to a throwaway container that
# runs as root.
remove_state() {
    [[ -e docker/state ]] || return 0
    rm -rf docker/state 2>/dev/null && return 0
    docker run --rm -v "$PWD/docker:/w" alpine:3 rm -rf /w/state >/dev/null 2>&1 || true
}

# `-v` drops the named volumes; the bind-mounted *arr config is not one of them,
# so it has to go separately or the next run starts with stale API keys.
#
# Every command is made non-fatal and the entry status is restored on the way out.
# This runs as an EXIT trap under `set -e`, so a single failure in here would
# otherwise replace the script's exit status — turning a fully passing run into a
# non-zero exit, which is exactly what used to happen when `rm -rf` hit the
# root-owned directory above.
teardown() {
    local status=$?
    "${COMPOSE[@]}" --profile indexer down -v --remove-orphans || true
    remove_state
    return $status
}
trap teardown EXIT

# Wait for a service to answer over its published port. Replaces a fixed sleep:
# first start does a database migration whose duration is not something to guess
# at.
#
# Host-side rather than an in-container healthcheck because the host is what the
# next step needs: `seed-arr` opens the *arr databases directly and the API keys
# are scraped from a bind mount, so "reachable from here" is the property that
# actually has to hold.
# The fourth argument is a regex of acceptable HTTP statuses, because "answering"
# and "answering 2xx" are not the same question here. qBittorrent 5.x validates the
# Host header, and a request arriving on the *published* port carries a host:port
# the server does not recognise — so it answers 401 to everything from outside the
# container. That 401 is still proof the WebUI is up, which is all this needs to
# know. Matching on success instead made this hang for the full 180s against a
# perfectly healthy container.
wait_for() {
    local name=$1 port=$2 path=$3 accept=${4:-'^2[0-9][0-9]$'} deadline=$((SECONDS + 180)) code
    printf 'waiting for %s' "$name"
    while true; do
        code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 \
            "http://127.0.0.1:$port$path" 2>/dev/null || true)
        [[ $code =~ $accept ]] && break
        if ((SECONDS > deadline)); then
            echo " — gave up after 180s (last status: ${code:-no response})"
            "${COMPOSE[@]}" logs "$name" | tail -30
            return 1
        fi
        printf .
        # Unbuffers the progress dots when stdout is a file rather than a TTY,
        # so a captured log shows progress instead of nothing until the end.
        [[ -t 1 ]] || printf '\n'
        sleep 2
    done
    echo " ok"
}

# Scrape the API key an *arr app generated on first start.
#
# Guarded: an empty match would otherwise be written to the vault as an empty
# credential and surface much later as an opaque 401 from `doctor`, a long way
# from the step that actually went wrong.
api_key() {
    local name=$1 file="docker/state/$1/config.xml" key
    if [[ ! -f $file ]]; then
        echo "error: $file does not exist — did $name finish its first start?" >&2
        return 1
    fi
    key=$(sed -n 's:.*<ApiKey>\(.*\)</ApiKey>.*:\1:p' "$file")
    if [[ -z $key ]]; then
        echo "error: no <ApiKey> in $file" >&2
        return 1
    fi
    printf %s "$key"
}

# Pipe a secret into the vault on stdin.
#
# The value is never interpolated into the remote shell command. It used to be
# spliced into a single-quoted `sh -c` string, so a qBittorrent temporary password
# containing an apostrophe broke the quoting — and could run arbitrary text inside
# the container.
vault_set() {
    local key=$1 value=$2
    if [[ -z $value ]]; then
        echo "error: refusing to store an empty value for $key" >&2
        return 1
    fi
    printf %s "$value" | "${COMPOSE[@]}" exec -T sharerr sharerr vault set "$key"
}

# 1. Generate the synthetic library (idempotent — same bytes every time).
cargo run -q -p sharerr-testkit --bin gen-fixtures -- tests/fixtures/media

# 2. Bring the stack up. The *arr config directories are bind mounts, so they have
#    to exist and be ours before the containers claim them — if Docker auto-creates
#    the parent it does so as root, and neither `seed-arr` nor teardown can then
#    touch it.
remove_state
mkdir -p docker/state/sonarr docker/state/radarr
"${COMPOSE[@]}" up -d --build

wait_for sonarr 18989 /ping
wait_for radarr 17878 /ping

# 3. Give Sonarr and Radarr something tagged to find.
#
#    Written straight into their databases rather than added through the API: the
#    add path does a metadata lookup against services.sonarr.tv / api.radarr.video,
#    and every fixture title is invented, so the lookup would find nothing. Both
#    apps must be stopped first; they hold these databases open and ignore
#    external writes.
"${COMPOSE[@]}" stop sonarr radarr
cargo run -q -p sharerr-testkit --bin seed-arr -- --sonarr "$SONARR_DB" --radarr "$RADARR_DB"
"${COMPOSE[@]}" start sonarr radarr

wait_for sonarr 18989 /ping
wait_for radarr 17878 /ping
# Waited on too, though nothing used to: step 5 execs into sharerr and the step
# below greps qBittorrent's log, so both have to be up. Their absence from the
# original wait list is what the fixed sleeps were quietly compensating for.
#
# qBittorrent answers 401 rather than 200 — see `wait_for` on why that counts.
wait_for qbittorrent 18080 / '^(2[0-9][0-9]|401)$'
wait_for sharerr 18477 /health

# 4. Collect the credentials each app generated on first start.
SONARR_KEY=$(api_key sonarr)
RADARR_KEY=$(api_key radarr)

# qBittorrent prints a temporary admin password to its log on first start. It only
# does so when it has no stored password, so a miss means the config volume
# survived a previous run — say that, rather than dying on `pipefail` with no
# explanation.
if ! QBITTORRENT_PW=$("${COMPOSE[@]}" logs qbittorrent 2>/dev/null |
    grep -i 'temporary password' | awk '{print $NF}' | tail -n 1) ||
    [[ -z ${QBITTORRENT_PW:-} ]]; then
    echo "error: qBittorrent logged no temporary password." >&2
    echo "       Its config volume is not fresh — run: ${COMPOSE[*]} down -v" >&2
    exit 1
fi

# 5. Load them into sharerr. Either open http://127.0.0.1:18477/ and paste them
#    into Settings, or pipe them in — the two write to the same vault.
#
#    `qbittorrent.username` is deliberately not here: it is a config field in
#    sharerr.toml, not a vault key, so writing it only earned a warning from
#    `sharerr vault set` and stored something nothing ever read.
vault_set sonarr.api_key "$SONARR_KEY"
vault_set radarr.api_key "$RADARR_KEY"
vault_set qbittorrent.password "$QBITTORRENT_PW"

# The Torznab feed is closed without a key, so nothing exercised it. Set one and
# the endpoint becomes checkable in step 6.
TORZNAB_KEY=sharerr-test-torznab-key
vault_set torznab.api_key "$TORZNAB_KEY"

# 6. Check the wiring before trying to sync. The UI's per-service "Test connection"
#    buttons cover the same ground for the services themselves; `doctor`
#    additionally resolves the path mappings, which is what the deliberately
#    disagreeing mounts in this stack exist to exercise.
#
#    Retried rather than slept on: the credential writes above invalidated the
#    syncer, and polling the thing we care about is honest where a fixed sleep is
#    a guess at an internal interval.
printf 'waiting for sharerr to accept its credentials'
doctor_deadline=$((SECONDS + 120))
until "${COMPOSE[@]}" exec -T sharerr sharerr doctor >/tmp/sharerr-doctor.log 2>&1; do
    if ((SECONDS > doctor_deadline)); then
        echo " — gave up after 120s"
        cat /tmp/sharerr-doctor.log
        exit 1
    fi
    printf .
    [[ -t 1 ]] || printf '\n'
    sleep 5
done
echo " ok"
cat /tmp/sharerr-doctor.log

# 6b. The Torznab feed a friend's Prowlarr would index. Cheap, and it was never
#     covered by anything — the endpoint stays closed without a key, so no earlier
#     run of this script could have reached it.
if ! curl -sf "http://127.0.0.1:18477/api?t=caps&apikey=$TORZNAB_KEY" | grep -q '<caps>'; then
    echo "error: the Torznab caps endpoint did not return a <caps> document" >&2
    exit 1
fi
echo "torznab caps ok"

# The same document over Jackett's URL shape, so a Jackett-configured client works
# unmodified. Checked here because it is pure routing — exactly the kind of thing a
# unit test can pass while the assembled binary serves a 404.
jackett_url="http://127.0.0.1:18477/api/v2.0/indexers/sharerr/results/torznab/api"
if ! curl -sf "$jackett_url?t=caps&apikey=$TORZNAB_KEY" | grep -q '<caps>'; then
    echo "error: the Jackett-shaped Torznab path did not return a <caps> document" >&2
    exit 1
fi
echo "jackett path ok"

# 6c. The web UI is reachable and its guard is on. This stack has no operator
#     account, so every protected page must bounce to /setup — which is also the
#     cheapest proof that the auth middleware is wired in the real binary and not
#     only in the unit tests.
for page in / /settings /diagnostics /peers; do
    code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:18477$page")
    if [[ $code != 303 ]]; then
        echo "error: $page returned $code, expected a 303 redirect to /setup" >&2
        exit 1
    fi
done
echo "web ui guard ok"

# 7. The opt-in suite. Serialised because all three tests share this one stack and
#    each of them runs a real sync.
cargo test -p sharerr --features e2e -- --ignored --test-threads=1
