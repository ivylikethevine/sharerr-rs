//! `sharerr vault` — manage the encrypted credential store.

use std::io::{IsTerminal, Read};

use anyhow::{Context, Result, bail};
use secrecy::SecretString;
use sharerr_core::Config;
use sharerr_core::config::secret_keys;
use sharerr_store::{Vault, master_key_from_env};

fn open(config: &Config) -> Result<Vault> {
    let master = master_key_from_env()?;
    Vault::open(config.vault_path(), &master)
        .with_context(|| format!("opening vault at {}", config.vault_path().display()))
}

pub fn set(config: &Config, key: &str) -> Result<()> {
    // Others are allowed — a deployment may stash extra values — but a typo in one
    // of the keys sharerr reads would be silently ineffective, so it draws a warning.
    if !secret_keys::ALL.contains(&key) {
        eprintln!(
            "warning: {key:?} is not a key sharerr reads. Known keys: {}",
            secret_keys::ALL.join(", ")
        );
    }

    let value = read_secret(key)?;
    let mut vault = open(config)?;
    vault.put(key, &value)?;

    println!("stored {key:?} in {}", config.vault_path().display());
    Ok(())
}

pub fn list(config: &Config) -> Result<()> {
    let vault = open(config)?;

    if vault.is_empty() {
        println!("vault is empty ({})", config.vault_path().display());
        println!("add credentials with: sharerr vault set <key>");
        return Ok(());
    }

    println!("{}:", config.vault_path().display());
    for key in vault.keys() {
        // Values are never printed, by design.
        println!("  {key}");
    }
    Ok(())
}

pub fn remove(config: &Config, key: &str) -> Result<()> {
    let mut vault = open(config)?;
    if vault.remove(key)? {
        println!("removed {key:?}");
    } else {
        println!("{key:?} was not in the vault");
    }
    Ok(())
}

/// Read a secret from a TTY prompt, or from stdin when piped.
///
/// The piped form is what makes this scriptable:
/// `printf %s "$API_KEY" | sharerr vault set sonarr.api_key`
fn read_secret(key: &str) -> Result<SecretString> {
    let value = if std::io::stdin().is_terminal() {
        rpassword::prompt_password(format!("value for {key}: "))
            .context("reading secret from terminal")?
    } else {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("reading secret from stdin")?;
        buf
    };

    // Trailing newlines come free with `echo` and would silently become part of
    // the API key, producing 401s that are miserable to debug.
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("refusing to store an empty value for {key:?}");
    }

    Ok(SecretString::from(trimmed.to_owned()))
}
