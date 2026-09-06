#!/usr/bin/env bash
#
# Tier 3: bring up three independent sharerr nodes and one independent
# lighthouse, mesh all three pairwise so trust-on-first-use binds every
# identity, then sever the direct A<->C link and rotate A's advertised
# endpoint — the one scenario no other tier can exercise: does the
# reconnection/gossip system actually recover a friend's new address through
# a mutual friend and through the lighthouse, not just in a unit test against
# a stub. See docker/README.md's "The mesh stack" section.
#
#   ./scripts/run_docker_tests_mesh.sh
#
# Deliberately no *arr app and no torrent client in this stack — tiers 1-2
# already prove the media path; this one proves the mesh. See
# docker/compose.mesh.yml's own header for the full reasoning, including why
# the three gossip/lighthouse intervals are seconds here rather than the
# production default.
#
# Safe to re-run: every step is idempotent, and the trap tears the stack down
# however the script exits.

set -euo pipefail

# One level up from this script's own `scripts/` directory — see
# run_docker_tests.sh's comment on the same line for why `readlink -f` matters.
cd "$(dirname "$(readlink -f "$0")")/.."

COMPOSE=(docker compose -f docker/compose.mesh.yml)

# Must match docker/compose.mesh.yml's own published ports, and
# LIGHTHOUSE_PORT must match crates/sharerr/tests/e2e_mesh.rs's identical
# constant — the Rust assertion has no other way to find this stack.
LIGHTHOUSE_PORT=63878
PORT_A=63481
PORT_B=63482
PORT_C=63483

SETUP_PW=sharerr-test-admin-password
STATE=docker/state-mesh

# See run_docker_tests.sh's own copy of this function for the reasoning —
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
# `DEBUG_NO_TEARDOWN=1 ./run_docker_tests_mesh.sh` skips the trap, leaving the
# stack up for a live post-mortem — `docker compose -f docker/compose.mesh.yml
# logs sharerr-b`, the peers page on any of the three published ports, and so
# on. Clean up by hand afterwards with the same command `teardown` runs.
[[ -n ${DEBUG_NO_TEARDOWN:-} ]] || trap teardown EXIT

# Unchanged from run_docker_tests.sh's own copy — see its comment for the
# full reasoning.
wait_for() {
    local name=$1 port=$2 path=$3 accept=${4:-'^2[0-9][0-9]$'} deadline=$((SECONDS + 120)) code
    printf 'waiting for %s' "$name"
    while true; do
        code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 \
            "http://127.0.0.1:$port$path" 2>/dev/null || true)
        [[ $code =~ $accept ]] && break
        if ((SECONDS > deadline)); then
            echo " — gave up after 120s (last status: ${code:-no response})"
            "${COMPOSE[@]}" logs "$name" | tail -30
            return 1
        fi
        printf .
        [[ -t 1 ]] || printf '\n'
        sleep 2
    done
    echo " ok"
}

# Claim a fresh instance's operator account, the same `/setup` POST
# run_docker_tests.sh and run_docker_tests_two_instance.sh both use. The
# cookie jar is this node's session for the rest of the script.
claim() {
    local node=$1 port=$2
    curl -sf -c "/tmp/sharerr-mesh-cookies-$node" \
        --data-urlencode "username=admin" \
        --data-urlencode "password=$SETUP_PW" \
        --data-urlencode "confirm=$SETUP_PW" \
        "http://127.0.0.1:$port/setup" -o /dev/null
}

# Sign back in after a restart. Sessions live in an in-process map
# (`web/auth.rs`), never persisted to the store, so a `docker compose
# restart` — which keeps the data volume but starts a fresh process — leaves
# every cookie minted before it pointing at a session that no longer exists.
# The account itself does survive (it lives in the store, not in memory), so
# this is a login, never another `/setup`.
sign_in_again() {
    local node=$1 port=$2
    curl -sf -c "/tmp/sharerr-mesh-cookies-$node" \
        --data-urlencode "username=admin" \
        --data-urlencode "password=$SETUP_PW" \
        "http://127.0.0.1:$port/login" -o /dev/null
}

