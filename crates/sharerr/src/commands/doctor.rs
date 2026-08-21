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
use sharerr_core::config::{ServiceConfig, TorrentBackend, secret_keys};
use sharerr_core::{Config, MediaSource};
use sharerr_store::{Store, Vault};
use url::Url;

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

/// Prints up to [`MAX_LISTED`] items via `show`, then a "... and N more" line
/// for the rest — the truncation shape both `check_paths` overflow reports
/// share.
fn report_capped<T>(
    report: &mut Report,
    items: &[T],
    mut show: impl FnMut(&mut Report, &T),
    more_suffix: &str,
) {
    for item in items.iter().take(MAX_LISTED) {
        show(report, item);
    }
    if items.len() > MAX_LISTED {
        report.info(format!(
            "  ... and {} more{more_suffix}",
            items.len() - MAX_LISTED
        ));
    }
}

pub async fn run(
    config: &Config,
    config_error: Option<&str>,
    args: &crate::cli::DoctorArgs,
) -> Result<()> {
    let fix = args.fix;
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
        discovered.extend(check_arr(kind, service, config, vault.as_ref(), fix, &mut report).await);
    }
    for library in &config.library {
        report.section(&format!("library {}", library.path.display()));
        discovered.extend(check_library(library, &mut report));
    }
    if sources.is_empty() && config.library.is_empty() {
        report.section("library sources");
        report
            .fail("no *arr app or [[library]] directory is configured — there is nothing to share");
    }

    report.section(config.torrent_backend.as_str());
    check_qbit(config, vault.as_ref(), fix, &mut report).await;

    if config.gluetun.control_url.is_some() {
        report.section("gluetun");
        check_gluetun(config, vault.as_ref(), &mut report).await;
    }

    report.section("tracker");
    check_tracker(config, &mut report);
    check_reachability(config, &mut report).await;

    report.section("paths");
    check_paths(config, &discovered, &mut report);

    if args.suggest_paths {
        report.section("path suggestions");
        suggest_paths(
            config,
            &discovered,
            args.search_root.as_deref(),
            &mut report,
        );
    }

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
    let vault = match crate::secrets::open_vault(config) {
        Ok(vault) => vault,
        Err(err) => {
            // `{:#}` renders anyhow's context chain, which already names the
            // vault path for an open failure and correctly does not for a
            // missing master key.
            report.fail(format!("{err:#}"));
            return None;
        }
    };

    report.ok(format!("opened {}", config.vault_path().display()));

    // Report only which keys are present. Values are never printed, by design.
    // The password key follows the *configured* client — hardcoding qBittorrent's
    // would fail a perfectly good Transmission setup that never reads it.
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
            Ok(None) => fail_missing(report, key),
            Err(err) => fail_unreadable(report, key, err),
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
                fail_unreadable(report, key, err);
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
            fail_missing(report, key);
        }
        return;
    };

    match vault.get(password_key) {
        Ok(Some(_)) => report.ok(format!("{password_key} is set")),
        Ok(None) => fail_missing(report, password_key),
        Err(err) => fail_unreadable(report, password_key, err),
    }
}

/// How to supply a missing secret.
///
/// Names the web UI first and the CLI second, which is the order of least effort
/// since the UI needs no shell inside the container. Both write the same vault.
fn fix_hint(key: &str) -> String {
    format!("set it in Settings on the web UI, or run: sharerr vault set {key}")
}

/// Report that `key` is absent from the vault, with the fix.
fn fail_missing(report: &mut Report, key: &str) {
    report.fail(format!("{key} is missing — {}", fix_hint(key)));
}

