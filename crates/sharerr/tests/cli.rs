//! Driving the `sharerr` binary itself.
//!
//! Everything else in this tree tests a function; this tests the *program* —
//! argument parsing, the config-load-or-recover step every subcommand goes
//! through, the dispatch table, and the exit code. Those live in `main.rs`,
//! which no unit test can reach, and they are the first thing an operator meets.
//!
//! Only the subcommands that need nothing running are exercised here. `doctor`,
//! `sync` and `serve` all want an *arr app or a torrent client; they belong to
//! the tier-2 suite, and `doctor` is asserted against the live stack there.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::Command;

fn bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sharerr"));
    // Otherwise the binary reads the developer's own config, or /config in a
    // container, and the test's behaviour depends on the machine it runs on.
    cmd.env_remove("SHARERR_CONFIG");
    cmd.env_remove("RUST_LOG");
    cmd
}

/// A config path that does not exist. `load_or_recover` must treat that as
/// "defaults", not as a failure — the wizard's whole premise is that a first
/// run has no file yet.
fn with_missing_config(cmd: &mut Command, dir: &std::path::Path) {
    cmd.arg("--config").arg(dir.join("sharerr.toml"));
}

#[test]
fn openapi_writes_the_document_to_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let out_path = dir.path().join("openapi.json");

    let mut cmd = bin();
    with_missing_config(&mut cmd, dir.path());
    let out = cmd
        .arg("openapi")
        .arg("--output")
        .arg(&out_path)
        .output()
        .unwrap();

    assert!(out.status.success(), "{out:?}");
    let document: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out_path).unwrap()).unwrap();
    assert!(document["paths"].is_object(), "{document}");
}

#[test]
fn openapi_writes_to_stdout_when_given_no_output() {
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = bin();
    with_missing_config(&mut cmd, dir.path());
    let out = cmd.arg("openapi").output().unwrap();

    assert!(out.status.success(), "{out:?}");
    let document: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(document["openapi"].as_str().unwrap_or_default()[..1], *"3");
}

/// The vault refuses to open without a master key, and says so rather than
/// falling back to plaintext. That refusal is a security property, so it is
/// asserted rather than worked around.
#[test]
fn vault_list_without_a_master_key_refuses_and_explains() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("sharerr.toml");
    std::fs::write(&config, format!("data_dir = {:?}\n", dir.path())).unwrap();

    let out = bin()
        .env_remove("SHARERR_MASTER_KEY")
        .env_remove("SHARERR_MASTER_KEY_FILE")
        .arg("--config")
        .arg(&config)
        .arg("vault")
        .arg("list")
        .output()
        .unwrap();

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no master key"), "{stderr}");
    assert!(stderr.contains("plaintext"), "the reason matters: {stderr}");
}

/// With a key, the same command opens the vault. A fresh one holds nothing, and
/// says so with the command that would change that — this is the first thing an
/// operator following the README runs. The env var is set on the *child*, so
/// this does not race the in-process `OnceLock` a `std::env::set_var` would.
#[test]
fn vault_list_with_a_master_key_opens_an_empty_vault() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("sharerr.toml");
    std::fs::write(&config, format!("data_dir = {:?}\n", dir.path())).unwrap();

    let out = bin()
        .env(
            "SHARERR_MASTER_KEY",
            "a-throwaway-key-for-a-throwaway-vault",
        )
        .arg("--config")
        .arg(&config)
        .arg("vault")
        .arg("list")
        .output()
        .unwrap();

    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("vault is empty"), "{stdout}");
    assert!(
        stdout.contains("sharerr vault set"),
        "an empty vault has to say what would fill it: {stdout}"
    );
    // Listing does not *create* the vault — nothing has been stored yet, and a
    // read-only command minting a file would be a surprise.
    assert!(!dir.path().join("vault.bin").exists());
}

/// The comment on `load_or_recover` in `main.rs` says a malformed config is
/// "loud but survivable" — the containerised `serve` would otherwise restart-loop
/// with no HTTP surface, and the web UI is how an operator would fix the file.
/// So: the error is on stderr, and the command still runs.
#[test]
fn a_malformed_config_is_reported_but_does_not_abort_the_command() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("sharerr.toml");
    std::fs::write(&config, "this is not = valid = toml [[[").unwrap();

    // `--output` so the document does not land in the stream the log shares.
    let out = bin()
        .arg("--config")
        .arg(&config)
        .arg("openapi")
        .arg("--output")
        .arg(dir.path().join("openapi.json"))
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "a bad config must not stop a command that does not need it: {out:?}"
    );
    // `tracing_subscriber::fmt` writes to stdout, not stderr.
    let logged = String::from_utf8_lossy(&out.stdout);
    assert!(
        logged.contains("configuration could not be loaded"),
        "the failure has to be loud: {logged}"
    );
}

