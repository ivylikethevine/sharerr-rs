//! Persistence for sharerr: the encrypted credential vault and the SQLite store.

pub mod db;
pub mod endpoints;
pub mod peers;
pub mod runs;
pub mod swarm;
pub mod users;
pub mod vault;

pub use db::{SeedingSummary, Store, StoreError};
pub use endpoints::{EndpointKind, ObservedVia, PeerEndpoint};
pub use peers::{Peer, PeerScope};
pub use runs::{RunRecord, RunSummary};
pub use swarm::SwarmSample;

pub use vault::{Vault, VaultError, master_key_from_env};

/// `N` bytes from the OS CSPRNG.
///
/// The one entropy call in the store — vault salts, record nonces, and password
/// salts all come through here, so they cannot diverge in how they are drawn or
/// how a failure surfaces. Callers wrap the error in their own type.
pub(crate) fn random_array<const N: usize>() -> Result<[u8; N], getrandom::Error> {
    let mut buf = [0u8; N];
    getrandom::fill(&mut buf)?;
    Ok(buf)
}
