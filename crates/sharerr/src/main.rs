mod checks;
mod cli;
mod commands;
mod gluetun;
mod gossip;
mod jackett;
mod library;
mod notify;
mod secrets;
mod settings;
mod state;
mod sync;
mod torznab;
mod tracker;
mod web;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

use cli::{Cli, Command, VaultCommand};

#[tokio::main]
async fn main() -> Result<()> {
    let args = Cli::parse();
    init_tracing(args.verbose);

    // Deliberately not fatal. A malformed `sharerr.toml` used to abort every
    // subcommand, which for the containerised `serve` meant a restart loop with no
    // HTTP surface — and the web UI is how an operator would fix the file. So the
    // failure is loud but survivable, and `serve` carries the reason into the UI.
    let (config, config_error) = settings::load_or_recover(&args.config);
    if let Some(error) = &config_error {
        // `error`, and repeated on every command, because the salvage is *not* what
        // the operator wrote: with `data_dir` defaulted, `vault set` would quietly
        // create a second, empty vault somewhere else.
        tracing::error!(
            error,
            path = %args.config.display(),
            "configuration could not be loaded; continuing with defaults"
        );
    }
    tracing::debug!(config = ?config, "configuration loaded");

    match args.command {
        Command::Doctor(args) => {
            commands::doctor::run(&config, config_error.as_deref(), args.fix).await
        }
        Command::Sync(args) => commands::sync::run(&config, args.dry_run).await,
        // The config *path* travels with the config, not just its contents: the web
        // UI writes settings back to this same file, and it is the CLI flag or
        // SHARERR_CONFIG that decides which one that is.
        Command::Serve => commands::serve::run(&config, &args.config, config_error).await,
        Command::Vault(VaultCommand::Set { key }) => commands::vault::set(&config, &key),
        Command::Vault(VaultCommand::List) => commands::vault::list(&config),
        Command::Vault(VaultCommand::Remove { key }) => commands::vault::remove(&config, &key),
    }
}

/// `RUST_LOG` wins when set, so an operator can always get full control;
/// otherwise `-v` / `-vv` pick a sensible level.
fn init_tracing(verbose: u8) {
    let fallback = match verbose {
        0 => "sharerr=info,warn",
        1 => "sharerr=debug,info",
        _ => "sharerr=trace,debug",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(fallback));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}
