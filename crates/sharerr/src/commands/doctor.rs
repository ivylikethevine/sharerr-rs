//! `sharerr doctor` — check everything a sync depends on, before a sync needs it.
//!
//! Almost every way sharerr can fail is silent: a tag that does not exist, a path
//! mapping that does not apply, an API key that was stored with a trailing newline.
//! None of those raise an error at startup, and all of them produce the same
//! symptom — nothing happens. This command exists to turn each one into a specific,
//! actionable line of output.
//!
//! Every check runs even after an earlier one fails, so a single invocation reports
//! the full picture rather than the first problem. The exit code is non-zero when
//! anything failed, which makes it usable as a container healthcheck.

use anyhow::{Result, bail};
use secrecy::SecretString;
use sharerr_arr::{ArrClient, Discovered};
use sharerr_core::config::{ServiceConfig, TrackerBackend, secret_keys};
use sharerr_core::{Config, MediaSource};
use sharerr_qbit::QbitClient;
use sharerr_store::{Store, Vault, master_key_from_env};

/// How many individual problem files to name before summarising the rest. A
/// library with a broken mapping has *every* file broken; printing all of them
/// buries the advice that would fix it.
const MAX_LISTED: usize = 5;

#[derive(Default)]
struct Report {
    failures: usize,
    warnings: usize,
}

impl Report {
    fn section(&self, title: &str) {
        println!("\n{title}");
    }

    fn ok(&self, message: impl std::fmt::Display) {
        println!("  [ok]   {message}");
    }

    fn info(&self, message: impl std::fmt::Display) {
        println!("         {message}");
    }

    fn warn(&mut self, message: impl std::fmt::Display) {
        self.warnings += 1;
        println!("  [warn] {message}");
    }

    fn fail(&mut self, message: impl std::fmt::Display) {
        self.failures += 1;
        println!("  [FAIL] {message}");
    }

    /// Report an error together with its cause chain.
    ///
    /// The distinction matters here: reqwest's own `Display` is just "error
    /// sending request for url (...)", and the part an operator actually needs —
    /// `Connection refused`, `dns error`, `operation timed out` — lives further
    /// down the chain.
    fn fail_err(&mut self, err: &dyn std::error::Error) {
        self.fail(chain(err));
    }
}

pub(crate) fn chain(err: &dyn std::error::Error) -> String {
    let mut rendered = err.to_string();
    let mut cause = err.source();

    while let Some(next) = cause {
        let text = next.to_string();
        // `#[source]` fields are often interpolated into the parent's message
        // already; only append what is genuinely new.
        if !rendered.contains(&text) {
            rendered.push_str(": ");
            rendered.push_str(&text);
        }
        cause = next.source();
    }

    rendered
}

pub async fn run(config: &Config, config_error: Option<&str>) -> Result<()> {
    let mut report = Report::default();

    report.section("configuration");
    // Reported as a check rather than bailing, so the rest of the picture is still
    // printed — and so the summary below explains why every subsequent line is
    // describing defaults instead of what the operator wrote.
    if let Some(error) = config_error {
        report.fail(error);
        report.info("everything below reflects built-in defaults, not this file");
    }
    print_config_summary(config);

    report.section("vault");
    let vault = check_vault(config, &mut report);

    report.section("database");
    check_database(config, &mut report).await;

    let mut discovered = Vec::new();
    if let Some(sonarr) = &config.sonarr {
        report.section("sonarr");
        discovered.extend(
            check_arr(
                MediaSource::Sonarr,
                sonarr,
                config,
                vault.as_ref(),
                &mut report,
            )
            .await,
        );
    }
    if let Some(radarr) = &config.radarr {
        report.section("radarr");
        discovered.extend(
            check_arr(
                MediaSource::Radarr,
                radarr,
                config,
                vault.as_ref(),
                &mut report,
            )
            .await,
        );
    }
    if config.sonarr.is_none() && config.radarr.is_none() {
        report.section("sonarr / radarr");
        report.fail("neither sonarr nor radarr is configured — there is nothing to share");
    }

    report.section("qbittorrent");
    check_qbit(config, vault.as_ref(), &mut report).await;

    report.section("tracker");
    check_tracker(config, &mut report);

    report.section("paths");
    check_paths(config, &discovered, &mut report);

    println!();
    match (report.failures, report.warnings) {
        (0, 0) => {
            println!("all checks passed");
            Ok(())
        }
        (0, warnings) => {
            println!("all checks passed, {warnings} warning(s)");
            Ok(())
        }
        (failures, warnings) => {
            bail!("{failures} check(s) failed, {warnings} warning(s)")
        }
    }
}

