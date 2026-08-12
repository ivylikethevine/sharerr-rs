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
use sharerr_core::Config;
use sharerr_core::config::secret_keys;
use sharerr_store::{Vault, master_key_from_env};

use super::WebState;
use super::config_io::{ConfigFile, Edit, parse_path_map};
use super::templates::{PathRow, SettingsPage, render};
use crate::torznab::public_base_url;

/// Mint a fresh secret and show it once.
///
/// The Torznab API key is unlike every other secret on this page: it has to be
/// *copied into a friend's Prowlarr*, so a write-only field that can never be read
/// back would make it unusable. Generating server-side and revealing the value on
/// exactly the response that created it is the compromise — the key is never in a
/// URL, never in a redirect, and never re-readable. Lose it and generate another.
pub async fn generate_secret(
    State(state): State<WebState>,
    axum::extract::Path(field): axum::extract::Path<String>,
) -> Response {
    let key = match field.as_str() {
        "torznab" => secret_keys::TORZNAB_API_KEY,
        "tracker" => secret_keys::TRACKER_TOKEN,
        _ => return reject(&state, "There is no such secret to generate.").await,
    };

    let generated = match random_key() {
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

/// 160 bits, hex encoded — long enough that guessing is not a strategy, short
/// enough to paste. Same source the vault uses for its nonces.
fn random_key() -> Result<String, String> {
    let mut bytes = [0u8; 20];
    getrandom::fill(&mut bytes).map_err(|e| format!("could not generate a key: {e}"))?;

    let mut key = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(key, "{byte:02x}");
    }
    Ok(key)
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
    username: String,
    password: String,
    #[serde(default)]
    clear_password: Option<String>,
    category: String,
    tag: String,
    #[serde(default)]
    skip_checking: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TrackerForm {
    backend: String,
    advertised_host: String,
    port: String,
    token: String,
    #[serde(default)]
    clear_token: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SyncForm {
    #[serde(default)]
    enabled: Option<String>,
    interval_secs: String,
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
        file.apply([Edit::str("tag", tag)]);
        Ok(())
    })
    .await
}

pub async fn save_sonarr(State(state): State<WebState>, Form(form): Form<ArrForm>) -> Response {
    save_arr(state, form, "sonarr.url", secret_keys::SONARR_API_KEY).await
}

pub async fn save_radarr(State(state): State<WebState>, Form(form): Form<ArrForm>) -> Response {
    save_arr(state, form, "radarr.url", secret_keys::RADARR_API_KEY).await
}

async fn save_arr(
    state: WebState,
    form: ArrForm,
    url_path: &'static str,
    secret_key: &'static str,
) -> Response {
    // Both halves come from the path the caller already had in hand. Deriving one
    // from the other by string comparison would mean a third *arr app silently
    // writing to whichever branch it fell through to. Subslicing a `&'static str`
    // stays `'static`, which is what `Edit` requires.
    let section = url_path.trim_end_matches(".url");

    if let Err(message) = apply_secret(&state, secret_key, &form.api_key, form.clear_api_key).await
    {
        return reject(&state, &message).await;
    }

    write_config(&state, section, |file| {
        let url = form.url.trim();
        if url.is_empty() {
            // Removing the whole table, not just the URL: `sonarr` is
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
    if let Err(message) = apply_secret(
        &state,
        secret_keys::QBITTORRENT_PASSWORD,
        &form.password,
        form.clear_password,
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
            Edit::str("qbittorrent.url", normalise_url(url)?),
            Edit::str("qbittorrent.username", form.username.trim()),
            Edit::str("qbittorrent.category", form.category.trim()),
            Edit::str("qbittorrent.tag", form.tag.trim()),
            Edit::bool("qbittorrent.skip_checking", checked(&form.skip_checking)),
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
        let backend = match form.backend.as_str() {
            // Matched against a fixed set rather than written through: this lands
            // in a `deny_unknown_fields` enum, and an unrecognised string would be
            // a config file that will not load.
            "builtin" => "builtin",
            _ => "qbittorrent-embedded",
        };

        let mut edits = vec![
            Edit::str("tracker.backend", backend),
            Edit::str_or_unset("tracker.advertised_host", &form.advertised_host),
        ];

        let port = form.port.trim();
        if port.is_empty() {
            edits.push(Edit::unset("tracker.port"));
        } else {
            let parsed: u16 = port.parse().map_err(|_| {
                anyhow::anyhow!("{port:?} is not a port number between 1 and 65535")
            })?;
            edits.push(Edit::int("tracker.port", i64::from(parsed)));
        }

        file.apply(edits);
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
            Edit::bool("sync.enabled", checked(&form.enabled)),
            Edit::int("sync.interval_secs", i64::try_from(interval).unwrap_or(900)),
        ]);
        Ok(())
    })
    .await
}

pub async fn save_paths(State(state): State<WebState>, Form(form): Form<PathsForm>) -> Response {
    // Rows arrive as three parallel lists; a short one means a malformed submission
    // rather than an empty field, and zipping blindly would silently pair the wrong
    // paths together — which is the single most damaging thing this page can get
    // wrong, because it makes qBittorrent seed the wrong file.
    let rows: Vec<(String, String, String)> = (0..form.arr.len())
        .map(|i| {
            (
                form.arr[i].clone(),
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

    let mut file = match ConfigFile::open(&path) {
        Ok(file) => file,
        Err(err) => return reject(state, &format!("{err:#}")).await,
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
async fn secrets_present(config: &Config) -> BTreeSet<String> {
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
async fn reject(state: &WebState, message: &str) -> Response {
    tracing::warn!(message, "rejected a settings change");
    let page = build_page(state, None, Some(message.to_owned())).await;
    (axum::http::StatusCode::BAD_REQUEST, render(&page)).into_response()
}

/// An HTML checkbox submits nothing at all when unticked, so absence is `false`.
fn checked(field: &Option<String>) -> bool {
    field.is_some()
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

async fn build_page(
    state: &WebState,
    saved: Option<String>,
    error: Option<String>,
) -> SettingsPage {
    let config = state.serve.config().await;
    let secrets = secrets_present(&config).await;
    let is_set = |key: &str| secrets.contains(key);

    SettingsPage {
        signed_in: true,
        saved,
        error,
        master_key_present: master_key_from_env().is_ok(),
        locks: super::config_io::env_overrides(),

        tag: config.tag.clone(),

        sonarr_url: config
            .sonarr
            .as_ref()
            .map(|s| s.url.to_string())
            .unwrap_or_default(),
        sonarr_key_set: is_set(secret_keys::SONARR_API_KEY),
        radarr_url: config
            .radarr
            .as_ref()
            .map(|r| r.url.to_string())
            .unwrap_or_default(),
        radarr_key_set: is_set(secret_keys::RADARR_API_KEY),

        qbit_url: config.qbittorrent.url.to_string(),
        qbit_username: config.qbittorrent.username.clone(),
        qbit_password_set: is_set(secret_keys::QBITTORRENT_PASSWORD),
        qbit_category: config.qbittorrent.category.clone(),
        qbit_tag: config.qbittorrent.tag.clone(),
        qbit_skip_checking: config.qbittorrent.skip_checking,

        tracker_builtin: matches!(
            config.tracker.backend,
            sharerr_core::config::TrackerBackend::Builtin
        ),
        tracker_advertised_host: config.tracker.advertised_host.clone().unwrap_or_default(),
        tracker_port: config
            .tracker
            .port
            .map(|p| p.to_string())
            .unwrap_or_default(),
        tracker_token_set: is_set(secret_keys::TRACKER_TOKEN),
        torznab_key_set: is_set(secret_keys::TORZNAB_API_KEY),
        torznab_url: format!("{}/api", public_base_url(&config)),
        tracker_builtin_selected: matches!(
            config.tracker.backend,
            sharerr_core::config::TrackerBackend::Builtin
        ),
        revealed: None,

        sync_enabled: config.sync.enabled,
        sync_interval_secs: config.sync.interval_secs,

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
}
