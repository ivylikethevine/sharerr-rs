//! The settings page: every credential and setting sharerr needs, from a browser.
//!
//! # Two destinations, one page
//!
//! A save can land in either of two places, and the page deliberately does not
//! make the operator care which. Non-secret settings are rewritten into
//! `sharerr.toml` by [`super::config_io`]; API keys and passwords go into the
//! encrypted vault. So one visible form — "Sonarr" — writes a URL to the file and
//! an API key to the vault.
//!
//! # Secrets are write-only
//!
//! A stored secret is never rendered back, not even masked. The page reports only
//! whether a key is *set*, which is exactly what `sharerr vault list` has always
//! done. A blank secret input means "leave it alone", so saving a URL does not
//! silently wipe the key next to it; clearing one is an explicit checkbox.
//!
//! # Fields the environment has taken
//!
//! figment layers `SHARERR_*` over the file, so a value saved here is silently
//! discarded on reload if the matching variable is set. Those inputs are rendered
//! disabled and name the variable instead of accepting a write that goes nowhere.

use std::collections::BTreeSet;

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::Form;
use secrecy::SecretString;
use serde::Deserialize;
use sharerr_core::config::{config_paths, secret_keys};
use sharerr_core::{Config, MediaSource};
use sharerr_store::{Vault, master_key_from_env};

use crate::gluetun::GluetunTarget;

use super::WebState;
use super::config_io::{ConfigFile, Edit, parse_libraries, parse_path_map};
use super::templates::{ArrSection, LibraryRow, PathRow, SettingsPage, render};

/// Mint a fresh secret and show it once.
///
/// Only the tracker token is minted this way now — a friend's own key, generated
/// on the Friends page, is what opens the Torznab feed to them.
pub async fn generate_secret(
    State(state): State<WebState>,
    axum::extract::Path(field): axum::extract::Path<String>,
) -> Response {
    let key = match field.as_str() {
        "tracker" => secret_keys::TRACKER_TOKEN,
        _ => return reject(&state, "There is no such secret to generate.").await,
    };

    let generated = match crate::secrets::random_hex(crate::secrets::KEY_BYTES) {
        Ok(generated) => generated,
        Err(reason) => return reject(&state, &reason).await,
    };

    if let Err(message) = apply_secret(&state, key, &generated, None).await {
        return reject(&state, &message).await;
    }

    let mut page = build_page(&state, Some(field), None).await;
    page.revealed = Some(generated);
    render(&page)
}

#[derive(Debug, Default, Deserialize)]
pub struct PageQuery {
    /// Which section was just saved, for the confirmation banner. Set by the
    /// post/redirect/get after a successful write so a refresh does not re-submit.
    saved: Option<String>,
}

pub async fn page(State(state): State<WebState>, Query(query): Query<PageQuery>) -> Response {
    render(&build_page(&state, query.saved, None).await)
}

// ---------------------------------------------------------------------------
// Forms
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct GeneralForm {
    tag: String,
}

