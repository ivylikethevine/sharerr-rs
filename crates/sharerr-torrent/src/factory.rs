//! Building a `.torrent` over a file that already exists, without touching it.

use std::path::{Path, PathBuf};

use std::collections::BTreeMap;
use std::io::Read;

use crate::bencode::Value;
use crate::error::{Result, TorrentError};
use crate::metainfo::Torrent;
use sha1::{Digest, Sha1};
use sha2::Sha256;
use sharerr_core::MediaMeta;

/// BEP 52's leaf size: every v2 hash tree is built over 16 KiB blocks.
const V2_BLOCK: usize = 16 * 1024;

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
    /// Also write BEP 52's v2 fields, making a hybrid v1/v2 torrent: `meta
    /// version`, a one-file `file tree` with the file's SHA-256 `pieces root`,
    /// and its `piece layers`. The v2 keys live in the info dictionary, so a
    /// hybrid build has a different v1 info hash from a plain one of the same
    /// file; see `sharerr_core::config::SeedingConfig::hybrid`.
    pub hybrid: bool,
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
    /// For a hybrid torrent, the v2 identity its swarm announces under: SHA-256
    /// of the info dictionary truncated to 20 bytes, hex. See
    /// [`Torrent::info_hash_v2`].
    pub info_hash_v2: Option<String>,
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
        let (pieces, v2) = if request.hybrid {
            let (pieces, v2) = hash_pieces_hybrid(path, piece_length)?;
            (pieces, Some(v2))
        } else {
            (hash_pieces(path, &[], piece_length)?, None)
        };

        let mut info = base_info(name, piece_length, pieces, request.private);
        info.insert(b"length".to_vec(), int_of(size));
        let mut extra = BTreeMap::new();
        if let Some(v2) = v2 {
            v2.write_into(name, size, &mut info, &mut extra);
        }

        Ok(finish(
            path,
            info,
            extra,
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

        // v1 only: a hybrid multi-file torrent needs BEP 47 padding files so each
        // file starts on a piece boundary, and a client without BEP 47 writes
        // those as real files beside the media.
        Ok(finish(
            path,
            info,
            BTreeMap::new(),
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
/// re-adding media. The one exception is a hybrid build (`TorrentRequest::hybrid`),
/// whose v2 keys have to be in here by BEP 52 and which is opt-in for exactly that
/// reason.
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

/// Wrap a finished info dictionary in the announce fields and comment, plus
/// whatever other top-level fields `extra` already holds (a hybrid's `piece
/// layers`).
fn finish(
    path: &Path,
    info: BTreeMap<Vec<u8>, Value>,
    mut extra: BTreeMap<Vec<u8>, Value>,
    announce: &crate::AnnounceSet,
    media: Option<&MediaMeta>,
    piece_length: u64,
    size: u64,
) -> BuiltTorrent {
    // `comment` lives outside the info dictionary by BEP 3, which is also
    // what keeps it from moving the info hash.
    if let Some(comment) = media.and_then(describe) {
        extra.insert(b"comment".to_vec(), Value::string(&comment));
    }

    let torrent = Torrent {
        announce: Some(announce.primary().to_string()),
        // An announce *list* only when there is genuinely more than one
        // endpoint: BEP 12 clients ignore `announce` the moment the list
        // exists, so a one-entry list would only add bytes and change
        // nothing.
        announce_list: announce.tier_list(),
        info,
        extra,
    };

    let info_hash = torrent.info_hash();
    let info_hash_v2 = torrent.info_hash_v2();
    let data = torrent.encode();

    tracing::debug!(
        file = %path.display(),
        info_hash = %info_hash,
        info_hash_v2 = info_hash_v2.as_deref().unwrap_or("(v1 only)"),
        piece_length,
        size,
        "built torrent"
    );

    BuiltTorrent {
        info_hash,
        info_hash_v2,
        data,
        size,
    }
}

/// BEP 52's hashes for one file: its `pieces root`, and the `piece layers`
/// entry a file longer than one piece carries.
struct V2Hashes {
    root: Option<[u8; 32]>,
    /// The layer of the file's hash tree whose nodes each cover one piece,
    /// truncated to the file's piece count; `None` for a file within one piece,
    /// whose layer would be the root itself.
    layer: Option<Vec<u8>>,
}

impl V2Hashes {
    /// Write the v2 half of a hybrid single-file torrent: `meta version` and the
    /// one-file `file tree` into `info`, `piece layers` beside it.
    fn write_into(
        &self,
        name: &str,
        size: u64,
        info: &mut BTreeMap<Vec<u8>, Value>,
        extra: &mut BTreeMap<Vec<u8>, Value>,
    ) {
        let mut file = BTreeMap::new();
        file.insert(b"length".to_vec(), int_of(size));
        // An empty file has no blocks to hash, so BEP 52 leaves its root out.
        if let Some(root) = self.root {
            file.insert(b"pieces root".to_vec(), Value::Bytes(root.to_vec()));
        }
        // `file tree` nests directories by name, each file under its name as the
        // empty key: one file at the top level is `{name: {"": {...}}}`.
        let mut entry = BTreeMap::new();
        entry.insert(Vec::new(), Value::Dict(file));
        let mut tree = BTreeMap::new();
        tree.insert(name.as_bytes().to_vec(), Value::Dict(entry));

        info.insert(b"meta version".to_vec(), Value::Int(2));
        info.insert(b"file tree".to_vec(), Value::Dict(tree));

        let mut layers = BTreeMap::new();
        if let (Some(root), Some(layer)) = (self.root, &self.layer) {
            layers.insert(root.to_vec(), Value::Bytes(layer.clone()));
        }
        extra.insert(b"piece layers".to_vec(), Value::Dict(layers));
    }
}

/// [`hash_pieces`] for one file, computing BEP 52's hashes in the same read.
///
/// Each piece's 16 KiB blocks are hashed into a subtree padded with zero
/// leaves to the piece's block count; the piece hashes form the `piece layers`
/// entry and, padded with the hash of an all-zero piece to a power of two,
/// reduce to the `pieces root`. A file within one piece has no layer and its
/// root is the tree over its blocks padded to a power of two, which is the
/// layout a client verifies against.
fn hash_pieces_hybrid(path: &Path, piece_length: u64) -> Result<(Vec<u8>, V2Hashes)> {
    let blocks_per_piece = usize::try_from(piece_length / V2_BLOCK as u64)
        .unwrap_or(1)
        .max(1);
    let mut pieces = Vec::new();
    let mut piece_hashes: Vec<[u8; 32]> = Vec::new();
    let mut first_piece_leaves: Vec<[u8; 32]> = Vec::new();
    for_each_piece(path, &[], piece_length, |piece| {
        pieces.extend_from_slice(&Sha1::digest(piece));
        let leaves: Vec<[u8; 32]> = piece
            .chunks(V2_BLOCK)
            .map(|block| Sha256::digest(block).into())
            .collect();
        if piece_hashes.is_empty() {
            first_piece_leaves.clone_from(&leaves);
        }
        piece_hashes.push(merkle_root(&leaves, blocks_per_piece, [0; 32]));
    })?;

    let v2 = match piece_hashes.len() {
        0 => V2Hashes {
            root: None,
            layer: None,
        },
        1 => V2Hashes {
            root: Some(merkle_root(
                &first_piece_leaves,
                first_piece_leaves.len().next_power_of_two(),
                [0; 32],
            )),
            layer: None,
        },
        count => {
            let zero_piece = merkle_root(&[], blocks_per_piece, [0; 32]);
            let root = merkle_root(&piece_hashes, count.next_power_of_two(), zero_piece);
            V2Hashes {
                root: Some(root),
                layer: Some(piece_hashes.concat()),
            }
        }
    };
    Ok((pieces, v2))
}

/// The root of a binary SHA-256 merkle tree over `leaves`, padded with `pad`
/// to `width` leaves (a power of two, at least `leaves.len()` and 1).
fn merkle_root(leaves: &[[u8; 32]], width: usize, pad: [u8; 32]) -> [u8; 32] {
    let mut layer: Vec<[u8; 32]> = leaves.to_vec();
    layer.resize(width.max(1), pad);
    while layer.len() > 1 {
        layer = layer
            .chunks(2)
            .map(|pair| {
                let mut hasher = Sha256::new();
                hasher.update(pair[0]);
                hasher.update(pair.get(1).unwrap_or(&pair[0]));
                hasher.finalize().into()
            })
            .collect();
    }
    layer[0]
}

/// SHA-1 every piece of the file at `path`, then of each of `others` as one
/// continuous stream (pieces span file boundaries, per BEP 3), concatenated, in
/// order.
///
/// Read in piece-sized chunks rather than mapping the whole file: a media
/// library's files are gigabytes, and this runs on a blocking thread whose
/// memory footprint should not scale with the file.
fn hash_pieces(path: &Path, others: &[PathBuf], piece_length: u64) -> Result<Vec<u8>> {
    let mut pieces = Vec::new();
    for_each_piece(path, others, piece_length, |piece| {
        pieces.extend_from_slice(&Sha1::digest(piece));
    })?;
    Ok(pieces)
}

/// Read the file at `path`, then each of `others`, as one stream in
/// `piece_length` chunks, handing each piece to `on_piece` in order. Every piece
/// is full except the last.
fn for_each_piece(
    path: &Path,
    others: &[PathBuf],
    piece_length: u64,
    mut on_piece: impl FnMut(&[u8]),
) -> Result<()> {
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
    let chunk = usize::try_from(piece_length).unwrap_or(usize::MAX);
    let mut buf = vec![0u8; chunk];
    loop {
        // A piece is `piece_length` bytes except the last, so keep reading
        // until the buffer is full or the file ends.
        let mut filled = 0;
        while filled < chunk {
            let n = stream.read(&mut buf[filled..]).map_err(io)?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        if filled == 0 {
            break;
        }
        on_piece(&buf[..filled]);
        if filled < chunk {
            break;
        }
    }
    Ok(())
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

/// The name a torrent's `.torrent` file goes by.
pub fn torrent_file_name(info_hash: &str) -> String {
    format!("{info_hash}.torrent")
}

/// Where sharerr keeps a copy of each `.torrent` it has produced.
pub fn torrent_file_path(dir: &Path, info_hash: &str) -> PathBuf {
    dir.join(torrent_file_name(info_hash))
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

    torrent.announce = Some(announce.primary().to_string());
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
    },
    /// The announce did not match; here is the rewritten, re-encoded
    /// `.torrent` and the announce it carried before.
    Updated {
        previous: Option<String>,
        torrent: Torrent,
        data: Vec<u8>,
    },
}

impl Retargeted {
    /// The parsed torrent, whichever way it came out.
    pub fn torrent(&self) -> &Torrent {
        match self {
            Self::Current { torrent } | Self::Updated { torrent, .. } => torrent,
        }
    }
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
    if previous.as_deref() == Some(announce.primary().as_str()) {
        return Ok(Retargeted::Current { torrent });
    }

    torrent.announce = Some(announce.primary().to_string());
    torrent.announce_list = announce.tier_list();
    let data = torrent.encode();

    Ok(Retargeted::Updated {
        previous,
        torrent,
        data,
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
                hybrid: false,
            })
            .unwrap()
    }

    fn announce_set(primary: &str) -> crate::AnnounceSet {
        crate::AnnounceSet::single(primary.parse().unwrap())
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
            &announce_set("http://seed.example:51413/announce/tok"),
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
        let set = announce_set("http://seed.example:51413/announce/tok");

        let bare = TorrentFactory
            .create(&TorrentRequest {
                path: &path,
                announce: &set,
                media: None,
                private: true,
                hybrid: false,
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
                hybrid: false,
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
        let set = announce_set("http://seed.example:51413/announce/tok");
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
        let set = announce_set("http://seed.example:51413/announce/tok");
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
        let set = announce_set("http://seed.example:51413/announce/tok");
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
        let before = built(&dir, &announce_set("http://old.example:51413/announce/a"));

        let moved = announce_set("http://new.example:6881/announce/b");
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
        let set = announce_set("http://seed.example:51413/announce/tok");
        let torrent = built(&dir, &set);

        let outcome = retarget_announce(&torrent.data, &set).unwrap();
        match outcome {
            Retargeted::Current { torrent: parsed } => {
                assert_eq!(parsed.info_hash(), torrent.info_hash);
                assert_eq!(parsed.content_length(), torrent.size);
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
        let before = built(&dir, &announce_set("http://old.example:51413/announce/a"));
        let moved = announce_set("http://new.example:6881/announce/b");

        let outcome = retarget_announce(&before.data, &moved).unwrap();
        let Retargeted::Updated {
            previous,
            torrent,
            data,
        } = outcome
        else {
            panic!("a changed announce must report Updated");
        };
        assert_eq!(torrent.content_length(), before.size);

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
        let before = built(&dir, &announce_set("http://old.example:51413/announce/a"));

        let single = announce_set("http://new.example:6881/announce/b");
        let rewritten = rewrite_announce(&before.data, &single).unwrap();
        let parsed = Torrent::read_from_bytes(&rewritten).unwrap();
        assert!(
            parsed.announce_list.is_none(),
            "one tier should emit no list"
        );

        let two = crate::AnnounceSet::from_tiers(vec![
            "http://new.example:6881/announce/b".parse().unwrap(),
            "http://old.example:51413/announce/a".parse().unwrap(),
        ])
        .unwrap();
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
                &announce_set("http://x.example/announce")
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
        let set = announce_set("http://seed.example:51413/announce/tok");

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
                    announce: &announce_set(announce),
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
        let set = announce_set("http://seed.example/announce");

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
        let set = announce_set("http://seed.example/announce");
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
                hybrid: false,
            })
            .unwrap_err();
        assert!(
            matches!(err, TorrentError::NoFileName { .. }),
            "got {err:?}"
        );
    }
}

#[cfg(test)]
mod hybrid_tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use sha2::Sha256;

    fn announce() -> crate::AnnounceSet {
        crate::AnnounceSet::single("http://seed.example:51413/announce/tok".parse().unwrap())
    }

    fn build(path: &Path, hybrid: bool) -> BuiltTorrent {
        let announce = announce();
        TorrentFactory
            .create(&TorrentRequest {
                path,
                announce: &announce,
                media: None,
                private: true,
                hybrid,
            })
            .unwrap()
    }

    fn sha256(parts: &[&[u8]]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        for part in parts {
            hasher.update(part);
        }
        hasher.finalize().into()
    }

    fn pieces_root(torrent: &Torrent, name: &str) -> Vec<u8> {
        torrent.info[b"file tree".as_slice()].as_dict().unwrap()[name.as_bytes()]
            .as_dict()
            .unwrap()[b"".as_slice()]
        .as_dict()
        .unwrap()[b"pieces root".as_slice()]
        .as_bytes()
        .unwrap()
        .to_vec()
    }

    /// A file within one 16 KiB block has a one-leaf tree: its root is the
    /// block's own SHA-256, by BEP 52's definition and nothing of this crate's.
    #[test]
    fn a_one_block_file_has_its_own_hash_as_root_and_no_piece_layer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("short.mkv");
        let data = vec![3u8; 10_000];
        std::fs::write(&path, &data).unwrap();

        let built = build(&path, true);
        let torrent = Torrent::read_from_bytes(&built.data).unwrap();
        assert_eq!(torrent.info[b"meta version".as_slice()].as_int(), Some(2));
        assert_eq!(
            pieces_root(&torrent, "short.mkv"),
            sha256(&[&data]).to_vec()
        );
        assert_eq!(
            torrent.extra[b"piece layers".as_slice()]
                .as_dict()
                .unwrap()
                .len(),
            0,
            "within one piece, no layer"
        );
        // The v1 half is intact and the file still identifies as one file.
        assert_eq!(torrent.length(), Some(10_000));
        assert!(torrent.is_single_file());
        assert!(torrent.is_private());
        assert_eq!(built.info_hash_v2.as_deref().map(str::len), Some(40));
    }

    /// Three blocks pad to a four-leaf tree with zero hashes, worked out here by
    /// hand against the spec rather than through `merkle_root`.
    #[test]
    fn a_file_within_one_piece_pads_its_blocks_to_a_power_of_two() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("three-blocks.mkv");
        let data: Vec<u8> = (0..(2 * V2_BLOCK + 100)).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &data).unwrap();

        let (b0, rest) = data.split_at(V2_BLOCK);
        let (b1, b2) = rest.split_at(V2_BLOCK);
        let zero = [0u8; 32];
        let left = sha256(&[&sha256(&[b0]), &sha256(&[b1])]);
        let right = sha256(&[&sha256(&[b2]), &zero]);
        let expected = sha256(&[&left, &right]);

        let torrent = Torrent::read_from_bytes(&build(&path, true).data).unwrap();
        assert_eq!(pieces_root(&torrent, "three-blocks.mkv"), expected.to_vec());
    }

    /// A file longer than a piece carries a `piece layers` entry keyed by its
    /// root, one 32-byte hash per piece; the root is the tree over those piece
    /// hashes, padded with the hash of an all-zero piece.
    #[test]
    fn a_multi_piece_file_has_a_piece_layer_the_root_is_built_from() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("long.mkv");
        // 256 KiB pieces (the ladder's smallest): 2.5 pieces, so 3 piece hashes
        // padded to 4 at the root.
        let piece = 256 * 1024;
        let data: Vec<u8> = (0..(2 * piece + piece / 2))
            .map(|i| (i % 253) as u8)
            .collect();
        std::fs::write(&path, &data).unwrap();

        let built = build(&path, true);
        let torrent = Torrent::read_from_bytes(&built.data).unwrap();
        let root = pieces_root(&torrent, "long.mkv");
        let layers = torrent.extra[b"piece layers".as_slice()].as_dict().unwrap();
        let layer = layers[&root].as_bytes().unwrap();
        assert_eq!(layer.len(), 3 * 32, "one hash per piece, none for padding");

        // Each piece hash is the tree over its 16 blocks; the half piece pads
        // with zero leaves.
        let blocks_per_piece = piece / V2_BLOCK;
        let piece_hash = |bytes: &[u8]| {
            let leaves: Vec<[u8; 32]> = bytes.chunks(V2_BLOCK).map(|b| sha256(&[b])).collect();
            merkle_root(&leaves, blocks_per_piece, [0; 32])
        };
        let hashes: Vec<[u8; 32]> = data.chunks(piece).map(piece_hash).collect();
        assert_eq!(layer, hashes.concat().as_slice());
        let zero_piece = merkle_root(&[], blocks_per_piece, [0; 32]);
        let expected_root = sha256(&[
            &sha256(&[&hashes[0], &hashes[1]]),
            &sha256(&[&hashes[2], &zero_piece]),
        ]);
        assert_eq!(root, expected_root.to_vec());
    }

    /// The v2 keys sit inside the info dictionary, so a hybrid build is a
    /// different v1 identity from a plain one; a plain build has no v2 at all.
    #[test]
    fn hybrid_is_opt_in_and_changes_the_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("movie.mkv");
        std::fs::write(&path, vec![7u8; 512 * 1024]).unwrap();

        let plain = build(&path, false);
        let hybrid = build(&path, true);
        assert_eq!(plain.info_hash_v2, None);
        assert!(hybrid.info_hash_v2.is_some());
        assert_ne!(plain.info_hash, hybrid.info_hash);
        let torrent = Torrent::read_from_bytes(&plain.data).unwrap();
        assert!(!torrent.info.contains_key(b"meta version".as_slice()));
        assert!(!torrent.extra.contains_key(b"piece layers".as_slice()));
    }

    /// Rewriting the announce leaves `piece layers` in place and both
    /// identities where they were.
    #[test]
    fn a_hybrid_survives_an_announce_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("movie.mkv");
        std::fs::write(&path, vec![9u8; 300 * 1024]).unwrap();
        let built = build(&path, true);

        let moved = crate::AnnounceSet::single("http://moved.example/announce".parse().unwrap());
        let rewritten = rewrite_announce(&built.data, &moved).unwrap();
        let torrent = Torrent::read_from_bytes(&rewritten).unwrap();
        assert_eq!(
            torrent.announce.as_deref(),
            Some("http://moved.example/announce")
        );
        assert_eq!(torrent.info_hash(), built.info_hash);
        assert_eq!(torrent.info_hash_v2(), built.info_hash_v2);
        assert!(torrent.extra.contains_key(b"piece layers".as_slice()));
    }
}
