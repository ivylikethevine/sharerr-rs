//! Building a `.torrent` over a file that already exists, without touching it.

use std::path::{Path, PathBuf};

use crate::error::{Result, TorrentError};
use lava_torrent::torrent::v1::TorrentBuilder;

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
    /// Where peers announce: the current endpoint first, then the recently held
    /// ones as fallback tiers.
    pub announce: &'a crate::AnnounceSet,
}

/// A finished torrent, in memory.
///
/// The filename inside the torrent equals the on-disk basename by design; it is
/// not carried here because nothing reads it back — the `.torrent` itself is the
/// record.
#[derive(Debug, Clone)]
pub struct BuiltTorrent {
    /// Hex-encoded SHA-1 of the info dictionary — the identity qBittorrent uses.
    pub info_hash: String,
    /// The bencoded `.torrent`, ready to hand to a client.
    pub data: Vec<u8>,
    pub size: u64,
}

#[derive(Debug, Default, Clone, Copy)]
/// Creates torrents, backed by `lava_torrent`.
///
/// `lava_torrent` is in maintenance mode; swapping it for hand-rolled bencoding
/// and hashing should cost only this type's one method and no call sites.
pub struct LavaTorrentFactory;

impl LavaTorrentFactory {
    /// Build a torrent describing the file exactly where it already sits,
    /// without moving, renaming, or rewriting anything.
    ///
    /// **Synchronous on purpose.** Building a torrent means SHA-1 over the entire
    /// file, which for a media library is gigabytes of CPU-bound work. An `async
    /// fn` here would invite callers to run it directly on the runtime and stall
    /// every other task. Callers offload it with `tokio::task::spawn_blocking`.
    pub fn create(self, request: &TorrentRequest<'_>) -> Result<BuiltTorrent> {
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

        // A path with no printable filename cannot name the file inside the
        // torrent, so it is rejected before any hashing starts.
        if path.file_name().and_then(|n| n.to_str()).is_none() {
            return Err(TorrentError::NoFileName {
                path: path.to_path_buf(),
            });
        }

        let size = metadata.len();
        let piece_length = piece_length_for(size);

        let mut builder = TorrentBuilder::new(path, piece_length as i64)
            .set_announce(Some(request.announce.primary.to_string()))
            // Friend-to-friend sharing must not leak into the public DHT or PEX.
            // This is the whole reason the tracker exists.
            .set_privacy(true);
        // An announce *list* only when there is genuinely more than one endpoint:
        // BEP 12 clients ignore `announce` the moment the list exists, so a
        // one-entry list would only add bytes and change nothing.
        if let Some(tiers) = request.announce.tier_list() {
            builder = builder.set_announce_list(tiers);
        }
        let torrent = builder.build().map_err(|source| TorrentError::Build {
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

/// The primary announce URL a stored `.torrent` carries.
///
/// This is what decides whether a rotation of the advertised endpoint has left a
/// torrent pointing somewhere stale — see [`rewrite_announce`].
pub fn read_announce(data: &[u8]) -> Result<Option<String>> {
    let torrent = lava_torrent::torrent::v1::Torrent::read_from_bytes(data)
        .map_err(|source| TorrentError::Reparse { source })?;
    Ok(torrent.announce)
}

/// The info hash of a `.torrent` held in memory, lowercase hex.
///
/// The counterpart to [`torrent_file_path`], which names a cache entry by this
/// value: bytes that arrived from somewhere other than [`LavaTorrentFactory`]
/// have to be asked what they actually describe before being filed under a
/// hash. Caching the wrong file under the right name serves a friend a torrent
/// for a different swarm than the one they were pointed at.
pub fn read_info_hash(data: &[u8]) -> Result<String> {
    let torrent = lava_torrent::torrent::v1::Torrent::read_from_bytes(data)
        .map_err(|source| TorrentError::Reparse { source })?;
    Ok(torrent.info_hash())
}

/// Rewrite a stored `.torrent`'s announce URL and tiers, leaving everything else
/// — the info dictionary above all — untouched.
///
/// The announce fields live *outside* the info dictionary, so the info hash is
/// unchanged: the rewritten file still describes the same torrent, every client
/// already seeding it keeps its identity, and only where it announces moves. This
/// is what makes a rotated gluetun port survivable — the feed serves the
/// rewritten file, and a friend who re-downloads it gets the live endpoint.
pub fn rewrite_announce(data: &[u8], announce: &crate::AnnounceSet) -> Result<Vec<u8>> {
    let mut torrent = lava_torrent::torrent::v1::Torrent::read_from_bytes(data)
        .map_err(|source| TorrentError::Reparse { source })?;

    torrent.announce = Some(announce.primary.to_string());
    torrent.announce_list = announce.tier_list();

    torrent
        .encode()
        .map_err(|source| TorrentError::Reencode { source })
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

    /// Build a real `.torrent` over a temp file, so the round-trip functions
    /// are exercised against bytes `lava_torrent` actually produced rather
    /// than a hand-written fixture that could drift from its encoder.
    fn built(dir: &tempfile::TempDir, announce: &crate::AnnounceSet) -> BuiltTorrent {
        let path = dir.path().join("Lanternwick Hollow S01E01.mkv");
        // Deterministic bytes: the info hash has to be stable across machines.
        std::fs::write(&path, vec![7u8; 512 * 1024]).unwrap();

        LavaTorrentFactory
            .create(&TorrentRequest {
                path: &path,
                announce,
            })
            .unwrap()
    }

    fn announce_set(primary: &str, tiers: &[&str]) -> crate::AnnounceSet {
        crate::AnnounceSet {
            primary: primary.parse().unwrap(),
            tiers: tiers.iter().map(|t| t.parse().unwrap()).collect(),
        }
    }

    #[test]
    fn a_built_torrent_reports_the_announce_url_it_was_given() {
        let dir = tempfile::tempdir().unwrap();
        let set = announce_set("http://seed.example:51413/announce/tok", &[]);
        let torrent = built(&dir, &set);

        assert_eq!(
            read_announce(&torrent.data).unwrap().as_deref(),
            Some("http://seed.example:51413/announce/tok")
        );
    }

    /// Bytes filed under the wrong hash serve a friend a torrent for a
    /// different swarm than the one they were pointed at, so the cache name
    /// has to come from the bytes rather than from the caller.
    #[test]
    fn the_info_hash_read_back_matches_the_one_the_builder_reported() {
        let dir = tempfile::tempdir().unwrap();
        let set = announce_set("http://seed.example:51413/announce/tok", &[]);
        let torrent = built(&dir, &set);

        assert_eq!(read_info_hash(&torrent.data).unwrap(), torrent.info_hash);
    }

    /// The property the whole rotation story rests on. The announce fields sit
    /// outside the info dictionary, so moving them must leave the torrent's
    /// identity alone -- otherwise every client already seeding it would lose
    /// the swarm the moment a gluetun port changed.
    #[test]
    fn rewriting_the_announce_url_leaves_the_info_hash_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let before = built(
            &dir,
            &announce_set("http://old.example:51413/announce/a", &[]),
        );

        let moved = announce_set("http://new.example:6881/announce/b", &[]);
        let after = rewrite_announce(&before.data, &moved).unwrap();

        assert_eq!(
            read_info_hash(&after).unwrap(),
            before.info_hash,
            "the rewrite changed the torrent's identity"
        );
        assert_eq!(
            read_announce(&after).unwrap().as_deref(),
            Some("http://new.example:6881/announce/b"),
            "the rewrite did not move the announce URL"
        );
    }

    /// A single-entry announce-list would add bytes and change nothing --
    /// clients ignore `announce` the moment a list is present.
    #[test]
    fn a_rewrite_with_fallback_tiers_emits_a_tier_list_and_one_without_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let before = built(
            &dir,
            &announce_set("http://old.example:51413/announce/a", &[]),
        );

        let single = announce_set(
            "http://new.example:6881/announce/b",
            &["http://new.example:6881/announce/b"],
        );
        let rewritten = rewrite_announce(&before.data, &single).unwrap();
        let parsed = lava_torrent::torrent::v1::Torrent::read_from_bytes(&rewritten).unwrap();
        assert!(
            parsed.announce_list.is_none(),
            "one tier should emit no list"
        );

        let two = announce_set(
            "http://new.example:6881/announce/b",
            &[
                "http://new.example:6881/announce/b",
                "http://old.example:51413/announce/a",
            ],
        );
        let rewritten = rewrite_announce(&before.data, &two).unwrap();
        let parsed = lava_torrent::torrent::v1::Torrent::read_from_bytes(&rewritten).unwrap();
        assert_eq!(parsed.announce_list.unwrap().len(), 2);
    }

    /// These run over whatever a friend or an operator handed us, so malformed
    /// input has to come back as an error rather than a panic.
    #[test]
    fn reading_bytes_that_are_not_a_torrent_is_an_error() {
        assert!(read_info_hash(b"not a torrent at all").is_err());
        assert!(read_announce(b"not a torrent at all").is_err());
        assert!(
            rewrite_announce(
                b"not a torrent at all",
                &announce_set("http://x.example/announce", &[])
            )
            .is_err()
        );
    }

    #[test]
    fn torrent_files_are_named_by_info_hash() {
        let path = torrent_file_path(Path::new("/data/torrents"), "abc123");
        assert_eq!(path, Path::new("/data/torrents/abc123.torrent"));
    }
}