#[derive(Debug, Deserialize)]
pub struct ArrForm {
    url: String,
    api_key: String,
    #[serde(default)]
    clear_api_key: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct QbitForm {
    url: String,
    api_key: String,
    #[serde(default)]
    clear_api_key: Option<String>,
    category: String,
    tag: String,
    #[serde(default)]
    skip_checking: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TrackerForm {
    advertised_host: String,
    port: String,
    advertised_url: String,
    token: String,
    #[serde(default)]
    clear_token: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GluetunForm {
    #[serde(default)]
    enabled: Option<String>,
    control_url: String,
    api_key: String,
    #[serde(default)]
    clear_api_key: Option<String>,
    poll_secs: String,
}

#[derive(Debug, Deserialize)]
pub struct SyncForm {
    #[serde(default)]
    enabled: Option<String>,
    interval_secs: String,
}

#[derive(Debug, Deserialize)]
pub struct NotificationsForm {
    webhook_url: String,
    #[serde(default)]
    clear_webhook_url: Option<String>,
    kind: String,
    peer_quiet_secs: String,
}

/// Repeated inputs, one entry per row. `axum_extra`'s `Form` is what makes this
/// work — axum's own uses `serde_urlencoded`, which cannot decode repeated keys
/// into a `Vec`.
#[derive(Debug, Deserialize)]
pub struct PathsForm {
    #[serde(default)]
    arr: Vec<String>,
    #[serde(default)]
    sharerr: Vec<String>,
    #[serde(default)]
    qbit: Vec<String>,
}

/// The `[[library]]` rows, same repeated-input shape as [`PathsForm`].
#[derive(Debug, Deserialize)]
pub struct LibrariesForm {
    #[serde(default)]
    path: Vec<String>,
    #[serde(default)]
    kind: Vec<String>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub async fn save_general(
    State(state): State<WebState>,
    Form(form): Form<GeneralForm>,
) -> Response {
    let tag = form.tag.trim();
    if tag.is_empty() {
        return reject(
            &state,
            "The tag cannot be blank — it is what sharerr looks for.",
        )
        .await;
    }

    write_config(&state, "general", |file| {
        file.apply([Edit::str(config_paths::TAG, tag)]);
        Ok(())
    })
    .await
}

/// One handler for all five *arr apps: the path segment parses straight to a
/// [`MediaSource`], and the config path and vault key both come from the same
/// per-source accessors every other consumer uses — adding a sixth app touches
/// neither this function nor the template's `{% for %}` block.
pub async fn save_arr(
    State(state): State<WebState>,
    axum::extract::Path(source): axum::extract::Path<MediaSource>,
    Form(form): Form<ArrForm>,
) -> Response {
    // The directory source parses as a `MediaSource` but has neither a URL nor
    // an API key; its settings live in the Libraries section.
    let (Some(url_path), Some(secret_key)) = (
        config_paths::url_for(source),
        secret_keys::api_key_for(source),
    ) else {
        return reject(&state, "There is no such service to configure.").await;
    };
    let section = source.as_str();

    if let Err(message) = apply_secret(&state, secret_key, &form.api_key, form.clear_api_key).await
    {
        return reject(&state, &message).await;
    }

    write_config(&state, section, |file| {
        let url = form.url.trim();
        if url.is_empty() {
            // Removing the whole table, not just the URL: each *arr section is
            // `Option<ServiceConfig>`, and a table with no `url` fails to parse
            // where an absent table correctly means "not configured".
            file.apply([Edit::unset(section)]);
        } else {
            file.apply([Edit::str(url_path, normalise_url(url)?)]);
        }
        Ok(())
    })
    .await
}

pub async fn save_qbittorrent(
    State(state): State<WebState>,
    Form(form): Form<QbitForm>,
) -> Response {
    // Checked here rather than at the first sync, because a key pasted with a
    // missing character otherwise stores fine and surfaces hours later as a
    // rejected credential with no hint that the *shape* is wrong.
    let api_key = form.api_key.trim();
    if !api_key.is_empty() && !sharerr_qbit::looks_like_api_key(api_key) {
        return reject(
            &state,
            "That does not look like a qBittorrent API key. Keys are 32 characters: \
             `qbt_` followed by 28 letters and digits, from Options -> Web UI -> API key.",
        )
        .await;
    }

    if let Err(message) = apply_secret(
        &state,
        secret_keys::QBITTORRENT_API_KEY,
        &form.api_key,
        form.clear_api_key,
    )
    .await
    {
        return reject(&state, &message).await;
    }

    write_config(&state, "qbittorrent", |file| {
        let url = form.url.trim();
        if url.is_empty() {
            anyhow::bail!("qBittorrent's URL is required — sharerr cannot seed without it");
        }
        file.apply([
            Edit::str(config_paths::QBITTORRENT_URL, normalise_url(url)?),
            Edit::str(config_paths::QBITTORRENT_CATEGORY, form.category.trim()),
            Edit::str(config_paths::QBITTORRENT_TAG, form.tag.trim()),
            Edit::bool(
                config_paths::QBITTORRENT_SKIP_CHECKING,
                checked(&form.skip_checking),
            ),
        ]);
        Ok(())
    })
    .await
}

pub async fn save_tracker(
    State(state): State<WebState>,
    Form(form): Form<TrackerForm>,
) -> Response {
    if let Err(message) = apply_secret(
        &state,
        secret_keys::TRACKER_TOKEN,
        &form.token,
        form.clear_token,
    )
    .await
    {
        return reject(&state, &message).await;
    }

    write_config(&state, "tracker", |file| {
        let host = form.advertised_host.trim();
        if !host.is_empty() {
            // Wrong at save time is loud; wrong after the fact is a torrent nobody
            // can announce to. Doctor already catches this against a *running*
            // instance, but a hand-typed loopback or private address is knowable
            // right here, for free.
            validate_advertised_host(host)?;
        }

        let mut edits = vec![Edit::str_or_unset(
            config_paths::TRACKER_ADVERTISED_HOST,
            &form.advertised_host,
        )];

        let port = form.port.trim();
        if port.is_empty() {
            edits.push(Edit::unset(config_paths::TRACKER_PORT));
        } else {
            let parsed: u16 = port.parse().map_err(|_| {
                anyhow::anyhow!("{port:?} is not a port number between 1 and 65535")
            })?;
            edits.push(Edit::int(config_paths::TRACKER_PORT, i64::from(parsed)));
        }

        let advertised_url = form.advertised_url.trim();
        if advertised_url.is_empty() {
            edits.push(Edit::unset(config_paths::TRACKER_ADVERTISED_URL));
        } else {
            // Validated here rather than at the next sync: a URL that does not
            // parse would otherwise store fine and surface later as torrents
            // nobody can announce to.
            edits.push(Edit::str(
                config_paths::TRACKER_ADVERTISED_URL,
                normalise_url(advertised_url)?,
            ));
        }

        file.apply(edits);
        Ok(())
    })
    .await
}

pub async fn save_gluetun(
    State(state): State<WebState>,
    Form(form): Form<GluetunForm>,
) -> Response {
    save_gluetun_section(
        state,
        form,
        "gluetun",
        config_paths::GLUETUN_ENABLED,
        config_paths::GLUETUN_CONTROL_URL,
        config_paths::GLUETUN_POLL_SECS,
        secret_keys::GLUETUN_API_KEY,
    )
    .await
}

/// The second poller — the torrent client's own tunnel, when it is a separate
/// one from the tracker's. Same form shape, same rules, a different section:
/// see `docs/roadmap.md`'s "a peer with two addresses".
pub async fn save_gluetun_client(
    State(state): State<WebState>,
    Form(form): Form<GluetunForm>,
) -> Response {
    save_gluetun_section(
        state,
        form,
        "gluetun_client",
        config_paths::GLUETUN_CLIENT_ENABLED,
        config_paths::GLUETUN_CLIENT_CONTROL_URL,
        config_paths::GLUETUN_CLIENT_POLL_SECS,
        secret_keys::GLUETUN_CLIENT_API_KEY,
    )
    .await
}

/// The save logic both gluetun sections share — only the paths and vault key
/// differ between the tracker's poller and the client's.
async fn save_gluetun_section(
    state: WebState,
    form: GluetunForm,
    section: &'static str,
    enabled_path: &'static str,
    control_url_path: &'static str,
    poll_secs_path: &'static str,
    api_key_secret: &'static str,
) -> Response {
    if let Err(message) =
        apply_secret(&state, api_key_secret, &form.api_key, form.clear_api_key).await
    {
        return reject(&state, &message).await;
    }

    write_config(&state, section, move |file| {
        file.apply([Edit::bool(enabled_path, checked(&form.enabled))]);

        let url = form.control_url.trim();
        if url.is_empty() {
            file.apply([Edit::unset(control_url_path)]);
        } else {
            file.apply([Edit::str(control_url_path, normalise_url(url)?)]);
        }

        let poll = form.poll_secs.trim();
        if !poll.is_empty() {
            let secs: u64 = poll.parse().map_err(|_| {
                anyhow::anyhow!("the poll interval must be a whole number of seconds")
            })?;
            if secs < sharerr_core::config::GluetunConfig::MIN_POLL_SECS {
                anyhow::bail!(
                    "the poll interval must be at least {} seconds",
                    sharerr_core::config::GluetunConfig::MIN_POLL_SECS
                );
            }
            file.apply([Edit::int(
                poll_secs_path,
                i64::try_from(secs).unwrap_or(60),
            )]);
        }
        Ok(())
    })
    .await
}

pub async fn save_sync(State(state): State<WebState>, Form(form): Form<SyncForm>) -> Response {
    write_config(&state, "sync", |file| {
        let interval: u64 =
            form.interval_secs.trim().parse().map_err(|_| {
                anyhow::anyhow!("the sync interval must be a whole number of seconds")
            })?;

        // Mirrors the `.max(60)` the background loop already applies. Saying so
        // here beats silently storing 5 and running at 60.
        if interval < 60 {
            anyhow::bail!("the sync interval must be at least 60 seconds");
        }

        file.apply([
            Edit::bool(config_paths::SYNC_ENABLED, checked(&form.enabled)),
            Edit::int(
                config_paths::SYNC_INTERVAL_SECS,
                i64::try_from(interval).unwrap_or(900),
            ),
        ]);
        Ok(())
    })
    .await
}

/// A webhook fired on sync failure or a peer going quiet.
///
/// The URL is a vault secret — see
/// [`sharerr_core::config::secret_keys::NOTIFICATIONS_WEBHOOK_URL`] for why —
/// so this is two writes, same shape as [`save_gluetun`]: a secret and a
/// config section.
pub async fn save_notifications(
    State(state): State<WebState>,
    Form(form): Form<NotificationsForm>,
) -> Response {
    let webhook = form.webhook_url.trim();
    if !webhook.is_empty() && url::Url::parse(webhook).is_err() {
        return reject(&state, "That does not look like a valid webhook URL.").await;
    }

    if let Err(message) = apply_secret(
        &state,
        secret_keys::NOTIFICATIONS_WEBHOOK_URL,
        webhook,
        form.clear_webhook_url,
    )
    .await
    {
        return reject(&state, &message).await;
    }

    write_config(&state, "notifications", |file| {
        let Some(kind) = sharerr_core::config::NotifyKind::parse(form.kind.trim()) else {
            anyhow::bail!("{:?} is not a known notification kind", form.kind);
        };
        file.apply([Edit::str(config_paths::NOTIFICATIONS_KIND, kind.as_str())]);

        let secs: u64 = form.peer_quiet_secs.trim().parse().map_err(|_| {
            anyhow::anyhow!("the peer-quiet threshold must be a whole number of seconds")
        })?;
        file.apply([Edit::int(
            config_paths::NOTIFICATIONS_PEER_QUIET_SECS,
            i64::try_from(secs).unwrap_or(604_800),
        )]);
        Ok(())
    })
    .await
}

pub async fn save_libraries(
    State(state): State<WebState>,
    Form(form): Form<LibrariesForm>,
) -> Response {
    // Rows arrive as two parallel lists; index them from `path` the way
    // `save_paths` does, so a malformed submission cannot pair the wrong kind
    // with a directory.
    let rows: Vec<(String, String)> = form
        .path
        .iter()
        .enumerate()
        .map(|(i, path)| (path.clone(), form.kind.get(i).cloned().unwrap_or_default()))
        .collect();

    let libraries = match parse_libraries(&rows) {
        Ok(libraries) => libraries,
        Err(err) => return reject(&state, &format!("{err:#}")).await,
    };

    write_config(&state, "libraries", |file| {
        file.set_libraries(&libraries);
        Ok(())
    })
    .await
}

pub async fn save_paths(State(state): State<WebState>, Form(form): Form<PathsForm>) -> Response {
    // Rows arrive as three parallel lists; a short one means a malformed submission
    // rather than an empty field, and zipping blindly would silently pair the wrong
    // paths together — which is the single most damaging thing this page can get
    // wrong, because it makes qBittorrent seed the wrong file.
    let rows: Vec<(String, String, String)> = form
        .arr
        .iter()
        .enumerate()
        .map(|(i, arr)| {
            (
                arr.clone(),
                form.sharerr.get(i).cloned().unwrap_or_default(),
                form.qbit.get(i).cloned().unwrap_or_default(),
            )
        })
        .collect();

    let mappings = match parse_path_map(&rows) {
        Ok(mappings) => mappings,
        Err(err) => return reject(&state, &format!("{err:#}")).await,
    };

    write_config(&state, "paths", |file| {
        file.set_path_map(&mappings);
        Ok(())
    })
    .await
}

// ---------------------------------------------------------------------------
// Shared machinery
// ---------------------------------------------------------------------------

/// Open the config file, let `edit` mutate it, then validate, write, and reload.
///
/// Every settings handler goes through here so that no path can skip the
/// validate-before-write step or forget to invalidate the syncer.
async fn write_config<F>(state: &WebState, section: &str, edit: F) -> Response
where
    F: FnOnce(&mut ConfigFile) -> anyhow::Result<()>,
{
    let path = state.serve.config_path().to_path_buf();

    let mut file = if state.serve.config_error().await.is_some() {
        replacement_for(state, &path).await
    } else {
        match ConfigFile::open(&path) {
            Ok(file) => file,
            Err(err) => return reject(state, &format!("{err:#}")).await,
        }
    };

    if let Err(err) = edit(&mut file) {
        return reject(state, &format!("{err:#}")).await;
    }

    match file.save() {
        Ok(config) => {
            // Swap the new config in *and* drop the cached syncer, so the change is
            // live within one recovery interval instead of at the next restart.
            state.serve.replace_config(config).await;
            tracing::info!(section, path = %path.display(), "settings saved");
            Redirect::to(&format!("/settings?saved={section}")).into_response()
        }
        Err(err) => reject(state, &format!("{err:#}")).await,
    }
}

/// A blank document to write over a `sharerr.toml` that did not load.
///
/// Editing that file in place cannot repair it: whatever `Config` rejected is
/// still in there, so `settings::validate` keeps refusing every save and the
/// operator is stuck behind a page that promises to help. Replacing it works
/// because a file that did not load is not in effect — the process is already
/// running on the values written here.
///
/// The two carried forward are the two [`crate::settings::load_or_recover`]
/// salvages, and they are carried for the same reason it salvages them: `data_dir`
/// is where the vault and database live, and dropping it now would move the
/// operator's instance out from under them at the next restart.
async fn replacement_for(state: &WebState, path: &std::path::Path) -> ConfigFile {
    let config = state.serve.config().await;
    let mut file = ConfigFile::replacing(path);

    file.apply([
        Edit::str(
            config_paths::DATA_DIR,
            config.data_dir.to_string_lossy().as_ref(),
        ),
        Edit::str(config_paths::SERVER_BIND, config.server.bind.to_string()),
    ]);
    file
}

/// Store, clear, or leave a vault secret alone.
///
/// A blank input is *not* a deletion. The page shows only whether a key is set, so
/// a blank field is the normal state of a form the operator opened to change
/// something else — treating it as "remove the key" would wipe credentials as a
/// side effect of editing a URL.
async fn apply_secret(
    state: &WebState,
    key: &'static str,
    value: &str,
    clear: Option<String>,
) -> Result<(), String> {
    let clearing = clear.is_some();
    let value = value.trim().to_owned();

    if !clearing && value.is_empty() {
        return Ok(());
    }

    let mut vault = state.serve.open_vault().await?;

    if clearing {
        vault
            .remove(key)
            .map_err(|err| format!("removing {key}: {err}"))?;
        tracing::info!(key, "secret cleared through the web ui");
    } else {
        vault
            .put(key, &SecretString::from(value))
            .map_err(|err| format!("storing {key}: {err}"))?;
        tracing::info!(key, "secret stored through the web ui");
    }

    // A changed credential is worthless until the syncer is rebuilt with it —
    // `ensure_ready` caches the first syncer that works and would otherwise keep
    // authenticating with the old value.
    state.serve.invalidate("a credential changed").await;
    Ok(())
}

/// Which vault keys currently hold a value.
///
/// Reads the names straight out of the file rather than opening the vault. They
/// are cleartext in the format by design, so this costs a ~200-byte read instead
/// of the ~16ms Argon2 derivation `open_vault` pays — which is what drawing a page
/// used to cost, on every render and again after every save.
///
/// It also means the page tells the truth when the master key is missing or wrong:
/// a secret that cannot currently be decrypted is still stored, and reporting it
/// as unset would invite the operator to overwrite it.
pub(super) async fn secrets_present(config: &Config) -> BTreeSet<String> {
    let path = config.vault_path();
    tokio::task::spawn_blocking(move || Vault::key_names(&path))
        .await
        .ok()
        .and_then(std::result::Result::ok)
        .map(|keys| keys.into_iter().collect())
        .unwrap_or_default()
}

/// Re-render the page with an error rather than redirecting.
///
/// Post/redirect/get on success, straight render on failure: a redirect would
/// either drop the message or smuggle it through the query string, and this way
/// the reason sits next to the form that caused it.
pub(super) async fn reject(state: &WebState, message: &str) -> Response {
    tracing::warn!(message, "rejected a settings change");
    let page = build_page(state, None, Some(message.to_owned())).await;
    (axum::http::StatusCode::BAD_REQUEST, render(&page)).into_response()
}

/// An HTML checkbox submits nothing at all when unticked, so absence is `false`.
fn checked(field: &Option<String>) -> bool {
    field.is_some()
}

/// What gluetun last actually reported for one endpoint, or `None` when
/// nothing has been observed yet — the settings page's short version of what
/// Diagnostics shows in full.
fn gluetun_last_observed(endpoint: &sharerr_core::endpoint::AdvertisedEndpoint) -> Option<String> {
    let observed = endpoint.last_observed()?;
    Some(format!(
        "{} ({})",
        observed.base,
        super::peers::ago(observed.observed_at)
    ))
}

/// The most recent failure `target`'s poller hit, if it has one right now —
/// cleared the moment a poll succeeds, so this never shows a stale error next
/// to a working endpoint.
async fn gluetun_last_error(state: &crate::state::ServeState, target: GluetunTarget) -> Option<String> {
    state.gluetun_status(target).snapshot().await.last_error
}

/// Reject a `tracker.advertised_host` that could never work for anyone outside
/// this machine or network.
///
/// A DNS name is let through unchecked — resolving one means a network call this
/// handler has no business making, and `doctor` already flags a mismatch against
/// a running instance. What is catchable for free is a literal address: a
/// loopback or private IP, or `localhost` itself, is never reachable by a friend
/// on another network, and typing one is almost always a copy-paste of the
/// `server.bind` value instead.
fn validate_advertised_host(host: &str) -> anyhow::Result<()> {
    if host.eq_ignore_ascii_case("localhost") {
        anyhow::bail!(
            "{host:?} only resolves on this machine — a friend elsewhere could never reach it"
        );
    }
    if let Ok(ip) = host.trim_matches(['[', ']']).parse::<std::net::IpAddr>()
        && sharerr_core::endpoint::is_private_ip(ip)
    {
        anyhow::bail!(
            "{host:?} is a loopback or private address — a friend outside this network could \
             never reach it"
        );
    }
    Ok(())
}

/// Accept `qbit:8080` as well as `http://qbit:8080`.
///
/// `Url::parse` rejects a bare host, and "relative URL without a base" is not a
/// message that tells anyone to add `http://`.
fn normalise_url(raw: &str) -> anyhow::Result<String> {
    let candidate = if raw.contains("://") {
        raw.to_owned()
    } else {
        format!("http://{raw}")
    };

    url::Url::parse(&candidate)
        .map(|url| url.to_string())
        .map_err(|err| anyhow::anyhow!("{raw:?} is not a valid URL: {err}"))
}

/// How the app's name is written, for headings — `as_str` is the lowercase
/// wire/storage form.
pub(super) fn title_case(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The example URL shown in each app's empty URL field: its documented default
/// port, which is the strongest hint a placeholder can give.
fn url_placeholder(source: MediaSource) -> &'static str {
    use MediaSource::{Directory, Lidarr, Radarr, Readarr, Sonarr, Whisparr};
    match source {
        Sonarr => "http://sonarr:8989",
        Radarr => "http://radarr:7878",
        Lidarr => "http://lidarr:8686",
        Readarr => "http://readarr:8787",
        Whisparr => "http://whisparr:6969",
        // Never rendered — the page loops the URL-bearing sources — but the
        // match must stay total, and a directory has no URL to hint at.
        Directory => "",
    }
}

async fn build_page(
    state: &WebState,
    saved: Option<String>,
    error: Option<String>,
) -> SettingsPage {
    let config = state.serve.config().await;
    let secrets = secrets_present(&config).await;
    let is_set = |key: &str| secrets.contains(key);
    let locks = super::config_io::env_overrides();

    // One section per app, from the same list everything else iterates — the
    // settings page used to be the one surface that hand-enumerated two of the
    // five and silently could not configure the rest. `ARRS`, not `ALL`: the
    // directory source has no URL or key and gets the Libraries section below.
    let arrs = MediaSource::ARRS
        .iter()
        .copied()
        .filter_map(|kind| {
            let url_path = config_paths::url_for(kind)?;
            let key = secret_keys::api_key_for(kind)?;
            Some(ArrSection {
                source: kind.as_str(),
                title: title_case(kind.as_str()),
                url: config
                    .service(kind)
                    .map(|s| s.url.to_string())
                    .unwrap_or_default(),
                key_set: is_set(key),
                placeholder: url_placeholder(kind),
                url_path,
                primary: matches!(kind, MediaSource::Sonarr | MediaSource::Radarr),
            })
        })
        .collect::<Vec<_>>();
    let secondary_arr_configured = arrs
        .iter()
        .any(|arr| !arr.primary && (!arr.url.is_empty() || arr.key_set));

    // The one state where what is on disk and what the page renders disagree, so
    // the operator has to be told which of the two a save keeps — and where the
    // other one goes.
    let config_error = state.serve.config_error().await;
    let config_notice = if config_error.is_some() {
        ConfigFile::replacing(state.serve.config_path())
            .backup_path()
            .map(|aside| {
                format!(
                    "The current file will be kept as {} — nothing in it is lost, \
                     but only what this page shows stays in effect.",
                    aside.display()
                )
            })
    } else {
        None
    };

    SettingsPage {
        signed_in: true,
        saved,
        error,
        config_error,
        config_notice,
        master_key_present: master_key_from_env().is_ok(),
        locks,

        tag: config.tag.clone(),

        arrs,
        secondary_arr_configured,

        qbit_url: config.qbittorrent.url.to_string(),
        qbit_api_key_set: is_set(secret_keys::QBITTORRENT_API_KEY),
        qbit_category: config.qbittorrent.category.clone(),
        qbit_tag: config.qbittorrent.tag.clone(),
        qbit_skip_checking: config.qbittorrent.skip_checking,

        tracker_advertised_host: config.tracker.advertised_host.clone().unwrap_or_default(),
        tracker_port: config
            .tracker
            .port
            .map(|p| p.to_string())
            .unwrap_or_default(),
        tracker_advertised_url: config
            .tracker
            .advertised_url
            .as_ref()
            .map(url::Url::to_string)
            .unwrap_or_default(),
        tracker_token_set: is_set(secret_keys::TRACKER_TOKEN),
        gluetun_control_url: config
            .gluetun
            .control_url
            .as_ref()
            .map(url::Url::to_string)
            .unwrap_or_default(),
        gluetun_enabled: config.gluetun.enabled,
        gluetun_api_key_set: is_set(secret_keys::GLUETUN_API_KEY),
        gluetun_poll_secs: config.gluetun.poll_secs,
        gluetun_last_observed: gluetun_last_observed(&state.serve.endpoint()),
        gluetun_last_error: gluetun_last_error(&state.serve, GluetunTarget::Tracker).await,

        gluetun_client_control_url: config
            .gluetun_client
            .control_url
            .as_ref()
            .map(url::Url::to_string)
            .unwrap_or_default(),
        gluetun_client_enabled: config.gluetun_client.enabled,
        gluetun_client_api_key_set: is_set(secret_keys::GLUETUN_CLIENT_API_KEY),
        gluetun_client_poll_secs: config.gluetun_client.poll_secs,
        gluetun_client_last_observed: gluetun_last_observed(&state.serve.client_endpoint()),
        gluetun_client_last_error: gluetun_last_error(&state.serve, GluetunTarget::Client).await,
        gluetun_client_configured: config.gluetun_client.control_url.is_some(),

        revealed: None,

        sync_enabled: config.sync.enabled,
        sync_interval_secs: config.sync.interval_secs,

        notifications_webhook_set: is_set(secret_keys::NOTIFICATIONS_WEBHOOK_URL),
        notifications_kind: config.notifications.kind.as_str(),
        notifications_peer_quiet_secs: config.notifications.peer_quiet_secs,

        // A spare blank row so "add a library" needs no JavaScript, same as
        // the path map below.
        libraries: config
            .library
            .iter()
            .map(|library| LibraryRow {
                path: library.path.display().to_string(),
                kind: library.kind.as_str(),
            })
            .chain(std::iter::once(LibraryRow::default()))
            .collect(),

        // A spare blank row so "add a mapping" needs no JavaScript — the form
        // simply has one more row than there are mappings, and blank rows are
        // dropped on save.
        path_map: config
            .path_map
            .iter()
            .map(|m| PathRow {
                arr: m.arr.display().to_string(),
                sharerr: m.sharerr.display().to_string(),
                qbit: m
                    .qbit
                    .as_ref()
                    .map(|q| q.display().to_string())
                    .unwrap_or_default(),
            })
            .chain(std::iter::once(PathRow::default()))
            .collect(),

        min_password_len: super::auth::MIN_PASSWORD_LEN,
        data_dir: config.data_dir.display().to_string(),
        bind: config.server.bind.to_string(),
        config_path: state.serve.config_path().display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_bare_host_gains_a_scheme() {
        assert_eq!(normalise_url("qbit:8080").unwrap(), "http://qbit:8080/");
        assert_eq!(
            normalise_url("http://sonarr:8989").unwrap(),
            "http://sonarr:8989/"
        );
        assert_eq!(
            normalise_url("https://seed.example/path").unwrap(),
            "https://seed.example/path"
        );
    }

    #[test]
    fn a_hopeless_url_is_named_rather_than_silently_dropped() {
        let err = normalise_url("http://").expect_err("this cannot be a url");
        assert!(format!("{err:#}").contains("not a valid URL"), "{err:#}");
    }

    #[test]
    fn an_unticked_checkbox_reads_as_false() {
        assert!(!checked(&None));
        // Browsers send "on"; the value is irrelevant, presence is the signal.
        assert!(checked(&Some("on".to_owned())));
    }

    #[test]
    fn a_loopback_or_private_advertised_host_is_refused() {
        for host in [
            "127.0.0.1",
            "localhost",
            "LocalHost",
            "::1",
            "10.0.0.5",
            "192.168.1.20",
        ] {
            let err = validate_advertised_host(host).expect_err(host);
            assert!(
                format!("{err:#}").contains(host) || host.eq_ignore_ascii_case("localhost"),
                "{err:#}"
            );
        }
    }

    #[test]
    fn a_bracketed_ipv6_literal_is_checked_the_same_way() {
        assert!(validate_advertised_host("[::1]").is_err());
        assert!(validate_advertised_host("[2001:db8::1]").is_ok());
    }

    #[test]
    fn a_public_address_or_hostname_is_accepted() {
        assert!(validate_advertised_host("203.0.113.9").is_ok());
        assert!(validate_advertised_host("sharerr.example").is_ok());
    }
}
