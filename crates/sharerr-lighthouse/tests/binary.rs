//! Driving the `sharerr-lighthouse` binary as its container runs it.
//!
//! The library tests cover the protocol; this covers the *program* around it —
//! argument and environment parsing, minting the decoy secret on first run and
//! reusing it on the second, actually serving, and shutting down on SIGTERM.
//!
//! That last one is not incidental. The binary is PID 1 in its image, and PID 1
//! with no installed handler ignores SIGTERM — `docker stop` would wait out its
//! grace period and then SIGKILL. The handler exists for that, so it is worth a
//! test that actually sends the signal.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Serialises port allocation and spawning.
///
/// `free_port` works by binding a port and immediately releasing it, so two
/// tests racing between the release and the child's `bind` can be handed the
/// same number — and then one child dies while the *other* satisfies the first
/// test's readiness check. Holding this for the whole allocate-spawn-wait means
/// the port is live again before the next allocation runs, so the OS will not
/// offer it twice.
static STARTUP: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A port nothing is listening on, by binding one and letting it go. Only safe
/// under [`STARTUP`].
///
/// Duplicates `sharerr_testkit::net::closed_port`'s one-liner rather than
/// pulling in a dev-dependency on it: the lighthouse crate's whole point is
/// standing apart from the rest of the workspace's dependency graph, and that
/// is worth six duplicated lines here.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct Lighthouse {
    child: Child,
    port: u16,
}

impl Lighthouse {
    /// Start on a port nobody else is using, retrying if that turns out to be
    /// untrue.
    ///
    /// `STARTUP` only serialises *this* test binary. Cargo runs several test
    /// binaries at once, and the `sharerr` crate's CLI tests allocate ports the
    /// same way, so two processes can still be handed the same number — after
    /// which one child fails to bind. Retrying on a fresh port is what makes
    /// that a delay instead of a flake.
    fn start(secret_file: &std::path::Path) -> Self {
        for attempt in 0..5 {
            if let Some(lighthouse) = Self::try_start(secret_file) {
                return lighthouse;
            }
            std::thread::sleep(Duration::from_millis(100 * (attempt + 1)));
        }
        panic!("the lighthouse never came up on any port");
    }

    fn try_start(secret_file: &std::path::Path) -> Option<Self> {
        let guard = STARTUP
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let port = free_port();
        let child = Command::new(env!("CARGO_BIN_EXE_sharerr-lighthouse"))
            .arg("--bind")
            .arg(format!("127.0.0.1:{port}"))
            .arg("--secret-file")
            .arg(secret_file)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let lighthouse = Self { child, port };
        let ready = lighthouse.wait_until_ready();
        drop(guard);
        ready.then_some(lighthouse)
    }

    /// Ready means `/health` answers, not merely that the port accepts a
    /// connection. The port is connectable from `bind`, but the SIGTERM handler
    /// only exists once `axum::serve(..).with_graceful_shutdown(..)` starts —
    /// a signal in that window kills the process by default disposition, which
    /// is the very thing the handler is there to prevent.
    fn wait_until_ready(&self) -> bool {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if TcpStream::connect(("127.0.0.1", self.port)).is_ok()
                && self
                    .get("/lighthouse/v1/health")
                    .starts_with("HTTP/1.1 200")
            {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// One HTTP/1.1 request, hand-rolled — the crate has no HTTP client among
    /// its dependencies and this needs one line of protocol, not a dependency.
    fn get(&self, path: &str) -> String {
        // Fallible throughout: this is also the readiness probe, so it runs
        // against a port that may not be serving yet.
        let Ok(mut stream) = TcpStream::connect(("127.0.0.1", self.port)) else {
            return String::new();
        };
        if stream
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .is_err()
        {
            return String::new();
        }
        let mut response = String::new();
        let _ = stream.read_to_string(&mut response);
        response
    }
}

impl Drop for Lighthouse {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn it_serves_and_mints_a_secret_on_first_run() {
    let dir = tempfile::tempdir().unwrap();
    let secret_file = dir.path().join("nested").join("lighthouse.secret");

    let lighthouse = Lighthouse::start(&secret_file);

    // The parent directory did not exist either — first run has to create it.
    assert!(secret_file.is_file(), "the secret was not minted");
    assert_eq!(std::fs::read(&secret_file).unwrap().len(), 32);

    let response = lighthouse.get("/lighthouse/v1/health");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
}

/// The decoy secret has to survive a restart, or every decoy reshuffles and a
/// probe can tell "restarted" from "real record" by watching answers change.
#[test]
fn a_restart_reuses_the_secret_it_already_minted() {
    let dir = tempfile::tempdir().unwrap();
    let secret_file = dir.path().join("lighthouse.secret");

    {
        let _first = Lighthouse::start(&secret_file);
    }
    let minted = std::fs::read(&secret_file).unwrap();

    {
        let _second = Lighthouse::start(&secret_file);
    }
    assert_eq!(
        std::fs::read(&secret_file).unwrap(),
        minted,
        "a restart must not mint a new secret"
    );
}

/// Two runs against *different* secret files must not share a secret — proves
/// the file is read rather than a constant being used.
#[test]
fn separate_deployments_get_separate_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a.secret"), dir.path().join("b.secret"));

    {
        let _one = Lighthouse::start(&a);
    }
    {
        let _two = Lighthouse::start(&b);
    }

    assert_ne!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());
}

#[cfg(unix)]
#[test]
fn the_secret_file_is_not_world_readable() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let secret_file = dir.path().join("lighthouse.secret");
    {
        let _lighthouse = Lighthouse::start(&secret_file);
    }

    let mode = std::fs::metadata(&secret_file)
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o077,
        0,
        "group and other must have no access: {mode:o}"
    );
}

/// The reason the signal handler exists: PID 1 without one ignores SIGTERM and
/// `docker stop` degrades into a ten-second wait followed by SIGKILL.
#[cfg(unix)]
#[test]
fn sigterm_shuts_it_down_rather_than_being_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let mut lighthouse = Lighthouse::start(&dir.path().join("lighthouse.secret"));

    // `kill(1)` rather than a `libc` dependency added for one call.
    let killed = Command::new("kill")
        .arg("-TERM")
        .arg(lighthouse.child.id().to_string())
        .status()
        .unwrap();
    assert!(killed.success(), "could not signal the child");

    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(status) = lighthouse.child.try_wait().unwrap() {
            assert!(
                status.success(),
                "a graceful shutdown is a clean exit: {status:?}"
            );
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("SIGTERM was ignored — docker stop would have to SIGKILL this");
}

/// The bind address comes from the environment too, which is how the compose
/// files set it.
#[test]
fn the_bind_address_can_come_from_the_environment() {
    let dir = tempfile::tempdir().unwrap();
    let guard = STARTUP
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let port = free_port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_sharerr-lighthouse"))
        .env("LIGHTHOUSE_BIND", format!("127.0.0.1:{port}"))
        .env("LIGHTHOUSE_SECRET_FILE", dir.path().join("from-env.secret"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut listening = false;
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            listening = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(guard);
    let _ = child.kill();
    let _ = child.wait();

    assert!(listening, "LIGHTHOUSE_BIND was not honoured");
    assert!(dir.path().join("from-env.secret").is_file());
}

#[test]
fn an_unparseable_bind_address_fails_fast() {
    let out = Command::new(env!("CARGO_BIN_EXE_sharerr-lighthouse"))
        .arg("--bind")
        .arg("not-an-address")
        .output()
        .unwrap();

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("bind"), "{stderr}");
}
