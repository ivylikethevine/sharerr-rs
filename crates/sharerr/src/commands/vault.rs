//! `sharerr vault` — manage the encrypted credential store.

use std::io::{IsTerminal, Read};

use anyhow::{Context, Result, bail};
use secrecy::SecretString;
use sharerr_core::Config;
use sharerr_core::config::secret_keys;

use crate::secrets::open_vault;

pub fn set(config: &Config, key: &str) -> Result<()> {
    // Others are allowed — a deployment may stash extra values — but a typo in one
    // of the keys sharerr reads would be silently ineffective, so it draws a warning.
    if let Some(warning) = unknown_key_warning(key) {
        eprintln!("{warning}");
    }

    let value = read_secret(key)?;
    let mut vault = open_vault(config)?;
    vault.put(key, &value)?;

    println!("stored {key:?} in {}", config.vault_path().display());
    Ok(())
}

/// A warning to print when `key` is not one sharerr actually reads, or `None`
/// when it is a recognized key. Split out from [`set`] so the message text
/// (and the recognition rule) can be exercised without a real vault.
fn unknown_key_warning(key: &str) -> Option<String> {
    if secret_keys::ALL.contains(&key) {
        return None;
    }
    Some(format!(
        "warning: {key:?} is not a key sharerr reads. Known keys: {}",
        secret_keys::ALL.join(", ")
    ))
}

pub fn list(config: &Config) -> Result<()> {
    let vault = open_vault(config)?;
    let keys: Vec<&str> = vault.keys().collect();
    print!("{}", format_listing(&config.vault_path(), &keys));
    Ok(())
}

/// The text `list` prints for a vault at `path` holding `keys`. Pure so the
/// empty and non-empty cases can be checked without opening a real vault.
fn format_listing(path: &std::path::Path, keys: &[&str]) -> String {
    if keys.is_empty() {
        return format!(
            "vault is empty ({})\nadd credentials with: sharerr vault set <key>\n",
            path.display()
        );
    }

    let mut out = format!("{}:\n", path.display());
    for key in keys {
        // Values are never printed, by design.
        out.push_str(&format!("  {key}\n"));
    }
    out
}

pub fn remove(config: &Config, key: &str) -> Result<()> {
    let mut vault = open_vault(config)?;
    let removed = vault.remove(key)?;
    println!("{}", remove_message(key, removed));
    Ok(())
}

/// The text `remove` prints, given whether `key` was actually present.
fn remove_message(key: &str, removed: bool) -> String {
    if removed {
        format!("removed {key:?}")
    } else {
        format!("{key:?} was not in the vault")
    }
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

    validate_secret(key, &value)
}

/// Trim `raw` and reject it if that leaves nothing.
///
/// Trailing newlines come free with `echo` and would silently become part of
/// the API key, producing 401s that are miserable to debug.
fn validate_secret(key: &str, raw: &str) -> Result<SecretString> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("refusing to store an empty value for {key:?}");
    }

    Ok(SecretString::from(trimmed.to_owned()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::result_large_err)]

    use secrecy::ExposeSecret;

    use super::*;

    #[test]
    fn unknown_key_warning_none_for_recognized_key() {
        let key = secret_keys::ALL
            .first()
            .expect("secret_keys::ALL is non-empty");
        assert_eq!(unknown_key_warning(key), None);
    }

    #[test]
    fn unknown_key_warning_flags_typo() {
        let warning = unknown_key_warning("sonarr.apikey").expect("should warn");
        assert!(warning.contains("\"sonarr.apikey\""));
        assert!(warning.contains("is not a key sharerr reads"));
        // The known-key list is meant to help the operator fix the typo.
        for key in secret_keys::ALL {
            assert!(warning.contains(key), "warning should mention {key}");
        }
    }

    #[test]
    fn format_listing_empty_vault() {
        let text = format_listing(std::path::Path::new("/data/vault.bin"), &[]);
        assert_eq!(
            text,
            "vault is empty (/data/vault.bin)\nadd credentials with: sharerr vault set <key>\n"
        );
    }

    #[test]
    fn format_listing_lists_keys_without_values() {
        let text = format_listing(
            std::path::Path::new("/data/vault.bin"),
            &["sonarr.api_key", "radarr.api_key"],
        );
        assert_eq!(
            text,
            "/data/vault.bin:\n  sonarr.api_key\n  radarr.api_key\n"
        );
    }

    #[test]
    fn remove_message_when_present() {
        assert_eq!(
            remove_message("sonarr.api_key", true),
            "removed \"sonarr.api_key\""
        );
    }

    #[test]
    fn remove_message_when_absent() {
        assert_eq!(
            remove_message("sonarr.api_key", false),
            "\"sonarr.api_key\" was not in the vault"
        );
    }

    #[test]
    fn validate_secret_trims_whitespace_and_newline() {
        let secret = validate_secret("k", "  hunter2  \n").expect("should validate");
        assert_eq!(secret.expose_secret(), "hunter2");
    }

    #[test]
    fn validate_secret_rejects_empty_after_trim() {
        let err = validate_secret("sonarr.api_key", "   \n  ").unwrap_err();
        assert!(
            err.to_string()
                .contains("refusing to store an empty value for \"sonarr.api_key\"")
        );
    }

    #[test]
    fn validate_secret_rejects_truly_empty() {
        assert!(validate_secret("k", "").is_err());
    }

    /// `list`/`remove` both open the vault before doing anything else, and
    /// this checks that a missing `SHARERR_MASTER_KEY` surfaces as an error
    /// rather than a panic, regardless of which verb hits it first.
    ///
    /// `secrets.rs` has a `#[test]` that legitimately sets `SHARERR_MASTER_KEY`
    /// via `figment::Jail`, so relying on the var being merely *unset in this
    /// process* would race it under the parallel test runner. `Jail` clears the
    /// env for its closure and serializes against every other Jail-based test,
    /// so wrapping these in a `Jail` too — even though neither reads or writes
    /// an env var directly — is what actually makes "no master key" safe to
    /// assert here instead of racy.
    fn assert_open_vault_error_in_jail(open: impl FnOnce(&Config) -> Result<()>) {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            let config = Config {
                data_dir: jail.directory().to_path_buf(),
                ..Config::default()
            };
            assert!(open(&config).is_err());
            Ok(())
        });
    }

    #[test]
    fn list_without_a_master_key_is_an_error_not_a_panic() {
        assert_open_vault_error_in_jail(list);
    }

    #[test]
    fn remove_without_a_master_key_is_an_error_not_a_panic() {
        assert_open_vault_error_in_jail(|config| remove(config, "sonarr.api_key"));
    }

    /// `list`/`remove`'s success paths both need an actual open vault, which
    /// means a real `SHARERR_MASTER_KEY` — safe here (unlike a plain
    /// `std::env::set_var`) because `Jail` scopes it to this closure and
    /// serializes against every other Jail-based test in the binary.
    #[test]
    fn list_and_remove_succeed_against_a_real_vault() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("SHARERR_MASTER_KEY", "a-master-key");
            let config = Config {
                data_dir: jail.directory().to_path_buf(),
                ..Config::default()
            };

            let mut vault = open_vault(&config).expect("vault opens");
            vault
                .put("sonarr.api_key", &SecretString::from("s3cret".to_owned()))
                .expect("put succeeds");
            drop(vault);

            assert!(list(&config).is_ok());
            assert!(remove(&config, "sonarr.api_key").is_ok());
            assert!(
                remove(&config, "sonarr.api_key").is_ok(),
                "removing an absent key is not an error"
            );
            Ok(())
        });
    }
}