# Add a friend labelled $label on $node, and reveal the key this node just
# issued them. Prints "<peer-id> <key>" on success.
#
# The id is scraped from the *same* response `add()` renders — the "Where
# friends are" section's `<h3>{label}</h3>` heading is immediately followed
# by that friend's own `/peers/{id}/gossip` form, so the nearest such id
# after the matching heading is this friend's row. `add()` rejects a
# duplicate label (400), which is the one way this can fail against a fresh
# stack: a leftover peer from a previous, uncleanly-torn-down run.
add_peer() {
    local node=$1 port=$2 label=$3 body key id
    body=$(curl -sf -b "/tmp/sharerr-mesh-cookies-$node" \
        --data-urlencode "label=$label" \
        --data-urlencode "scope=all" \
        "http://127.0.0.1:$port/peers")
    key=$(sed -n 's/.*<code>\([^<]*\)<\/code>.*/\1/p' <<<"$body" | head -n1)
    id=$(grep -A10 "<h3>$label</h3>" <<<"$body" | grep -oP '/peers/\K[0-9]+(?=/gossip)' | head -n1)
    if [[ -z $key || -z $id ]]; then
        echo "error: node $node could not add friend $label (key=${key:-<none>} id=${id:-<none>})" >&2
        return 1
    fi
    echo "$id $key"
}

# Configure the outbound half of a friendship: where their sharerr is, and
# the key they issued this node. An empty `$url` (and empty `$key`) clears
# it — `store.set_peer_gossip_url`'s `Some("")` maps to `None` — which is how
# the script severs the A<->C link later without deleting either peer row or
# forgetting the pubkey each side already bound.
set_gossip() {
    local node=$1 port=$2 id=$3 url=$4 key=$5
    curl -sf -b "/tmp/sharerr-mesh-cookies-$node" \
        --data-urlencode "url=$url" \
        --data-urlencode "key=$key" \
        "http://127.0.0.1:$port/peers/$id/gossip" -o /dev/null
}

# The most recent endpoint this node's peers page shows for $label, and how
# it was learned (direct / gossip / lighthouse / restored) — the one field
# every scenario below actually asserts on. Prints "<addr> <via>", or nothing
# if $label has no recorded endpoint yet.
#
# peers.html renders the address and the "(kind, seen, via)" hint on two
# separate template lines, so each is extracted with its own single-line-safe
# pattern rather than one pattern spanning both — a `sed` expression assuming
# they shared a line matched nothing here on the first pass of this script.
peer_endpoint() {
    local node=$1 port=$2 label=$3 body block addr via
    body=$(curl -sf -b "/tmp/sharerr-mesh-cookies-$node" "http://127.0.0.1:$port/peers")
    block=$(grep -A8 "<h3>$label</h3>" <<<"$body")
    addr=$(grep -o '<code>[^<]*</code>' <<<"$block" | head -n1 | sed 's/<code>//;s#</code>##')
    [[ -z $addr ]] && return 0
    via=$(grep -oP '\([a-z]+, [^,]+, \K[a-z]+(?=\))' <<<"$block" | head -n1)
    echo "$addr $via"
}

# 1. Fresh, live copies of each node's config — never the checked-in
#    template directories themselves. See compose.mesh.yml's own header:
#    step 6 below rewrites node A's copy in place through a real settings
#    POST, and that must land on a throwaway file, not the tracked one.
remove_state
mkdir -p "$STATE/config-a" "$STATE/config-b" "$STATE/config-c"
cp docker/config-mesh-a/sharerr.toml "$STATE/config-a/sharerr.toml"
cp docker/config-mesh-b/sharerr.toml "$STATE/config-b/sharerr.toml"
cp docker/config-mesh-c/sharerr.toml "$STATE/config-c/sharerr.toml"

# 2. Bring the mesh up. No fixtures, no seeding — see compose.mesh.yml's own
#    header for why this stack carries neither an *arr app nor a torrent
#    client.
"${COMPOSE[@]}" up -d --build

wait_for lighthouse "$LIGHTHOUSE_PORT" /lighthouse/v1/health
wait_for sharerr-a "$PORT_A" /health
wait_for sharerr-b "$PORT_B" /health
wait_for sharerr-c "$PORT_C" /health

# 3. Claim all three.
claim A "$PORT_A"
claim B "$PORT_B"
claim C "$PORT_C"

