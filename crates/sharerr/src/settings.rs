//! Configuration loading: defaults -> TOML file -> `SHARERR_*` environment.
//!
//! Nested keys use a double underscore, so `SHARERR_QBITTORRENT__URL` sets
//! `qbittorrent.url`. A missing config file is not an error — a deployment can
//! be configured entirely through the environment, which is the common case for
//! a docker-compose setup.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use figment::Figment;
use figment::providers::{Env, Format, Serialized, Toml};
use sharerr_core::Config;
use sharerr_core::config::config_paths;
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
    // sharerr command.
    "E2E_MEDIA",
    "E2E_COMPOSE",
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

/// Load, or recover enough of a configuration to keep serving.
///
/// A malformed `sharerr.toml` used to abort startup, which under Docker is a
/// restart loop with no HTTP surface — and the web UI is the tool an operator
/// would use to fix it. So the error is returned alongside a usable `Config`
/// instead of replacing it; the caller decides how loudly to say so.
///
/// The fallback is *not* a bare `Config::default()`. `data_dir` and `server.bind`
/// are salvaged from the file when it parses as TOML at all, and from the
/// environment either way, because those two decide whether the operator can reach
/// the UI: `bind` is the port, and `data_dir` is where [`Config::database_path`]
/// and [`Config::vault_path`] point. Defaulting `data_dir` past a typo elsewhere in
/// the file would drop the operator into a *fresh, empty* instance and make their
/// real vault look lost.
pub fn load_or_recover(path: &Path) -> (Config, Option<String>) {
    let error = match load(path) {
        Ok(config) => return (config, None),
        Err(err) => format!("{err:#}"),
    };

    let mut config = Config::default();

    // Same order figment uses: the document first, then the environment on top.
    // Both reads address the fields through `config_paths`, so renaming a config
    // field breaks this in the same compile/test as everything else — the salvage
    // quietly falling back to defaults is precisely the failure it exists to
    // prevent.
    if let Ok(text) = std::fs::read_to_string(path)
        && let Ok(doc) = text.parse::<toml_edit::DocumentMut>()
    {
        if let Some(dir) = doc_str(&doc, config_paths::DATA_DIR) {
            config.data_dir = PathBuf::from(dir);
        }
        if let Some(bind) = doc_str(&doc, config_paths::SERVER_BIND)
            && let Ok(addr) = bind.parse()
        {
            config.server.bind = addr;
        }
    }

    if let Ok(dir) = std::env::var(config_paths::env_var(config_paths::DATA_DIR)) {
        config.data_dir = PathBuf::from(dir);
    }
    if let Ok(Ok(addr)) =
        std::env::var(config_paths::env_var(config_paths::SERVER_BIND)).map(|bind| bind.parse())
    {
        config.server.bind = addr;
    }

    (config, Some(error))
}

