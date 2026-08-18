//! Torrent construction over real (synthetic) files on disk.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use lava_torrent::torrent::v1::Torrent;
use sharerr_testkit::{deterministic_bytes, write_media_file};
use sharerr_torrent::{
    AnnounceSet, LavaTorrentFactory, TorrentError, TorrentRequest, piece_length_for,
    read_announce, rewrite_announce,
};
use url::Url;

fn announce() -> AnnounceSet {
    AnnounceSet::single(Url::parse("http://sharerr.example:9000/announce").unwrap())
}

fn build(path: &Path) -> sharerr_torrent::BuiltTorrent {
    let announce = announce();
    LavaTorrentFactory
        .create(&TorrentRequest {
            path,
            announce: &announce,
        })
        .unwrap()
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
    let dir = tempfile::tempdir().unwrap();
    let path = dir
        .path()
        .join("tv/Lanternwick Hollow/lanternwick.s02e01.mkv");
    write_media_file(&path, 768 * 1024, 1).unwrap();

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
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lanternwick.s02e01.mkv");
    write_media_file(&path, 4096, 1).unwrap();

    let built = build(&path);

    let decoded = Torrent::read_from_bytes(&built.data).unwrap();
    assert_eq!(decoded.name, "lanternwick.s02e01.mkv");
    assert!(
        decoded.files.is_none(),
        "a single file must not become a multi-file torrent"
    );
}

/// Friend-to-friend sharing must not leak into the public DHT or PEX. This flag is
/// the only thing preventing that, so it gets its own assertion.
#[test]
fn torrents_are_private() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file.mkv");
    write_media_file(&path, 4096, 1).unwrap();

    let decoded = Torrent::read_from_bytes(&build(&path).data).unwrap();
    assert!(decoded.is_private(), "the private flag was not set");
}

#[test]
fn the_announce_url_is_embedded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file.mkv");
    write_media_file(&path, 4096, 1).unwrap();

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
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();

    let a = first.path().join("file.mkv");
    let b = second.path().join("file.mkv");
    write_media_file(&a, 768 * 1024, 99).unwrap();
    write_media_file(&b, 768 * 1024, 99).unwrap();

    assert_eq!(build(&a).info_hash, build(&b).info_hash);
    assert_eq!(build(&a).data, build(&b).data);
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
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file.mkv");
    // Deliberately not a whole number of pieces: the tail piece is short.
    write_media_file(&path, 700 * 1024, 1).unwrap();

    let built = build(&path);
    let decoded = Torrent::read_from_bytes(&built.data).unwrap();

    let expected = (700 * 1024_u64).div_ceil(piece_length_for(built.size));
    assert_eq!(decoded.pieces.len() as u64, expected);
    assert_eq!(decoded.length as u64, built.size);
}

#[test]
fn a_missing_file_is_reported_rather_than_panicking() {
    let dir = tempfile::tempdir().unwrap();
    let announce = announce();
    let path = dir.path().join("does-not-exist.mkv");

    let err = LavaTorrentFactory
        .create(&TorrentRequest {
            path: &path,
            announce: &announce,
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

    let err = LavaTorrentFactory
        .create(&TorrentRequest {
            path: dir.path(),
            announce: &announce,
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
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file.mkv");
    write_media_file(&path, 4096, 1).unwrap();

    let announce = tiered(&[
        "http://203.0.113.9:41234/announce",
        "http://static.example:8477/announce",
    ]);
    let built = LavaTorrentFactory
        .create(&TorrentRequest {
            path: &path,
            announce: &announce,
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
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file.mkv");
    write_media_file(&path, 768 * 1024, 7).unwrap();

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
