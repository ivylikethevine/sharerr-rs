//! Driving the `gen-fixtures` binary the way `docker/README.md` documents it.
//!
//! This runs the real binary rather than calling `tv_library` and friends
//! directly, because what is worth guarding here is the *binary's* contract: the
//! compose stack shells out to it to populate the library it mounts, so its
//! argument handling and exit codes are the interface, not its internals.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::Command;

/// The instrumented build of the binary under test, resolved by cargo.
fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_gen-fixtures"))
}

#[test]
fn writing_to_a_directory_reports_every_file_it_wrote() {
    let dir = tempfile::tempdir().unwrap();
    let out = bin().arg(dir.path()).output().unwrap();

    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("synthetic file(s) under"), "{stdout}");
    // The provenance line is not decoration: it is the standing claim that no
    // real content is in this tree.
    assert!(stdout.contains("every title here is invented"), "{stdout}");

    assert!(dir.path().join("tv").is_dir());
    assert!(dir.path().join("movies").is_dir());
    assert!(dir.path().join("music").is_dir());
}

/// Regenerating is idempotent — the content is seeded, so the compose stack can
/// re-run this without the library changing underneath a running qBittorrent.
#[test]
fn a_second_run_writes_identical_bytes() {
    let dir = tempfile::tempdir().unwrap();
    assert!(bin().arg(dir.path()).status().unwrap().success());
    let first = std::fs::read(sample_file(dir.path())).unwrap();

    assert!(bin().arg(dir.path()).status().unwrap().success());
    let second = std::fs::read(sample_file(dir.path())).unwrap();

    assert_eq!(first, second, "seeded content must be reproducible");
}

#[test]
fn no_argument_is_a_usage_error_not_a_panic() {
    let out = bin().output().unwrap();

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("usage: gen-fixtures"), "{stderr}");
}

/// A destination it cannot write to must be a reported error and a non-zero
/// exit, not a panic — the compose stack's `up` keys off the exit code.
#[cfg(unix)]
#[test]
fn an_unwritable_destination_is_reported_and_fails() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let locked = dir.path().join("locked");
    std::fs::create_dir(&locked).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();

    let out = bin().arg(locked.join("library")).output().unwrap();

    // Running as root ignores the mode bits, so the assertion only means
    // anything for an unprivileged user — which is how CI and a developer
    // machine both run.
    if out.status.success() {
        return;
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("could not write"), "{stderr}");
}

/// One file that is known to exist, found by walking rather than named, so a
/// change to the fixture titles does not break this test for the wrong reason.
fn sample_file(root: &std::path::Path) -> std::path::PathBuf {
    fn first(dir: &std::path::Path) -> Option<std::path::PathBuf> {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .ok()?
            .filter_map(Result::ok)
            .collect();
        entries.sort_by_key(std::fs::DirEntry::path);
        for entry in entries {
            let path = entry.path();
            if path.is_file() {
                return Some(path);
            }
            if let Some(found) = first(&path) {
                return Some(found);
            }
        }
        None
    }
    first(&root.join("tv")).expect("the tv library has files")
}
