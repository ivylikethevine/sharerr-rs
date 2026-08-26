//! The two-instance tier-2 test: the assertion that justifies the whole
//! heavier stack, not local-add safety (`e2e.rs` already covers that) but
//! the actual friend-to-friend loop.
//!
//! Opt-in, and inert unless the `e2e` feature is on:
//!
//! ```text
//! ./run_docker_tests_two_instance.sh
//! ```
//!
//! which brings up two independent sharerr+Radarr+qBittorrent stacks, shares
//! a file from instance A, registers instance A as a real Torznab indexer on
//! instance B's Radarr, triggers a real automatic search and grab, and ends
//! by running this. Driving it by hand is documented in
//! `docker/README.md`'s "The two-instance stack" section; the invocation
//! expected there is
//! `cargo test -p sharerr --features e2e -- --ignored two_instance --test-threads=1`.
//!
//! The one assertion: after a real Radarr automatic search, a real grab, and
//! a real BitTorrent transfer between two separate containers, the bytes
//! instance B's qBittorrent saved are byte-for-byte identical to instance
//! A's copy. Nothing here re-checks local-add safety, path mapping, or any
//! of `e2e.rs`'s ground — this is the one property only a second, genuinely
//! independent instance can prove.

#![cfg(feature = "e2e")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/sharerr.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// Where instance B's qBittorrent actually saves what it downloads — a bind
/// mount (unlike every other qBittorrent in this project), specifically so
/// this test can read the bytes directly rather than needing a docker exec.
fn instance_b_downloads() -> PathBuf {
    repo_root().join("docker/state-two-instance/b/downloads")
}

/// Every regular file under `root`, recursively.
fn files_under(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(t) if t.is_dir() => stack.push(path),
                Ok(t) if t.is_file() => out.push(path),
                _ => {}
            }
        }
    }

    out
}

/// The first byte offset at which two files differ, for a failure message
/// that says *where* rather than just *that*.
fn first_difference(a: &[u8], b: &[u8]) -> Option<usize> {
    a.iter().zip(b.iter()).position(|(x, y)| x != y)
}

#[test]
#[ignore = "requires the two-instance compose stack; run ./run_docker_tests_two_instance.sh"]
fn a_grabbed_release_lands_byte_identical_on_the_requesting_friends_disk() {
    let media_root = repo_root().join("tests/fixtures/media");
    let original = sharerr_testkit::library::movie_files(&media_root)
        .into_iter()
        .next()
        .expect("gen-fixtures has not run — see ./run_docker_tests_two_instance.sh");
    let original_bytes = std::fs::read(&original.disk_path).unwrap_or_else(|e| {
        panic!(
            "reading instance A's original copy at {}: {e}",
            original.disk_path.display()
        )
    });

    let downloads = instance_b_downloads();
    let candidates = files_under(&downloads);
    assert!(
        !candidates.is_empty(),
        "nothing under {} — the stack is not up, or the grab has not \
         landed yet; see ./run_docker_tests_two_instance.sh",
        downloads.display()
    );

    let matched_size = candidates
        .iter()
        .find(|p| std::fs::metadata(p).is_ok_and(|m| m.len() == original_bytes.len() as u64));
    let Some(downloaded) = matched_size else {
        panic!(
            "no file under {} matches the original's size ({} bytes) — found: {:?}",
            downloads.display(),
            original_bytes.len(),
            candidates
        );
    };

    let downloaded_bytes = std::fs::read(downloaded)
        .unwrap_or_else(|e| panic!("reading {}: {e}", downloaded.display()));
    if let Some(at) = first_difference(&original_bytes, &downloaded_bytes) {
        panic!(
            "{} was moved, rewritten, or corrupted in transit — first differs from \
             {} at byte {at}",
            downloaded.display(),
            original.disk_path.display()
        );
    }
}
