//! Test-only helpers shared by more than one module's `#[cfg(test)] mod
//! tests`. `vault_in` was hand-rolled byte-for-byte in four places
//! (`gossip`, `lighthouse_client`, `sync::tests`, `commands::doctor`) before
//! landing here.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use secrecy::SecretString;

/// A freshly opened vault backed by a throwaway file under `dir`, keyed by a
/// fixed master password that never touches the process env — see
/// `CLAUDE.md`'s "no tier-1 fixture opens a real vault". Anything under test
/// that takes a plain `&Vault` rather than going through `ServeState` (e.g.
/// `build_arr`/`build_client`/`build_tracker`) can be exercised directly
/// against one of these, no `SHARERR_MASTER_KEY` required.
pub(crate) fn vault_in(dir: &tempfile::TempDir) -> sharerr_store::Vault {
    sharerr_store::Vault::open(dir.path().join("vault.bin"), &SecretString::from("master"))
        .expect("opening a fresh vault file cannot fail")
}
