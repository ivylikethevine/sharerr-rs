#!/usr/bin/env bash
#
# Bring up the compose test stack, seed it with tagged content, and run the
# opt-in tier-2 suite against it. See docker/README.md for what each piece is.
#
# Safe to re-run: every step is idempotent, and the trap tears the stack down
# however the script exits.

set -euo pipefail

cd "$(dirname "$0")"

COMPOSE=(docker compose -f docker/compose.test.yml)
SONARR_DB=docker/state/sonarr/sonarr.db
RADARR_DB=docker/state/radarr/radarr.db

# `-v` drops the named volumes; the bind-mounted *arr config is not one of them,
# so it has to go separately or the next run starts with stale API keys.
teardown() {
    "${COMPOSE[@]}" down -v
    rm -rf docker/state
}
trap teardown EXIT

# Wait for an app to answer its own API. Replaces a fixed sleep: first start does
# a database migration whose duration is not something to guess at.
wait_for() {
    local name=$1 port=$2 deadline=$((SECONDS + 180))
    printf 'waiting for %s' "$name"
    until curl -sf "http://127.0.0.1:$port/ping" >/dev/null 2>&1; do
        if ((SECONDS > deadline)); then
            echo " — gave up after 180s"
            "${COMPOSE[@]}" logs "$name" | tail -30
            return 1
        fi
        printf .
        sleep 2
    done
    echo " ok"
}

api_key() {
    sed -n 's:.*<ApiKey>\(.*\)</ApiKey>.*:\1:p' "docker/state/$1/config.xml"
}

# 1. Generate the synthetic library (idempotent — same bytes every time).
cargo run -q -p sharerr-testkit --bin gen-fixtures -- tests/fixtures/media

# 2. Bring the stack up. The *arr config directories are bind mounts, so they have
#    to exist and be ours before the containers claim them.
mkdir -p docker/state/sonarr docker/state/radarr
"${COMPOSE[@]}" up -d --build

wait_for sonarr 18989
wait_for radarr 17878

# 3. Give Sonarr and Radarr something tagged to find.
#
#    Written straight into their databases rather than added through the API: the
#    add path does a metadata lookup against services.sonarr.tv / api.radarr.video,
#    which the internal network deliberately blocks — and every fixture title is
#    invented, so the lookup would find nothing even with egress. Both apps must be
#    stopped first; they hold these databases open and ignore external writes.
"${COMPOSE[@]}" stop sonarr radarr
cargo run -q -p sharerr-testkit --bin seed-arr -- --sonarr "$SONARR_DB" --radarr "$RADARR_DB"
"${COMPOSE[@]}" start sonarr radarr

wait_for sonarr 18989
wait_for radarr 17878

# 4. Collect the credentials each app generated on first start.
SONARR_KEY=$(api_key sonarr)
RADARR_KEY=$(api_key radarr)
# qBittorrent prints a temporary admin password to its log on first start.
QBITTORRENT_PW=$("${COMPOSE[@]}" logs qbittorrent | grep -i 'temporary password' | awk '{print $NF}' | tail -n 1)

# 5. Load them into sharerr. Either open http://127.0.0.1:18477/ and paste them
#    into Settings, or pipe them in — the two write to the same vault.
vault_set() {
    "${COMPOSE[@]}" exec -T sharerr sh -c "printf %s '$2' | sharerr vault set $1"
}
vault_set sonarr.api_key "$SONARR_KEY"
vault_set radarr.api_key "$RADARR_KEY"
vault_set qbittorrent.password "$QBITTORRENT_PW"
vault_set qbittorrent.username admin

# The credential write invalidates the syncer; give the recovery loop its interval
# to rebuild before asking whether everything is wired up.
sleep 20

# 6. Check the wiring before trying to sync. The UI's per-service "Test connection"
#    buttons cover the same ground for the services themselves; `doctor`
#    additionally resolves the path mappings, which is what the deliberately
#    disagreeing mounts in this stack exist to exercise.
"${COMPOSE[@]}" exec -T sharerr sharerr doctor

# 7. The opt-in suite. Serialised because all three tests share this one stack and
#    each of them runs a real sync.
cargo test -p sharerr --features e2e -- --ignored --test-threads=1
