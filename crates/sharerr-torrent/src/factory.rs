//! Building a `.torrent` over a file that already exists, without touching it.

use std::path::{Path, PathBuf};

use std::collections::BTreeMap;
use std::io::Read;

use crate::bencode::Value;
use crate::error::{Result, TorrentError};
use crate::metainfo::Torrent;
use sha1::{Digest, Sha1};
use sharerr_core::MediaMeta;

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
    /// What the file is, when known — written into the torrent's `comment` field
    /// for whoever opens the `.torrent` by hand. `None` writes no comment at all
    /// rather than an empty one.
    pub media: Option<&'a sharerr_core::MediaMeta>,
    /// Whether to set BEP 27's private flag. See
    /// `sharerr_core::config::SeedingConfig::private` for what turning this
    /// off costs: DHT/PEX become available, so revoking a friend no longer
    /// removes them from the swarm.
    pub private: bool,
}

/// What to build a multi-file torrent over: one media file and the sidecar files
/// beside it, still exactly where they sit.
///
/// A multi-file torrent's data lives at `<save path>/<root_name>/<file>`, so
/// `root_name` must be the name of the directory holding the files **as the
/// client sees it**, and the client's save path is that directory's parent.
#[derive(Debug, Clone)]
pub struct SidecarRequest<'a> {
    /// The media file, as **sharerr** sees it. Hashed first.
    pub path: &'a Path,
    /// The names of the files to include beside it, in hashing order.
    pub sidecars: &'a [String],
    pub root_name: &'a str,
    pub announce: &'a crate::AnnounceSet,
    pub media: Option<&'a sharerr_core::MediaMeta>,
    pub private: bool,
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
/// Creates v1 torrents over files exactly where they sit: one file, or one
/// media file and its sidecars.
///
/// Hashing and bencoding are done in this crate (see [`crate::bencode`] and
/// [`crate::metainfo`]); the output is byte-identical to what the
/// `lava_torrent` crate this used to wrap produced, which the
/// `info_hashes_are_stable_across_implementations` test pins.
pub struct TorrentFactory;

impl TorrentFactory {
    /// Build a torrent describing the file exactly where it already sits,
    /// without moving, renaming, or rewriting anything.
    ///
    /// **Synchronous on purpose.** Building a torrent means SHA-1 over the entire
    /// file, which for a media library is gigabytes of CPU-bound work. An `async
    /// fn` here would invite callers to run it directly on the runtime and stall
    /// every other task. Callers offload it with `tokio::task::spawn_blocking`.
    pub fn create(self, request: &TorrentRequest<'_>) -> Result<BuiltTorrent> {
        let path = request.path;
        let (size, name) = source_file(path)?;

        let piece_length = piece_length_for(size);
        let pieces = hash_pieces(path, &[], piece_length)?;

        let mut info = base_info(name, piece_length, pieces, request.private);
        info.insert(b"length".to_vec(), int_of(size));

        Ok(finish(
            path,
            info,
            request.announce,
            request.media,
            piece_length,
            size,
        ))
    }

    /// [`Self::create`] for a media file and its sidecars: a multi-file torrent
    /// over them, moving and rewriting nothing. Synchronous for the same reason.
    pub fn create_with_sidecars(self, request: &SidecarRequest<'_>) -> Result<BuiltTorrent> {
        let path = request.path;
        let (mut size, name) = source_file(path)?;

        let dir = path.parent().unwrap_or(Path::new(""));
        let others: Vec<PathBuf> = request.sidecars.iter().map(|n| dir.join(n)).collect();

        let file_entry = |name: &str, length: u64| {
            let mut entry = BTreeMap::new();
            entry.insert(b"length".to_vec(), int_of(length));
            entry.insert(b"path".to_vec(), Value::List(vec![Value::string(name)]));
            Value::Dict(entry)
        };
        let mut files = vec![file_entry(name, size)];
        for (name, other) in request.sidecars.iter().zip(&others) {
            let length = std::fs::metadata(other)
                .map_err(|source| TorrentError::Unreadable {
                    path: other.clone(),
                    source,
                })?
                .len();
            size = size.saturating_add(length);
            files.push(file_entry(name, length));
        }

        let piece_length = piece_length_for(size);
        let pieces = hash_pieces(path, &others, piece_length)?;

        // `create`'s fields, with `files` for `length` and the directory's name
        // for the file's.
        let mut info = base_info(request.root_name, piece_length, pieces, request.private);
        info.insert(b"files".to_vec(), Value::List(files));

        Ok(finish(
            path,
            info,
            request.announce,
            request.media,
            piece_length,
            size,
        ))
    }
}

