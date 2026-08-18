//! Sonarr and Radarr v3 clients.
//!
//! The two APIs are near-identical, so there is one transport ([`ArrClient`]) and
//! two resource walks. Discovery answers a single question: *which files carry the
//! sharerr tag, and what does the friend's Sonarr/Radarr need to know about them?*
//!
//! ```no_run
//! # async fn example() -> Result<(), sharerr_arr::ArrError> {
//! use secrecy::SecretString;
//! use sharerr_core::MediaSource;
//!
//! let url = "http://localhost:8989".parse().expect("literal url");
//! let client = sharerr_arr::ArrClient::new(MediaSource::Sonarr, &url, SecretString::from("key"))?;
//! for item in client.discover("sharerr").await? {
//!     println!("{} -> {}", item.spec, item.arr_path.display());
//! }
//! # Ok(())
//! # }
//! ```

mod client;
mod error;
mod lidarr;
mod models;
mod radarr;
mod readarr;
mod sonarr;

/// How many tagged entities a discovery walk has in flight at once.
///
/// Sonarr, Lidarr and Readarr each need one or two extra round trips per tagged
/// entity, and awaiting them one entity at a time made a large library pay that
/// latency serially — a 200-series Sonarr is 200 sequential round trips, and
/// discovery is the long pole of a sync pass. Bounded rather than unbounded so a
/// big library does not open hundreds of sockets against an *arr that is very
/// possibly running on the same small box.
pub(crate) const DISCOVERY_CONCURRENCY: usize = 8;

pub use client::ArrClient;
pub use error::{ArrError, Result};
pub use models::SystemStatus;
// Re-exported from core (it moved there for the directory source, which is not
// an *arr app) so existing importers keep compiling unchanged.
pub use sharerr_core::Discovered;
