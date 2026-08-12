//! Persistence for sharerr: the encrypted credential vault and the SQLite store.

pub mod db;
pub mod users;
pub mod vault;

pub use db::{RunRecord, RunSummary, Store, StoreError};

pub use vault::{Vault, VaultError, master_key_from_env};