// ------------------------------------------------------------------ vault

fn check_vault(config: &Config, report: &mut Report) -> Option<Vault> {
    let master = match master_key_from_env() {
        Ok(master) => master,
        Err(err) => {
            report.fail_err(&err);
            return None;
        }
    };

    let vault = match Vault::open(config.vault_path(), &master) {
        Ok(vault) => vault,
        Err(err) => {
            report.fail(format!(
                "{} — {}",
                config.vault_path().display(),
                chain(&err)
            ));
            return None;
        }
    };

    report.ok(format!("opened {}", config.vault_path().display()));

    // Report only which keys are present. Values are never printed, by design.
    let mut expected = vec![secret_keys::QBITTORRENT_PASSWORD];
    if config.sonarr.is_some() {
        expected.push(secret_keys::SONARR_API_KEY);
    }
    if config.radarr.is_some() {
        expected.push(secret_keys::RADARR_API_KEY);
    }

    for key in expected {
        match vault.get(key) {
            Ok(Some(_)) => report.ok(format!("{key} is set")),
            Ok(None) => report.fail(format!("{key} is missing — {}", fix_hint(key))),
            Err(err) => report.fail(format!("{key} could not be read: {err}")),
        }
    }

    // A warning, not a failure: an instance can reconcile and seed perfectly well
    // without ever publishing a feed. It just cannot be found by a friend.
    match vault.get(secret_keys::TORZNAB_API_KEY) {
        Ok(Some(_)) => report.ok(format!("{} is set", secret_keys::TORZNAB_API_KEY)),
        Ok(None) => report.warn(
            "no torznab.api_key — the indexer feed is closed, so no friend can \
             find what this instance shares. Generate one in Settings.",
        ),
        Err(err) => report.fail(format!("torznab.api_key could not be read: {err}")),
    }

    Some(vault)
}

/// How to supply a missing secret.
///
/// Names the web UI first and the CLI second, which is the order of least effort
/// since the UI needs no shell inside the container. Both write the same vault.
fn fix_hint(key: &str) -> String {
    format!("set it in Settings on the web UI, or run: sharerr vault set {key}")
}

/// Fetch a secret, reporting the precise reason it is unavailable — **exactly
/// once**. A read error and a missing entry have different fixes, and reporting a
/// decryption failure as a missing value sends the operator the wrong way.
fn secret(vault: Option<&Vault>, key: &str, report: &mut Report) -> Option<SecretString> {
    let Some(vault) = vault else {
        // The vault section already said why it could not be opened; do not
        // restate it, but do record that this check could not run.
        report.fail(format!(
            "skipped: {key} is unreadable until the vault opens"
        ));
        return None;
    };

    match vault.get(key) {
        Ok(Some(value)) => Some(value),
        Ok(None) => {
            report.fail(format!("{key} is missing — {}", fix_hint(key)));
            None
        }
        Err(err) => {
            report.fail(format!("{key} could not be read: {}", chain(&err)));
            None
        }
    }
}

// ------------------------------------------------------------------ database

async fn check_database(config: &Config, report: &mut Report) {
    let path = config.database_path();
    match Store::open(&path).await {
        Ok(store) => {
            report.ok(format!("opened {} and applied migrations", path.display()));
            match store.all_items().await {
                Ok(items) => report.info(format!("{} item(s) on record", items.len())),
                Err(err) => report.fail(format!("reading shared_items: {err}")),
            }
        }
        Err(err) => report.fail(format!("{}: {}", path.display(), chain(&err))),
    }
}

// ------------------------------------------------------------------ *arr

