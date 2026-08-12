//! Torrent construction and tracker resolution.
//!
//! Everything here serves one requirement: **the file is never touched.** A
//! torrent is built by reading the media where it already sits, and the resulting
//! `.torrent` describes it at that exact location under its existing name. Nothing
//! is copied, renamed, or re-linked.
//!
//! Three pieces:
//!
//! * [`TorrentFactory`] — builds the `.torrent`. Behind a trait because
//!   `lava_torrent` is in maintenance mode.
//! * [`TrackerProvider`] — decides where it announces.
//! * [`title`] — picks the release title, which travels *alongside* the torrent
//!   rather than inside it. That distinction is load-bearing; see the module docs.

pub mod error;
pub mod factory;
pub mod title;
pub mod tracker;

pub use error::{Result, TorrentError};
pub use factory::{
    BuiltTorrent, LavaTorrentFactory, TorrentFactory, TorrentRequest, piece_length_for,
    torrent_file_path,
};
pub use title::{ParsedTitle, parse, resolve, synthesize};
pub use tracker::{BuiltinTracker, QbitEmbeddedTracker, TrackerProvider};