/// The size and name of the file a torrent is built over. A path with no printable
/// filename cannot name the file inside the torrent, so it is rejected before any
/// hashing starts.
fn source_file(path: &Path) -> Result<(u64, &str)> {
    let metadata = std::fs::metadata(path).map_err(|source| TorrentError::Unreadable {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(TorrentError::NotAFile {
            path: path.to_path_buf(),
        });
    }
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return Err(TorrentError::NoFileName {
            path: path.to_path_buf(),
        });
    };
    Ok((metadata.len(), name))
}

fn int_of(value: u64) -> Value {
    Value::Int(i64::try_from(value).unwrap_or(i64::MAX))
}

/// The info dictionary's fields shared by every torrent: exactly what BEP 3 needs,
/// plus BEP 27's private flag when `private` asks for it.
///
/// Friend-to-friend sharing defaults to not leaking into the public DHT or PEX; that
/// flag is the whole reason the tracker exists, and an operator has to opt out of it
/// explicitly (see `sharerr_core::config::SeedingConfig::private`). Nothing else goes
/// in here: the info hash is a function of the *file*, and adding metadata to this
/// dictionary would make two torrents over one unchanged file stop matching, breaking
/// the "reuse the torrent that already covers this file" path that keeps sharerr from
/// re-adding media.
fn base_info(
    name: &str,
    piece_length: u64,
    pieces: Vec<u8>,
    private: bool,
) -> BTreeMap<Vec<u8>, Value> {
    let mut info = BTreeMap::new();
    info.insert(b"name".to_vec(), Value::string(name));
    info.insert(b"piece length".to_vec(), int_of(piece_length));
    info.insert(b"pieces".to_vec(), Value::Bytes(pieces));
    // Omitted entirely when off, not written as `Value::Int(0)`: present-and-
    // zero and absent are different bencodings, so writing a literal zero
    // would still change the info hash relative to a build that never
    // mentioned the key. Absent is the conventional encoding for "not private".
    if private {
        info.insert(b"private".to_vec(), Value::Int(1));
    }
    info
}

/// Wrap a finished info dictionary in the announce fields and comment.
fn finish(
    path: &Path,
    info: BTreeMap<Vec<u8>, Value>,
    announce: &crate::AnnounceSet,
    media: Option<&MediaMeta>,
    piece_length: u64,
    size: u64,
) -> BuiltTorrent {
    let mut extra = BTreeMap::new();
    // `comment` lives outside the info dictionary by BEP 3, which is also
    // what keeps it from moving the info hash.
    if let Some(comment) = media.and_then(describe) {
        extra.insert(b"comment".to_vec(), Value::string(&comment));
    }

    let torrent = Torrent {
        announce: Some(announce.primary.to_string()),
        // An announce *list* only when there is genuinely more than one
        // endpoint: BEP 12 clients ignore `announce` the moment the list
        // exists, so a one-entry list would only add bytes and change
        // nothing.
        announce_list: announce.tier_list(),
        info,
        extra,
    };

    let info_hash = torrent.info_hash();
    let data = torrent.encode();

    tracing::debug!(
        file = %path.display(),
        info_hash = %info_hash,
        piece_length,
        size,
        "built torrent"
    );

    BuiltTorrent {
        info_hash,
        data,
        size,
    }
}

