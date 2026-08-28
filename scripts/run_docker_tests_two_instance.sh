#!/usr/bin/env bash
#
# Bring up two independent friend-to-friend sharerr stacks, wire instance A up
# as a Torznab indexer and instance B as its requesting friend, trigger a real
# Radarr automatic search and grab, and run the one assertion that justifies
# the whole thing: the bytes instance B's qBittorrent actually saved are
# byte-identical to instance A's copy. See docker/README.md's "The
# two-instance stack" section.
#
#   ./scripts/run_docker_tests_two_instance.sh
#
# Heavier and slower than run_docker_tests.sh's suite — two Radarrs, two
# qBittorrents, two sharerr instances, and a real Radarr automatic search
# against a real Torznab feed — and never runs as part of it; see
# docker/README.md for why this is its own script rather than another flag
# there.
#
# Safe to re-run: every step is idempotent, and the trap tears the stack down
# however the script exits.

set -euo pipefail

# One level up from this script's own `scripts/` directory — see
# run_docker_tests.sh's comment on the same line for why `readlink -f` matters.
cd "$(dirname "$(readlink -f "$0")")/.."

COMPOSE=(docker compose -f docker/compose.two-instance.yml)
STATE=docker/state-two-instance

RADARR_A_PORT=58878
RADARR_B_PORT=59878
QBIT_A_PORT=58080
QBIT_B_PORT=59080
SHARERR_A_PORT=58477
SHARERR_B_PORT=59477
PROWLARR_PORT=59696

RADARR_A_DB=$STATE/a/radarr/radarr.db
RADARR_B_DB=$STATE/b/radarr/radarr.db

# The fixture movie's own id in `sharerr-testkit`'s library — see
# `crates/sharerr-testkit/src/library.rs`'s `MOVIE_ID`. Not read out of
# either database, since `seed-arr --radarr-wanted` writes it verbatim on
# instance B and this is the id `MoviesSearch` needs to name.
MOVIE_ID=31

# See `run_docker_tests.sh`'s own copy of this function for the reasoning —
# unchanged here.
remove_state() {
    [[ -e $STATE ]] || return 0
    rm -rf "$STATE" 2>/dev/null && return 0
    docker run --rm -v "$PWD/docker:/w" alpine:3 rm -rf "/w/$(basename "$STATE")" \
        >/dev/null 2>&1 || true
}

teardown() {
    local status=$?
    "${COMPOSE[@]}" down -v --remove-orphans || true
    remove_state
    return $status
}
# `DEBUG_NO_TEARDOWN=1 ./run_docker_tests_two_instance.sh` skips the trap,
# leaving both stacks up for a live post-mortem — `docker compose -f
# docker/compose.two-instance.yml exec qbittorrent-b sh`, the WebUI on
# $QBIT_B_PORT, `docker logs`, and so on. Clean up by hand afterwards with
# the same command `teardown` runs.
[[ -n ${DEBUG_NO_TEARDOWN:-} ]] || trap teardown EXIT

# Unchanged from `run_docker_tests.sh` — see its own comment for why this is
# host-side and why the fourth argument exists.
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
        [[ -t 1 ]] || printf '\n'
        sleep 2
    done
    echo " ok"
}

