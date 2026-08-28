//! The lighthouse's standalone binary: its own image, its own port, per
//! `docs/ROADMAP.md`'s "The lighthouse". See [`sharerr_lighthouse`] for the
//! protocol and the privacy property this serves.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use sharerr_lighthouse::LighthouseState;

#[derive(Debug, Parser)]
#[command(
    name = "sharerr-lighthouse",
    about = "The sharerr lighthouse rendezvous service"
)]
struct Cli {
    /// Address to listen on.
    #[arg(long, env = "LIGHTHOUSE_BIND", default_value = "0.0.0.0:7878")]
    bind: SocketAddr,
    /// Where the decoy secret persists across restarts. Generated on first
    /// run if the file does not exist. Losing it just means decoys reshuffle
    /// after a restart — nothing a real reporter relies on is stored here.
    #[arg(
        long,
        env = "LIGHTHOUSE_SECRET_FILE",
        default_value = "/data/lighthouse.secret"
    )]
    secret_file: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    let secret = load_or_create_secret(&cli.secret_file)?;
    let state = Arc::new(LighthouseState::new(secret));
    let app = sharerr_lighthouse::routes(state);

    let listener = tokio::net::TcpListener::bind(cli.bind).await?;
    tracing::info!(bind = %cli.bind, "lighthouse listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(sharerr_lighthouse::shutdown_signal())
        .await?;
    Ok(())
}

/// Load the decoy secret from disk, minting one on first run.
///
/// A plain file rather than sharerr's encrypted vault: the lighthouse has no
/// operator password to derive a vault key from, and per the design brief it
/// carries no data worth an attacker breaking in for beyond the ability to
/// tell its own decoys from real records — which reading this file from the
/// same host already implies far worse access.
fn load_or_create_secret(path: &std::path::Path) -> anyhow::Result<[u8; 32]> {
    use std::io::Write;

    if let Ok(bytes) = std::fs::read(path)
        && let Ok(secret) = <[u8; 32]>::try_from(bytes.as_slice())
    {
        return Ok(secret);
    }

    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).map_err(|err| anyhow::anyhow!("generating a secret: {err}"))?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::File::create(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(&secret)?;
    tracing::info!(path = %path.display(), "minted a lighthouse decoy secret");
    Ok(secret)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// A path with no parent at all (the root itself) must skip trying to
    /// create one rather than panic on `Option::unwrap` — this only asks
    /// that the branch is skipped cleanly; the mint still fails afterward for
    /// the unrelated reason that `/` cannot be opened as a file.
    #[cfg(unix)]
    #[test]
    fn a_path_with_no_parent_skips_creating_one() {
        let result = load_or_create_secret(std::path::Path::new("/"));
        assert!(
            result.is_err(),
            "writing directly to / must fail (it is a directory), not panic"
        );
    }

    /// The common case on every restart after the first: the secret already on
    /// disk is read back verbatim rather than being reminted, which would
    /// reshuffle every decoy for no reason.
    #[test]
    fn an_existing_secret_is_read_back_rather_than_reminted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lighthouse.secret");
        let original = [7u8; 32];
        std::fs::write(&path, original).unwrap();

        let loaded = load_or_create_secret(&path).unwrap();

        assert_eq!(loaded, original);
        // Unchanged on disk, not silently reminted over top of it.
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    /// First run: no file exists yet, so one is minted, persisted, and handed
    /// back — and the parent directory is created along the way, since a
    /// fresh container's data volume may not have it yet.
    #[test]
    fn a_missing_secret_is_minted_and_persisted_under_a_new_parent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("lighthouse.secret");
        assert!(
            !path.parent().unwrap().exists(),
            "setup: must not exist yet"
        );

        let minted = load_or_create_secret(&path).unwrap();

        assert_eq!(
            std::fs::read(&path).unwrap(),
            minted,
            "the minted secret must be exactly what was persisted"
        );
        assert_ne!(minted, [0u8; 32], "a real secret, not the zeroed buffer");
    }

    /// A file that exists but is not 32 bytes (truncated, corrupted, or left
    /// over from a different format) must not be trusted as a secret — it is
    /// silently reminted rather than failing the whole process over a
    /// value nothing relies on for correctness, only for decoy consistency.
    #[test]
    fn a_malformed_secret_file_is_reminted_rather_than_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lighthouse.secret");
        std::fs::write(&path, b"too short").unwrap();

        let minted = load_or_create_secret(&path).unwrap();

        assert_eq!(minted.len(), 32);
        assert_eq!(std::fs::read(&path).unwrap(), minted);
    }

    /// A freshly minted secret file must not be world- or group-readable — the
    /// one thing worth protecting, per the module doc, is decoy-vs-real
    /// distinguishability, and a readable-by-anyone file on a shared host
    /// would hand that away.
    #[cfg(unix)]
    #[test]
    fn a_minted_secret_file_is_not_group_or_world_readable() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lighthouse.secret");

        load_or_create_secret(&path).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "group/world bits must be clear: {mode:o}");
    }
}
