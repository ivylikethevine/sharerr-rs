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

use crate::checks::{self, ArrOutcome, DirOutcome, QbitOutcome, chain};
use secrecy::SecretString;
use sharerr_arr::Discovered;
use sharerr_core::config::{ServiceConfig, secret_keys};
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
    let sources = config.configured_sources();
    for kind in sources.iter().copied() {
        // `service()` is Some for everything `configured_sources` returned.
        let Some(service) = config.service(kind) else {
            continue;
        };
        report.section(kind.as_str());
        discovered.extend(check_arr(kind, service, config, vault.as_ref(), &mut report).await);
    }
    for library in &config.library {
        report.section(&format!("library {}", library.path.display()));
        discovered.extend(check_library(library, &mut report));
    }
    if sources.is_empty() && config.library.is_empty() {
        report.section("library sources");
        report.fail(
            "no *arr app or [[library]] directory is configured — there is nothing to share",
        );
    }

    report.section(config.torrent_backend.as_str());
    check_qbit(config, vault.as_ref(), &mut report).await;

    if config.gluetun.control_url.is_some() {
        report.section("gluetun");
        check_gluetun(config, &mut report).await;
    }

    report.section("tracker");
    check_tracker(config, &mut report);
    check_reachability(config, &mut report).await;

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
            report.fail(chain(&err));
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
    // The torrent client is satisfied by *either* of its credentials, so it is
    // checked as a pair rather than as two independent keys — demanding both would
    // fail an operator who moved to an API key and, correctly, cleared the
    // password they no longer use.
    let client = config.torrent_client();
    check_torrent_credential(&vault, client.api_key_key, client.password_key, report);

    for key in config
        .configured_sources()
        .into_iter()
        .filter_map(secret_keys::api_key_for)
    {
        match vault.get(key) {
            Ok(Some(_)) => report.ok(format!("{key} is set")),
            Ok(None) => report.fail(format!("{key} is missing — {}", fix_hint(key))),
            Err(err) => report.fail(format!("{key} could not be read: {err}")),
        }
    }

    Some(vault)
}

/// Report on the credential the configured torrent client will authenticate with.
///
/// One report line, not two, because the keys are alternatives: whichever is
/// present is the one that will be used, and only the absence of *both* is a
/// problem worth failing on.
fn check_torrent_credential(
    vault: &Vault,
    api_key_key: Option<&'static str>,
    password_key: Option<&'static str>,
    report: &mut Report,
) {
    let api_key = match api_key_key {
        Some(key) => match vault.get(key) {
            Ok(value) => value.map(|_| key),
            Err(err) => {
                report.fail(format!("{key} could not be read: {err}"));
                None
            }
        },
        None => None,
    };

    if let Some(key) = api_key {
        match password_key {
            Some(password_key) => report.ok(format!(
                "{key} is set — it takes precedence over {password_key}"
            )),
            None => report.ok(format!("{key} is set")),
        }
        return;
    }

    // No password concept for this backend either — qBittorrent authenticates by
    // API key alone, so a missing key here is the whole story.
    let Some(password_key) = password_key else {
        if let Some(key) = api_key_key {
            report.fail(format!("{key} is missing — {}", fix_hint(key)));
        }
        return;
    };

    match vault.get(password_key) {
        Ok(Some(_)) => report.ok(format!("{password_key} is set")),
        Ok(None) => report.fail(format!(
            "{password_key} is missing — {}",
            fix_hint(password_key)
        )),
        Err(err) => report.fail(format!("{password_key} could not be read: {err}")),
    }
}

/// How to supply a missing secret.
///
/// Names the web UI first and the CLI second, which is the order of least effort
/// since the UI needs no shell inside the container. Both write the same vault.
fn fix_hint(key: &str) -> String {
    format!("set it in Settings on the web UI, or run: sharerr vault set {key}")
}

