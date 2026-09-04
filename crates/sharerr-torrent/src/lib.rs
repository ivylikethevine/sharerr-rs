//! Torrent construction and tracker resolution.
//!
//! Everything here serves one requirement: **the file is never touched.** A
//! torrent is built by reading the media where it already sits, and the resulting
//! `.torrent` describes it at that exact location under its existing name. Nothing
//! is copied, renamed, or re-linked.
//!
//! Three pieces:
//!
//! * [`TorrentFactory`] — builds the `.torrent`, with its own bencode and
//!   metainfo handling ([`bencode`], [`metainfo`]) rather than a third-party
//!   torrent crate.
//! * [`TrackerProvider`] — decides where it announces.
//! * [`title`] — picks the release title, which travels *alongside* the torrent
//!   rather than inside it. That distinction is load-bearing; see the module docs.

pub mod announce;
pub mod bencode;
pub mod error;
pub mod factory;
pub mod metainfo;
pub mod title;
pub mod tracker;

pub use announce::{
    AnnounceError, AnnounceRequest, AnnounceResponse, Event, InfoHash, SwarmStats, SwarmView,
    Swarms,
};
pub use error::{Result, TorrentError};
pub use factory::{
    BuiltTorrent, Retargeted, TorrentFactory, TorrentRequest, piece_length_for, read_announce,
    read_info_hash, retarget_announce, rewrite_announce, torrent_file_path,
};
pub use metainfo::{MetainfoError, Torrent};
pub use title::{ParsedTitle, humanize, join_title, parse, resolve, synthesize};
pub use tracker::{
    ANNOUNCE_PATH, AnnounceSet, BuiltinTracker, SCRAPE_PATH, TrackerProvider, announce_set_for,
    announce_url, token_from_announce_url,
};