async fn check_arr(
    kind: MediaSource,
    service: &ServiceConfig,
    config: &Config,
    vault: Option<&Vault>,
    report: &mut Report,
) -> Vec<Discovered> {
    let key_name = match kind {
        MediaSource::Sonarr => secret_keys::SONARR_API_KEY,
        MediaSource::Radarr => secret_keys::RADARR_API_KEY,
    };

    let Some(api_key) = secret(vault, key_name, report) else {
        return Vec::new();
    };

    let client = match ArrClient::new(kind, &service.url, api_key) {
        Ok(client) => client,
        Err(err) => {
            report.fail_err(&err);
            return Vec::new();
        }
    };

    // Reachability and authentication first: every later check would report the
    // same underlying problem in a less useful way.
    match client.system_status().await {
        Ok(status) => {
            let name = if status.app_name.is_empty() {
                kind.as_str()
            } else {
                &status.app_name
            };
            report.ok(format!(
                "{} responded ({name} {})",
                service.url, status.version
            ));
        }
        Err(err) => {
            report.fail_err(&err);
            return Vec::new();
        }
    }

    match client.discover(&config.tag).await {
        Ok(items) if items.is_empty() => {
            report.warn(format!(
                "tag {:?} exists but nothing carries it — apply it to content you want to share",
                config.tag
            ));
            Vec::new()
        }
        Ok(items) => {
            report.ok(format!("{} file(s) tagged {:?}", items.len(), config.tag));
            if kind == MediaSource::Sonarr {
                // Not a defect, but it surprises people: Sonarr has no
                // episode-level tags, so tagging a series shares all of it.
                report.info("note: Sonarr tags are series-level, so every episode file is shared");
            }
            items
        }
        Err(err) => {
            report.fail_err(&err);
            Vec::new()
        }
    }
}

// ------------------------------------------------------------------ qBittorrent

async fn check_qbit(config: &Config, vault: Option<&Vault>, report: &mut Report) {
    let Some(password) = secret(vault, secret_keys::QBITTORRENT_PASSWORD, report) else {
        return;
    };

    let qbit = match QbitClient::new(
        &config.qbittorrent.url,
        &config.qbittorrent.username,
        password,
    ) {
        Ok(qbit) => qbit,
        Err(err) => {
            report.fail_err(&err);
            return;
        }
    };

    match qbit.version().await {
        Ok(version) => report.ok(format!("{} responded ({version})", config.qbittorrent.url)),
        Err(err) => {
            report.fail_err(&err);
            return;
        }
    }

    match qbit
        .torrents_info(Some(&config.qbittorrent.category), None)
        .await
    {
        Ok(torrents) => report.info(format!(
            "{} torrent(s) already in category {:?}",
            torrents.len(),
            config.qbittorrent.category
        )),
        Err(err) => report.warn(format!("could not list torrents: {err}")),
    }

    if config.tracker.backend == TrackerBackend::QbittorrentEmbedded {
        match qbit.preferences().await {
            Ok(prefs) if prefs.enable_embedded_tracker => {
                report.ok(format!(
                    "embedded tracker on, port {}",
                    prefs.embedded_tracker_port
                ));
                report.info(format!(
                    "port {} must be reachable by friends, not just on the docker network",
                    prefs.embedded_tracker_port
                ));
            }
            Ok(_) => report.warn("embedded tracker is off — sharerr will enable it on first sync"),
            Err(err) => report.fail(format!("reading preferences: {err}")),
        }
    }
}

// ------------------------------------------------------------------ tracker

fn check_tracker(config: &Config, report: &mut Report) {
    match config.tracker.backend {
        TrackerBackend::QbittorrentEmbedded => {
            report.ok("backend: qbittorrent-embedded");
        }
        TrackerBackend::Builtin => {
            report.ok("backend: builtin — sharerr serves /announce itself");
            // The one way this backend can look configured and still not work:
            // `doctor` and `sync` are one-shot commands, and the announce endpoint
            // only exists while `serve` is running.
            report.info(
                "announces are answered by `sharerr serve`; a one-shot sync builds \
                 correct torrents whose announces fail until it is running",
            );
        }
    }

    match &config.tracker.advertised_host {
        Some(host) => report.ok(format!("advertised host: {host}")),
        None => report.fail(
            "tracker.advertised_host is unset. sharerr cannot guess the address friends \
             reach it on, and a wrong guess produces torrents nobody can announce to",
        ),
    }
}