/// Report that `key` could not be read from the vault, with the cause chain —
/// vault errors wrap a source (a decryption or I/O failure), and that source is
/// what actually tells the operator what to go and fix.
fn fail_unreadable(report: &mut Report, key: &str, err: impl std::error::Error) {
    report.fail(format!("{key} could not be read: {}", chain(&err)));
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
            fail_missing(report, key);
            None
        }
        Err(err) => {
            fail_unreadable(report, key, err);
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
    fix: bool,
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

    // Cloned before the first check consumes it: `--fix` needs a live credential
    // to create the tag with, and re-deriving it from the vault a second time
    // would mean opening it twice for one command.
    let api_key_for_fix = api_key.clone();

    let outcome = checks::check_arr(kind, Some(&service.url), Ok(Some(api_key)), &config.tag).await;

    // `TagMissing` is the one mechanical case `--fix` can close here: creating
    // the tag turns it into `TagUnused` (or `Ready`, if content already carries
    // it by the time this runs) without a restart or a second invocation.
    if fix && let ArrOutcome::TagMissing { .. } = &outcome {
        return fix_missing_tag(kind, service, config, api_key_for_fix, report).await;
    }

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
            if kind.has_coarse_tagging() {
                // Not a defect, but it surprises people: these apps have no
                // per-item tags, so tagging one thing shares its whole run.
                report.info(format!(
                    "note: {kind} tags are not per-item, so tagging one thing shares everything under it"
                ));
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
        // Distinct from TagUnused above: a tag that does not exist needs
        // creating, not applying.
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

/// Create the configured tag in `kind`, for `check_arr`'s `--fix` path.
///
/// Never returns any discovered items: a tag that had to be created cannot yet
/// carry any content, so there is nothing new to walk this run — the operator
/// still has to go and apply it, same as [`ArrOutcome::TagUnused`] already says.
async fn fix_missing_tag(
    kind: MediaSource,
    service: &ServiceConfig,
    config: &Config,
    api_key: SecretString,
    report: &mut Report,
) -> Vec<Discovered> {
    let client = match sharerr_arr::ArrClient::new(kind, &service.url, api_key) {
        Ok(client) => client,
        Err(err) => {
            report.fail(format!("could not create tag {:?}: {err}", config.tag));
            return Vec::new();
        }
    };

    match client.create_tag(&config.tag).await {
        Ok(()) => {
            report.ok(format!("created tag {:?} in {}", config.tag, kind.as_str()));
            report.info("apply it to content there to start sharing it");
        }
        Err(err) => report.fail(format!("could not create tag {:?}: {err}", config.tag)),
    }
    Vec::new()
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
async fn check_qbit(config: &Config, vault: Option<&Vault>, fix: bool, report: &mut Report) {
    let settings = config.torrent_client();
    let (url, label) = (settings.url, settings.category);

    // An API key, when stored, is what will authenticate — so it is what `doctor`
    // must test. Read quietly: its absence is the ordinary case and the password
    // check below is the one that reports.
    let api_key = settings
        .api_key_key
        .and_then(|key| quiet_secret(vault, key));
    // Cloned before `credential` consumes it: the category check below needs its
    // own qBittorrent-specific client, since "category" is not a concept the
    // generic `TorrentClient` trait carries — Transmission has none.
    let api_key_for_category = api_key.clone();

    let credential = match api_key {
        Some(api_key) => checks::TorrentCredential::ApiKey(api_key),
        None => match settings.password_key {
            Some(password_key) => match secret(vault, password_key, report) {
                Some(password) => checks::TorrentCredential::Password(password),
                None => return,
            },
            None => {
                if let Some(key) = settings.api_key_key {
                    fail_missing(report, key);
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

    // Categories are qBittorrent's own concept — Transmission has only labels,
    // which need no pre-creation — so this only runs for that backend, and only
    // once an API key is actually available to build a second client with.
    if config.torrent_backend == TorrentBackend::Qbittorrent
        && let Some(api_key) = api_key_for_category
    {
        check_qbit_category(url, api_key, label, fix, report).await;
    }
}

/// Whether `label` is a category qBittorrent already knows, and — with `--fix`
/// — create it if not.
///
/// Unlike a missing tag this is not a hard failure: a torrent adds fine under a
/// category qBittorrent has never seen, it simply will not appear in the
/// WebUI's own category list until one exists. Still worth naming, since "the
/// category picker is empty" is a real point of confusion for an operator who
/// has not looked at qBittorrent's own settings.
async fn check_qbit_category(
    url: &Url,
    api_key: SecretString,
    label: &str,
    fix: bool,
    report: &mut Report,
) {
    if label.is_empty() {
        return;
    }

    let client = match sharerr_qbit::QbitClient::with_api_key(url, api_key) {
        Ok(client) => client,
        Err(err) => {
            report.warn(format!("could not check qBittorrent's categories: {err}"));
            return;
        }
    };

    let categories = match client.categories().await {
        Ok(categories) => categories,
        Err(err) => {
            report.warn(format!("could not list qBittorrent's categories: {err}"));
            return;
        }
    };

    if categories.contains_key(label) {
        report.ok(format!("category {label:?} exists"));
        return;
    }

    if !fix {
        report.warn(format!(
            "category {label:?} does not exist in qBittorrent yet — torrents still add \
             fine, but it will not appear in the WebUI's category list until created. \
             Re-run with --fix, or create it under Options -> Categories"
        ));
        return;
    }

    match client.create_category(label).await {
        Ok(()) => report.ok(format!("created category {label:?}")),
        Err(err) => report.fail(format!("could not create category {label:?}: {err}")),
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
async fn check_gluetun(config: &Config, vault: Option<&Vault>, report: &mut Report) {
    // Guarded by the caller; the arm exists because the function is total.
    let Some(control) = &config.gluetun.control_url else {
        return;
    };

    let api_key = quiet_secret(vault, sharerr_core::config::secret_keys::GLUETUN_API_KEY);
    let client = match crate::gluetun::GluetunClient::new(control, api_key) {
        Ok(client) => client,
        Err(err) => {
            report.fail(format!("{err}"));
            return;
        }
    };

    let (ip_result, port_result) = tokio::join!(client.public_ip(), client.forwarded_port());

    let ip = match ip_result {
        Ok(ip) => {
            report.ok(format!("public IP per {control}: {ip}"));
            Some(ip)
        }
        Err(err) => {
            report.fail(format!("{err}"));
            None
        }
    };

    match port_result {
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

    report_capped(
        report,
        &paths.invalid,
        |r, reason| r.fail(reason),
        " unresolvable path(s)",
    );

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
        report_capped(
            report,
            &paths.missing,
            |r, path| r.info(format!("  {}", path.display())),
            "",
        );
        report.info("fix the [[path_map]] rules so the arr view maps onto sharerr's mount");
    }
}

/// `--suggest-paths`: propose `[[path_map]]` rules by matching tagged files
/// against what actually exists under `search_root` (default `/media`), by
/// name and size. See `crate::pathsuggest` for the algorithm and why it never
/// searches anywhere the operator has not named.
fn suggest_paths(
    config: &Config,
    discovered: &[Discovered],
    search_root: Option<&std::path::Path>,
    report: &mut Report,
) {
    let default_root = std::path::Path::new("/media");
    let root = search_root.unwrap_or(default_root);

    if !root.is_dir() {
        report.fail(format!(
            "{} is not a directory sharerr can see — pass --search-root, or mount \
             the library there",
            root.display()
        ));
        return;
    }
    if discovered.is_empty() {
        report.info("nothing tagged, so there is nothing to match against");
        return;
    }

    let existing: std::collections::HashSet<(&std::path::Path, &std::path::Path)> = config
        .path_map
        .iter()
        .map(|m| (m.arr.as_path(), m.sharerr.as_path()))
        .collect();

    let suggestions: Vec<_> = crate::pathsuggest::suggest(discovered, root)
        .into_iter()
        .filter(|s| !existing.contains(&(s.arr.as_path(), s.sharerr.as_path())))
        .collect();

    if suggestions.is_empty() {
        report.info(format!(
            "no new mapping found under {} — every match either already has a rule, \
             or nothing tagged matched a file there by name and size",
            root.display()
        ));
        return;
    }

    for s in &suggestions {
        report.ok(format!(
            "{} -> {} ({} file(s) agree)",
            s.arr.display(),
            s.sharerr.display(),
            s.agreement
        ));
        report.info(format!(
            "  add as: arr = \"{}\", sharerr = \"{}\"",
            s.arr.display(),
            s.sharerr.display()
        ));
    }
    report.info(
        "proposals only — nothing was written; add the ones that look right under \
         Settings or in [[path_map]]",
    );
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
