mod cli;
mod commands;
mod settings;
mod sync;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

use cli::{Cli, Command, VaultCommand};

#[tokio::main]
async fn main() -> Result<()> {
    let args = Cli::parse();
    init_tracing(args.verbose);

    let config = settings::load(&args.config)?;
    tracing::debug!(config = ?config, "configuration loaded");

    match args.command {
        Command::Doctor => commands::doctor::run(&config).await,
        Command::Sync(args) => commands::sync::run(&config, args.dry_run).await,
        Command::Serve => commands::serve::run(&config).await,
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