/// `-v` and `-vv` pick the fallback filter. Asserted through observable output
/// rather than by calling `init_tracing`, which installs a process-global
/// subscriber and cannot be called twice in one test binary.
#[test]
fn verbosity_flags_raise_the_log_level() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("sharerr.toml");
    std::fs::write(&config, "this is not = valid = toml [[[").unwrap();

    let run = |extra: &[&str]| {
        let mut cmd = bin();
        cmd.arg("--config").arg(&config);
        for arg in extra {
            cmd.arg(arg);
        }
        let out = cmd
            .arg("openapi")
            .arg("--output")
            .arg(dir.path().join("openapi.json"))
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    // The `debug!("configuration loaded")` line is below the default filter and
    // above the `-vv` one, so it is the difference between the two.
    assert!(!run(&[]).contains("configuration loaded"));
    assert!(run(&["-vv"]).contains("configuration loaded"));
}

/// `RUST_LOG` has to win over `-v`, or an operator cannot get full control.
#[test]
fn rust_log_overrides_the_verbosity_flags() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("sharerr.toml");
    std::fs::write(&config, "this is not = valid = toml [[[").unwrap();

    let out = bin()
        .env("RUST_LOG", "error")
        .arg("--config")
        .arg(&config)
        .arg("-vv")
        .arg("openapi")
        .arg("--output")
        .arg(dir.path().join("openapi.json"))
        .output()
        .unwrap();

    let logged = String::from_utf8_lossy(&out.stdout);
    assert!(
        !logged.contains("configuration loaded"),
        "RUST_LOG=error must silence the debug line -vv would show: {logged}"
    );
    // ...while the error-level line it does allow still comes through.
    assert!(
        logged.contains("configuration could not be loaded"),
        "{logged}"
    );
}

#[test]
fn an_unknown_subcommand_fails_with_usage() {
    let out = bin().arg("frobnicate").output().unwrap();

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Usage") || stderr.contains("usage"),
        "{stderr}"
    );
}

#[test]
fn help_lists_every_subcommand() {
    let out = bin().arg("--help").output().unwrap();

    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    for command in ["doctor", "sync", "serve", "vault", "preview", "openapi"] {
        assert!(
            stdout.contains(command),
            "{command} missing from --help: {stdout}"
        );
    }
}

// ------------------------------------------------------------------- serve

/// Serialises port allocation and spawning — see the identical note in
/// `sharerr-lighthouse`'s binary test. `free_port` releases the port before the
/// child binds it, so two tests racing there can be handed the same number.
static STARTUP: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A running `sharerr serve`, torn down however the test exits.
struct Serving {
    child: std::process::Child,
    port: u16,
}

impl Serving {
    /// Start against a config that names nothing — no *arr app, no torrent
    /// client, no master key. That is a first run, and it has to come up: the
    /// web UI is how an operator configures the rest, so a `serve` that refused
    /// to start until it was already configured could never be configured.
    /// Retries on a fresh port if the first one turns out to be taken.
    ///
    /// `STARTUP` only serialises *this* test binary. Cargo runs several at
    /// once, and `sharerr-lighthouse`'s binary tests allocate ports the same
    /// way, so two processes can still be handed the same number — after which
    /// one child fails to bind. Retrying makes that a delay, not a flake.
    fn unconfigured() -> (tempfile::TempDir, Self) {
        for attempt in 0..5 {
            if let Some(started) = Self::try_unconfigured() {
                return started;
            }
            std::thread::sleep(std::time::Duration::from_millis(100 * (attempt + 1)));
        }
        panic!("serve never came up on any port");
    }

