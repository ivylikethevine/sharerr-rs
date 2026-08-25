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

use std::future::Future;

use futures::stream::{self, StreamExt, TryStreamExt};

/// An *arr entity at the level the sharerr tag is applied — a series, movie,
/// artist, or author.
pub(crate) trait Tagged {
    fn tags(&self) -> &[i64];
}

/// The discovery scaffold every walk shares: keep the entities carrying
/// `tag_id`, run `fetch` over them [`DISCOVERY_CONCURRENCY`] at a time, and hand
/// back each tagged entity paired with what was fetched for it.
///
/// `buffered` preserves input order, which is what makes the zip sound — and
/// it is why the fetch results are collected before any `Discovered` is built:
/// assembling one is pure CPU, and the output order must not depend on which
/// response landed first. `what` names the entity in the scan log line.
pub(crate) async fn fetch_tagged<'a, E, P, F, Fut>(
    client: &'a ArrClient,
    entities: &'a [E],
    tag_id: i64,
    what: &'static str,
    fetch: F,
) -> Result<Vec<(&'a E, P)>>
where
    E: Tagged,
    F: Fn(&'a ArrClient, &'a E) -> Fut,
    Fut: Future<Output = Result<P>>,
{
    let tagged: Vec<&'a E> = entities
        .iter()
        .filter(|e| e.tags().contains(&tag_id))
        .collect();

    tracing::debug!(
        total = entities.len(),
        tagged = tagged.len(),
        "{what} scanned for the sharerr tag"
    );

    let fetched: Vec<P> = stream::iter(tagged.iter().copied())
        .map(|entity| fetch(client, entity))
        .buffered(DISCOVERY_CONCURRENCY)
        .try_collect()
        .await?;

    Ok(tagged.into_iter().zip(fetched).collect())
}

pub use client::ArrClient;
pub use error::{ArrError, Result};
pub use models::SystemStatus;
// Re-exported from core (it moved there for the directory source, which is not
// an *arr app) so existing importers keep compiling unchanged.
pub use sharerr_core::Discovered;
