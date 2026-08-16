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

use crate::checks::{self, ArrOutcome, QbitOutcome, chain};
use secrecy::SecretString;
use sharerr_arr::Discovered;
use sharerr_core::config::{ServiceConfig, TorrentBackend, TrackerBackend, secret_keys};
use sharerr_core::{Config, MediaSource};
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

    report.section(config.torrent_backend.as_str());
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
    // The password key follows the *configured* client. Demanding qBittorrent's
    // regardless was a check that failed on a perfectly good Transmission setup —
    // and told the operator to go and set a credential nothing would ever read.
    let mut expected = vec![match config.torrent_backend {
        TorrentBackend::Qbittorrent => secret_keys::QBITTORRENT_PASSWORD,
        TorrentBackend::Transmission => secret_keys::TRANSMISSION_PASSWORD,
    }];
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
    let key_name = secret_keys::api_key_for(kind);

    // `secret` reports its own failure, in this command's voice and with the
    // `vault set` hint. Handing `checks` an `Ok(None)` afterwards would report it a
    // second time, so a missing credential short-circuits here instead.
    let Some(api_key) = secret(vault, key_name, report) else {
        return Vec::new();
    };

    let outcome = checks::check_arr(kind, Some(&service.url), Ok(Some(api_key)), &config.tag).await;

    // The rendering is this command's own — a terminal report in the third person,
    // where the web UI writes a badge in the second. What the two can no longer do
    // is disagree about which condition they found.
    match outcome {
        ArrOutcome::Ready {
            version,
            app_name,
            items,
        } => {
            report.ok(format!("{} responded ({app_name} {version})", service.url));
            report.ok(format!("{} file(s) tagged {:?}", items.len(), config.tag));
            if kind == MediaSource::Sonarr {
                // Not a defect, but it surprises people: Sonarr has no
                // episode-level tags, so tagging a series shares all of it.
                report.info("note: Sonarr tags are series-level, so every episode file is shared");
            }
            items
        }
        ArrOutcome::TagUnused { version } => {
            report.ok(format!("{} responded ({version})", service.url));
            report.warn(format!(
                "tag {:?} exists but nothing carries it — apply it to content you want to share",
                config.tag
            ));
            Vec::new()
        }
        // Distinct from the above, and it was not previously reported here at all:
        // a tag that does not exist needs creating, not applying.
        ArrOutcome::TagMissing { version } => {
            report.ok(format!("{} responded ({version})", service.url));
            report.fail(format!(
                "no tag named {:?} exists in {} — create it, then apply it to content",
                config.tag,
                kind.as_str()
            ));
            Vec::new()
        }
        ArrOutcome::AuthRejected => {
            report.fail(format!(
                "{} rejected the API key — check {key_name}",
                service.url
            ));
            Vec::new()
        }
        ArrOutcome::Unreachable(reason) => {
            report.fail(format!("{} could not be reached: {reason}", service.url));
            Vec::new()
        }
        ArrOutcome::BadUrl(reason) | ArrOutcome::Failed(reason) => {
            report.fail(reason);
            Vec::new()
        }
        // Both are unreachable from here: the credential was resolved above.
        ArrOutcome::NotConfigured | ArrOutcome::NoCredential => Vec::new(),
        ArrOutcome::CredentialUnreadable(reason) => {
            report.fail(reason);
            Vec::new()
        }
    }
}

// ------------------------------------------------------------ torrent client

