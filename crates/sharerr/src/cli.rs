//! Command-line surface.
//!
//! Since the web UI landed, none of this is *required*: `serve` alone is enough to
//! configure an instance from a browser, credentials included. The subcommands
//! remain because they are the right tool for a different job — `vault set` reads
//! a piped secret, which is what a scripted deployment or a secrets manager wants,
//! and `doctor` exits non-zero, which is what a healthcheck wants. Neither is a
//! stopgap for the UI.

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
    Doctor(DoctorArgs),

    /// Run one reconciliation pass and exit.
    Sync(SyncArgs),

    /// Run continuously: periodic reconciliation plus the HTTP server.
    Serve,

    /// Manage the encrypted credential vault.
    #[command(subcommand)]
    Vault(VaultCommand),
}

#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Attempt to fix the mechanical problems this command can already name
    /// precisely: create a missing *arr tag, or a qBittorrent category that
    /// does not exist yet. Everything else — a wrong URL, a rejected
    /// credential, a bad path mapping — still needs a person.
    #[arg(long)]
    pub fix: bool,

    /// Propose `[[path_map]]` rules instead of asking you to derive them:
    /// matches tagged files against what actually exists under
    /// `--search-root` by name and size. Proposals only — nothing is written
    /// to `sharerr.toml`.
    #[arg(long)]
    pub suggest_paths: bool,

    /// Where to look for the actual files when `--suggest-paths` is set.
    /// Defaults to `/media`, the mount point every deployment example in this
    /// repository uses for sharerr's own view of the library.
    #[arg(long)]
    pub search_root: Option<PathBuf>,
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
        /// e.g. sonarr.api_key, radarr.api_key, qbittorrent.api_key
        key: String,
    },
    /// List the keys held in the vault. Never prints values.
    List,
    /// Remove a secret.
    Remove { key: String },
}