# 4. Mesh every pair — A<->B, B<->C, and (deliberately, unlike the line
#    topology a permanent deployment might use) A<->C too. Trust-on-first-use
#    only binds a peer's pubkey once their sharerr has authenticated directly
#    against a credential this node issued them (see gossip.rs's module
#    docs), so proving the *lighthouse recovery* path later — C finding A
#    again after their direct link is gone — needs A and C to have already
#    gossiped directly at least once. A permanent line topology could never
#    reach that state; a temporarily-full mesh that gets one edge cut can.
echo "meshing all three pairs"
read -r ID_B_FOR_A KEY_B_FOR_A <<<"$(add_peer B "$PORT_B" A)"
read -r ID_B_FOR_C KEY_B_FOR_C <<<"$(add_peer B "$PORT_B" C)"
read -r ID_A_FOR_B KEY_A_FOR_B <<<"$(add_peer A "$PORT_A" B)"
read -r ID_C_FOR_B KEY_C_FOR_B <<<"$(add_peer C "$PORT_C" B)"
read -r ID_A_FOR_C KEY_A_FOR_C <<<"$(add_peer A "$PORT_A" C)"
read -r ID_C_FOR_A KEY_C_FOR_A <<<"$(add_peer C "$PORT_C" A)"

set_gossip A "$PORT_A" "$ID_A_FOR_B" "http://sharerr-b:8477" "$KEY_B_FOR_A"
set_gossip B "$PORT_B" "$ID_B_FOR_A" "http://sharerr-a:8477" "$KEY_A_FOR_B"
set_gossip B "$PORT_B" "$ID_B_FOR_C" "http://sharerr-c:8477" "$KEY_C_FOR_B"
set_gossip C "$PORT_C" "$ID_C_FOR_B" "http://sharerr-b:8477" "$KEY_B_FOR_C"
set_gossip A "$PORT_A" "$ID_A_FOR_C" "http://sharerr-c:8477" "$KEY_C_FOR_A"
set_gossip C "$PORT_C" "$ID_C_FOR_A" "http://sharerr-a:8477" "$KEY_A_FOR_C"

# 5. Let the mesh settle: every pair exchanges directly (binding every pubkey
#    via TOFU) and every node reports itself to the lighthouse under each
#    friend's key hash. `SHARERR_GOSSIP__EXCHANGE_SECS=3` in the compose file
#    means several passes fit inside this wait comfortably — the production
#    default (900s) would make this a fifteen-minute sleep instead.
printf 'letting the mesh settle'
settle_deadline=$((SECONDS + 60))
while true; do
    a_sees_b=$(peer_endpoint A "$PORT_A" B)
    a_sees_c=$(peer_endpoint A "$PORT_A" C)
    b_sees_a=$(peer_endpoint B "$PORT_B" A)
    b_sees_c=$(peer_endpoint B "$PORT_B" C)
    c_sees_a=$(peer_endpoint C "$PORT_C" A)
    c_sees_b=$(peer_endpoint C "$PORT_C" B)
    if [[ -n $a_sees_b && -n $a_sees_c && -n $b_sees_a && -n $b_sees_c && -n $c_sees_a && -n $c_sees_b ]]; then
        break
    fi
    if ((SECONDS > settle_deadline)); then
        echo " — gave up after 60s"
        echo "A: B=[$a_sees_b] C=[$a_sees_c]" >&2
        echo "B: A=[$b_sees_a] C=[$b_sees_c]" >&2
        echo "C: A=[$c_sees_a] B=[$c_sees_b]" >&2
        exit 1
    fi
    printf .
    [[ -t 1 ]] || printf '\n'
    sleep 3
done
echo " ok — every pair sees the other directly"

# 6. Sever the direct A<->C link — both directions — without touching either
#    peer row or the pubkey each side already bound in step 4. This is what
#    makes the next two assertions mean something: any update either learns
#    about the other from here on cannot have come from talking to them
#    directly.
echo "severing the direct A<->C link"
set_gossip A "$PORT_A" "$ID_A_FOR_C" "" ""
set_gossip C "$PORT_C" "$ID_C_FOR_A" "" ""

