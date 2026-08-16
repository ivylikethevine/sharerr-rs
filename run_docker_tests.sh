#!/usr/bin/env bash
#
# Bring up the compose test stack, seed it with tagged content, and run the
# opt-in tier-2 suite against it. See docker/README.md for what each piece is.
#
#   ./run_docker_tests.sh                the plain stack
#   ./run_docker_tests.sh --vpn          qBittorrent behind gluetun
#   ./run_docker_tests.sh --transmission Transmission instead of qBittorrent
#
# The --vpn variant is the same suite against a genuinely different topology: the
# torrent client has no address of its own, its ports belong to the VPN container,
# and the announce address describes the tunnel rather than the machine. See the
# header of docker/compose.vpn.yml.
#
# Safe to re-run: every step is idempotent, and the trap tears the stack down
# however the script exits.

set -euo pipefail

# `readlink -f` rather than a bare `dirname $0`: everything below is relative to
# the repo root, so an invocation through a symlink would otherwise land in the
# symlink's directory and fail on paths that look fine in the source.
cd "$(dirname "$(readlink -f "$0")")"

# Which stack, and everything that differs between them. Ports are offset in the
# VPN stack so both can be up at once.
VPN=0
TM=0
case "${1:-}" in
    --vpn) VPN=1 ;;
    --transmission) TM=1 ;;
    "") ;;
    *)
        echo "usage: $0 [--vpn|--transmission]" >&2
        exit 2
        ;;
esac

if ((TM)); then
    COMPOSE=(docker compose -f docker/compose.transmission.yml)
    STATE=docker/state-tm
    SONARR_PORT=38989 RADARR_PORT=37878 QBIT_PORT=39091 SHARERR_PORT=38477
    QBIT_SERVICE=transmission
elif ((VPN)); then
    COMPOSE=(docker compose -f docker/compose.vpn.yml)
    STATE=docker/state-vpn
    SONARR_PORT=28989 RADARR_PORT=27878 QBIT_PORT=28080 SHARERR_PORT=28477
    # qBittorrent's WebUI is published by gluetun, because qBittorrent has no
    # network of its own. Waiting on the gluetun service is waiting on both.
    QBIT_SERVICE=gluetun
else
    COMPOSE=(docker compose -f docker/compose.test.yml)
    STATE=docker/state
    SONARR_PORT=18989 RADARR_PORT=17878 QBIT_PORT=18080 SHARERR_PORT=18477
    QBIT_SERVICE=qbittorrent
fi

SONARR_DB=$STATE/sonarr/sonarr.db
RADARR_DB=$STATE/radarr/radarr.db