/// SHA-1 every piece of the file at `path`, then of each of `others` as one
/// continuous stream (pieces span file boundaries, per BEP 3), concatenated, in
/// order.
///
/// Read in piece-sized chunks rather than mapping the whole file: a media
/// library's files are gigabytes, and this runs on a blocking thread whose
/// memory footprint should not scale with the file.
fn hash_pieces(path: &Path, others: &[PathBuf], piece_length: u64) -> Result<Vec<u8>> {
    let io = |source| TorrentError::Build {
        path: path.to_path_buf(),
        source,
    };
    let mut stream: Box<dyn Read> = Box::new(std::fs::File::open(path).map_err(io)?);
    for other in others {
        let file = std::fs::File::open(other).map_err(|source| TorrentError::Build {
            path: other.clone(),
            source,
        })?;
        stream = Box::new(stream.chain(file));
    }
    let mut reader = std::io::BufReader::new(stream);
    let chunk = usize::try_from(piece_length).unwrap_or(usize::MAX);
    let mut buf = vec![0u8; chunk];
    let mut pieces = Vec::new();
    loop {
        // A piece is `piece_length` bytes except the last, so keep reading
        // until the buffer is full or the file ends.
        let mut filled = 0;
        while filled < chunk {
            let n = reader.read(&mut buf[filled..]).map_err(io)?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        if filled == 0 {
            break;
        }
        pieces.extend_from_slice(&Sha1::digest(&buf[..filled]));
        if filled < chunk {
            break;
        }
    }
    Ok(pieces)
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

/// Rewrite a stored `.torrent`'s announce URL and tiers, leaving everything else
/// — the info dictionary above all — untouched.
///
/// The announce fields live *outside* the info dictionary, so the info hash is
/// unchanged: the rewritten file still describes the same torrent, every client
/// already seeding it keeps its identity, and only where it announces moves. This
/// is what makes a rotated gluetun port survivable — the feed serves the
/// rewritten file, and a friend who re-downloads it gets the live endpoint.
pub fn rewrite_announce(data: &[u8], announce: &crate::AnnounceSet) -> Result<Vec<u8>> {
    let mut torrent = Torrent::read_from_bytes(data)?;

    torrent.announce = Some(announce.primary.to_string());
    torrent.announce_list = announce.tier_list();

    Ok(torrent.encode())
}

/// The outcome of [`retarget_announce`]: either the `.torrent` already
/// pointed at `announce.primary`, or it now does.
#[derive(Debug)]
pub enum Retargeted {
    /// The parsed announce already matched `announce.primary`. Nothing was
    /// re-encoded — a caller with nothing further to do can skip a write
    /// entirely, and one that still needs the bytes for another reason
    /// (adopting a torrent sharerr did not cache before) already has its own
    /// input buffer in scope to reuse.
    Current {
        /// The parse, for a caller that wants its info hash: computing one means
        /// hashing the piece list, and the common caller only compares the URL.
        torrent: Torrent,
        /// The torrent's recorded content length — the file's size at the
        /// moment this torrent was built. A caller can compare it against
        /// `fs::metadata` on the file as it stands now to detect an
        /// in-place upgrade under the same path, with no second parse and
        /// no re-hash.
        length: u64,
    },
    /// The announce did not match; here is the rewritten, re-encoded
    /// `.torrent` and the announce it carried before.
    Updated {
        previous: Option<String>,
        torrent: Torrent,
        data: Vec<u8>,
        /// The torrent's recorded content length — see the `Current`
        /// variant's `length` field.
        length: u64,
    },
}

/// Whether a stored `.torrent` already announces to `announce.primary`, and
/// [`rewrite_announce`] if not, as one parse instead of two.
///
/// Every caller that needs "is this stale, and if so what should replace
/// it" was paying to bencode-parse the same `.torrent` twice — once to read
/// the current announce, once inside `rewrite_announce` to write the new
/// one — which for a large file means walking its whole piece-hash list a
/// second time just to compare one URL. This reads it once: the parse
/// comes back for a caller that needs more of it (announce fields live outside
/// the info dictionary, so rewriting them never moves the hash — see
/// `sharerr::sync::seed::Seeder::adopt`), and the rewrite itself only happens when the
/// comparison actually calls for it.
pub fn retarget_announce(data: &[u8], announce: &crate::AnnounceSet) -> Result<Retargeted> {
    let mut torrent = Torrent::read_from_bytes(data)?;
    let previous = torrent.announce.clone();
    // The content's total size when the torrent was built: the file's, or the
    // sum of a multi-file torrent's. A missing or negative value is not producible by anything sharerr
    // writes; `0` just means a corrupt/hand-edited torrent reads as size zero,
    // which never matches a real file and so still triggers the rebuild its
    // caller falls back to on a mismatch.
    let length = torrent
        .length()
        .and_then(|length| u64::try_from(length).ok())
        .unwrap_or(0);

    if previous.as_deref() == Some(announce.primary.as_str()) {
        return Ok(Retargeted::Current { torrent, length });
    }

    torrent.announce = Some(announce.primary.to_string());
    torrent.announce_list = announce.tier_list();
    let data = torrent.encode();

    Ok(Retargeted::Updated {
        previous,
        torrent,
        data,
        length,
    })
}

/// One human-readable line describing the file, for the torrent's `comment`.
///
/// Space-separated rather than dotted: this is read by a person opening the
/// `.torrent`, not parsed by an *arr, and the release title is already carrying
/// the parseable form. `None` when nothing is known, so no comment is written at
/// all — an empty comment field is worse than none.
fn describe(media: &MediaMeta) -> Option<String> {
    // The audio pair is formatted rather than raw, because this line is the one
    // place a person reads it: `24-bit` and `48 kHz` are how a person writes what
    // `24` and `48000` mean. The feeds still publish the raw numbers.
    let bit_depth = media
        .audio_bit_depth
        .as_deref()
        .map(|bits| format!("{bits}-bit"));
    let sample_rate = media.sample_rate_khz();

    let parts: Vec<&str> = [
        media.resolution.as_deref(),
        media.video_codec.as_deref(),
        media.dynamic_range.as_deref(),
        media.audio_codec.as_deref(),
        media.audio_channels.as_deref(),
        bit_depth.as_deref(),
        sample_rate.as_deref(),
        media.runtime.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect();

    (!parts.is_empty()).then(|| parts.join(" "))
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

    /// The ladder itself, at both sides of every step: a changed info hash
    /// for an unchanged file is a new swarm, so the steps must not move.
    #[test]
    fn piece_lengths_step_exactly_at_each_threshold() {
        const KIB: u64 = 1024;
        const MIB: u64 = 1024 * KIB;
        for (size, expected) in [
            (512 * MIB, 256 * KIB),
            (512 * MIB + 1, 512 * KIB),
            (1024 * MIB, 512 * KIB),
            (1024 * MIB + 1, MIB),
            (2048 * MIB, MIB),
            (2048 * MIB + 1, 2 * MIB),
            (4096 * MIB, 2 * MIB),
            (4096 * MIB + 1, 4 * MIB),
            (8192 * MIB, 4 * MIB),
            (8192 * MIB + 1, 8 * MIB),
            (16384 * MIB, 8 * MIB),
            (16384 * MIB + 1, 16 * MIB),
        ] {
            assert_eq!(piece_length_for(size), expected, "size {size}");
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
    /// are exercised against bytes the factory actually produced rather than
    /// a hand-written fixture that could drift from its encoder.
    fn built(dir: &tempfile::TempDir, announce: &crate::AnnounceSet) -> BuiltTorrent {
        let path = dir.path().join("Lanternwick Hollow S01E01.mkv");
        // Deterministic bytes: the info hash has to be stable across machines.
        std::fs::write(&path, vec![7u8; 512 * 1024]).unwrap();

        TorrentFactory
            .create(&TorrentRequest {
                path: &path,
                announce,
                media: None,
                private: true,
            })
            .unwrap()
    }

    fn announce_set(primary: &str, tiers: &[&str]) -> crate::AnnounceSet {
        crate::AnnounceSet {
            primary: primary.parse().unwrap(),
            tiers: tiers.iter().map(|t| t.parse().unwrap()).collect(),
        }
    }

    /// The info hash is a torrent's identity in every client already seeding
    /// it and the name of every cached `.torrent` on disk, so the bytes this
    /// factory writes must never change between releases. These values were
    /// recorded from the `lava_torrent`-backed implementation this replaced
    /// and cross-checked against an independent encoder; a mismatch here
    /// means every existing share would be re-added under a new identity.
    #[test]
    fn info_hashes_are_stable_across_implementations() {
        let dir = tempfile::tempdir().unwrap();
        let built = built(
            &dir,
            &announce_set("http://seed.example:51413/announce/tok", &[]),
        );
        assert_eq!(built.info_hash, "5eeab2c54e0d08a30dfc821c34d0d7f38ba14629");
        assert_eq!(built.data.len(), 201);
        assert_eq!(
            hex::encode(sha2::Sha256::digest(&built.data)),
            "292dba8c8c72ced76d463d592ac6e2c1b12430f977c6403d2e76cf30fd12b54c"
        );
    }

    /// The invariant the whole comment feature is subordinate to.
    ///
    /// The info hash identifies the *file*. `Seeder::seed` reuses an existing
    /// torrent when one already covers a path, `set_seeding` records the hash as
    /// the item's identity, and withdrawal finds the torrent by it. Were the
    /// comment written inside the info dictionary, every one of those would start
    /// missing as soon as a file's metadata was learned — a re-probe would
    /// silently mint a second identity for media already being seeded.
    #[test]
    fn a_comment_does_not_move_the_info_hash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Lanternwick Hollow S01E01.mkv");
        std::fs::write(&path, vec![7u8; 512 * 1024]).unwrap();
        let set = announce_set("http://seed.example:51413/announce/tok", &[]);

        let bare = TorrentFactory
            .create(&TorrentRequest {
                path: &path,
                announce: &set,
                media: None,
                private: true,
            })
            .unwrap();
        let described = TorrentFactory
            .create(&TorrentRequest {
                path: &path,
                announce: &set,
                media: Some(&MediaMeta {
                    resolution: Some("1920x1080".to_owned()),
                    video_codec: Some("HEVC".to_owned()),
                    audio_codec: Some("EAC3".to_owned()),
                    audio_channels: Some("5.1".to_owned()),
                    ..MediaMeta::default()
                }),
                private: true,
            })
            .unwrap();

        assert_eq!(
            bare.info_hash, described.info_hash,
            "the comment must live outside the info dictionary"
        );
        // ...and it must actually be there, or the assertion above passes for
        // the wrong reason.
        assert!(described.data.len() > bare.data.len());
        assert!(
            String::from_utf8_lossy(&described.data).contains("1920x1080 HEVC EAC3 5.1"),
            "the comment is written verbatim"
        );
    }

    /// Nothing known means no `comment` key at all — an empty one is a field a
    /// client will render as blank rather than omit.
    #[test]
    fn nothing_known_writes_no_comment_at_all() {
        let dir = tempfile::tempdir().unwrap();
        let set = announce_set("http://seed.example:51413/announce/tok", &[]);
        let torrent = built(&dir, &set);

        assert!(!String::from_utf8_lossy(&torrent.data).contains("comment"));
        assert_eq!(describe(&MediaMeta::default()), None);
    }

    #[test]
    fn a_comment_lists_only_what_is_known() {
        let partial = MediaMeta {
            resolution: Some("1280x720".to_owned()),
            runtime: Some("0:42:11".to_owned()),
            ..MediaMeta::default()
        };
        assert_eq!(describe(&partial).as_deref(), Some("1280x720 0:42:11"));
    }

    /// A music file's comment is built from the fields a video file has none of.
    #[test]
    fn a_comment_describes_audio_in_the_terms_a_person_writes() {
        let track = MediaMeta {
            audio_codec: Some("FLAC".to_owned()),
            audio_channels: Some("2.0".to_owned()),
            audio_bit_depth: Some("24".to_owned()),
            audio_sample_rate: Some("96000".to_owned()),
            ..MediaMeta::default()
        };
        assert_eq!(
            describe(&track).as_deref(),
            Some("FLAC 2.0 24-bit 96 kHz"),
            "the raw hertz belong in the feed, not in a line a person reads"
        );
    }

    #[test]
    fn a_built_torrent_reports_the_announce_url_it_was_given() {
        let dir = tempfile::tempdir().unwrap();
        let set = announce_set("http://seed.example:51413/announce/tok", &[]);
        let torrent = built(&dir, &set);

        assert_eq!(
            Torrent::read_from_bytes(&torrent.data)
                .unwrap()
                .announce
                .as_deref(),
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

        assert_eq!(
            Torrent::read_from_bytes(&torrent.data).unwrap().info_hash(),
            torrent.info_hash
        );
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
            Torrent::read_from_bytes(&after).unwrap().info_hash(),
            before.info_hash,
            "the rewrite changed the torrent's identity"
        );
        assert_eq!(
            Torrent::read_from_bytes(&after)
                .unwrap()
                .announce
                .as_deref(),
            Some("http://new.example:6881/announce/b"),
            "the rewrite did not move the announce URL"
        );
    }

    /// The single-parse fast path: an already-current announce reports
    /// `Current` and does no rewriting.
    #[test]
    fn retargeting_an_already_current_announce_reports_current() {
        let dir = tempfile::tempdir().unwrap();
        let set = announce_set("http://seed.example:51413/announce/tok", &[]);
        let torrent = built(&dir, &set);

        let outcome = retarget_announce(&torrent.data, &set).unwrap();
        match outcome {
            Retargeted::Current {
                torrent: parsed,
                length,
            } => {
                assert_eq!(parsed.info_hash(), torrent.info_hash);
                assert_eq!(length, torrent.size);
            }
            Retargeted::Updated { .. } => panic!("an unchanged announce must report Current"),
        }
    }

    /// The single-parse replacement for reading the announce plus
    /// [`rewrite_announce`]: a stale announce reports `Updated` with the
    /// same previous URL, the same rewritten bytes, and the same untouched
    /// info hash those two calls would have produced separately.
    #[test]
    fn retargeting_a_stale_announce_reports_updated_and_matches_the_two_call_version() {
        let dir = tempfile::tempdir().unwrap();
        let before = built(
            &dir,
            &announce_set("http://old.example:51413/announce/a", &[]),
        );
        let moved = announce_set("http://new.example:6881/announce/b", &[]);

        let outcome = retarget_announce(&before.data, &moved).unwrap();
        let Retargeted::Updated {
            previous,
            torrent,
            data,
            length,
        } = outcome
        else {
            panic!("a changed announce must report Updated");
        };
        assert_eq!(length, before.size);

        assert_eq!(
            previous.as_deref(),
            Some("http://old.example:51413/announce/a")
        );
        assert_eq!(
            torrent.info_hash(),
            before.info_hash,
            "the rewrite moved the identity"
        );
        assert_eq!(
            data,
            rewrite_announce(&before.data, &moved).unwrap(),
            "single-parse and two-call rewrites must produce identical bytes"
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
        let parsed = Torrent::read_from_bytes(&rewritten).unwrap();
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
        let parsed = Torrent::read_from_bytes(&rewritten).unwrap();
        assert_eq!(parsed.announce_list.unwrap().len(), 2);
    }

    /// These run over whatever a friend or an operator handed us, so malformed
    /// input has to come back as an error rather than a panic.
    #[test]
    fn reading_bytes_that_are_not_a_torrent_is_an_error() {
        assert!(Torrent::read_from_bytes(b"not a torrent at all").is_err());
        assert!(Torrent::read_from_bytes(b"not a torrent at all").is_err());
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

    /// A multi-file torrent over a media file and its sidecars: the media file
    /// first, pieces spanning the boundary, and a client finding the data at
    /// `<save path>/<root_name>/<file>`.
    #[test]
    fn a_sidecar_torrent_lists_the_media_first_and_hashes_across_the_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let media_bytes = vec![7u8; 300 * 1024];
        let sub_bytes = vec![9u8; 1024];
        let media = dir.path().join("Show.S01E01.mkv");
        std::fs::write(&media, &media_bytes).unwrap();
        std::fs::write(dir.path().join("Show.S01E01.en.srt"), &sub_bytes).unwrap();
        let set = announce_set("http://seed.example:51413/announce/tok", &[]);

        let built = TorrentFactory
            .create_with_sidecars(&SidecarRequest {
                path: &media,
                sidecars: &["Show.S01E01.en.srt".to_owned()],
                root_name: "Season 01",
                announce: &set,
                media: None,
                private: true,
            })
            .unwrap();

        let torrent = Torrent::read_from_bytes(&built.data).unwrap();
        assert!(!torrent.is_single_file());
        assert!(torrent.is_private());
        assert_eq!(torrent.name(), Some("Season 01"));
        assert_eq!(
            torrent.file_paths(),
            vec!["Show.S01E01.mkv", "Show.S01E01.en.srt"]
        );
        assert_eq!(built.size, 301 * 1024);
        assert_eq!(torrent.length(), Some(301 * 1024));

        let whole: Vec<u8> = [media_bytes, sub_bytes].concat();
        let piece = usize::try_from(piece_length_for(built.size)).unwrap();
        let expected: Vec<u8> = whole
            .chunks(piece)
            .flat_map(|chunk| Sha1::digest(chunk).to_vec())
            .collect();
        assert_eq!(
            torrent.info.get(b"pieces".as_slice()),
            Some(&Value::Bytes(expected)),
            "a client verifying the concatenated files must find these hashes"
        );
    }

    /// Stable across builds, or a friend's copy of the same content and a rebuild
    /// after a lost cache would be different swarms.
    #[test]
    fn a_sidecar_torrent_has_one_identity_for_one_set_of_files() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("a.mkv");
        std::fs::write(&media, vec![1u8; 1024]).unwrap();
        std::fs::write(dir.path().join("a.srt"), vec![2u8; 16]).unwrap();
        let sidecars = ["a.srt".to_owned()];
        let build = |announce: &str, comment: Option<&MediaMeta>| {
            TorrentFactory
                .create_with_sidecars(&SidecarRequest {
                    path: &media,
                    sidecars: &sidecars,
                    root_name: "d",
                    announce: &announce_set(announce, &[]),
                    media: comment,
                    private: true,
                })
                .unwrap()
                .info_hash
        };
        let described = MediaMeta {
            resolution: Some("1920x1080".to_owned()),
            ..MediaMeta::default()
        };
        assert_eq!(
            build("http://a.example/announce", None),
            build("http://b.example/announce", Some(&described))
        );
    }

    #[test]
    fn a_missing_sidecar_is_an_error_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("a.mkv");
        std::fs::write(&media, b"bytes").unwrap();
        let set = announce_set("http://seed.example/announce", &[]);

        let err = TorrentFactory
            .create_with_sidecars(&SidecarRequest {
                path: &media,
                sidecars: &["gone.srt".to_owned()],
                root_name: "d",
                announce: &set,
                media: None,
                private: true,
            })
            .unwrap_err();
        assert!(
            matches!(&err, TorrentError::Unreadable { path, .. } if path.ends_with("gone.srt")),
            "{err:?}"
        );
    }

    #[test]
    fn a_sidecar_torrent_over_a_directory_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let set = announce_set("http://seed.example/announce", &[]);
        let err = TorrentFactory
            .create_with_sidecars(&SidecarRequest {
                path: dir.path(),
                sidecars: &[],
                root_name: "d",
                announce: &set,
                media: None,
                private: true,
            })
            .unwrap_err();
        assert!(matches!(err, TorrentError::NotAFile { .. }), "{err:?}");
    }

    /// A name the torrent cannot carry — bytes that are not UTF-8 — is
    /// refused before any hashing starts, naming the path.
    #[cfg(unix)]
    #[test]
    fn a_file_whose_name_is_not_utf8_is_refused() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join(std::ffi::OsStr::from_bytes(b"movie-\xff.mkv"));
        std::fs::write(&path, b"pretend media bytes").unwrap();
        let announce =
            crate::AnnounceSet::single(url::Url::parse("http://sharerr.example/announce").unwrap());

        let err = TorrentFactory
            .create(&TorrentRequest {
                path: &path,
                announce: &announce,
                media: None,
                private: true,
            })
            .unwrap_err();
        assert!(
            matches!(err, TorrentError::NoFileName { .. }),
            "got {err:?}"
        );
    }
}
