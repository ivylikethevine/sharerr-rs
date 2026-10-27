mod api;
mod checks;
mod cli;
mod commands;
mod gluetun;
mod gossip;
mod jackett;
mod library;
mod lighthouse_client;
mod localtime;
mod metrics;
mod notify;
mod openapi;
mod pairing;
mod pathsuggest;
mod secrets;
mod settings;
mod state;
mod swarm_history;
mod sync;
mod system_stats;
#[cfg(test)]
mod test_support;
mod torznab;
mod totp;
mod tracker;
mod web;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

use cli::{AccountCommand, Cli, Command, LogFormat, VaultCommand};

/// The version this binary reports, everywhere it reports one: `--version`,
/// the web footer, the OpenAPI document, Jackett's `server_config`. Set by
/// build.rs from `SHARERR_VERSION` (the `v*` tag in a release build), and
/// otherwise Cargo.toml's placeholder.
pub const VERSION: &str = env!("SHARERR_VERSION");

#[tokio::main]
async fn main() -> Result<()> {
    let args = Cli::parse();
    init_tracing(args.verbose, args.log_format);

    // Deliberately not fatal: aborting every subcommand on a malformed `sharerr.toml`
    // would mean a restart loop for the containerised `serve`, with no HTTP surface —
    // and the web UI is how an operator would fix the file. So the failure is loud
    // but survivable, and `serve` carries the reason into the UI.
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
            commands::doctor::run(&config, config_error.as_deref(), &args).await
        }
        Command::Sync(args) => commands::sync::run(&config, args.dry_run).await,
        // The config *path* travels with the config, not just its contents: the web
        // UI writes settings back to this same file, and it is the CLI flag or
        // SHARERR_CONFIG that decides which one that is.
        Command::Serve => commands::serve::run(&config, &args.config, config_error).await,
        Command::Vault(VaultCommand::Set { key }) => commands::vault::set(&config, &key),
        Command::Vault(VaultCommand::List) => commands::vault::list(&config),
        Command::Vault(VaultCommand::Remove { key }) => commands::vault::remove(&config, &key),
        Command::Account(AccountCommand::DisableTotp { login }) => {
            commands::account::disable_totp(&config, &login).await
        }
        Command::Preview(args) => commands::preview::run(args.bind).await,
        // Before the config is even consulted, in spirit: this reads nothing.
        Command::Openapi(args) => commands::openapi::run(args.output.as_deref()),
    }
}

/// Install [`log_subscriber`]'s choice as the process-wide subscriber.
fn init_tracing(verbose: u8, format: LogFormat) {
    use tracing_subscriber::util::SubscriberInitExt as _;
    log_subscriber(verbose, format).init();
}

/// `RUST_LOG` wins when set, so an operator can always get full control;
/// otherwise `-v` / `-vv` pick a sensible level. `format` picks the line
/// shape; JSON keeps the target, since a log pipeline filters on it. Built
/// apart from installing it, so every combination can be exercised in a
/// process that already has a subscriber.
fn log_subscriber(verbose: u8, format: LogFormat) -> Box<dyn tracing::Subscriber + Send + Sync> {
    let fallback = match verbose {
        0 => "sharerr=info,warn",
        1 => "sharerr=debug,info",
        _ => "sharerr=trace,debug",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(fallback));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    match format {
        LogFormat::Text => Box::new(builder.with_target(false).finish()),
        LogFormat::Json => Box::new(builder.json().finish()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every verbosity and both shapes build; what each prints is
    /// tracing-subscriber's own business.
    #[test]
    fn every_log_shape_and_verbosity_builds() {
        for verbose in 0..3 {
            for format in [LogFormat::Text, LogFormat::Json] {
                let _subscriber = log_subscriber(verbose, format);
            }
        }
    }
}