/// Walk a dotted `config_paths` path through a TOML document to a string value.
fn doc_str<'a>(doc: &'a toml_edit::DocumentMut, path: &str) -> Option<&'a str> {
    let mut segments = path.split('.');
    let mut item = doc.get(segments.next()?)?;
    for segment in segments {
        item = item.as_table_like()?.get(segment)?;
    }
    item.as_str()
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

    /// `Jail` gives each test an isolated cwd and environment, so these are safe
    /// to run in parallel despite touching process-global env vars.
    #[test]
    fn defaults_apply_when_no_file_or_env() {
        figment::Jail::expect_with(|jail| {
            let cfg = load(&jail.directory().join("absent.toml")).expect("defaults load");
            assert_eq!(cfg.tag, "sharerr");
            assert_eq!(cfg.server.bind.port(), 8477);
            assert!(cfg.qbittorrent.skip_checking);
            Ok(())
        });
    }

    /// The breaking change the roadmap promised an exact error for: a config
    /// still naming the removed `tracker.backend` must fail to load with the
    /// migration story, not a generic "unknown field".
    #[test]
    fn a_config_naming_the_removed_tracker_backend_gets_the_migration_error() {
        figment::Jail::expect_with(|jail| {
            jail.create_file(
                "sharerr.toml",
                "[tracker]\nbackend = \"qbittorrent-embedded\"\n",
            )?;
            let err = load(&jail.directory().join("sharerr.toml"))
                .expect_err("the removed setting must be rejected");
            let rendered = format!("{err:#}");
            assert!(
                rendered.contains("tracker.backend has been removed"),
                "the error must say what changed: {rendered}"
            );
            assert!(
                rendered.contains("qbittorrent-embedded"),
                "the error should quote the stale value: {rendered}"
            );
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
                category = "from-file"
                "#,
            )?;
            jail.set_env("SHARERR_QBITTORRENT__CATEGORY", "from-env");

            let cfg = load(&jail.directory().join("sharerr.toml")).expect("layered load");
            assert_eq!(cfg.tag, "from-file", "file overrides default");
            assert_eq!(cfg.qbittorrent.url.as_str(), "http://qbit:8080/");
            assert_eq!(cfg.qbittorrent.category, "from-env", "env overrides file");
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

    #[test]
    fn a_valid_file_recovers_to_itself_with_no_error() {
        figment::Jail::expect_with(|jail| {
            jail.create_file("sharerr.toml", "tag = \"from-file\"\n")?;
            let (cfg, err) = load_or_recover(&jail.directory().join("sharerr.toml"));
            assert_eq!(cfg.tag, "from-file");
            assert_eq!(err, None);
            Ok(())
        });
    }

    #[test]
    fn a_rejected_key_yields_defaults_plus_the_reason() {
        figment::Jail::expect_with(|jail| {
            jail.create_file("sharerr.toml", "taag = \"typo\"\n")?;
            let (cfg, err) = load_or_recover(&jail.directory().join("sharerr.toml"));
            assert_eq!(cfg.tag, "sharerr", "falls back to the default");
            assert!(
                err.expect("a rejected key must be reported")
                    .contains("taag"),
                "the reason must name the offending key"
            );
            Ok(())
        });
    }

    /// The whole point of salvaging rather than defaulting: the vault and database
    /// live under `data_dir`, so losing it to an unrelated typo would present the
    /// operator with an empty instance instead of theirs.
    #[test]
    fn recovery_keeps_the_data_dir_and_bind_a_broken_file_still_states() {
        figment::Jail::expect_with(|jail| {
            jail.create_file(
                "sharerr.toml",
                r#"
                taag = "typo"
                data_dir = "/srv/sharerr"
                [server]
                bind = "127.0.0.1:9999"
                "#,
            )?;
            let (cfg, err) = load_or_recover(&jail.directory().join("sharerr.toml"));
            assert!(err.is_some());
            assert_eq!(cfg.data_dir, Path::new("/srv/sharerr"));
            assert_eq!(cfg.server.bind.port(), 9999);
            Ok(())
        });
    }

    #[test]
    fn the_environment_still_wins_during_recovery() {
        figment::Jail::expect_with(|jail| {
            jail.create_file(
                "sharerr.toml",
                "taag = \"typo\"\ndata_dir = \"/from-file\"\n",
            )?;
            jail.set_env("SHARERR_DATA_DIR", "/from-env");
            jail.set_env("SHARERR_SERVER__BIND", "0.0.0.0:9100");

            let (cfg, _) = load_or_recover(&jail.directory().join("sharerr.toml"));
            assert_eq!(cfg.data_dir, Path::new("/from-env"));
            assert_eq!(cfg.server.bind.port(), 9100);
            Ok(())
        });
    }

    /// Nothing to salvage from — but there must still be a config to serve with,
    /// because this is precisely the state the UI has to be reachable to repair.
    #[test]
    fn a_file_that_is_not_toml_at_all_still_yields_a_usable_config() {
        figment::Jail::expect_with(|jail| {
            jail.create_file("sharerr.toml", "this is not toml at all {{{\n")?;
            let (cfg, err) = load_or_recover(&jail.directory().join("sharerr.toml"));
            assert!(err.is_some());
            assert_eq!(cfg.server.bind.port(), 8477);
            assert_eq!(cfg.data_dir, Path::new("/data"));
            Ok(())
        });
    }
}