// ------------------------------------------------------------------ paths

/// The check this command exists for.
///
/// A file has up to three absolute paths at once, and a mismatch between them is
/// the most likely cause of a sync that appears to work and shares nothing.
fn check_paths(config: &Config, discovered: &[Discovered], report: &mut Report) {
    if config.path_map.is_empty() {
        report.info("no path_map configured — all three views are assumed identical");
    } else {
        report.ok(format!(
            "{} mapping rule(s) configured",
            config.path_map.len()
        ));
    }

    if discovered.is_empty() {
        report.info("nothing tagged, so there are no paths to resolve");
        return;
    }

    let resolver = config.resolver();
    let mut unmapped = 0usize;
    let mut missing = Vec::new();
    let mut invalid = 0usize;
    let mut sample = None;

    for item in discovered {
        match resolver.resolve(&item.arr_path) {
            Ok(paths) => {
                if !paths.mapping_applied {
                    unmapped += 1;
                }
                if !paths.sharerr.exists() {
                    missing.push(paths.sharerr.clone());
                }
                if sample.is_none() {
                    sample = Some(paths);
                }
            }
            Err(err) => {
                invalid += 1;
                if invalid <= MAX_LISTED {
                    report.fail_err(&err);
                }
            }
        }
    }

    if let Some(paths) = sample {
        report.info(format!(
            "example resolution, {} file(s) checked:",
            discovered.len()
        ));
        report.info(format!("  arr view     {}", paths.arr.display()));
        report.info(format!("  sharerr view {}", paths.sharerr.display()));
        // sharerr cannot stat this one — it is another container's filesystem.
        report.info(format!(
            "  qbit view    {} (verify against qBittorrent)",
            paths.qbit.display()
        ));
    }

    if unmapped > 0 && !config.path_map.is_empty() {
        report.warn(format!(
            "{unmapped} of {} file(s) matched no mapping rule and passed through unchanged",
            discovered.len()
        ));
    }

    if missing.is_empty() {
        report.ok(format!(
            "all {} tagged file(s) are readable by sharerr",
            discovered.len()
        ));
    } else {
        report.fail(format!(
            "{} of {} tagged file(s) do not exist at their sharerr-view path",
            missing.len(),
            discovered.len()
        ));
        for path in missing.iter().take(MAX_LISTED) {
            report.info(format!("  {}", path.display()));
        }
        if missing.len() > MAX_LISTED {
            report.info(format!("  ... and {} more", missing.len() - MAX_LISTED));
        }
        report.info("fix the [[path_map]] rules so the arr view maps onto sharerr's mount");
    }
}

// ------------------------------------------------------------------ summary

fn print_config_summary(config: &Config) {
    println!("  data dir:  {}", config.data_dir.display());
    println!("  tag:       {}", config.tag);
    println!("  bind:      {}", config.server.bind);
    println!(
        "  sonarr:    {}",
        config
            .sonarr
            .as_ref()
            .map_or("(not configured)", |s| s.url.as_str())
    );
    println!(
        "  radarr:    {}",
        config
            .radarr
            .as_ref()
            .map_or("(not configured)", |s| s.url.as_str())
    );
    println!(
        "  qbit:      {} (user {})",
        config.qbittorrent.url, config.qbittorrent.username
    );
    println!(
        "  tracker:   {:?} advertised_host={}",
        config.tracker.backend,
        config
            .tracker
            .advertised_host
            .as_deref()
            .unwrap_or("(unset)")
    );
    if config.path_map.is_empty() {
        println!("  path map:  (none — all three views assumed identical)");
    } else {
        for m in &config.path_map {
            println!(
                "  path map:  arr {} -> sharerr {} -> qbit {}",
                m.arr.display(),
                m.sharerr.display(),
                m.qbit.as_ref().unwrap_or(&m.sharerr).display()
            );
        }
    }
}
