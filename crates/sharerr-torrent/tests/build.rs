//! Torrent construction over real (synthetic) files on disk.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use sharerr_testkit::{deterministic_bytes, write_media_file};
use sharerr_torrent::{
    AnnounceSet, Torrent, TorrentError, TorrentFactory, TorrentRequest, piece_length_for,
    read_announce, rewrite_announce,
};
use url::Url;

fn announce() -> AnnounceSet {
    AnnounceSet::single(Url::parse("http://sharerr.example:9000/announce").unwrap())
}

fn build(path: &Path) -> sharerr_torrent::BuiltTorrent {
    let announce = announce();
    TorrentFactory
        .create(&TorrentRequest {
            path,
            announce: &announce,
            media: None,
            private: true,
        })
        .unwrap()
}

/// A synthetic media file of `size` bytes at `name` inside a fresh tempdir.
/// The tempdir rides along so the file outlives the call.
fn fixture(name: &str, size: usize, seed: u64) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    write_media_file(&path, size, seed).unwrap();
    (dir, path)
}

fn tiered(urls: &[&str]) -> AnnounceSet {
    let tiers: Vec<Url> = urls.iter().map(|u| Url::parse(u).unwrap()).collect();
    AnnounceSet {
        primary: tiers[0].clone(),
        tiers,
    }
}

#[test]
fn builds_a_torrent_over_a_file_where_it_already_is() {
    let (_dir, path) = fixture(
        "tv/Lanternwick Hollow/lanternwick.s02e01.mkv",
        768 * 1024,
        1,
    );

    let before = std::fs::metadata(&path).unwrap();
    let built = build(&path);

    assert_eq!(built.size, 768 * 1024);
    assert_eq!(piece_length_for(built.size), 256 * 1024);
    assert_eq!(
        built.info_hash.len(),
        40,
        "info hash should be hex-encoded SHA-1"
    );

    // The file must be exactly as it was: same length, same content, same place.
    let after = std::fs::metadata(&path).unwrap();
    assert_eq!(before.len(), after.len());
    assert_eq!(
        std::fs::read(&path).unwrap(),
        deterministic_bytes(1, 768 * 1024)
    );
}

/// The mistake that produces a torrent stuck at 0%: naming it after the release
/// rather than the file. A single-file torrent's `name` *is* the filename a client
/// looks for inside the save path.
#[test]
fn the_torrent_is_named_after_the_file_not_the_release() {
    let (_dir, path) = fixture("lanternwick.s02e01.mkv", 4096, 1);

    let built = build(&path);

    let decoded = Torrent::read_from_bytes(&built.data).unwrap();
    assert_eq!(decoded.name(), Some("lanternwick.s02e01.mkv"));
    assert!(
        decoded.is_single_file(),
        "a single file must not become a multi-file torrent"
    );
}

/// Friend-to-friend sharing must not leak into the public DHT or PEX. This flag is
/// the only thing preventing that, so it gets its own assertion.
#[test]
fn torrents_are_private() {
    let (_dir, path) = fixture("file.mkv", 4096, 1);

    let decoded = Torrent::read_from_bytes(&build(&path).data).unwrap();
    assert!(decoded.is_private(), "the private flag was not set");
}

#[test]
fn the_announce_url_is_embedded() {
    let (_dir, path) = fixture("file.mkv", 4096, 1);

    let decoded = Torrent::read_from_bytes(&build(&path).data).unwrap();
    assert_eq!(
        decoded.announce.as_deref(),
        Some("http://sharerr.example:9000/announce")
    );
}

/// Reproducibility is what lets a friend re-add the same content and get the same
/// torrent, and what makes the fixtures usable as regression anchors.
#[test]
fn identical_content_produces_an_identical_info_hash() {
    let (_first, a) = fixture("file.mkv", 768 * 1024, 99);
    let (_second, b) = fixture("file.mkv", 768 * 1024, 99);

    let (built_a, built_b) = (build(&a), build(&b));
    assert_eq!(built_a.info_hash, built_b.info_hash);
    assert_eq!(built_a.data, built_b.data);
}

#[test]
fn different_content_produces_a_different_info_hash() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a/file.mkv");
    let b = dir.path().join("b/file.mkv");
    write_media_file(&a, 768 * 1024, 1).unwrap();
    write_media_file(&b, 768 * 1024, 2).unwrap();

    assert_ne!(build(&a).info_hash, build(&b).info_hash);
}

