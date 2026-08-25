//! Generating and comparing the secrets sharerr mints for itself.
//!
//! Three consumers needed the same two operations and had grown their own copies:
//! the session table and the settings page both minted random hex, and the tracker
//! and the Torznab endpoint both compared a supplied secret against a stored one.
//! They live here so the *properties* — a single entropy source, and a comparison
//! that does not short-circuit — hold everywhere rather than per call site.

use anyhow::Context;
use secrecy::{ExposeSecret, SecretString};
use sharerr_core::Config;
use sharerr_store::{Vault, master_key_from_env};

/// The entropy of every secret sharerr mints: 160 bits, hex encoded. Long
/// enough that guessing is not a strategy, short enough to paste into another
/// application's settings box. One constant, so peer keys and the Torznab and
/// tracker secrets cannot quietly diverge in strength.
pub const KEY_BYTES: usize = 20;

/// A fresh secret: `bytes` bytes of entropy, hex encoded.
///
/// Same source the vault uses for its salts and nonces. Hex rather than base64 so
/// the result survives being pasted into a URL, a config file, or another app's
/// settings box without escaping.
pub fn random_hex(bytes: usize) -> Result<String, String> {
    let mut raw = vec![0u8; bytes];
    getrandom::fill(&mut raw).map_err(|err| format!("could not generate a secret: {err}"))?;
    Ok(hex::encode(raw))
}

/// `N` raw bytes from the same entropy source, for key material that is not a
/// pasteable secret — the gossip signing key and the lighthouse decoy seed.
pub fn random_bytes<const N: usize>() -> Result<[u8; N], String> {
    let mut raw = [0u8; N];
    getrandom::fill(&mut raw).map_err(|err| format!("could not generate key material: {err}"))?;
    Ok(raw)
}

/// Load a 32-byte seed stored hex-encoded under `key`, minting and storing
/// one on first use. `what` names it in errors ("identity key", "lighthouse
/// decoy seed"). The returned flag is `true` when this call minted it, so the
/// caller can log the event with whatever it derives from the seed.
///
/// One function for the gossip signing key and the lighthouse decoy seed:
/// both are 32 bytes of key material that must persist across restarts, and
/// the load-or-mint dance is the same for each.
pub fn load_or_create_seed(
    vault: &mut Vault,
    key: &'static str,
    what: &str,
) -> Result<([u8; 32], bool), String> {
    if let Ok(Some(stored)) = vault.get(key) {
        let mut bytes = [0u8; 32];
        hex::decode_to_slice(stored.expose_secret(), &mut bytes)
            .map_err(|_| format!("the stored {what} is not 32 hex bytes"))?;
        return Ok((bytes, false));
    }

    let seed = random_bytes::<32>().map_err(|err| format!("generating a {what}: {err}"))?;
    vault
        .put(key, &SecretString::from(hex::encode(seed)))
        .map_err(|err| format!("storing the {what}: {err}"))?;
    Ok((seed, true))
}

/// Compare two secrets without short-circuiting on the first difference.
///
/// A timing attack against a tracker token over a home connection is not a
/// realistic threat, and this is not here because it is. It is here because `==`
/// on a secret is the kind of line that gets copied into somewhere it *does*
/// matter, and because the constant-time version costs nothing.
///
/// Note the length comparison is not constant-time and cannot be: differing
/// lengths must be rejected, and that fact is observable however it is written.
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |differences, (x, y)| differences | (x ^ y))
            == 0
}

/// Open the credential vault `config` names.
///
/// The one place the master key is read and the vault path is resolved, so the
/// CLI's `vault` verb, `doctor`, the one-shot `sync`, the syncer, and `ServeState`
/// share one edit instead of five when how the vault opens changes.
pub fn open_vault(config: &Config) -> anyhow::Result<Vault> {
    let master = master_key_from_env()?;
    Vault::open(config.vault_path(), &master)
        .with_context(|| format!("opening vault at {}", config.vault_path().display()))
}

/// [`open_vault`], off the async runtime.
///
/// Opening the vault derives its key with Argon2 — tens of milliseconds of solid
/// CPU and ~19 MiB, considerably more on the ARM boxes this ships to. A container
/// pinned to one CPU has exactly one runtime worker, so doing this inline stalls
/// every other request, `/health` included, for the duration. Every async caller
/// goes through here rather than repeating the `spawn_blocking` and the reason
/// for it.
pub async fn open_vault_async(config: &Config) -> anyhow::Result<Vault> {
    open_vault_at(config.vault_path()).await
}

/// [`open_vault_async`] for a caller that already has the path and would
/// otherwise clone a whole [`Config`] to hand it over.
pub async fn open_vault_at(path: std::path::PathBuf) -> anyhow::Result<Vault> {
    let master = master_key_from_env()?;

    tokio::task::spawn_blocking(move || {
        Vault::open(&path, &master).with_context(|| format!("opening vault at {}", path.display()))
    })
    .await?
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::result_large_err)]

    use super::*;

    #[test]
    fn a_generated_secret_is_hex_of_the_requested_width() {
        let key = random_hex(20).unwrap();
        assert_eq!(key.len(), 40, "one byte renders as two hex digits");
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()));

        assert_eq!(random_hex(32).unwrap().len(), 64);
    }

    #[test]
    fn two_secrets_are_never_the_same() {
        assert_ne!(random_hex(32).unwrap(), random_hex(32).unwrap());
    }

    #[test]
    fn comparison_accepts_only_an_exact_match() {
        assert!(constant_time_eq("s3cret", "s3cret"));
        assert!(constant_time_eq("", ""));

        for wrong in ["", "s3cre", "s3crets", "S3CRET", "s3crey"] {
            assert!(!constant_time_eq("s3cret", wrong), "{wrong:?} was accepted");
        }
    }

    #[test]
    fn key_material_is_full_width_and_never_repeats() {
        let a = random_bytes::<20>().unwrap();
        let b = random_bytes::<20>().unwrap();
        assert_eq!(a.len(), 20);
        assert_ne!(a, b);
    }

    // Jail scopes the env vars to the closure (and serializes with every other
    // Jail-based test), which is why this can safely touch SHARERR_MASTER_KEY —
    // see the note on `sharerr_prefixed_non_config_vars_do_not_break_loading` in
    // settings.rs for why a plain `std::env::set_var` cannot, in a suite that
    // does not scope env vars per test.
    #[test]
    fn opening_a_vault_without_a_master_key_fails_with_no_side_effects() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            let config = Config {
                data_dir: jail.directory().to_path_buf(),
                ..Config::default()
            };
            assert!(open_vault(&config).is_err());
            assert!(!config.vault_path().exists());
            Ok(())
        });
    }

    // `Jail::expect_with` is synchronous, but its closure can still host a
    // freshly-built runtime's `block_on` — that keeps the env mutation scoped
    // and serialized with every other Jail-based test rather than racing a
    // plain `std::env::set_var` against the rest of the (parallel) suite.
    #[test]
    fn open_vault_at_opens_the_vault_named_by_a_master_key() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("SHARERR_MASTER_KEY", "a-master-key");
            let path = jail.directory().join("vault.bin");

            let runtime = tokio::runtime::Runtime::new().unwrap();
            let vault = runtime.block_on(open_vault_at(path)).unwrap();
            assert!(vault.get("anything").unwrap().is_none());

            runtime
                .block_on(open_vault_async(&Config {
                    data_dir: jail.directory().to_path_buf(),
                    ..Config::default()
                }))
                .unwrap();
            Ok(())
        });
    }
}