    fn try_unconfigured() -> Option<(tempfile::TempDir, Self)> {
        let dir = tempfile::tempdir().unwrap();
        let guard = STARTUP
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let port = free_port();
        std::fs::write(
            dir.path().join("sharerr.toml"),
            format!(
                "data_dir = {:?}\n\n[server]\nbind = \"127.0.0.1:{port}\"\n",
                dir.path().join("data")
            ),
        )
        .unwrap();

        let child = bin()
            .arg("--config")
            .arg(dir.path().join("sharerr.toml"))
            .arg("serve")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();

        let serving = Self { child, port };
        let ready = serving.wait_until_ready();
        drop(guard);
        ready.then_some((dir, serving))
    }

    /// Ready means `/health` answers, not merely that the port accepts a
    /// connection.
    ///
    /// The two are not the same, and the difference is load-bearing: the port
    /// is connectable from `TcpListener::bind`, but the SIGTERM handler is only
    /// installed once `axum::serve(..).with_graceful_shutdown(..)` starts. A
    /// signal delivered in that window kills the process by default
    /// disposition. Waiting for a served response means the handler is
    /// necessarily in place.
    fn wait_until_ready(&self) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            if self.get("/health").starts_with("HTTP/1.1 200") {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        false
    }

    /// One HTTP/1.1 request by hand — `sharerr`'s own dependencies include a
    /// client, but this needs a status line, not a runtime.
    fn get(&self, path: &str) -> String {
        use std::io::{Read, Write};

        // Fallible throughout: this doubles as the readiness probe, so it runs
        // against a port that may not be serving yet.
        let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", self.port)) else {
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
        // A body that is not valid UTF-8 is not this test's concern.
        let mut raw = Vec::new();
        let _ = stream.read_to_end(&mut raw);
        String::from_utf8_lossy(&raw).into_owned()
    }
}

