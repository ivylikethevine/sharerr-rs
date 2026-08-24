//! Tier-2 end-to-end test against the live compose stack.
//!
//! Opt-in, and inert unless the `e2e` feature is on:
//!
//! ```text
//! ./run_docker_tests.sh
//! ```
//!
//! which brings the stack up, seeds Sonarr, Radarr, and (on the plain stack)
//! Lidarr with tagged content, loads the credentials, and ends by running these.
//! Driving it by hand is documented in
//! `docker/README.md`; the invocation these expect is
//! `cargo test -p sharerr --features e2e -- --ignored --test-threads=1`.
//!
//! All three depend on there *being* tagged content — without it `sharerr sync`
//! bails with "no library source could be scanned" — so the seeding step is not optional
//! scaffolding, it is a precondition.
//!
//! The assertion that justifies the whole tier: after a real sync through a real
//! qBittorrent, every media file has the same inode, mtime, and length it started
//! with. Mocks cannot prove that — only a client that genuinely tried to manage
//! the files can.

#![cfg(feature = "e2e")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

/// Which stack these run against.
///
/// Overridden by `run_docker_tests.sh --vpn`, which brings up the same services
/// with qBittorrent inside a VPN container's network namespace. The assertions are
/// identical on purpose — the point is that a different topology does not change
/// what sharerr is supposed to do.
///
/// Listed in `settings::NON_CONFIG_ENV`, or `deny_unknown_fields` would turn this
/// into a startup failure for anyone who exports it.
fn compose_file() -> String {
    std::env::var("SHARERR_E2E_COMPOSE").unwrap_or_else(|_| "docker/compose.test.yml".to_owned())
}

/// Whether the running stack tags music through a Lidarr container.
///
/// Only the plain stack carries one — Transmission, rTorrent, and VPN exist to
/// prove a client/topology concern, not indexer coverage, and duplicating the
/// Lidarr flow into all three would triple that cost for no additional
/// coverage of what those stacks are actually for; see `LIDARR` in
/// `run_docker_tests.sh`. Defaults to `true` so driving this suite by hand
/// against the plain stack, without the script setting the variable, keeps
/// today's assumption.
fn lidarr_configured() -> bool {
    std::env::var("SHARERR_E2E_LIDARR")
        .ok()
        .is_none_or(|v| v != "0")
}

/// Identity of a file in the sense that matters here: *is it still the same file,
/// in the same place, unmodified?*
#[derive(Debug, PartialEq, Eq)]
struct Identity {
    inode: u64,
    mtime: SystemTime,
    len: u64,
}

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/sharerr.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn media_root() -> PathBuf {
    std::env::var("SHARERR_E2E_MEDIA") // listed in settings::NON_CONFIG_ENV
        .map(PathBuf::from)
        .unwrap_or_else(|_| repo_root().join("tests/fixtures/media"))
}

/// Every regular file under `root`, by path relative to it.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Identity> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let entries =
            std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()));

        for entry in entries.flatten() {
            let path = entry.path();
            let meta = entry.metadata().unwrap();

            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                let relative = path.strip_prefix(root).unwrap().to_path_buf();
                out.insert(
                    relative,
                    Identity {
                        inode: meta.ino(),
                        mtime: meta.modified().unwrap(),
                        len: meta.len(),
                    },
                );
            }
        }
    }

    out
}

fn compose(args: &[&str]) -> std::process::Output {
    Command::new("docker")
        .current_dir(repo_root())
        .arg("compose")
        .args(["-f", &compose_file()])
        .args(args)
        .output()
        .expect("docker compose is required for the e2e suite")
}

/// Run a sharerr subcommand inside the running container.
fn sharerr(args: &[&str]) -> std::process::Output {
    let mut full = vec!["exec", "-T", "sharerr", "sharerr"];
    full.extend_from_slice(args);
    compose(&full)
}

fn require_stack_running() {
    let out = compose(&["ps", "--status", "running", "--services"]);
    let services = String::from_utf8_lossy(&out.stdout);
    assert!(
        services.contains("sharerr"),
        "the compose stack is not running — see docker/README.md.\nstdout: {services}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[ignore = "requires the compose stack; run with --ignored"]
fn doctor_passes_against_the_live_stack() {
    require_stack_running();

    let out = sharerr(&["doctor"]);
    println!("{}", String::from_utf8_lossy(&out.stdout));
    assert!(
        out.status.success(),
        "doctor failed against the live stack:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The proof that "never move files" holds against a real torrent client.
#[test]
#[ignore = "requires the compose stack; run with --ignored"]
fn a_real_sync_never_moves_or_rewrites_the_library() {
    require_stack_running();
    let media = media_root();

    let before = snapshot(&media);
    assert!(
        !before.is_empty(),
        "no fixtures under {} — run gen-fixtures",
        media.display()
    );

    let out = sharerr(&["sync"]);
    let report = String::from_utf8_lossy(&out.stdout);
    println!("{report}");
    assert!(
        out.status.success(),
        "sync failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Without this the test passes vacuously: a sync that discovers nothing also
    // moves nothing. The count comes from the same fixtures `seed-arr` tags, so it
    // cannot drift from what Sonarr and Radarr were told about — except for music,
    // which only the plain stack's Lidarr container tags at all; every other stack
    // (Transmission, rTorrent, VPN) exists to prove a client/topology concern, not
    // indexer coverage, and carries no Lidarr — see `LIDARR` in
    // `run_docker_tests.sh`.
    let expected = sharerr_testkit::library::tv_files(&media).len()
        + sharerr_testkit::library::movie_files(&media).len()
        + if lidarr_configured() {
            sharerr_testkit::library::music_files(&media).len()
        } else {
            0
        };
    assert!(
        report.contains(&format!("{expected} discovered")),
        "expected {expected} tagged file(s) — is the stack seeded? got: {report}"
    );

    // qBittorrent hash-checks on add; give it a moment to finish and settle.
    std::thread::sleep(std::time::Duration::from_secs(10));

    let after = snapshot(&media);

    assert_eq!(
        before.keys().collect::<Vec<_>>(),
        after.keys().collect::<Vec<_>>(),
        "a file appeared or disappeared during the sync"
    );
    for (path, identity) in &before {
        assert_eq!(
            Some(identity),
            after.get(path),
            "{} was moved, rewritten, or replaced",
            path.display()
        );
    }
}

/// Idempotency against the real thing, not a mock: the second pass must add nothing.
#[test]
#[ignore = "requires the compose stack; run with --ignored"]
fn a_second_real_sync_is_a_no_op() {
    require_stack_running();

    assert!(sharerr(&["sync"]).status.success(), "first sync failed");
    let out = sharerr(&["sync"]);
    let report = String::from_utf8_lossy(&out.stdout);
    println!("{report}");

    assert!(out.status.success(), "second sync failed");
    assert!(
        report.contains("0 added"),
        "the second pass should add nothing, got: {report}"
    );
}
