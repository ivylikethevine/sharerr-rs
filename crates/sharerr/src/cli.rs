//! Command-line surface.
//!
//! Milestone 1 is headless — there is no web UI yet — so the vault subcommands
//! are the only way to get API keys into sharerr. They are not a stopgap: an
//! operator automating a deployment wants them regardless of the UI.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "sharerr",
    version,
    about = "Share your media library with friends over the tools you already run"
)]
pub struct Cli {
    /// Path to sharerr.toml.
    #[arg(
        long,
        short,
        env = "SHARERR_CONFIG",
        default_value = "/config/sharerr.toml",
        global = true
    )]
    pub config: PathBuf,

    /// Increase log verbosity. Overridden by RUST_LOG if set.
    #[arg(long, short, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Check configuration, service reachability, tag presence, and path mappings.
    Doctor,

    /// Run one reconciliation pass and exit.
    Sync(SyncArgs),

    /// Run continuously: periodic reconciliation plus the HTTP server.
    Serve,

    /// Manage the encrypted credential vault.
    #[command(subcommand)]
    Vault(VaultCommand),
}

#[derive(Debug, Args)]
pub struct SyncArgs {
    /// Report what would change without creating torrents or touching qBittorrent.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Subcommand)]
pub enum VaultCommand {
    /// Store a secret. The value is read from a prompt, or stdin when piped.
    Set {
        /// e.g. sonarr.api_key, radarr.api_key, qbittorrent.password
        key: String,
    },
    /// List the keys held in the vault. Never prints values.
    List,
    /// Remove a secret.
    Remove { key: String },
}