impl Serving {
    /// Fetch a path, waiting for the server to come up first. Used by the
    /// preview test, which has no `/health` to poll.
    fn get_when_ready(&self, path: &str) -> String {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            if std::net::TcpStream::connect(("127.0.0.1", self.port)).is_ok() {
                let response = self.get(path);
                if !response.is_empty() {
                    return response;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("nothing served {path} on {}", self.port);
    }
}

impl Drop for Serving {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn serve_comes_up_on_a_config_that_names_nothing() {
    let (_dir, serving) = Serving::unconfigured();

    assert!(
        serving.get("/health").starts_with("HTTP/1.1 200"),
        "the orchestrator's health probe has to answer before anything is configured"
    );
}

/// An unconfigured instance sends the operator to the wizard rather than to an
/// empty dashboard.
#[test]
fn serve_redirects_an_unconfigured_root_to_the_wizard() {
    let (_dir, serving) = Serving::unconfigured();

    let response = serving.get("/");
    assert!(response.starts_with("HTTP/1.1 303"), "{response}");
}

/// `/ready` is the other half of the probe pair, and answers differently from
/// `/health`: healthy means the process is up, ready means it could actually
/// work. Unconfigured, it must not claim readiness.
#[test]
fn serve_is_healthy_but_not_ready_before_it_is_configured() {
    let (_dir, serving) = Serving::unconfigured();

    assert!(serving.get("/health").starts_with("HTTP/1.1 200"));
    let ready = serving.get("/ready");
    assert!(
        !ready.starts_with("HTTP/1.1 200"),
        "an instance with no master key and no services is not ready: {ready}"
    );
}

/// The tracker and Torznab routes are merged into the same listener, so a
/// misplaced route is visible here and nowhere else in the unit tests.
#[test]
fn serve_mounts_the_tracker_and_indexer_routes() {
    let (_dir, serving) = Serving::unconfigured();

    // Unauthenticated and unconfigured, so these refuse — but a refusal proves
    // something is routed there, where a 404 would prove nothing is.
    for path in ["/announce", "/api?t=caps"] {
        let response = serving.get(path);
        assert!(
            !response.starts_with("HTTP/1.1 404"),
            "{path} is not mounted: {response}"
        );
    }
}

/// Same reason as the lighthouse's: `sharerr` is PID 1 in its image, and PID 1
/// with no handler ignores SIGTERM, so `docker stop` would SIGKILL every
/// restart and deploy.
#[cfg(unix)]
#[test]
fn serve_shuts_down_on_sigterm() {
    let (_dir, mut serving) = Serving::unconfigured();

    let killed = Command::new("kill")
        .arg("-TERM")
        .arg(serving.child.id().to_string())
        .status()
        .unwrap();
    assert!(killed.success());

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        if let Some(status) = serving.child.try_wait().unwrap() {
            assert!(
                status.success(),
                "a graceful shutdown is a clean exit: {status:?}"
            );
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("SIGTERM was ignored — docker stop would have to SIGKILL this");
}

// -------------------------------------------------------------------- sync

/// A config complete enough for `sync` to get as far as the torrent client:
/// a data dir, a vault with the qBittorrent key in it, an advertised tracker
/// host, and a `[[library]]` directory with real (synthetic) files in it.
///
/// Each of those is a separate refusal on the way — `sync` checks them in order
/// and names the missing one — so building the whole thing is what lets the
/// tests below reach the reconciliation pass rather than one of the guards.
fn sync_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    // The same synthetic library the compose stack and the unit tests use,
    // built through the library rather than the `gen-fixtures` binary —
    // `CARGO_BIN_EXE_*` only resolves binaries of the crate under test.
    let media = dir.path().join("media");
    sharerr_testkit::tv_library(&media).unwrap();

    std::fs::write(
        dir.path().join("sharerr.toml"),
        format!(
            "data_dir = {:?}\n\n[tracker]\nadvertised_host = \"seed.example\"\n\n\
             [[library]]\npath = {:?}\nkind = \"tv\"\n",
            dir.path().join("data"),
            media.join("tv"),
        ),
    )
    .unwrap();

    // The vault refuses to open without this, so `sync` would stop there.
    let mut child = bin()
        .env(
            "SHARERR_MASTER_KEY",
            "a-throwaway-key-for-a-throwaway-vault",
        )
        .arg("--config")
        .arg(dir.path().join("sharerr.toml"))
        .arg("vault")
        .arg("set")
        .arg("qbittorrent.api_key")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().unwrap();
        stdin
            .write_all(b"qbt_notarealkeynotarealkeynotare")
            .unwrap();
    }
    assert!(child.wait().unwrap().success());

    dir
}

fn sync(dir: &tempfile::TempDir, extra: &[&str]) -> std::process::Output {
    let mut cmd = bin();
    cmd.env(
        "SHARERR_MASTER_KEY",
        "a-throwaway-key-for-a-throwaway-vault",
    )
    .arg("--config")
    .arg(dir.path().join("sharerr.toml"))
    .arg("sync");
    for arg in extra {
        cmd.arg(arg);
    }
    cmd.output().unwrap()
}

/// A dry run says so before it does anything. The banner is the operator's only
/// confirmation that nothing is about to be written.
#[test]
fn sync_dry_run_announces_itself_before_doing_anything() {
    let dir = sync_fixture();
    let out = sync(&dir, &["--dry-run"]);

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("dry run — nothing will be created, changed, or removed"),
        "{stdout}"
    );
}

/// An unreachable torrent client is a failed run with a named cause, not a
/// panic and not a silent success — a cron wrapper keys off the exit code.
#[test]
fn sync_against_an_unreachable_client_fails_with_a_named_cause() {
    let dir = sync_fixture();
    let out = sync(&dir, &["--dry-run"]);

    assert!(!out.status.success(), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("qBittorrent"),
        "the cause has to name it: {stderr}"
    );
}

/// No master key at all: the vault cannot open, and there is deliberately no
/// plaintext fallback, so `sync` stops before it touches anything.
#[test]
fn sync_without_a_master_key_refuses_and_says_why() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("sharerr.toml");
    std::fs::write(
        &config,
        format!("data_dir = {:?}\n", dir.path().join("data")),
    )
    .unwrap();

    let out = bin()
        .env_remove("SHARERR_MASTER_KEY")
        .env_remove("SHARERR_MASTER_KEY_FILE")
        .arg("--config")
        .arg(&config)
        .arg("sync")
        .output()
        .unwrap();

    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no master key"),
        "{out:?}"
    );
}

