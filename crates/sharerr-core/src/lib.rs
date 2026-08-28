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
    Discovered, ExternalIds, MediaMeta, MediaSource, MediaSpec, ShareState, SharedItem,
};
pub use paths::{PathError, PathResolver};
