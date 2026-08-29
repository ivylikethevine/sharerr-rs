//! Synthetic fixtures for sharerr's tests.
//!
//! Two rules this crate exists to enforce, both from the project's testing
//! requirements:
//!
//! 1. **No real files and no real titles.** Every show, movie, path, and release
//!    group here is invented. Nothing in the test suite touches a real library.
//! 2. **Deterministic.** Media content comes from a seeded PRNG, so a torrent built
//!    over a fixture has the same info hash on every machine and every run. That is
//!    what makes "piece hashes are stable" an assertion rather than a hope.
//!
//! Dev-dependency only; it is never compiled into a release binary.

pub mod library;
pub mod media;
pub mod mock;
pub mod net;
pub mod secrets;

pub use library::{Library, movie_library, music_library, tv_library};
pub use media::{deterministic_bytes, write_media_file};
