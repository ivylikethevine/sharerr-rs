//! Persistence for sharerr: the encrypted credential vault and the SQLite store.

pub mod catalog;
pub mod db;
pub mod endpoints;
pub mod peers;
pub mod retention;
pub mod runs;
pub mod swarm;
pub mod transfers;
pub mod users;
pub mod vault;

#[cfg(feature = "fault-injection")]
mod fault;
#[cfg(test)]
mod upgrade;

pub use catalog::ItemOrigin;
pub use db::{FileStamp, SeedingSummary, Store, StoreError};
pub use endpoints::{EndpointKind, ObservedVia, PeerEndpoint};
pub use peers::{Audiences, Peer, PeerLimits, PeerScope, Touch};
pub use retention::Retention;
pub use runs::{RunRecord, RunSummary};
pub use swarm::SwarmSample;
pub use transfers::TransferTotals;

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

/// Fixtures the store's own test modules share.
#[cfg(test)]
pub(crate) mod test_support {
    #![allow(clippy::unwrap_used)]

    use secrecy::SecretString;

    use crate::{Peer, PeerScope, Store};

    /// Add a friend named `label`, seeing everything, keyed `<label>-key` in
    /// lowercase.
    pub(crate) async fn add_peer(store: &Store, label: &str) -> Peer {
        let key = SecretString::from(format!("{}-key", label.to_lowercase()));
        store
            .create_peer(label, &key, PeerScope::All)
            .await
            .unwrap()
    }

    /// An empty in-memory store holding one friend, `label`, and their id.
    pub(crate) async fn store_with_peer(label: &str) -> (Store, i64) {
        let store = Store::open_in_memory().await.unwrap();
        let peer = add_peer(&store, label).await;
        (store, peer.id)
    }
}
