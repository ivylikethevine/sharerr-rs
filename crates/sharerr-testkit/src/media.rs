//! Deterministic synthetic media files.

use std::io::Write;
use std::path::Path;

/// Pseudo-random but reproducible bytes.
///
/// An xorshift64* generator, written out rather than pulled from `rand`, because
/// the guarantee that matters is that *this exact byte sequence* comes back for a
/// given seed — forever, across dependency updates. A crate could change its
/// algorithm in a minor release and silently invalidate every recorded info hash.
///
/// The output must not compress trivially: a file of zeroes would give every
/// fixture of the same length an identical piece hash and hide real bugs.
pub fn deterministic_bytes(seed: u64, len: usize) -> Vec<u8> {
    // Any non-zero seed works; xorshift is degenerate at zero.
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut out = Vec::with_capacity(len);

    while out.len() < len {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let chunk = state.wrapping_mul(0x2545_F491_4F6C_DD1D).to_le_bytes();

        let remaining = len - out.len();
        out.extend_from_slice(&chunk[..remaining.min(chunk.len())]);
    }

    out
}

/// Write a synthetic media file, creating parent directories as needed.
///
/// Sizes in tests are kilobytes, not gigabytes: the point is to exercise the
/// hashing and path plumbing, not to move real volumes of data.
pub fn write_media_file(path: &Path, size: usize, seed: u64) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut file = std::fs::File::create(path)?;
    file.write_all(&deterministic_bytes(seed, size))?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn the_same_seed_always_produces_the_same_bytes() {
        assert_eq!(deterministic_bytes(42, 4096), deterministic_bytes(42, 4096));
    }

    #[test]
    fn different_seeds_produce_different_bytes() {
        assert_ne!(deterministic_bytes(1, 4096), deterministic_bytes(2, 4096));
    }

    #[test]
    fn a_prefix_of_a_longer_stream_matches_the_shorter_one() {
        let long = deterministic_bytes(7, 1024);
        let short = deterministic_bytes(7, 64);
        assert_eq!(&long[..64], &short[..]);
    }

    #[test]
    fn requested_lengths_are_exact_even_when_not_a_multiple_of_the_word_size() {
        for len in [0usize, 1, 7, 8, 9, 1023, 4096] {
            assert_eq!(deterministic_bytes(3, len).len(), len);
        }
    }

    /// A file of zeroes would give every same-length fixture an identical piece
    /// hash, which would quietly defeat the tests that rely on them differing.
    #[test]
    fn output_is_not_trivially_uniform() {
        let bytes = deterministic_bytes(11, 4096);
        let distinct: std::collections::HashSet<_> = bytes.iter().collect();
        assert!(
            distinct.len() > 200,
            "only {} distinct byte values",
            distinct.len()
        );
    }

    #[test]
    fn writing_creates_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tv/Some Show/Season 01/file.mkv");

        write_media_file(&path, 2048, 5).unwrap();

        assert_eq!(std::fs::metadata(&path).unwrap().len(), 2048);
        assert_eq!(std::fs::read(&path).unwrap(), deterministic_bytes(5, 2048));
    }
}