# Unchanged from `run_docker_tests.sh`, generalised to take the config.xml
# path directly rather than deriving it from a single `$STATE` — this script
# has two.
api_key() {
    local name=$1 file=$2 key
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

# Unchanged from `run_docker_tests.sh`, generalised to take the target
# container.
vault_set() {
    local container=$1 key=$2 value=$3
    if [[ -z $value ]]; then
        echo "error: refusing to store an empty value for $key on $container" >&2
        return 1
    fi
    printf %s "$value" | "${COMPOSE[@]}" exec -T "$container" sharerr vault set "$key"
}

# Unchanged in mechanism from `run_docker_tests.sh`'s own `qbittorrent_api_key`
# — see its comment for the full reasoning. Generalised to take which
# container to exec into, since instance A's own qBittorrent credential has
# to be minted from *inside sharerr-a's* container (the only one whose vault
# it belongs in).
qbittorrent_api_key() {
    local password=$1 host=$2 exec_container=$3 response key
    # shellcheck disable=SC2016
    response=$("${COMPOSE[@]}" exec -T -e QBIT_PW="$password" -e QBIT_HOST="$host" "$exec_container" sh -c '
        base="http://$QBIT_HOST:8080"
        curl -sf -c /tmp/qbit-cookie \
            --data-urlencode "username=admin" --data-urlencode "password=$QBIT_PW" \
            -H "Referer: $base" "$base/api/v2/auth/login" >/dev/null &&
        curl -sf -X POST -b /tmp/qbit-cookie -H "Referer: $base" "$base/api/v2/app/rotateAPIKey"
    ') || {
        echo "error: could not log in to $host to mint a WebUI API key" >&2
        return 1
    }
    key=$(sed -n 's/.*"apiKey":"\([^"]*\)".*/\1/p' <<<"$response")
    if [[ -z $key ]]; then
        echo "error: $host did not return an API key: $response" >&2
        return 1
    fi
    printf %s "$key"
}

# qBittorrent's temporary admin password, printed to its log on first start —
# unchanged mechanism from `run_docker_tests.sh`. Only instance A needs an
# API key minted from it (sharerr-a's own client never logs in — see
# `qbittorrent_api_key`'s doc); instance B's qBittorrent is only ever used
# as *Radarr-B's* download client, which authenticates the ordinary way, so
# its temporary password is used directly rather than rotated away.
qbittorrent_temp_password() {
    local service=$1 pw
    if ! pw=$("${COMPOSE[@]}" logs "$service" 2>/dev/null |
        grep -i 'temporary password' | awk '{print $NF}' | tail -n 1) || [[ -z ${pw:-} ]]; then
        echo "error: $service logged no temporary password." >&2
        echo "       Its config volume is not fresh — run: ${COMPOSE[*]} down -v" >&2
        return 1
    fi
    printf %s "$pw"
}

# 1. Fixtures — same synthetic library the single-instance stack shares.
cargo run -q -p sharerr-testkit --bin gen-fixtures -- tests/fixtures/media

# 2. Bring both stacks up. Config dirs are bind mounts and must exist and be
#    ours before the containers claim them — see `compose.test.yml`'s note on
#    why (Docker would otherwise create them as root).
remove_state
mkdir -p "$STATE/a/radarr" "$STATE/b/radarr" "$STATE/b/downloads" "$STATE/prowlarr"
"${COMPOSE[@]}" up -d --build

wait_for radarr-a "$RADARR_A_PORT" /ping
wait_for radarr-b "$RADARR_B_PORT" /ping

# 3. Seed: instance A gets the tagged, filed movie sharerr-a will share.
#    Instance B gets the *same* movie by TmdbId, wanted and fileless, so its
#    own automatic search — triggered in step 7 — has something to match.
#    Neither Radarr is touched through its API for this, for the reason
#    `seed-arr`'s own module doc gives: the add path does a metadata lookup
#    against api.radarr.video that invented fixture titles cannot satisfy.
"${COMPOSE[@]}" stop radarr-a radarr-b
cargo run -q -p sharerr-testkit --bin seed-arr -- --radarr "$RADARR_A_DB"
cargo run -q -p sharerr-testkit --bin seed-arr -- --radarr-wanted "$RADARR_B_DB"
"${COMPOSE[@]}" start radarr-a radarr-b

wait_for radarr-a "$RADARR_A_PORT" /ping
wait_for radarr-b "$RADARR_B_PORT" /ping
# qBittorrent answers 401 rather than 200 to a request on its published port
# — see `run_docker_tests.sh`'s `wait_for` comment for why that still counts.
wait_for qbittorrent-a "$QBIT_A_PORT" / '^(2[0-9][0-9]|401)$'
wait_for qbittorrent-b "$QBIT_B_PORT" / '^(2[0-9][0-9]|401)$'
wait_for sharerr-a "$SHARERR_A_PORT" /health
wait_for sharerr-b "$SHARERR_B_PORT" /health
wait_for prowlarr "$PROWLARR_PORT" /ping

# 4. Credentials. Only instance A's go into a vault — instance B is never
#    claimed and never synced in this test; see the compose file and
#    docker/README.md for why it still sits in the stack regardless.
#    Radarr-B's own key is still needed, though: every write to its API
#    (adding the indexer and download client, triggering the search) requires
#    `X-Api-Key`, unlike the bare `/ping` `wait_for` above already used.
RADARR_A_KEY=$(api_key radarr-a "$STATE/a/radarr/config.xml")
RADARR_B_KEY=$(api_key radarr-b "$STATE/b/radarr/config.xml")
PROWLARR_KEY=$(api_key prowlarr "$STATE/prowlarr/config.xml")
QBIT_A_PW=$(qbittorrent_temp_password qbittorrent-a) || exit 1
QBIT_A_KEY=$(qbittorrent_api_key "$QBIT_A_PW" qbittorrent-a sharerr-a) || exit 1
QBIT_B_PW=$(qbittorrent_temp_password qbittorrent-b) || exit 1

vault_set sharerr-a radarr.api_key "$RADARR_A_KEY"
vault_set sharerr-a qbittorrent.api_key "$QBIT_A_KEY"

# 5. Claim instance A and create one peer for instance B's Radarr to use as
#    its indexer key — same mechanism as `run_docker_tests.sh`, since neither
#    a CLI nor a SQL shortcut reaches the peers table (it lives in a named
#    volume, not a bind mount).
SETUP_PW=sharerr-test-admin-password
COOKIES=$(mktemp)
curl -sf -c "$COOKIES" \
    --data-urlencode "username=admin" \
    --data-urlencode "password=$SETUP_PW" \
    --data-urlencode "confirm=$SETUP_PW" \
    "http://127.0.0.1:$SHARERR_A_PORT/setup" -o /dev/null

PEERS_BODY=$(curl -sf -b "$COOKIES" \
    --data-urlencode "label=radarr-b" \
    --data-urlencode "scope=movies" \
    "http://127.0.0.1:$SHARERR_A_PORT/peers")
TORZNAB_KEY=$(sed -n 's/.*<code>\([^<]*\)<\/code>.*/\1/p' <<<"$PEERS_BODY" | head -n1)
if [[ -z $TORZNAB_KEY ]]; then
    echo "error: could not create a peer on instance A or extract its key" >&2
    echo "$PEERS_BODY" >&2
    exit 1
fi

# 6. Confirm instance A is actually wired before pointing Radarr-B at it, and
#    sync once so its feed is non-empty — Radarr's indexer test rejects an
#    empty feed the same way Sonarr's does, and a search against nothing
#    would fail for a reason that has nothing to do with the property this
#    test exists to check.
printf 'waiting for sharerr-a to accept its credentials'
doctor_deadline=$((SECONDS + 120))
until "${COMPOSE[@]}" exec -T sharerr-a sharerr doctor >/tmp/sharerr-a-doctor.log 2>&1; do
    if ((SECONDS > doctor_deadline)); then
        echo " — gave up after 120s"
        cat /tmp/sharerr-a-doctor.log
        exit 1
    fi
    printf .
    [[ -t 1 ]] || printf '\n'
    sleep 5
done
echo " ok"
cat /tmp/sharerr-a-doctor.log

"${COMPOSE[@]}" exec -T sharerr-a sharerr sync >/dev/null

# 7. Point Radarr-B at instance A *through Prowlarr*, not a direct Torznab
#    indexer — the first live run of this script grabbed by magnet instead of
#    the .torrent enclosure sharerr also advertises. Evidence: qBittorrent-B's
#    own torrent record showed `has_metadata: false`, and its `magnet_uri`'s
#    `dn=` was the release title, not the torrent's real internal filename
#    (see docs/ROADMAP.md item 11's diagnosis). A magnet can never complete
#    against a private torrent — nothing in the swarm will ever answer its
#    `ut_metadata` request, which is the whole reason the tracker exists —
#    and Radarr's own direct Torznab client has no setting to prefer the
#    .torrent instead: that preference exists only on Prowlarr's indexer,
#    "Prefer Magnet URL" (`torrentBaseSettings.preferMagnetUrl`, `false` by
#    default), so this is also what a real friend's setup should look like.
#
#    Both schemas are fetched rather than hand-typed: a Prowlarr upgrade that
#    renames or reorders `fields` cannot silently stop this from pinning the
#    one setting it exists to pin, and the response is read back to confirm
#    the pin actually took rather than assumed.
schema=$(curl -sf -H "X-Api-Key: $PROWLARR_KEY" \
    "http://127.0.0.1:$PROWLARR_PORT/api/v1/indexer/schema")
generic_torznab=$(jq -c '.[] | select(.name == "Generic Torznab")' <<<"$schema")
if [[ -z $generic_torznab ]]; then
    echo "error: prowlarr's indexer schema has no \"Generic Torznab\" entry" >&2
    exit 1
fi

# Every indexer needs an App Profile — Prowlarr ships one ("Standard") on
# first start, fetched rather than assumed to be id 1: an upgrade or a
# not-quite-fresh config could renumber or rename it.
app_profile_id=$(curl -sf -H "X-Api-Key: $PROWLARR_KEY" \
    "http://127.0.0.1:$PROWLARR_PORT/api/v1/appprofile" | jq -r '.[0].id')
if [[ -z $app_profile_id || $app_profile_id == null ]]; then
    echo "error: prowlarr has no app profile to assign the indexer to" >&2
    exit 1
fi

prowlarr_indexer=$(jq -c \
    --arg url "http://sharerr-a:8477" --arg key "$TORZNAB_KEY" \
    --argjson profile "$app_profile_id" '
    .enable = true | .name = "sharerr-a" | .appProfileId = $profile |
    .fields = (.fields | map(
        if .name == "baseUrl" then .value = $url
        elif .name == "apiKey" then .value = $key
        elif .name == "torrentBaseSettings.appMinimumSeeders" then .value = 1
        elif .name == "torrentBaseSettings.preferMagnetUrl" then .value = false
        else . end
    ))' <<<"$generic_torznab")

# Body and status captured separately — `-w '\n%{http_code}'` plus stripping
# the last line with `sed` silently ate the body whenever curl's own output
# had no trailing newline before the appended status, which is exactly what
# cost time chasing a blank error message the first time an add like this
# ran for real (see the download-client add below, which inherited the fix).
indexer_body=/tmp/prowlarr-indexer-response.json
indexer_status=$(curl -s -o "$indexer_body" -w '%{http_code}' -X POST \
    -H "X-Api-Key: $PROWLARR_KEY" -H 'Content-Type: application/json' \
    --data "$prowlarr_indexer" \
    "http://127.0.0.1:$PROWLARR_PORT/api/v1/indexer")
if ((indexer_status >= 300)); then
    echo "error: prowlarr refused to add sharerr-a as an indexer ($indexer_status):" >&2
    cat "$indexer_body" >&2
    exit 1
fi
if ! jq -e '.fields[] | select(.name == "torrentBaseSettings.preferMagnetUrl") | .value == false' \
    "$indexer_body" >/dev/null
then
    echo "error: prowlarr did not save preferMagnetUrl=false on sharerr-a's indexer:" >&2
    cat "$indexer_body" >&2
    exit 1
fi
echo "prowlarr: sharerr-a added as an indexer, preferMagnetUrl=false confirmed"

app_schema=$(curl -sf -H "X-Api-Key: $PROWLARR_KEY" \
    "http://127.0.0.1:$PROWLARR_PORT/api/v1/applications/schema")
radarr_app=$(jq -c '.[] | select(.implementation == "Radarr")' <<<"$app_schema")
prowlarr_app=$(jq -c \
    --arg base "http://radarr-b:7878" --arg key "$RADARR_B_KEY" '
    .enable = true | .name = "radarr-b" |
    .fields = (.fields | map(
        if .name == "prowlarrUrl" then .value = "http://prowlarr:9696"
        elif .name == "baseUrl" then .value = $base
        elif .name == "apiKey" then .value = $key
        else . end
    ))' <<<"$radarr_app")

app_body=/tmp/prowlarr-application-response.json
app_status=$(curl -s -o "$app_body" -w '%{http_code}' -X POST \
    -H "X-Api-Key: $PROWLARR_KEY" -H 'Content-Type: application/json' \
    --data "$prowlarr_app" \
    "http://127.0.0.1:$PROWLARR_PORT/api/v1/applications")
if ((app_status >= 300)); then
    echo "error: prowlarr refused to add radarr-b as an application ($app_status):" >&2
    cat "$app_body" >&2
    exit 1
fi
echo "prowlarr: radarr-b added as an application"

# Prowlarr pushes the indexer down to a `fullSync` application on save, in
# the background — polled here because radarr-b's own view of its indexers
# is the state the search below actually reads, not prowlarr's say-so. The
# synced copy is named "sharerr-a (Prowlarr)", not "sharerr-a" — Prowlarr's
# own convention for distinguishing an app-managed indexer from one added by
# hand — so this matches on the prefix rather than the exact name.
printf 'waiting for prowlarr to sync sharerr-a down to radarr-b'
sync_deadline=$((SECONDS + 60))
while true; do
    radarr_b_indexers=$(curl -sf -H "X-Api-Key: $RADARR_B_KEY" \
        "http://127.0.0.1:$RADARR_B_PORT/api/v3/indexer" || true)
    jq -e 'any(.[]; .name | startswith("sharerr-a"))' <<<"$radarr_b_indexers" >/dev/null 2>&1 && break
    if ((SECONDS > sync_deadline)); then
        echo " — gave up after 60s"
        echo "${radarr_b_indexers:-<no response from radarr-b>}" >&2
        exit 1
    fi
    printf .
    [[ -t 1 ]] || printf '\n'
    sleep 3
done
echo " ok"

# Instance B's own qBittorrent as Radarr-B's download client — Prowlarr only
# ever manages indexers, never download clients, so this half is still
# registered directly, same as before.
radarr_b_download_client=$(cat <<JSON
{
  "enable": true, "protocol": "torrent", "priority": 1,
  "name": "qbittorrent-b",
  "implementation": "QBittorrent", "implementationName": "qBittorrent",
  "configContract": "QBittorrentSettings",
  "fields": [
    {"name": "host", "value": "qbittorrent-b"},
    {"name": "port", "value": 8080},
    {"name": "username", "value": "admin"},
    {"name": "password", "value": "$QBIT_B_PW"},
    {"name": "movieCategory", "value": "sharerr-grab"}
  ]
}
JSON
)
client_body=/tmp/radarr-b-downloadclient-response.json
client_status=$(curl -s -o "$client_body" -w '%{http_code}' -X POST \
    -H "X-Api-Key: $RADARR_B_KEY" -H 'Content-Type: application/json' \
    --data "$radarr_b_download_client" \
    "http://127.0.0.1:$RADARR_B_PORT/api/v3/downloadclient")
if ((client_status >= 300)); then
    echo "error: radarr-b refused to add qbittorrent-b as a download client ($client_status):" >&2
    cat "$client_body" >&2
    exit 1
fi
echo "radarr-b: qbittorrent-b added as a download client"

# 8. The real thing: trigger Radarr-B's own automatic search, the same
#    command its UI's "Search Now" sends. A match grabs automatically —
#    "automatic" is what distinguishes this from an interactive search a
#    human picks a release from — and hands the release to the download
#    client just registered.
command_response=$(curl -sf -X POST \
    -H "X-Api-Key: $RADARR_B_KEY" -H 'Content-Type: application/json' \
    --data "{\"name\":\"MoviesSearch\",\"movieIds\":[$MOVIE_ID]}" \
    "http://127.0.0.1:$RADARR_B_PORT/api/v3/command")
# Radarr pretty-prints its JSON — `"id": 1`, with a space after the colon —
# which the first live run of this script did not account for and silently
# extracted nothing.
command_id=$(sed -n 's/.*"id":[[:space:]]*\([0-9]*\).*/\1/p' <<<"$command_response" | head -n1)
if [[ -z $command_id ]]; then
    echo "error: radarr-b did not accept the MoviesSearch command: $command_response" >&2
    exit 1
fi

printf 'waiting for radarr-b'"'"'s automatic search to complete'
search_deadline=$((SECONDS + 120))
while true; do
    status_body=$(curl -sf -H "X-Api-Key: $RADARR_B_KEY" \
        "http://127.0.0.1:$RADARR_B_PORT/api/v3/command/$command_id" || true)
    status=$(sed -n 's/.*"status":[[:space:]]*"\([a-z]*\)".*/\1/p' <<<"$status_body" | head -n1)
    [[ $status == completed ]] && break
    if [[ $status == failed ]] || ((SECONDS > search_deadline)); then
        echo " — did not complete (last status: ${status:-unknown})"
        echo "$status_body" >&2
        exit 1
    fi
    printf .
    [[ -t 1 ]] || printf '\n'
    sleep 3
done
echo " ok"

# 9. Wait for qBittorrent-B to actually finish the transfer — a genuine
#    BitTorrent handshake against qbittorrent-a, over the wire, not a mock.
#    Polled rather than a fixed sleep: the file only has to *exist* under the
#    bind mount for the Rust assertion below to have something to compare,
#    and a guess at how long a real handshake plus transfer takes is exactly
#    the kind of thing `wait_for` above already avoids elsewhere in this
#    script.
printf 'waiting for the file to land on instance B'"'"'s disk'
transfer_deadline=$((SECONDS + 60))
while ! find "$STATE/b/downloads" -type f 2>/dev/null | grep -q .; do
    if ((SECONDS > transfer_deadline)); then
        echo " — gave up after 60s"
        echo "nothing under $STATE/b/downloads yet — Radarr grabbed and handed" >&2
        echo "the release to qbittorrent-b, but the torrent transfer itself" >&2
        echo "has not landed a file. qbittorrent-b's own torrent list:" >&2
        # From inside the container, over its own "localhost" — qBittorrent
        # validates the Host header against how it was addressed, so the
        # same login from the host's published port answers 401 regardless
        # of credentials (cost real time to work out once already).
        # shellcheck disable=SC2016
        "${COMPOSE[@]}" exec -T -e QBIT_PW="$QBIT_B_PW" qbittorrent-b sh -c '
            curl -sf -c /tmp/qbit-cookie \
                --data-urlencode "username=admin" --data-urlencode "password=$QBIT_PW" \
                -H "Referer: http://localhost:8080" "http://localhost:8080/api/v2/auth/login" >/dev/null &&
            curl -sf -b /tmp/qbit-cookie "http://localhost:8080/api/v2/torrents/info"
        ' >&2 || echo "  (could not reach qbittorrent-b's API to report its state)" >&2
        break
    fi
    printf .
    [[ -t 1 ]] || printf '\n'
    sleep 3
done
echo " done"

echo "handing off to the Rust assertion"
# `--test e2e_two_instance` targets that one integration test binary
# directly, rather than name-filtering — `cargo test`'s filter matches the
# test *function* name, not the file it lives in, and this file's test name
# does not contain "two_instance" anywhere. Targeting the binary also keeps
# this from ever touching `e2e.rs`'s tests, which need the *other* stack.
cargo test -p sharerr --features e2e --test e2e_two_instance -- --ignored --test-threads=1