/// With a vault but nothing to read from, the refusal lists what would fix it.
/// The message *is* the remedy here — this is what an operator following the
/// README hits before they have configured a source.
#[test]
fn sync_with_no_library_source_lists_what_to_configure() {
    let dir = sync_fixture();
    // Same fixture, minus the `[[library]]` section.
    std::fs::write(
        dir.path().join("sharerr.toml"),
        format!(
            "data_dir = {:?}\n\n[tracker]\nadvertised_host = \"seed.example\"\n",
            dir.path().join("data")
        ),
    )
    .unwrap();

    let out = sync(&dir, &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(!out.status.success());
    assert!(
        stderr.contains("no library source is configured"),
        "{stderr}"
    );
    assert!(
        stderr.contains("[[library]]"),
        "it has to name the alternative: {stderr}"
    );
}

/// The tracker address cannot be guessed, and a torrent built without it would
/// announce somewhere no friend can reach — so this is a refusal, not a default.
#[test]
fn sync_without_an_advertised_address_refuses_rather_than_guessing() {
    let dir = sync_fixture();
    let media = dir.path().join("media");
    std::fs::write(
        dir.path().join("sharerr.toml"),
        format!(
            "data_dir = {:?}\n\n[[library]]\npath = {:?}\nkind = \"tv\"\n",
            dir.path().join("data"),
            media.join("tv"),
        ),
    )
    .unwrap();

    let out = sync(&dir, &["--dry-run"]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(!out.status.success());
    assert!(stderr.contains("advertised_host"), "{stderr}");
}

/// A configured gluetun control URL is consulted before the pass, so a manual
/// sync inside the VPN namespace builds torrents announcing the *live*
/// forwarded port. When that lookup fails there is no prior observation to fall
/// back on, so it warns and carries on with the static endpoint rather than
/// aborting — the pass is still worth running.
#[test]
fn sync_continues_when_the_gluetun_lookup_fails() {
    let dir = sync_fixture();
    let media = dir.path().join("media");
    // A control URL nothing answers on.
    let dead_port = free_port();
    std::fs::write(
        dir.path().join("sharerr.toml"),
        format!(
            "data_dir = {:?}\n\n[tracker]\nadvertised_host = \"seed.example\"\n\n\
             [gluetun]\ncontrol_url = \"http://127.0.0.1:{dead_port}\"\n\n\
             [[library]]\npath = {:?}\nkind = \"tv\"\n",
            dir.path().join("data"),
            media.join("tv"),
        ),
    )
    .unwrap();

    let out = sync(&dir, &["--dry-run"]);
    let logged = String::from_utf8_lossy(&out.stdout);

    assert!(
        logged.contains("continuing with the statically configured endpoint"),
        "a failed port lookup must warn, not abort: {logged}"
    );
    // ...and the run still got as far as the torrent client.
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("qBittorrent"),
        "{out:?}"
    );
}

// ----------------------------------------------------------------- preview

/// `sharerr preview` serves the whole UI against hand-built mock state, with no
/// config, no database and no services. It is how the pages get looked at
/// during development, so "does every route still render" is exactly the
/// question worth asking of it — a template that stopped compiling against its
/// struct fails here and nowhere else.
#[test]
fn preview_renders_every_page_without_any_configuration() {
    let guard = STARTUP
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let port = free_port();
    let child = bin()
        .arg("preview")
        .arg("--bind")
        .arg(format!("127.0.0.1:{port}"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let serving = Serving { child, port };
    drop(guard);

    for path in ["/", "/settings", "/peers", "/items", "/topology", "/debug"] {
        let response = serving.get_when_ready(path);
        assert!(
            response.starts_with("HTTP/1.1 200"),
            "{path} did not render: {}",
            response.lines().next().unwrap_or_default()
        );
        // A page that renders an empty body is a template that silently
        // produced nothing, which is the failure this is guarding against.
        assert!(response.len() > 500, "{path} rendered almost nothing");
    }

    // Stop it the way its own message says to. `Serving`'s `Drop` would SIGKILL,
    // and the point of the graceful handler is that it does not come to that.
    stop_gracefully(serving);
}

/// SIGTERM, then wait for a clean exit. Consumes the wrapper so its `Drop` has
/// nothing left to kill.
#[cfg(unix)]
fn stop_gracefully(mut serving: Serving) {
    assert!(
        Command::new("kill")
            .arg("-TERM")
            .arg(serving.child.id().to_string())
            .status()
            .unwrap()
            .success()
    );

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        if let Some(status) = serving.child.try_wait().unwrap() {
            assert!(
                status.success(),
                "a graceful shutdown is a clean exit: {status:?}"
            );
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("preview ignored SIGTERM — its own message promises Ctrl+C works");
}

#[cfg(not(unix))]
fn stop_gracefully(_serving: Serving) {}
