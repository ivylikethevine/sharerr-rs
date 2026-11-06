//! Shared domain types, configuration, and path handling for sharerr.
//!
//! This crate deliberately has no I/O dependencies: it describes *what* sharerr
//! works with, not how it talks to anything. Service clients live in
//! `sharerr-arr` / `sharerr-qbit`, persistence in `sharerr-store`.

mod macros;

pub mod config;
pub mod endpoint;
pub mod model;
pub mod paths;

pub use config::Config;
pub use endpoint::EndpointError;
pub use model::{
    CatalogEntry, Discovered, ExternalIds, MediaMeta, MediaSource, MediaSpec, SceneMeta,
    ShareState, SharedItem,
};
pub use paths::{PathError, PathResolver};

/// `n` as the `i64` SQLite stores, saturating rather than wrapping: a clamped
/// figure is less wrong than a negative one.
pub fn saturating_i64<T: TryInto<i64>>(n: T) -> i64 {
    n.try_into().unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::saturating_i64;

    #[test]
    fn saturating_i64_keeps_what_fits_and_clamps_what_does_not() {
        assert_eq!(saturating_i64(734_003_200_u64), 734_003_200);
        assert_eq!(saturating_i64(u64::MAX), i64::MAX);
    }
}
