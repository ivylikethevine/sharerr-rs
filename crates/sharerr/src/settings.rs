//! Configuration loading: defaults -> TOML file -> `SHARERR_*` environment.
//!
//! Nested keys use a double underscore, so `SHARERR_QBITTORRENT__URL` sets
//! `qbittorrent.url`. A missing config file is not an error — a deployment can
//! be configured entirely through the environment, which is the common case for
//! a docker-compose setup.

use std::path::Path;

use anyhow::{Context, Result};
use figment::Figment;
use figment::providers::{Env, Format, Serialized, Toml};
use sharerr_core::Config;
use sharerr_store::vault::{ENV_MASTER_KEY, ENV_MASTER_KEY_FILE};

/// `SHARERR_`-prefixed variables that are *not* config fields.
///
/// `Config` uses `deny_unknown_fields` so that a typo like `SHARERR_TAAG` is a
/// startup error rather than a setting that silently does nothing. That strictness
/// means every non-config variable sharing the prefix has to be excluded here, or
/// simply pointing `SHARERR_CONFIG` at a file would fail to load it.
pub const NON_CONFIG_ENV: &[&str] = &[
    "CONFIG",
    strip_prefix(ENV_MASTER_KEY),
    strip_prefix(ENV_MASTER_KEY_FILE),
    // Read by the opt-in e2e suite (crates/sharerr/tests/e2e.rs). Without this,
    // a developer who exports it gets an unrelated startup failure from any
    // sharerr command. Every new SHARERR_-prefixed variable that is not a config
    // field has to be listed here — that cost is the price of `deny_unknown_fields`
    // turning a typo into an error instead of a setting that silently does nothing.
    "E2E_MEDIA",
];

const fn strip_prefix(var: &str) -> &str {
    match var.as_bytes() {
        [b'S', b'H', b'A', b'R', b'E', b'R', b'R', b'_', ..] => match var.split_at_checked(8) {
            Some((_, rest)) => rest,
            None => var,
        },
        _ => var,
    }
}

pub fn load(path: &Path) -> Result<Config> {
    layered(Toml::file(path)).extract().with_context(|| {
        format!(
            "failed to load configuration (file: {}, env: SHARERR_*)",
            path.display()
        )
    })
}

/// Load from TOML held in memory rather than on disk.
///
/// The web UI uses this to prove an edited `sharerr.toml` still parses *before*
/// it replaces the real file. Going through the identical layering matters: with
/// `deny_unknown_fields`, a document that survives a bare TOML parse can still be
/// rejected by `extract`, and discovering that at the next startup would leave the
/// operator locked out of the UI they would need to fix it.
pub fn validate(toml_text: &str) -> Result<Config> {
    layered(Toml::string(toml_text))
        .extract()
        .context("the edited configuration is not valid")
}

/// The one definition of how the layers stack: defaults, then the document, then
/// `SHARERR_*`. Both entry points share it so validation can never drift from
/// what startup actually does.
fn layered<P: figment::Provider>(document: P) -> Figment {
    Figment::from(Serialized::defaults(Config::default()))
        .merge(document)
        .merge(Env::prefixed("SHARERR_").ignore(NON_CONFIG_ENV).split("__"))
}

#[cfg(test)]
mod tests {
    // `Jail::expect_with` takes a closure returning `figment::Error`, which is
    // large; that is figment's shape, not something this crate can reduce.
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::result_large_err)]

    use super::*;
    use sharerr_core::config::TrackerBackend;

    /// `Jail` gives each test an isolated cwd and environment, so these are safe
    /// to run in parallel despite touching process-global env vars.
    #[test]
    fn defaults_apply_when_no_file_or_env() {
        figment::Jail::expect_with(|jail| {
            let cfg = load(&jail.directory().join("absent.toml")).expect("defaults load");
            assert_eq!(cfg.tag, "sharerr");
            assert_eq!(cfg.server.bind.port(), 8477);
            assert_eq!(cfg.tracker.backend, TrackerBackend::QbittorrentEmbedded);
            assert!(!cfg.qbittorrent.skip_checking);
            Ok(())
        });
    }

    #[test]
    fn toml_overrides_defaults_and_env_overrides_toml() {
        figment::Jail::expect_with(|jail| {
            jail.create_file(
                "sharerr.toml",
                r#"
                tag = "from-file"
                [qbittorrent]
                url = "http://qbit:8080"
                username = "from-file"
                "#,
            )?;
            jail.set_env("SHARERR_QBITTORRENT__USERNAME", "from-env");

            let cfg = load(&jail.directory().join("sharerr.toml")).expect("layered load");
            assert_eq!(cfg.tag, "from-file", "file overrides default");
            assert_eq!(cfg.qbittorrent.url.as_str(), "http://qbit:8080/");
            assert_eq!(cfg.qbittorrent.username, "from-env", "env overrides file");
            Ok(())
        });
    }

    #[test]
    fn path_mappings_parse_as_an_array_of_tables() {
        figment::Jail::expect_with(|jail| {
            jail.create_file(
                "sharerr.toml",
                r#"
                [[path_map]]
                arr = "/tv"
                sharerr = "/media/tv"
                qbit = "/downloads/tv"

                [[path_map]]
                arr = "/movies"
                sharerr = "/media/movies"
                "#,
            )?;
            let cfg = load(&jail.directory().join("sharerr.toml")).expect("path map load");
            assert_eq!(cfg.path_map.len(), 2);
            assert_eq!(
                cfg.path_map[0].qbit.as_deref(),
                Some(Path::new("/downloads/tv"))
            );
            assert_eq!(cfg.path_map[1].qbit, None);
            Ok(())
        });
    }

    #[test]
    fn sharerr_prefixed_non_config_vars_do_not_break_loading() {
        // Regression: SHARERR_CONFIG and SHARERR_MASTER_KEY share the prefix but
        // are not config fields, and `deny_unknown_fields` rejected them.
        figment::Jail::expect_with(|jail| {
            jail.set_env("SHARERR_CONFIG", "/config/sharerr.toml");
            jail.set_env("SHARERR_MASTER_KEY", "a-master-key");
            jail.set_env("SHARERR_MASTER_KEY_FILE", "/run/secrets/key");
            jail.set_env("SHARERR_TAG", "still-works");

            let cfg = load(&jail.directory().join("absent.toml"))
                .expect("non-config SHARERR_* vars must be ignored, not rejected");
            assert_eq!(cfg.tag, "still-works");
            Ok(())
        });
    }

    #[test]
    fn unknown_keys_are_rejected_rather_than_silently_ignored() {
        figment::Jail::expect_with(|jail| {
            jail.create_file("sharerr.toml", "taag = \"typo\"\n")?;
            let err = load(&jail.directory().join("sharerr.toml"))
                .expect_err("a typo'd key must not be silently dropped");
            assert!(
                format!("{err:#}").contains("taag"),
                "error should name the offending key"
            );
            Ok(())
        });
    }
}
