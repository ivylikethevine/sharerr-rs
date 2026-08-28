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
pub(crate) async fn fetch_tagged<'a, E, P, F>(
    client: &'a ArrClient,
    entities: &'a [E],
    tag_id: i64,
    what: &'static str,
    fetch: F,
) -> Result<Vec<(&'a E, P)>>
where
    E: Tagged,
    F: AsyncFn(&'a ArrClient, &'a E) -> Result<P>,
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

    // The futures are built up front rather than inside a stream `map`: a
    // closure there is inferred higher-ranked over the entity borrow, and the
    // `async_trait` wrapper in the binary cannot then prove the walk is `Send`.
    let pending: Vec<_> = tagged.iter().map(|&entity| fetch(client, entity)).collect();
    let fetched: Vec<P> = stream::iter(pending)
        .buffered(DISCOVERY_CONCURRENCY)
        .try_collect()
        .await?;

    Ok(tagged.into_iter().zip(fetched).collect())
}

/// [`fetch_tagged`]'s counterpart for the second half a discovery walk needs
/// once it has its files: index `containers` by id once, then join each file
/// to its container in O(1) rather than a linear scan — a 400-track
/// discography was ~160k comparisons per artist without this. A file whose
/// container id names nothing in `containers` is warned about (under
/// `entity`, naming the tagged parent — the artist, author, or similar — and
/// `what`, naming the kind of container it could not find) and dropped: it
/// exists on disk with a record the *arr app itself no longer lists a parent
/// for, and sharing it would produce a release named after nothing.
///
/// Shared by every walk except Sonarr's: its files join to episode
/// *numbering*, which can point several episodes at one file and has to pick
/// among them, not a single owning container this shape assumes.
pub(crate) fn join_by_parent<'a, C, F>(
    containers: &'a [C],
    files: Vec<F>,
    container_id: impl Fn(&C) -> i64,
    file_parent: impl Fn(&F) -> i64,
    file_id: impl Fn(&F) -> i64,
    entity: &str,
    what: &'static str,
) -> Vec<(&'a C, F)> {
    let by_id: std::collections::HashMap<i64, &'a C> =
        containers.iter().map(|c| (container_id(c), c)).collect();

    files
        .into_iter()
        .filter_map(|file| match by_id.get(&file_parent(&file)).copied() {
            Some(container) => Some((container, file)),
            None => {
                tracing::warn!(
                    entity,
                    file_id = file_id(&file),
                    "file belongs to no listed {what}; skipping"
                );
                None
            }
        })
        .collect()
}

pub use client::ArrClient;
pub use error::{ArrError, Result};
pub use models::SystemStatus;
// Re-exported from core (it moved there for the directory source, which is not
// an *arr app) so existing importers keep compiling unchanged.
pub use sharerr_core::Discovered;
