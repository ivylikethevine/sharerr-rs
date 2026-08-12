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
mod models;
mod radarr;
mod sonarr;

use std::path::PathBuf;

use sharerr_core::{ExternalIds, MediaSource, MediaSpec, ShareState, SharedItem};

pub use client::ArrClient;
pub use error::{ArrError, Result};
pub use models::SystemStatus;

/// One tagged file, as the *arr app describes it.
///
/// This is everything discovery can know. The fields a [`SharedItem`] adds — the
/// database id, the info hash, the share state — belong to sharerr, not to Sonarr,
/// and are filled in by the reconciliation loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    pub source: MediaSource,
    /// Series or movie id within the *arr app.
    pub source_id: i64,
    /// `episodeFile` / `movieFile` id — the natural key sharerr diffs against.
    pub file_id: i64,
    pub spec: MediaSpec,
    /// The path **exactly as the *arr app reported it**, before any mapping is
    /// applied. Stored verbatim so that changing a path mapping later does not
    /// orphan existing rows.
    pub arr_path: PathBuf,
    pub size: u64,
    pub ids: ExternalIds,
    /// The original scene release name, when the file was imported from one. This
    /// is the best possible release title — it is already known to parse.
    pub scene_name: Option<String>,
}

impl Discovered {
    /// Stable identity of the underlying file, matching [`SharedItem::key`].
    pub fn key(&self) -> (MediaSource, i64) {
        (self.source, self.file_id)
    }

    /// Promote to a storable item. The release title is resolved separately
    /// because it needs rules this crate does not own.
    pub fn into_shared_item(self, release_title: String) -> SharedItem {
        SharedItem {
            id: None,
            source: self.source,
            source_id: self.source_id,
            file_id: self.file_id,
            spec: self.spec,
            release_title,
            arr_path: self.arr_path,
            size: self.size,
            ids: self.ids,
            info_hash: None,
            state: ShareState::Pending,
            last_error: None,
        }
    }
}