# 7. Rotate A's advertised endpoint — the same `/settings/tracker` POST an
#    operator's browser would send, which rewrites `sharerr.toml` in place
#    and hot-reloads the running config (CLAUDE.md's "The config file is
#    rewritten in place by the web UI"). No container restart needed, and
#    none of B or C's own config changes — this is purely A announcing a new
#    address, the one event the whole reconnection system exists to survive.
#    Must match e2e_mesh.rs's identical constant, which checks the
#    lighthouse ends up with this exact port too.
NEW_PORT=9999
curl -sf -b "/tmp/sharerr-mesh-cookies-A" \
    --data-urlencode "advertised_host=sharerr-a" \
    --data-urlencode "port=$NEW_PORT" \
    --data-urlencode "advertised_url=" \
    --data-urlencode "token=" \
    "http://127.0.0.1:$PORT_A/settings/tracker" -o /dev/null

# 8. The real thing. B is still directly linked to A, so it should pick up
#    the new address on the next exchange pass. C is not — its own view of A
#    can only move if B relays A's updated record (scenario 2, "mesh
#    convergence") or if C's own lighthouse client recovers it once A has
#    been quiet long enough (`SHARERR_LIGHTHOUSE__QUIET_SECS=8`; scenario 4,
#    "endpoint change" via the lighthouse). Either counts as reconnection
#    working — which one wins is a race this script does not care about.
printf 'waiting for the mesh to notice A moved to :%s' "$NEW_PORT"
recover_deadline=$((SECONDS + 60))
while true; do
    b_view=$(peer_endpoint B "$PORT_B" A)
    c_view=$(peer_endpoint C "$PORT_C" A)
    [[ $b_view == *":$NEW_PORT"* && $c_view == *":$NEW_PORT"* ]] && break
    if ((SECONDS > recover_deadline)); then
        echo " — gave up after 60s"
        echo "B's view of A: [$b_view]" >&2
        echo "C's view of A: [$c_view]" >&2
        exit 1
    fi
    printf .
    [[ -t 1 ]] || printf '\n'
    sleep 3
done
echo " ok"
echo "B learned A's new address $(peer_endpoint B "$PORT_B" A | cut -d' ' -f2 | sed 's/^/via /')"
echo "C learned A's new address $(peer_endpoint C "$PORT_C" A | cut -d' ' -f2 | sed 's/^/via /')"

# 9. Restart / rejoin (scenario 3): restart C, and confirm it re-reports and
#    re-converges rather than staying stuck on whatever it knew before it
#    went down. `docker compose restart` is enough to force this — both
#    `gossip::exchange_loop` and `lighthouse_client::sync_loop` run one pass
#    immediately on startup, not only after their first `sleep`. The account
#    itself survives (it lives in the store, on the data volume `restart`
#    keeps) but the session does not (see `sign_in_again`'s own comment), so
#    this is the one place the script logs back in rather than reusing an
#    existing cookie.
echo "restarting sharerr-c to prove rejoin"
"${COMPOSE[@]}" restart sharerr-c
wait_for sharerr-c "$PORT_C" /health
sign_in_again C "$PORT_C"

printf 'waiting for C to rejoin and re-converge with B'
rejoin_deadline=$((SECONDS + 60))
while true; do
    c_view_of_b=$(peer_endpoint C "$PORT_C" B)
    [[ -n $c_view_of_b ]] && break
    if ((SECONDS > rejoin_deadline)); then
        echo " — gave up after 60s"
        exit 1
    fi
    printf .
    [[ -t 1 ]] || printf '\n'
    sleep 3
done
echo " ok"

echo
echo "bash-side scenarios passed: mesh convergence, endpoint change, restart/rejoin"

# 10. Hand off "lighthouse discovery" — the one property that needs a real
#    Ed25519 verification, not a string match — to the Rust assertion. The
#    raw key is the same one `add_peer A ... C` revealed above (the key A
#    issued C), which A keeps reporting itself under regardless of whether
#    the A<->C link is up (see lighthouse_client.rs's `report`, which
#    iterates every non-revoked peer's key hash, not just the ones with a
#    live `gossip_url`) — so by now it should carry A's *rotated* endpoint.
#    `SHARERR_E2E_MESH_KEY` is listed in `settings::NON_CONFIG_ENV`, the same
#    as `SHARERR_E2E_COMPOSE`, so an operator who exports it does not get an
#    unrelated startup failure from every other `sharerr` command.
export SHARERR_E2E_MESH_KEY="$KEY_A_FOR_C"
echo "handing off to the Rust assertion"
cargo test -p sharerr --features e2e --test e2e_mesh -- --ignored --test-threads=1