#[test]
fn a_different_filename_changes_the_info_hash() {
    // `name` lives inside the info dictionary, so it is part of the identity.
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a/one.mkv");
    let b = dir.path().join("b/two.mkv");
    write_media_file(&a, 4096, 1).unwrap();
    write_media_file(&b, 4096, 1).unwrap();

    assert_ne!(build(&a).info_hash, build(&b).info_hash);
}

#[test]
fn the_piece_count_matches_the_file_size() {
    // Deliberately not a whole number of pieces: the tail piece is short.
    let (_dir, path) = fixture("file.mkv", 700 * 1024, 1);

    let built = build(&path);
    let decoded = Torrent::read_from_bytes(&built.data).unwrap();

    let expected = (700 * 1024_u64).div_ceil(piece_length_for(built.size));
    assert_eq!(decoded.piece_count() as u64, expected);
    assert_eq!(decoded.length(), Some(i64::try_from(built.size).unwrap()));
}

#[test]
fn a_missing_file_is_reported_rather_than_panicking() {
    let dir = tempfile::tempdir().unwrap();
    let announce = announce();
    let path = dir.path().join("does-not-exist.mkv");

    let err = TorrentFactory
        .create(&TorrentRequest {
            path: &path,
            announce: &announce,
            media: None,
            private: true,
        })
        .unwrap_err();

    assert!(
        matches!(&err, TorrentError::Unreadable { .. }),
        "got {err:?}"
    );
    assert!(err.to_string().contains("does-not-exist.mkv"), "{err}");
}

#[test]
fn a_directory_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let announce = announce();

    let err = TorrentFactory
        .create(&TorrentRequest {
            path: dir.path(),
            announce: &announce,
            media: None,
            private: true,
        })
        .unwrap_err();

    assert!(matches!(&err, TorrentError::NotAFile { .. }), "got {err:?}");
}

#[test]
fn piece_length_scales_with_file_size() {
    let small = piece_length_for(100 * 1024 * 1024);
    let large = piece_length_for(40 * 1024 * 1024 * 1024);
    assert!(
        large > small,
        "a 40 GiB file should use larger pieces than a 100 MiB one"
    );
}

// ------------------------------------------------------- announce rotation

/// More than one endpoint must become a BEP 12 announce-list, one URL per tier,
/// current endpoint first — that ordering is what lets a client fall back
/// through recently held addresses after a VPN reconnect.
#[test]
fn multiple_endpoints_become_ordered_announce_tiers() {
    let (_dir, path) = fixture("file.mkv", 4096, 1);

    let announce = tiered(&[
        "http://203.0.113.9:41234/announce",
        "http://static.example:8477/announce",
    ]);
    let built = TorrentFactory
        .create(&TorrentRequest {
            path: &path,
            announce: &announce,
            media: None,
            private: true,
        })
        .unwrap();

    let decoded = Torrent::read_from_bytes(&built.data).unwrap();
    assert_eq!(
        decoded.announce.as_deref(),
        Some("http://203.0.113.9:41234/announce")
    );
    assert_eq!(
        decoded.announce_list,
        Some(vec![
            vec!["http://203.0.113.9:41234/announce".to_owned()],
            vec!["http://static.example:8477/announce".to_owned()],
        ])
    );
}

/// The property the whole rotation story rests on: rewriting the announce
/// fields must not change the info hash, because that hash is the torrent's
/// identity in every client already seeding it.
#[test]
fn rewriting_the_announce_keeps_the_info_hash() {
    let (_dir, path) = fixture("file.mkv", 768 * 1024, 7);

    let built = build(&path);
    assert_eq!(
        read_announce(&built.data).unwrap().as_deref(),
        Some("http://sharerr.example:9000/announce")
    );

    let rotated = tiered(&[
        "http://203.0.113.9:52345/announce",
        "http://sharerr.example:9000/announce",
    ]);
    let rewritten = rewrite_announce(&built.data, &rotated).unwrap();

    let decoded = Torrent::read_from_bytes(&rewritten).unwrap();
    assert_eq!(
        decoded.info_hash(),
        built.info_hash,
        "the info dictionary must be untouched"
    );
    assert_eq!(
        decoded.announce.as_deref(),
        Some("http://203.0.113.9:52345/announce")
    );
    assert_eq!(decoded.announce_list.as_ref().map(Vec::len), Some(2));
    assert!(decoded.is_private(), "the private flag must survive");
}

/// Garbage in must be an error, not a panic — the torrent directory is on disk
/// and anything could have happened to it.
#[test]
fn rewriting_garbage_is_an_error() {
    let err = rewrite_announce(b"not a torrent", &announce()).unwrap_err();
    assert!(matches!(err, TorrentError::Reparse { .. }), "got {err:?}");
}