/// Fetch an *optional* secret without reporting its absence.
///
/// For credentials that are alternatives rather than requirements: a missing API
/// key is not a fault when a password is configured, and saying so would turn a
/// correct setup into a failing report.
fn quiet_secret(vault: Option<&Vault>, key: &str) -> Option<SecretString> {
    vault?.get(key).ok().flatten()
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
    // Only *arr apps reach here, and every *arr app has a vault key.
    let Some(key_name) = secret_keys::api_key_for(kind) else {
        return Vec::new();
    };

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

// ------------------------------------------------------------------ library

fn check_library(
    library: &sharerr_core::config::LibraryConfig,
    report: &mut Report,
) -> Vec<Discovered> {
    match checks::check_library(library) {
        DirOutcome::Missing => {
            report.fail(format!(
                "{} does not exist as sharerr sees it — check the mount",
                library.path.display()
            ));
            Vec::new()
        }
        DirOutcome::NotADirectory => {
            report.fail(format!(
                "{} is not a directory — [[library]] shares a folder, not a file",
                library.path.display()
            ));
            Vec::new()
        }
        DirOutcome::Unreadable(reason) => {
            report.fail(format!("could not scan: {reason}"));
            Vec::new()
        }
        DirOutcome::Empty => {
            report.warn(format!(
                "no {} files found — anything placed here is shared automatically",
                library.kind.as_str()
            ));
            Vec::new()
        }
        DirOutcome::Ready { skipped, items } => {
            report.ok(format!(
                "{} {} file(s) found",
                items.len(),
                library.kind.as_str()
            ));
            if skipped > 0 {
                report.warn(format!(
                    "{skipped} file(s) skipped — their names carry nothing a release could \
                     advertise. Rename them the way a release is named"
                ));
            }
            report.info("note: no external ids — a friend's app matches these by name alone");
            items
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
    let settings = config.torrent_client();
    let (url, label) = (settings.url, settings.category);

    // An API key, when stored, is what will authenticate — so it is what `doctor`
    // must test. Read quietly: its absence is the ordinary case and the password
    // check below is the one that reports.
    let api_key = settings
        .api_key_key
        .and_then(|key| quiet_secret(vault, key));

    let credential = match api_key {
        Some(api_key) => checks::TorrentCredential::ApiKey(api_key),
        None => match settings.password_key {
            Some(password_key) => match secret(vault, password_key, report) {
                Some(password) => checks::TorrentCredential::Password(password),
                None => return,
            },
            None => {
                if let Some(key) = settings.api_key_key {
                    report.fail(format!("{key} is missing — {}", fix_hint(key)));
                }
                return;
            }
        },
    };
    let noun = credential.noun();

    // Shared with the web UI's "Test connection" button — see `crate::checks`. The
    // client comes back on success so the checks below do not re-authenticate.
    let client = match checks::check_qbit(
        config.torrent_backend,
        url,
        settings.username,
        Ok(Some(credential)),
    )
    .await
    {
        QbitOutcome::Ready {
            version,
            kind,
            client,
        } => {
            report.ok(format!("{url} responded ({kind} {version})"));
            client
        }
        QbitOutcome::AuthRejected => {
            let checked = if noun == "API key" {
                settings.api_key_key
            } else {
                settings.password_key
            };
            report.fail(format!(
                "{url} rejected the {noun} — check {}",
                checked.unwrap_or("the stored credential")
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

    match client.list(Some(label)).await {
        Ok(torrents) => report.info(format!(
            "{} torrent(s) already labelled {label:?}",
            torrents.len()
        )),
        Err(err) => report.warn(format!("could not list torrents: {err}")),
    }
}

// ------------------------------------------------------------------ tracker

fn check_tracker(config: &Config, report: &mut Report) {
    // The one way the builtin tracker can look configured and still not work:
    // `doctor` and `sync` are one-shot commands, and the announce endpoint
    // only exists while `serve` is running.
    report.info(
        "announces are answered by `sharerr serve`; a one-shot sync builds \
         correct torrents whose announces fail until it is running",
    );

    match sharerr_core::endpoint::advertised_base(&config.tracker, config.server.bind.port()) {
        Ok(Some(base)) => report.ok(format!(
            "advertised endpoint: {}",
            sharerr_core::endpoint::base_string(&base)
        )),
        Ok(None) if config.gluetun.control_url.is_some() => report.info(
            "no static advertised address — the endpoint is resolved from gluetun, \
             see the gluetun section above",
        ),
        Ok(None) => report.fail(
            "neither tracker.advertised_host nor tracker.advertised_url is set. sharerr \
             cannot guess the address friends reach it on, and a wrong guess produces \
             torrents nobody can announce to",
        ),
        Err(err) => report.fail(err.to_string()),
    }
}

/// What gluetun's control server says the world sees, next to what the config
/// advertises — the mismatch this catches is a hand-typed address the tunnel no
/// longer holds.
async fn check_gluetun(config: &Config, report: &mut Report) {
    // Guarded by the caller; the arm exists because the function is total.
    let Some(control) = &config.gluetun.control_url else {
        return;
    };

    let client = match crate::gluetun::GluetunClient::new(control) {
        Ok(client) => client,
        Err(err) => {
            report.fail(format!("{err}"));
            return;
        }
    };

    let ip = match client.public_ip().await {
        Ok(ip) => {
            report.ok(format!("public IP per {control}: {ip}"));
            Some(ip)
        }
        Err(err) => {
            report.fail(format!("{err}"));
            None
        }
    };

    match client.forwarded_port().await {
        Ok(port) => report.ok(format!(
            "forwarded port: {port} — the endpoint is resolved dynamically"
        )),
        Err(err) => report.warn(format!("{err}")),
    }

    // The mismatch worth naming: a static advertised host that is not the
    // tunnel's exit. A DNS name cannot be compared without resolving it, so only
    // literal addresses are checked.
    if let Some(ip) = ip
        && let Some(host) = config.tracker.advertised_host.as_deref()
        && let Ok(advertised) = host.trim_matches(['[', ']']).parse::<std::net::IpAddr>()
        && advertised != ip
    {
        report.warn(format!(
            "tracker.advertised_host is {advertised}, but the tunnel's exit is {ip} — \
             torrents built with the static address cannot be announced to"
        ));
    }
}

/// Try to actually connect to the advertised endpoint.
///
/// From inside the namespace a closed forwarded port and a quiet swarm look
/// identical, so an answer either way is information: a completed TCP connect
/// proves the address routes back in, and a refused or timed-out one is worth a
/// warning rather than a failure — some networks cannot hairpin their own
/// public address even when it works from outside.
async fn check_reachability(config: &Config, report: &mut Report) {
    let base =
        match sharerr_core::endpoint::advertised_base(&config.tracker, config.server.bind.port()) {
            Ok(Some(base)) => base,
            // Already reported by check_tracker.
            _ => return,
        };

    let Some(host) = base.host_str() else {
        return;
    };
    let Some(port) = base.port_or_known_default() else {
        return;
    };
    let target = format!("{}:{port}", host.trim_matches(['[', ']']));

    match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::net::TcpStream::connect(&target),
    )
    .await
    {
        Ok(Ok(_)) => report.ok(format!("{target} accepts TCP connections from here")),
        Ok(Err(err)) => report.warn(format!(
            "could not connect to {target}: {err}. If this instance cannot reach its \
             own public address (common behind NAT), verify the port from outside"
        )),
        Err(_) => report.warn(format!(
            "connecting to {target} timed out. If this instance cannot reach its own \
             public address (common behind NAT), verify the port from outside"
        )),
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
    for kind in MediaSource::ARRS.iter().copied() {
        println!(
            "  {:<11}{}",
            format!("{kind}:"),
            config
                .service(kind)
                .map_or("(not configured)", |s| s.url.as_str())
        );
    }
    for library in &config.library {
        println!(
            "  library:   {} ({})",
            library.path.display(),
            library.kind.as_str()
        );
    }
    // The configured client, not always qBittorrent's — printing the unused
    // section's URL is how an operator ends up debugging the wrong service.
    let client = config.torrent_client();
    match client.username {
        Some(username) => println!(
            "  client:    {} {} (user {username})",
            config.torrent_backend.as_str(),
            client.url,
        ),
        None => println!(
            "  client:    {} {}",
            config.torrent_backend.as_str(),
            client.url,
        ),
    }
    println!(
        "  tracker:   builtin, advertised as {}",
        match sharerr_core::endpoint::advertised_base(&config.tracker, config.server.bind.port()) {
            Ok(Some(base)) => sharerr_core::endpoint::base_string(&base),
            Ok(None) => "(unset)".to_owned(),
            Err(err) => format!("(invalid: {err})"),
        }
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
