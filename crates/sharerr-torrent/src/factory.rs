//! Building a `.torrent` over a file that already exists, without touching it.

use std::path::{Path, PathBuf};

use lava_torrent::torrent::v1::TorrentBuilder;
use url::Url;

use crate::error::{Result, TorrentError};

/// What to build a torrent over.
///
/// Deliberately **no name field**. For a single-file v1 torrent, `info.name` is the
/// filename a client will look for inside the save path. Setting it to a release
/// title — which is the intuitive thing to do, and wrong — makes qBittorrent hunt
/// for a file that does not exist and sit at 0% forever. The name always comes from
/// the path, and the release title travels separately. See [`crate::title`].
#[derive(Debug, Clone)]
pub struct TorrentRequest<'a> {
    /// The file to hash, as **sharerr** sees it (not the *arr or qBittorrent view).
    pub path: &'a Path,
    /// Where peers announce. Every sharerr torrent has exactly one.
    pub announce: &'a Url,
}

/// A finished torrent, in memory.
#[derive(Debug, Clone)]
pub struct BuiltTorrent {
    /// Hex-encoded SHA-1 of the info dictionary — the identity qBittorrent uses.
    pub info_hash: String,
    /// The bencoded `.torrent`, ready to hand to a client.
    pub data: Vec<u8>,
    /// The filename inside the torrent. Equals the on-disk basename, by design.
    pub name: String,
    pub piece_length: u64,
    pub size: u64,
}

/// Creates torrents.
///
/// A trait because `lava_torrent` is in maintenance mode; swapping it for
/// hand-rolled `serde_bencode` + `sha1` should cost one impl and no call sites.
///
/// **Synchronous on purpose.** Building a torrent means SHA-1 over the entire
/// file, which for a media library is gigabytes of CPU-bound work. An `async fn`
/// here would invite callers to run it directly on the runtime and stall every
/// other task. Callers offload it with `tokio::task::spawn_blocking`.
pub trait TorrentFactory: Send + Sync + std::fmt::Debug {
    fn create(&self, request: &TorrentRequest<'_>) -> Result<BuiltTorrent>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct LavaTorrentFactory;

impl TorrentFactory for LavaTorrentFactory {
    fn create(&self, request: &TorrentRequest<'_>) -> Result<BuiltTorrent> {
        let path = request.path;

        let metadata = std::fs::metadata(path).map_err(|source| TorrentError::Unreadable {
            path: path.to_path_buf(),
            source,
        })?;
        if !metadata.is_file() {
            return Err(TorrentError::NotAFile {
                path: path.to_path_buf(),
            });
        }

        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| TorrentError::NoFileName {
                path: path.to_path_buf(),
            })?
            .to_owned();

        let size = metadata.len();
        let piece_length = piece_length_for(size);

        let torrent = TorrentBuilder::new(path, piece_length as i64)
            .set_announce(Some(request.announce.to_string()))
            // Friend-to-friend sharing must not leak into the public DHT or PEX.
            // This is the whole reason the tracker exists.
            .set_privacy(true)
            .build()
            .map_err(|source| TorrentError::Build {
                path: path.to_path_buf(),
                source,
            })?;

        let info_hash = torrent.info_hash();
        let data = torrent.encode().map_err(|source| TorrentError::Encode {
            path: path.to_path_buf(),
            source,
        })?;

        tracing::debug!(
            file = %path.display(),
            info_hash = %info_hash,
            piece_length,
            size,
            "built torrent"
        );

        Ok(BuiltTorrent {
            info_hash,
            data,
            name,
            piece_length,
            size,
        })
    }
}

/// Choose a piece length for a file of `size` bytes.
///
/// The tension: small pieces mean a large `.torrent` (every piece costs 20 bytes of
/// SHA-1), large pieces mean more waste re-downloading a damaged piece. The ladder
/// below keeps the piece count in the low thousands across the whole range of sizes
/// a media library contains, so no `.torrent` grows past a few hundred kilobytes.
///
/// Values are powers of two, as every client expects.
pub fn piece_length_for(size: u64) -> u64 {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;

    // An if-chain rather than a match: range patterns may not contain the
    // parenthesised arithmetic that makes these thresholds readable.
    if size <= 512 * MIB {
        256 * KIB
    } else if size <= 1024 * MIB {
        512 * KIB
    } else if size <= 2048 * MIB {
        MIB
    } else if size <= 4096 * MIB {
        2 * MIB
    } else if size <= 8192 * MIB {
        4 * MIB
    } else if size <= 16384 * MIB {
        8 * MIB
    } else {
        16 * MIB
    }
}

/// Where sharerr keeps a copy of each `.torrent` it has produced.
pub fn torrent_file_path(dir: &Path, info_hash: &str) -> PathBuf {
    dir.join(format!("{info_hash}.torrent"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn piece_lengths_are_powers_of_two() {
        for size in [0u64, 1, 1 << 20, 1 << 30, 1 << 33, 1 << 40, u64::MAX] {
            let length = piece_length_for(size);
            assert!(
                length.is_power_of_two(),
                "{length} for size {size} is not a power of two"
            );
        }
    }

    #[test]
    fn piece_lengths_stay_within_the_documented_range() {
        for size in [0u64, 1 << 20, 1 << 30, 1 << 35, u64::MAX] {
            let length = piece_length_for(size);
            assert!(
                (256 * 1024..=16 * 1024 * 1024).contains(&length),
                "{length}"
            );
        }
    }

    #[test]
    fn piece_length_never_shrinks_as_files_grow() {
        let mut previous = 0;
        for exponent in 0..42 {
            let length = piece_length_for(1u64 << exponent);
            assert!(
                length >= previous,
                "piece length went backwards at 2^{exponent}"
            );
            previous = length;
        }
    }

    /// The reason the ladder exists: every size produces a `.torrent` small enough
    /// to hand around, because the piece count stays bounded.
    #[test]
    fn piece_counts_stay_in_a_sane_range() {
        for gib in [1u64, 2, 4, 8, 16, 32, 64] {
            let size = gib * 1024 * 1024 * 1024;
            let pieces = size.div_ceil(piece_length_for(size));
            assert!(pieces <= 8192, "{gib} GiB would need {pieces} pieces");
        }
    }

    #[test]
    fn torrent_files_are_named_by_info_hash() {
        let path = torrent_file_path(Path::new("/data/torrents"), "abc123");
        assert_eq!(path, Path::new("/data/torrents/abc123.torrent"));
    }
}