# Remove `docker/state`, falling back to a root-owned delete.
#
# Docker creates the parent of a bind mount as root when it does not already
# exist, which leaves `docker/state` as `root:root` and its children unremovable
# by the invoking user — `rm -rf` gets EACCES on the entries inside. The script
# pre-creates the directory precisely to avoid that, but a tree left behind by an
# older run still has to be cleanable, so fall back to a throwaway container that
# runs as root.
remove_state() {
    [[ -e $STATE ]] || return 0
    rm -rf "$STATE" 2>/dev/null && return 0
    docker run --rm -v "$PWD/docker:/w" alpine:3 rm -rf "/w/$(basename "$STATE")" \
        >/dev/null 2>&1 || true
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
    local name=$1 file="$STATE/$1/config.xml" key
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
mkdir -p "$STATE/sonarr" "$STATE/radarr"
"${COMPOSE[@]}" up -d --build

wait_for sonarr "$SONARR_PORT" /ping
wait_for radarr "$RADARR_PORT" /ping

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

wait_for sonarr "$SONARR_PORT" /ping
wait_for radarr "$RADARR_PORT" /ping
# Waited on too, though nothing used to: step 5 execs into sharerr and the step
# below greps qBittorrent's log, so both have to be up. Their absence from the
# original wait list is what the fixed sleeps were quietly compensating for.
#
# qBittorrent answers 401 rather than 200 — see `wait_for` on why that counts.
wait_for "$QBIT_SERVICE" "$QBIT_PORT" / '^(2[0-9][0-9]|401)$'
wait_for sharerr "$SHARERR_PORT" /health

# 4. Collect the credentials each app generated on first start.
SONARR_KEY=$(api_key sonarr)
RADARR_KEY=$(api_key radarr)

# qBittorrent prints a temporary admin password to its log on first start. It only
# does so when it has no stored password, so a miss means the config volume
# survived a previous run — say that, rather than dying on `pipefail` with no
# explanation.
if ((TM)); then
    # Transmission is given its credentials up front by compose, so there is no
    # temporary password to scrape — and its vault key is a different one.
    QBITTORRENT_PW=transmission-test-password
elif ! QBITTORRENT_PW=$("${COMPOSE[@]}" logs qbittorrent 2>/dev/null |
    grep -i 'temporary password' | awk '{print $NF}' | tail -n 1) ||
    [[ -z ${QBITTORRENT_PW:-} ]]; then
    echo "error: qBittorrent logged no temporary password." >&2
    echo "       Its config volume is not fresh — run: ${COMPOSE[*]} down -v" >&2
    exit 1
fi

# 5. Load them into sharerr. Either open the UI on $SHARERR_PORT and paste them
#    into Settings, or pipe them in — the two write to the same vault.
#
#    `qbittorrent.username` is deliberately not here: it is a config field in
#    sharerr.toml, not a vault key, so writing it only earned a warning from
#    `sharerr vault set` and stored something nothing ever read.
vault_set sonarr.api_key "$SONARR_KEY"
vault_set radarr.api_key "$RADARR_KEY"
if ((TM)); then
    vault_set transmission.password "$QBITTORRENT_PW"
else
    vault_set qbittorrent.password "$QBITTORRENT_PW"
fi

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
# Captured before matching, not piped into `grep -q`. Under `pipefail`, grep
# exiting on its first match closes the pipe and the producer takes a SIGPIPE —
# turning a successful match into a failed pipeline. That cost a confusing
# debugging session once already.
caps_body=$(curl -sf "http://127.0.0.1:$SHARERR_PORT/api?t=caps&apikey=$TORZNAB_KEY" || true)
if ! grep -q '<caps>' <<<"$caps_body"; then
    echo "error: the Torznab caps endpoint did not return a <caps> document" >&2
    exit 1
fi
echo "torznab caps ok"

# The same document over Jackett's URL shape, so a Jackett-configured client works
# unmodified. Checked here because it is pure routing — exactly the kind of thing a
# unit test can pass while the assembled binary serves a 404.
jackett_url="http://127.0.0.1:$SHARERR_PORT/api/v2.0/indexers/sharerr/results/torznab/api"
jackett_caps=$(curl -sf "$jackett_url?t=caps&apikey=$TORZNAB_KEY" || true)
if ! grep -q '<caps>' <<<"$jackett_caps"; then
    echo "error: the Jackett-shaped Torznab path did not return a <caps> document" >&2
    exit 1
fi
echo "jackett path ok"

# Jackett's read-only admin surface, on the real binary. Routing and JSON shape are
# exactly the kind of thing that passes a unit test and 404s once assembled.
jackett_api="http://127.0.0.1:$SHARERR_PORT/api/v2.0"
indexers_body=$(curl -sf "$jackett_api/indexers?apikey=$TORZNAB_KEY" || true)
if ! grep -q '"id":"sharerr"' <<<"$indexers_body"; then
    echo "error: the Jackett indexer list did not describe this instance" >&2
    exit 1
fi
results_body=$(curl -sf "$jackett_api/indexers/sharerr/results?apikey=$TORZNAB_KEY" || true)
if ! grep -q '"Results"' <<<"$results_body"; then
    echo "error: the Jackett JSON results endpoint returned nothing usable" >&2
    exit 1
fi
# The security property: Jackett puts its own API key in this response, and sharerr
# must never do the same — that would turn one friend's key into a way to obtain the
# credential.
config_body=$(curl -sf "$jackett_api/server/config?apikey=$TORZNAB_KEY")
if ! grep -q '"api_key":""' <<<"$config_body"; then
    echo "error: server/config did not return an empty api_key: $config_body" >&2
    exit 1
fi
if grep -q "$TORZNAB_KEY" <<<"$config_body"; then
    echo "error: server/config echoed the presented key back" >&2
    exit 1
fi
# An unimplemented endpoint must say so rather than 404, so a gap is actionable.
code=$(curl -s -o /dev/null -w '%{http_code}' "$jackett_api/server/logs?apikey=$TORZNAB_KEY")
if [[ $code != 501 ]]; then
    echo "error: an unimplemented Jackett endpoint returned $code, expected 501" >&2
    exit 1
fi
echo "jackett admin ok"

# 6e. The check that actually matters to a friend: can a real Sonarr add this as an
#     indexer? The roadmap claimed this "should already work" — it did not. Sonarr
#     refused the whole feed because its items had no `pubDate`, and nothing caught
#     it until a real client was pointed at one. This is that client.
#
#     A sync has to happen first. Sonarr rejects an *empty* feed as well — "Query
#     successful, but no results in the configured categories" is an error, not a
#     warning — so an indexer added before anything is tagged fails its test. That
#     is worth knowing in its own right, and it is why this step syncs rather than
#     relying on the suite below to do it.
"${COMPOSE[@]}" exec -T sharerr sharerr sync >/dev/null
sonarr_indexer=$(cat <<JSON
{
  "enableRss": true, "enableAutomaticSearch": true, "enableInteractiveSearch": true,
  "supportsRss": true, "supportsSearch": true, "protocol": "torrent", "priority": 25,
  "name": "sharerr-direct",
  "implementation": "Torznab", "implementationName": "Torznab",
  "configContract": "TorznabSettings",
  "fields": [
    {"name": "baseUrl", "value": "http://sharerr:8477"},
    {"name": "apiPath", "value": "/api"},
    {"name": "apiKey", "value": "$TORZNAB_KEY"},
    {"name": "categories", "value": [5000]},
    {"name": "minimumSeeders", "value": 1}
  ]
}
JSON
)
indexer_test=$(curl -s -w '\n%{http_code}' -X POST \
    -H "X-Api-Key: $SONARR_KEY" -H 'Content-Type: application/json' \
    --data "$sonarr_indexer" "http://127.0.0.1:$SONARR_PORT/api/v3/indexer/test")
if [[ $(tail -n1 <<<"$indexer_test") != 200 ]]; then
    echo "error: a real Sonarr rejected sharerr as a Torznab indexer:" >&2
    sed '$d' <<<"$indexer_test" >&2
    exit 1
fi
echo "sonarr accepts sharerr as an indexer ok"

# 6f. What only the Transmission stack can prove.
if ((TM)); then
    # The client actually in use must be the configured one. `doctor` names it, so
    # a config that silently fell back to qBittorrent would show up here.
    #
    # Captured before matching rather than piped into `grep -q`: under `pipefail`,
    # grep exiting early on its first match closes the pipe, `doctor` takes a
    # SIGPIPE, and the pipeline reports failure *because the match succeeded*.
    doctor_out=$("${COMPOSE[@]}" exec -T sharerr sharerr doctor 2>&1 || true)
    if ! grep -qi "transmission" <<<"$doctor_out"; then
        echo "error: doctor did not report Transmission — is torrent_backend being read?" >&2
        echo "$doctor_out" >&2
        exit 1
    fi
    echo "transmission: doctor reports the configured client"

    # The constraint that makes this stack worth having: Transmission has no
    # embedded tracker, so announce URLs must come from sharerr's own. If the
    # torrents pointed at Transmission, nothing could announce to them.
    hash=$("${COMPOSE[@]}" exec -T sharerr sh -c 'ls /data/torrents/*.torrent 2>/dev/null | head -1')
    if [[ -z $hash ]]; then
        echo "error: no .torrent was built, so the announce URL cannot be checked" >&2
        exit 1
    fi
    # `grep -a`, not `strings`: the runtime image ships only ca-certificates and
    # curl, so binutils is not there. A .torrent is bencode, and the announce URL
    # is plain ASCII inside it.
    #
    # Matching the advertised host rather than just the word "announce" — the
    # bencode key is always present, so looking for it would pass even if the URL
    # pointed at the wrong place, which is the failure that actually matters.
    if ! "${COMPOSE[@]}" exec -T sharerr \
        grep -aq "localhost:$SHARERR_PORT/announce" "$hash"; then
        echo "error: the built torrent does not announce to sharerr's own tracker" >&2
        "${COMPOSE[@]}" exec -T sharerr sh -c "grep -ao 'http[^\"]*announce[^\"]*' '$hash'" >&2 || true
        exit 1
    fi
    echo "transmission: torrents carry an announce URL from sharerr's own tracker"
fi

# 6d. The assertions that only mean something in the VPN topology.
if ((VPN)); then
    # qBittorrent has no address of its own here, so the name in sharerr.toml has
    # to be gluetun's. Proving the *wrong* name fails is what makes this a real
    # check rather than a restatement of `doctor` passing.
    if "${COMPOSE[@]}" exec -T sharerr sh -c \
        'wget -q -T 5 -O /dev/null http://qbittorrent:8080/ 2>/dev/null'; then
        echo "error: http://qbittorrent:8080 resolved — the topology is not what this stack claims" >&2
        exit 1
    fi
    echo "vpn: qbittorrent has no address of its own, as expected"

    # And the announce URL must describe the tunnel's exit rather than a container
    # address, or the torrents are unannounceable by anyone but this host.
    announce=$("${COMPOSE[@]}" exec -T sharerr sharerr doctor 2>&1 |
        grep -i "embedded tracker" || true)
    echo "vpn: ${announce:-no tracker line in doctor output}"

    # A VPN drop must not take sharerr down. gluetun's killswitch severs egress
    # when the tunnel dies; the control plane runs over the LAN side and should be
    # unaffected, so /health has to keep answering.
    "${COMPOSE[@]}" stop vpn >/dev/null
    sleep 10
    code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 10 \
        "http://127.0.0.1:$SHARERR_PORT/health")
    if [[ $code != 200 ]]; then
        echo "error: sharerr stopped answering /health when the tunnel dropped (got $code)" >&2
        exit 1
    fi
    "${COMPOSE[@]}" start vpn >/dev/null
    echo "vpn: sharerr survived the tunnel dropping"
fi

# 6c. The web UI is reachable and its guard is on. This stack has no operator
#     account, so every protected page must bounce to /setup — which is also the
#     cheapest proof that the auth middleware is wired in the real binary and not
#     only in the unit tests.
for page in / /settings /diagnostics /peers; do
    code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$SHARERR_PORT$page")
    if [[ $code != 303 ]]; then
        echo "error: $page returned $code, expected a 303 redirect to /setup" >&2
        exit 1
    fi
done
echo "web ui guard ok"

# 7. The opt-in suite. Serialised because all three tests share this one stack and
#    each of them runs a real sync.
#    The suite shells out to `docker compose` itself, so it has to be told which
#    stack is up — otherwise it looks for the plain one and reports that nothing is
#    running.
if ((TM)); then
    export SHARERR_E2E_COMPOSE=docker/compose.transmission.yml
elif ((VPN)); then
    export SHARERR_E2E_COMPOSE=docker/compose.vpn.yml
fi
cargo test -p sharerr --features e2e -- --ignored --test-threads=1