/// Everything about whichever torrent client is configured.
///
/// Which one that is comes from `torrent_backend`. The section names in
/// `sharerr.toml` differ per client, so the URL, username and vault key are all
/// resolved together rather than assuming qBittorrent's.
async fn check_qbit(config: &Config, vault: Option<&Vault>, report: &mut Report) {
    let (url, username, key, label) = match config.torrent_backend {
        TorrentBackend::Qbittorrent => (
            &config.qbittorrent.url,
            config.qbittorrent.username.as_str(),
            secret_keys::QBITTORRENT_PASSWORD,
            config.qbittorrent.category.clone(),
        ),
        TorrentBackend::Transmission => (
            &config.transmission.url,
            config.transmission.username.as_str(),
            secret_keys::TRANSMISSION_PASSWORD,
            config.transmission.label.clone(),
        ),
    };

    let Some(password) = secret(vault, key, report) else {
        return;
    };

    // Shared with the web UI's "Test connection" button — see `crate::checks`. The
    // client comes back on success so the checks below do not re-authenticate.
    let client =
        match checks::check_qbit(config.torrent_backend, url, username, Ok(Some(password))).await {
            QbitOutcome::Ready {
                version,
                kind,
                client,
            } => {
                report.ok(format!("{url} responded ({kind} {version})"));
                client
            }
            QbitOutcome::AuthRejected => {
                report.fail(format!(
                    "{url} rejected the username or password — check {key}"
                ));
                return;
            }
            QbitOutcome::Unreachable(reason) => {
                report.fail(format!("{url} could not be reached: {reason}"));
                return;
            }
            QbitOutcome::BadUrl(reason)
            | QbitOutcome::Failed(reason)
            | QbitOutcome::CredentialUnreadable(reason) => {
                report.fail(reason);
                return;
            }
            // Unreachable: the credential was resolved above.
            QbitOutcome::NoCredential => return,
        };

    match client.list(Some(&label)).await {
        Ok(torrents) => report.info(format!(
            "{} torrent(s) already labelled {label:?}",
            torrents.len()
        )),
        Err(err) => report.warn(format!("could not list torrents: {err}")),
    }

    if config.tracker.backend == TrackerBackend::QbittorrentEmbedded {
        match client.embedded_tracker_port().await {
            Ok(Some(port)) if port > 0 => {
                report.ok(format!("embedded tracker on, port {port}"));
                report.info(format!(
                    "port {port} must be reachable by friends, not just on the docker network"
                ));
            }
            Ok(Some(_)) => {
                report.warn("embedded tracker is off — sharerr will enable it on first sync");
            }
            // The configuration mistake this whole abstraction exists to catch:
            // a client with no embedded tracker, selected as the tracker backend.
            // Every torrent built would announce to a port nothing listens on.
            Ok(None) => report.fail(format!(
                "{} has no embedded tracker — set tracker.backend to \"builtin\" so \
                 sharerr serves announces itself",
                client.kind()
            )),
            Err(err) => report.fail(format!("reading the tracker state: {err}")),
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

    // Resolution itself is shared with the web UI — see `crate::checks`. Only the
    // wording below belongs to this command.
    let paths = checks::check_paths(config, discovered);

    for reason in paths.invalid.iter().take(MAX_LISTED) {
        report.fail(reason);
    }
    if paths.invalid.len() > MAX_LISTED {
        report.info(format!(
            "  ... and {} more unresolvable path(s)",
            paths.invalid.len() - MAX_LISTED
        ));
    }

    if let Some(sample) = &paths.sample {
        report.info(format!(
            "example resolution, {} file(s) checked:",
            paths.checked
        ));
        report.info(format!("  arr view     {}", sample.arr.display()));
        report.info(format!("  sharerr view {}", sample.sharerr.display()));
        // sharerr cannot stat this one — it is another container's filesystem.
        report.info(format!(
            "  qbit view    {} (verify against qBittorrent)",
            sample.qbit.display()
        ));
    }

    if paths.unmapped > 0 && paths.rules > 0 {
        report.warn(format!(
            "{} of {} file(s) matched no mapping rule and passed through unchanged",
            paths.unmapped, paths.checked
        ));
    }

    if paths.missing.is_empty() {
        report.ok(format!(
            "all {} tagged file(s) are readable by sharerr",
            paths.checked
        ));
    } else {
        report.fail(format!(
            "{} of {} tagged file(s) do not exist at their sharerr-view path",
            paths.missing.len(),
            paths.checked
        ));
        for path in paths.missing.iter().take(MAX_LISTED) {
            report.info(format!("  {}", path.display()));
        }
        if paths.missing.len() > MAX_LISTED {
            report.info(format!(
                "  ... and {} more",
                paths.missing.len() - MAX_LISTED
            ));
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
    // The configured client, not always qBittorrent's — printing the unused
    // section's URL is how an operator ends up debugging the wrong service.
    let (client_url, client_user) = match config.torrent_backend {
        TorrentBackend::Qbittorrent => (
            config.qbittorrent.url.as_str(),
            config.qbittorrent.username.as_str(),
        ),
        TorrentBackend::Transmission => (
            config.transmission.url.as_str(),
            config.transmission.username.as_str(),
        ),
    };
    println!(
        "  client:    {} {client_url} (user {client_user})",
        config.torrent_backend.as_str()
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
