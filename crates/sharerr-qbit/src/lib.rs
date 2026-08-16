//! qBittorrent WebUI API v2 client.
//!
//! sharerr uses qBittorrent for exactly one thing: seeding files that already
//! exist, from where they already are. Everything in this crate is shaped by the
//! requirement that adding a share must never move, re-link, or delete media — see
//! [`QbitClient::add_torrent`] for the two settings that enforce it.
//!
//! ```no_run
//! # async fn example() -> Result<(), sharerr_qbit::QbitError> {
//! use secrecy::SecretString;
//! use sharerr_client::AddRequest;
//! use sharerr_qbit::QbitClient;
//!
//! let url = "http://localhost:8080".parse().expect("literal url");
//! let qbit = QbitClient::new(&url, "admin", SecretString::from("password"))?;
//!
//! let torrent: Vec<u8> = Vec::new(); // built by sharerr-torrent
//! qbit.add_torrent(
//!     &AddRequest::new(&torrent, "share.torrent", "/downloads/tv/Some Show")
//!         .category("sharerr")
//!         .tags("sharerr"),
//! )
//! .await?;
//! # Ok(())
//! # }
//! ```

mod adapter;
mod client;
mod error;
mod models;
mod prefs;
mod torrents;

pub use client::QbitClient;
pub use error::{QbitError, Result};
pub use models::{Preferences, TorrentFile, TorrentInfo};
// Re-exported because [`QbitClient::add_torrent`] takes it directly.
pub use sharerr_client::AddRequest;
