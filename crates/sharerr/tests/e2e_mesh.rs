//! Tier-3 mesh test: the one thing bash cannot check for itself.
//!
//! Opt-in, and inert unless the `e2e` feature is on:
//!
//! ```text
//! ./run_docker_tests_mesh.sh
//! ```
//!
//! which brings up three independent sharerr nodes and one independent
//! lighthouse, meshes every pair so trust-on-first-use binds all three
//! identities, severs the direct A<->C link, rotates node A's advertised
//! endpoint, and asserts — entirely in bash, by reading each node's own
//! peers page — that B picks up the new address directly and C picks it up
//! only through B's relay, then that C rejoins and re-converges after a
//! restart. Driving it by hand is documented in `docker/README.md`'s "The
//! mesh stack" section; the invocation expected there is
//! `cargo test -p sharerr --features e2e --test e2e_mesh -- --ignored --test-threads=1`
//! — targeting the binary directly, the same reason
//! `e2e_two_instance.rs` does.
//!
//! What is left to this file: proving the lighthouse's answer is a real,
//! cryptographically valid record and not one of its fabricated decoys — the
//! one check that genuinely needs `sharerr_lighthouse::verify`, which a bash
//! regex cannot stand in for. A decoy's signature is random bytes and
//! carries no relationship to its pubkey; only a real Ed25519 verification
//! tells the two apart. See `sharerr_lighthouse`'s own module docs for why
//! the lighthouse never distinguishes them in its own response.

#![cfg(feature = "e2e")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use sharerr_lighthouse::{EndpointRecord, hash_key, verify};

/// Must match `docker/compose.mesh.yml`'s own published port and
/// `scripts/run_docker_tests_mesh.sh`'s identical constant.
const LIGHTHOUSE_PORT: u16 = 63878;

/// Must match `run_docker_tests_mesh.sh`'s own `NEW_PORT` — the address it
/// rotates node A onto partway through the script, before this test ever
/// runs. Proving the lighthouse's record carries *this* port, not A's
/// original one, is what confirms A actually re-reported after moving.
const ROTATED_PORT: &str = "9999";

/// The raw key node A issued node C, revealed by `add_peer A ... C` and
/// exported by the script — see its own "Hand off" step. A keeps reporting
/// itself to the lighthouse under every friend's key hash regardless of
/// whether that friend's `gossip_url` is still set (see
/// `lighthouse_client.rs`'s `report`), so this is still the right key to
/// hash even after the script severs the direct A<->C link.
fn key_a_issued_c() -> String {
    std::env::var("SHARERR_E2E_MESH_KEY").unwrap_or_else(|_| {
        panic!(
            "SHARERR_E2E_MESH_KEY is not set — run ./run_docker_tests_mesh.sh, \
             which exports it right before invoking this test"
        )
    })
}

#[tokio::test]
#[ignore = "requires the mesh compose stack; run ./run_docker_tests_mesh.sh"]
async fn the_lighthouse_holds_a_real_record_for_node_a_at_its_rotated_endpoint() {
    let key_hash = hash_key(&key_a_issued_c());
    let url = format!("http://127.0.0.1:{LIGHTHOUSE_PORT}/lighthouse/v1/lookup/{key_hash}");

    let response = reqwest::get(&url)
        .await
        .unwrap_or_else(|e| panic!("GET {url}: {e} — is the mesh stack up?"));
    assert!(
        response.status().is_success(),
        "the lighthouse's lookup route is always 200 by design — a different \
         status means the stack is not the mesh stack, or is not up: {}",
        response.status()
    );

    let record: EndpointRecord = response
        .json()
        .await
        .unwrap_or_else(|e| panic!("lighthouse lookup response did not parse: {e}"));

    // The one check a bash regex cannot make: a decoy's signature is random
    // bytes with no relationship to its pubkey, so only a real Ed25519
    // verification tells a genuine report apart from one fabricated for an
    // unknown or never-reported key hash.
    verify(&record).unwrap_or_else(|reason| {
        panic!(
            "the lighthouse answered with a decoy, not a real record for node A \
             ({reason}) — either A never reported under this key hash, or the \
             script's mesh-and-sever setup did not run before this test"
        )
    });

    assert!(
        record
            .endpoints
            .iter()
            .any(|endpoint| endpoint.addr.ends_with(&format!(":{ROTATED_PORT}"))),
        "the lighthouse's record for A does not carry the rotated port {ROTATED_PORT} — \
         A verified, but has not re-reported since the script moved it: {record:?}"
    );
}
