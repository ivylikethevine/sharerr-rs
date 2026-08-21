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
    axum::serve(listener, app).await?;
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
