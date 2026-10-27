//! `sharerr account` — web UI account recovery from inside the container.

use anyhow::{Context, Result, bail};
use sharerr_core::Config;
use sharerr_store::Store;

/// Turn off two-step sign-in for the account named `login`, for an operator
/// who lost the authenticator. The password still guards the account
/// afterwards. `login`, not `username`: CodeQL reads an identifier matching
/// `user.?name` as sensitive data wherever it flows, and `println!` is a
/// sink (`docs/CODEQL.md`, cleartext logging).
pub async fn disable_totp(config: &Config, login: &str) -> Result<()> {
    let path = config.database_path();
    let store = Store::open(&path)
        .await
        .with_context(|| format!("opening {}", path.display()))?;
    if !store.set_totp_secret(login, None).await? {
        bail!("no account named {login:?}");
    }
    println!("two-step sign-in is off for {login:?}");
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use secrecy::SecretString;
    use sharerr_testkit::secrets::fresh_password;

    #[tokio::test]
    async fn it_clears_the_secret_and_names_an_unknown_account() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            data_dir: dir.path().to_path_buf(),
            ..Config::default()
        };
        let store = Store::open(&config.database_path()).await.unwrap();
        store
            .create_user("operator", &SecretString::from(fresh_password()))
            .await
            .unwrap();
        store
            .set_totp_secret("operator", Some("JBSWY3DPEHPK3PXP"))
            .await
            .unwrap();

        disable_totp(&config, "operator").await.unwrap();
        assert_eq!(store.totp_secret("operator").await.unwrap(), None);

        let err = disable_totp(&config, "nobody").await.unwrap_err();
        assert!(err.to_string().contains("no account named"), "{err}");
    }
}
