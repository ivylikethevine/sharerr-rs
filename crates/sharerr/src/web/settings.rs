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
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use sharerr_core::config::{SyncConfig, TorrentBackend, config_paths, secret_keys};
use sharerr_core::{Config, MediaSource};
use sharerr_store::{Vault, master_key_from_env};

use crate::gluetun::GluetunTarget;

use super::WebState;
use super::config_io::{ConfigFile, Edit, parse_libraries, parse_path_map};
use super::templates::{ArrSection, LibraryRow, PathRow, SettingsPage, render};

/// Mint a fresh secret and show it once.
///
/// Only the tracker token is minted this way — a friend's own key, generated
/// on the Friends page, is what opens the Torznab feed to them. Minting it
/// goes through [`rotate_tracker_token`], the same as hand-typing one in
/// [`save_tracker`], so a generated token gets the same rotation grace period
/// a typed one does.
pub async fn generate_secret(
    State(state): State<WebState>,
    axum::extract::Path(field): axum::extract::Path<String>,
) -> Response {
    if field != "tracker" {
        return reject(&state, "There is no such secret to generate.").await;
    }

    let generated = match crate::secrets::random_hex(crate::secrets::KEY_BYTES) {
        Ok(generated) => generated,
        Err(reason) => return reject(&state, &reason).await,
    };

    if let Err(message) = rotate_tracker_token(&state, &generated).await {
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

/// Where a save should land after it succeeds, instead of the ordinary
/// Settings page. The wizard is the only caller: its steps submit to these
/// same handlers with `?next=/wizard/...` on the form's `action`, so a save
/// made mid-wizard returns to the wizard step rather than dropping the
/// operator onto the full Settings page.
#[derive(Debug, Default, Deserialize)]
pub struct NextQuery {
    next: Option<String>,
}

/// Refuse anything that is not one of this app's own paths, so a crafted
/// `?next=` cannot turn a settings save into an open redirect. `next` only
/// ever comes from a URL this crate rendered, but the value still arrives
/// through the query string of a request nothing else validates.
///
/// "Own path" means a single leading `/` followed by nothing a browser or a
/// `Location` header would reinterpret: no second `/` or `\\` (browsers
/// normalise `/\\evil.example` to the scheme-relative `//evil.example`), no
/// control characters or whitespace (a CR/LF fails `HeaderValue` and turns a
/// successful save into a 500), and nothing outside printable ASCII at all —
/// every path this crate renders into `?next=` is plain ASCII.
fn sanitize_next(next: Option<String>) -> Option<String> {
    // `/\` needs no separate check: the backslash already fails the last clause.
    next.filter(|path| {
        path.starts_with('/')
            && !path.starts_with("//")
            && path.chars().all(|c| c.is_ascii_graphic() && c != '\\')
    })
}

pub async fn page(State(state): State<WebState>, Query(query): Query<PageQuery>) -> Response {
    render(&build_page(&state, query.saved, None).await)
}

// ---------------------------------------------------------------------------
// Forms
// ---------------------------------------------------------------------------

// Every field below that renders as a plain (non-checkbox) HTML input
// tolerates being entirely absent — even the ones the handler goes on to
// treat as required — because the input can arrive at the browser
// *disabled*, either with no master key set (`master_key_present`) or with
// its config path pinned by a `SHARERR_*` env var (`lock_attr`/`locks`), and
// a disabled `<input>` submits nothing at all. Without that, axum's `Form`
// extractor rejects a request missing that key before the handler — and
// therefore `reject()`'s own styled error page — ever runs, surfacing a bare
// unstyled "Failed to deserialize form body: missing field `x`" instead.
// Expressed once per struct via the container-level `#[serde(default)]`
// (backed by `#[derive(Default)]`, trivial for all-`String`/`Option` fields)
// rather than repeated per field, so a field added later inherits it
// automatically instead of needing to remember the attribute. Defaulting to
// `""` costs nothing: every handler already treats an empty field as
// "nothing typed" and reports *that* the ordinary, styled way.

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct GeneralForm {
    tag: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ArrForm {
    url: String,
    api_key: String,
    clear_api_key: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct QbitForm {
    url: String,
    api_key: String,
    clear_api_key: Option<String>,
    category: String,
    tag: String,
    skip_checking: Option<String>,
}

/// The fields Transmission and rTorrent share: both are an RPC endpoint
/// behind a username/password, with one label for sharerr's torrents.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct RpcClientForm {
    url: String,
    username: String,
    password: String,
    clear_password: Option<String>,
    label: String,
}

/// Which torrent client `torrent_backend` selects — its own tiny form,
/// separate from either client's own fields, because switching backends and
/// editing one backend's connection details are different actions with
/// different save buttons.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct TorrentBackendForm {
    backend: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct TrackerForm {
    advertised_host: String,
    port: String,
    advertised_url: String,
    token: String,
    clear_token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ChecksForm {
    reachability: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct LighthouseForm {
    enabled: Option<String>,
    mount: String,
    /// One lighthouse URL per line — see [`parse_lighthouse_urls`].
    urls: String,
}

/// Parse the settings form's lighthouse-URLs textarea: one URL per line,
/// blank lines dropped. A line that is not a valid URL is an error naming
/// the line, not a silent drop — same convention as
/// [`crate::web::config_io::parse_libraries`].
fn parse_lighthouse_urls(raw: &str) -> anyhow::Result<Vec<String>> {
    let mut urls = Vec::new();
    for (index, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parsed = url::Url::parse(line)
            .map_err(|_| anyhow::anyhow!("lighthouse URL {} is not valid: {line:?}", index + 1))?;
        urls.push(parsed.to_string());
    }
    Ok(urls)
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct GluetunForm {
    enabled: Option<String>,
    control_url: String,
    api_key: String,
    clear_api_key: Option<String>,
    poll_secs: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct SyncForm {
    enabled: Option<String>,
    interval_secs: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct NotificationsForm {
    webhook_url: String,
    clear_webhook_url: Option<String>,
    kind: String,
    peer_quiet_secs: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct MetricsForm {
    enabled: Option<String>,
    token: String,
    clear_token: Option<String>,
}

/// Repeated inputs, one entry per row. `axum_extra`'s `Form` is what makes this
/// work — axum's own uses `serde_urlencoded`, which cannot decode repeated keys
/// into a `Vec`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct PathsForm {
    arr: Vec<String>,
    sharerr: Vec<String>,
    qbit: Vec<String>,
}

/// The `[[library]]` rows, same repeated-input shape as [`PathsForm`].
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct LibrariesForm {
    path: Vec<String>,
    kind: Vec<String>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub async fn save_general(
    State(state): State<WebState>,
    Query(next): Query<NextQuery>,
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

    write_config(&state, "general", next.next, |file| {
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
    Query(next): Query<NextQuery>,
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

    write_config_and_secret(
        &state,
        section,
        next.next,
        |file| {
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
        },
        secret_key,
        &form.api_key,
        form.clear_api_key.is_some(),
    )
    .await
}

pub async fn save_qbittorrent(
    State(state): State<WebState>,
    Query(next): Query<NextQuery>,
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

    write_config_and_secret(
        &state,
        "qbittorrent",
        next.next,
        |file| {
            let url = form.url.trim();
            if url.is_empty() {
                anyhow::bail!("qBittorrent's URL is required — sharerr cannot seed without it");
            }
            file.apply([
                Edit::str(config_paths::QBITTORRENT_URL, normalise_url(url)?),
                // `str_or_unset` rather than `str`: a blank value here falls back to
                // the compiled default ("sharerr") instead of writing a literal empty
                // category/tag — the only way this field arrives blank is an unset
                // input (no master key yet, or the field is env-locked), never a
                // deliberate choice, since nothing in the UI offers "blank" as one.
                Edit::str_or_unset(config_paths::QBITTORRENT_CATEGORY, form.category.trim()),
                Edit::str_or_unset(config_paths::QBITTORRENT_TAG, form.tag.trim()),
                Edit::bool(
                    config_paths::QBITTORRENT_SKIP_CHECKING,
                    form.skip_checking.is_some(),
                ),
            ]);
            Ok(())
        },
        secret_keys::QBITTORRENT_API_KEY,
        &form.api_key,
        form.clear_api_key.is_some(),
    )
    .await
}

pub async fn save_transmission(
    State(state): State<WebState>,
    Query(next): Query<NextQuery>,
    Form(form): Form<RpcClientForm>,
) -> Response {
    save_rpc_client(&state, next.next, form, TorrentBackend::Transmission).await
}

pub async fn save_rtorrent(
    State(state): State<WebState>,
    Query(next): Query<NextQuery>,
    Form(form): Form<RpcClientForm>,
) -> Response {
    save_rpc_client(&state, next.next, form, TorrentBackend::Rtorrent).await
}

/// The save both RPC-style clients share — only the config paths, the vault
/// key and the wording of the missing-URL error differ between them.
async fn save_rpc_client(
    state: &WebState,
    next: Option<String>,
    form: RpcClientForm,
    backend: TorrentBackend,
) -> Response {
    let (section, url_path, username_path, label_path, password_key, missing_url) = match backend {
        TorrentBackend::Transmission => (
            "transmission",
            config_paths::TRANSMISSION_URL,
            config_paths::TRANSMISSION_USERNAME,
            config_paths::TRANSMISSION_LABEL,
            secret_keys::TRANSMISSION_PASSWORD,
            "Transmission's URL is required — sharerr cannot seed without it",
        ),
        TorrentBackend::Rtorrent => (
            "rtorrent",
            config_paths::RTORRENT_URL,
            config_paths::RTORRENT_USERNAME,
            config_paths::RTORRENT_LABEL,
            secret_keys::RTORRENT_PASSWORD,
            "rTorrent's URL is required — sharerr cannot seed without it. This is the \
             exact XML-RPC endpoint your reverse proxy answers on, not a base address.",
        ),
        TorrentBackend::Qbittorrent => {
            return reject(state, "There is no such service to configure.").await;
        }
    };

    write_config_and_secret(
        state,
        section,
        next,
        |file| {
            let url = form.url.trim();
            if url.is_empty() {
                anyhow::bail!("{missing_url}");
            }
            file.apply([
                Edit::str(url_path, normalise_url(url)?),
                // `str_or_unset`, not `str`: blank here means the input was never
                // touched (locked, or no master key yet) and should fall back to
                // the compiled default, not store a literal empty username/label —
                // same reasoning as qBittorrent's category and tag above.
                Edit::str_or_unset(username_path, form.username.trim()),
                Edit::str_or_unset(label_path, form.label.trim()),
            ]);
            Ok(())
        },
        password_key,
        &form.password,
        form.clear_password.is_some(),
    )
    .await
}

pub async fn save_torrent_backend(
    State(state): State<WebState>,
    Query(next): Query<NextQuery>,
    Form(form): Form<TorrentBackendForm>,
) -> Response {
    write_config(&state, "torrent_backend", next.next, |file| {
        let Some(backend) = TorrentBackend::parse(&form.backend) else {
            anyhow::bail!(
                "{:?} is not a torrent client sharerr supports",
                form.backend
            );
        };
        file.apply([Edit::str(config_paths::TORRENT_BACKEND, backend.as_str())]);
        Ok(())
    })
    .await
}

pub async fn save_tracker(
    State(state): State<WebState>,
    Query(next): Query<NextQuery>,
    Form(form): Form<TrackerForm>,
) -> Response {
    let pending = match prepare_config(&state, |file| {
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
    {
        Ok(pending) => pending,
        Err(message) => return reject(&state, &message).await,
    };

    let token = form.token.trim();
    if let Err(message) = secret_keys::validate_value(secret_keys::TRACKER_TOKEN, token) {
        return reject(&state, &message).await;
    }
    let outcome = if form.clear_token.is_some() {
        clear_tracker_token(&state).await
    } else if !token.is_empty() {
        rotate_tracker_token(&state, token).await
    } else {
        Ok(())
    };
    if let Err(message) = outcome {
        return reject(&state, &message).await;
    }

    commit_config(&state, "tracker", next.next, pending).await
}

/// Retire the previous announce token a rotation kept valid — the explicit
/// "I'm satisfied, cut it off" step. Vault-only, no config file involved, so
/// this does not go through [`write_config`]; it redirects the same way that
/// helper's success path does.
pub async fn finalize_tracker(State(state): State<WebState>) -> Response {
    if let Err(message) = finalize_tracker_token(&state).await {
        return reject(&state, &message).await;
    }
    Redirect::to("/settings?saved=tracker").into_response()
}

/// The embedded lighthouse: on/off, plus which of sharerr's own listeners
/// carries it when on. No secret involved — the decoy seed behind it is
/// minted and stored by [`crate::state::ServeState::lighthouse_state`] on
/// first use, not typed by an operator.
pub async fn save_lighthouse(
    State(state): State<WebState>,
    Form(form): Form<LighthouseForm>,
) -> Response {
    let Some(mount) = sharerr_core::config::LighthouseMount::parse(&form.mount) else {
        return reject(&state, "That is not a valid lighthouse listener choice.").await;
    };
    let urls = match parse_lighthouse_urls(&form.urls) {
        Ok(urls) => urls,
        Err(err) => return reject(&state, &format!("{err:#}")).await,
    };

    write_config(&state, "lighthouse", None, move |file| {
        file.apply([
            Edit::bool(config_paths::LIGHTHOUSE_ENABLED, form.enabled.is_some()),
            Edit::str(config_paths::LIGHTHOUSE_MOUNT, mount.as_str()),
            if urls.is_empty() {
                Edit::unset(config_paths::LIGHTHOUSE_URLS)
            } else {
                Edit::str_list(config_paths::LIGHTHOUSE_URLS, urls)
            },
        ]);
        Ok(())
    })
    .await
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct SeedingForm {
    upload_limit_kib: String,
    ratio_limit: String,
}

/// A blank field means no goal; anything else must be a whole number of
/// KiB/s. Named in the error the same way [`parse_lighthouse_urls`] names a
/// bad line, rather than silently discarding an unparseable value.
fn parse_upload_limit_kib(raw: &str) -> anyhow::Result<Option<u64>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    trimmed
        .parse::<u64>()
        .map(Some)
        .map_err(|_| anyhow::anyhow!("the upload limit must be a whole number of KiB/s"))
}

/// A blank field means no goal; anything else must be a positive ratio.
fn parse_ratio_limit(raw: &str) -> anyhow::Result<Option<f64>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let ratio: f64 = trimmed
        .parse()
        .map_err(|_| anyhow::anyhow!("the ratio limit must be a number, e.g. 2.0"))?;
    if !ratio.is_finite() || ratio <= 0.0 {
        anyhow::bail!("the ratio limit must be a positive number");
    }
    Ok(Some(ratio))
}

/// A per-torrent upload cap and seed-ratio goal, applied once when sharerr
/// hands a torrent to the client — see [`sharerr_core::config::SeedingConfig`]
/// and `docs/CONFIGURATION.md`'s `[seeding]` section.
pub async fn save_seeding(
    State(state): State<WebState>,
    Form(form): Form<SeedingForm>,
) -> Response {
    let upload_limit_kib = match parse_upload_limit_kib(&form.upload_limit_kib) {
        Ok(v) => v,
        Err(err) => return reject(&state, &format!("{err:#}")).await,
    };
    let ratio_limit = match parse_ratio_limit(&form.ratio_limit) {
        Ok(v) => v,
        Err(err) => return reject(&state, &format!("{err:#}")).await,
    };

    write_config(&state, "seeding", None, move |file| {
        file.apply([
            match upload_limit_kib {
                Some(kib) => Edit::int(
                    config_paths::SEEDING_UPLOAD_LIMIT_KIB,
                    i64::try_from(kib).unwrap_or(i64::MAX),
                ),
                None => Edit::unset(config_paths::SEEDING_UPLOAD_LIMIT_KIB),
            },
            match ratio_limit {
                Some(ratio) => Edit::float(config_paths::SEEDING_RATIO_LIMIT, ratio),
                None => Edit::unset(config_paths::SEEDING_RATIO_LIMIT),
            },
        ]);
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
/// one from the tracker's. Same form shape, same rules, a different section.
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
    write_config_and_secret(
        &state,
        section,
        None,
        |file| {
            file.apply([Edit::bool(enabled_path, form.enabled.is_some())]);

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
                file.apply([Edit::int(poll_secs_path, i64::try_from(secs).unwrap_or(60))]);
            }
            Ok(())
        },
        api_key_secret,
        &form.api_key,
        form.clear_api_key.is_some(),
    )
    .await
}

pub async fn save_sync(State(state): State<WebState>, Form(form): Form<SyncForm>) -> Response {
    write_config(&state, "sync", None, |file| {
        let interval: u64 =
            form.interval_secs.trim().parse().map_err(|_| {
                anyhow::anyhow!("the sync interval must be a whole number of seconds")
            })?;

        // The same floor the background loop clamps to, read from the one
        // constant rather than retyped. Saying so here beats silently storing 5
        // and running at 60.
        if interval < SyncConfig::MIN_INTERVAL_SECS {
            anyhow::bail!(
                "the sync interval must be at least {} seconds",
                SyncConfig::MIN_INTERVAL_SECS
            );
        }

        file.apply([
            Edit::bool(config_paths::SYNC_ENABLED, form.enabled.is_some()),
            Edit::int(
                config_paths::SYNC_INTERVAL_SECS,
                i64::try_from(interval).unwrap_or(900),
            ),
        ]);
        Ok(())
    })
    .await
}

/// The opt-in reachability probe — see
/// [`sharerr_core::config::ChecksConfig`] for why it is off by default.
pub async fn save_checks(State(state): State<WebState>, Form(form): Form<ChecksForm>) -> Response {
    write_config(&state, "checks", None, |file| {
        file.apply([Edit::bool(
            config_paths::CHECKS_REACHABILITY,
            form.reachability.is_some(),
        )]);
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

    write_config_and_secret(
        &state,
        "notifications",
        None,
        |file| {
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
        },
        secret_keys::NOTIFICATIONS_WEBHOOK_URL,
        webhook,
        form.clear_webhook_url.is_some(),
    )
    .await
}

/// `/metrics` and the dashboard-widget endpoint. Same two-write shape as
/// [`save_notifications`]: a bearer token in the vault, `enabled` in
/// `sharerr.toml`. Enabling without setting a token is accepted rather than
/// rejected — the settings page already says plainly that neither endpoint
/// answers without one, so this is a documented degraded state, not a
/// silent one, matching how `[gluetun]` can be enabled with no API key yet.
pub async fn save_metrics(
    State(state): State<WebState>,
    Form(form): Form<MetricsForm>,
) -> Response {
    write_config_and_secret(
        &state,
        "metrics",
        None,
        |file| {
            file.apply([Edit::bool(
                config_paths::METRICS_ENABLED,
                form.enabled.is_some(),
            )]);
            Ok(())
        },
        secret_keys::METRICS_TOKEN,
        form.token.trim(),
        form.clear_token.is_some(),
    )
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

    write_config(&state, "libraries", None, |file| {
        file.set_libraries(&libraries);
        Ok(())
    })
    .await
}

pub async fn save_paths(
    State(state): State<WebState>,
    Query(next): Query<NextQuery>,
    Form(form): Form<PathsForm>,
) -> Response {
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

    write_config(&state, "paths", next.next, |file| {
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
/// Every settings handler goes through here — or through
/// [`write_config_and_secret`] when it also has a secret to store — so that no
/// path can skip the validate-before-write step or forget to invalidate the
/// syncer.
async fn write_config<F>(state: &WebState, section: &str, next: Option<String>, edit: F) -> Response
where
    F: FnOnce(&mut ConfigFile) -> anyhow::Result<()>,
{
    match prepare_config(state, edit).await {
        Ok(pending) => commit_config(state, section, next, pending).await,
        Err(message) => reject(state, &message).await,
    }
}

/// [`write_config`] for a section that also stores a vault secret: config
/// first (open, edit, validate — nothing written), then the secret, then the
/// commit. The order is the point — see [`prepare_config`] — and every
/// handler with a secret goes through here so none can get it backwards.
/// `save_tracker` is the exception: its secret step is a token rotation, not
/// a plain store-or-clear.
async fn write_config_and_secret<F>(
    state: &WebState,
    section: &str,
    next: Option<String>,
    edit: F,
    key: &'static str,
    value: &str,
    clear: bool,
) -> Response
where
    F: FnOnce(&mut ConfigFile) -> anyhow::Result<()>,
{
    let pending = match prepare_config(state, edit).await {
        Ok(pending) => pending,
        Err(message) => return reject(state, &message).await,
    };

    if let Err(message) = apply_secret(state, key, value, clear).await {
        return reject(state, &message).await;
    }

    commit_config(state, section, next, pending).await
}

/// An edited, validated `sharerr.toml` that has not been written yet.
///
/// Carries the config-write lock, so between [`prepare_config`] and
/// [`commit_config`] no other save can open the file — dropping it without
/// committing simply releases the lock and writes nothing.
struct PendingConfig<'a> {
    _guard: tokio::sync::MutexGuard<'a, ()>,
    file: ConfigFile,
    /// The document as it will be written, already validated to `config` —
    /// so the commit serialises and validates once, not once per half.
    text: String,
    config: Config,
    path: std::path::PathBuf,
}

/// The first half of [`write_config`]: open, edit, and **validate** the
/// document, without writing it.
///
/// [`write_config_and_secret`] calls this first and the vault second: the
/// form's plain fields are checked before anything irreversible happens,
/// so a rejected save leaves the vault as it was. The other order — vault
/// first, validate second — committed the credential, invalidated the syncer,
/// and in `save_tracker`'s case consumed the one-slot rotation grace period,
/// while the error page implied nothing had been saved.
async fn prepare_config<'a, F>(state: &'a WebState, edit: F) -> Result<PendingConfig<'a>, String>
where
    F: FnOnce(&mut ConfigFile) -> anyhow::Result<()>,
{
    let guard = state.serve.lock_config_write().await;
    let path = state.serve.config_path().to_path_buf();

    let mut file = if state.serve.config_error().await.is_some() {
        replacement_for(state, &path).await
    } else {
        // The file read and parse are blocking; the guard stays on this task.
        let open_path = path.clone();
        tokio::task::spawn_blocking(move || ConfigFile::open(open_path))
            .await
            .map_err(|err| format!("opening the config file: {err}"))?
            .map_err(|err| format!("{err:#}"))?
    };

    edit(&mut file).map_err(|err| format!("{err:#}"))?;
    // Validated here, before anything irreversible, so a caller can reject
    // *before* touching the vault; `commit_config` writes exactly this text.
    let text = file.to_toml();
    let config = crate::settings::validate(&text).map_err(|err| format!("{err:#}"))?;

    Ok(PendingConfig {
        _guard: guard,
        file,
        text,
        config,
        path,
    })
}

/// The second half of [`write_config`]: write the prepared document, swap
/// the new config in, and redirect.
async fn commit_config(
    state: &WebState,
    section: &str,
    next: Option<String>,
    pending: PendingConfig<'_>,
) -> Response {
    let PendingConfig {
        _guard,
        file,
        text,
        config,
        path,
    } = pending;
    // The write is blocking file IO; `_guard` stays here, on this task, until
    // the function returns, so the lock is held across the write without
    // crossing threads.
    let written = tokio::task::spawn_blocking(move || file.write_validated(&text))
        .await
        .map_err(|err| format!("writing the config file: {err}"))
        .and_then(|result| result.map_err(|err| format!("{err:#}")));
    match written {
        Ok(()) => {
            // Swap the new config in *and* drop the cached syncer, so the change is
            // live within one recovery interval instead of at the next restart.
            state.serve.replace_config(config).await;
            tracing::info!(section, path = %path.display(), "settings saved");
            let destination = sanitize_next(next).unwrap_or_else(|| "/settings".to_owned());
            Redirect::to(&format!("{destination}?saved={section}")).into_response()
        }
        Err(message) => reject(state, &message).await,
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
///
/// `clear` is the explicit checkbox — an HTML checkbox submits nothing at all
/// when unticked, so callers pass `field.is_some()`.
async fn apply_secret(
    state: &WebState,
    key: &'static str,
    value: &str,
    clear: bool,
) -> Result<(), String> {
    let value = value.trim().to_owned();

    if !clear && value.is_empty() {
        return Ok(());
    }

    let mut vault = state.serve.open_vault().await?;

    if clear {
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

/// The vault-mutation core of a rotation, over a plain `&mut Vault` rather
/// than a `WebState` — so it can be unit tested against a vault opened
/// directly with a hand-picked key, no `SHARERR_MASTER_KEY` in the process
/// env required. See the module-level testing-tiers note in `CLAUDE.md`:
/// nothing else in this suite touches the real process env for exactly this
/// reason — it cannot be scoped per test under a parallel runner.
///
/// Preserves the value it replaces as [`secret_keys::TRACKER_TOKEN_PREVIOUS`]
/// so nothing already relying on it breaks mid-rotation — see
/// `crate::tracker::authenticate_token`. A no-op previous-slot write when
/// `new_value` already matches the current token: retyping the same value is
/// not a rotation, and treating it as one would make a double-submit look
/// like a real one happened.
fn rotate_tracker_token_in(vault: &mut Vault, new_value: &str) -> Result<(), String> {
    let current = vault
        .get(secret_keys::TRACKER_TOKEN)
        .map_err(|err| format!("reading the current announce token: {err}"))?;
    if let Some(current) = &current
        && current.expose_secret() != new_value
    {
        vault
            .put(secret_keys::TRACKER_TOKEN_PREVIOUS, current)
            .map_err(|err| format!("preserving the previous announce token: {err}"))?;
    }

    vault
        .put(secret_keys::TRACKER_TOKEN, &SecretString::from(new_value))
        .map_err(|err| format!("storing the announce token: {err}"))
}

/// [`rotate_tracker_token_in`], plus the live-process bookkeeping a rotation
/// through the running instance needs: dropping the cached syncer and the
/// cached token values (both would otherwise keep enforcing what was true
/// before this call), and resetting the previous-token usage status — a
/// fresh rotation means "unknown again" for whether the newly-demoted token
/// is still in use, since the prior answer described a different token.
///
/// The one place both [`save_tracker`]'s hand-typed path and
/// [`generate_secret`]'s minted path meet, so a rotation behaves identically
/// either way.
async fn rotate_tracker_token(state: &WebState, new_value: &str) -> Result<(), String> {
    let mut vault = state.serve.open_vault().await?;
    rotate_tracker_token_in(&mut vault, new_value)?;
    tracing::info!("tracker token rotated through the web ui");

    state.serve.invalidate("the tracker token rotated").await;
    state.serve.legacy_token_status().reset().await;
    Ok(())
}

/// Turn the announce-token requirement off entirely: removes both the
/// current and previous tokens. Leaving a previous token behind would be a
/// forgotten, never-checked-again secret — with no current token configured,
/// `authenticate_token`'s first branch admits every announce before either
/// vault key is even read.
fn clear_tracker_token_in(vault: &mut Vault) -> Result<(), String> {
    vault
        .remove(secret_keys::TRACKER_TOKEN)
        .map_err(|err| format!("removing the announce token: {err}"))?;
    vault
        .remove(secret_keys::TRACKER_TOKEN_PREVIOUS)
        .map_err(|err| format!("removing the previous announce token: {err}"))?;
    Ok(())
}

async fn clear_tracker_token(state: &WebState) -> Result<(), String> {
    let mut vault = state.serve.open_vault().await?;
    clear_tracker_token_in(&mut vault)?;
    tracing::info!("tracker token cleared through the web ui");

    state
        .serve
        .invalidate("the tracker token was cleared")
        .await;
    state.serve.legacy_token_status().reset().await;
    Ok(())
}

/// Finish a rotation: stop accepting the previous token, leaving the current
/// one untouched. Anything still announcing with the previous token is cut
/// off from here on — the explicit step an operator takes once satisfied
/// nothing needs it any more.
fn finalize_tracker_token_in(vault: &mut Vault) -> Result<(), String> {
    vault
        .remove(secret_keys::TRACKER_TOKEN_PREVIOUS)
        .map_err(|err| format!("removing the previous announce token: {err}"))?;
    Ok(())
}

async fn finalize_tracker_token(state: &WebState) -> Result<(), String> {
    let mut vault = state.serve.open_vault().await?;
    finalize_tracker_token_in(&mut vault)?;
    tracing::info!("tracker token rotation finalized through the web ui");

    state
        .serve
        .invalidate("the tracker rotation was finalized")
        .await;
    state.serve.legacy_token_status().reset().await;
    Ok(())
}

/// Which vault keys currently hold a value.
///
/// Reads the names straight out of the file rather than opening the vault. They
/// are cleartext in the format by design, so this costs a ~200-byte read instead
/// of the ~16ms Argon2 derivation `open_vault` pays on every render and again
/// after every save.
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

/// Whether a torrent client other than the selected one already holds a
/// credential — what decides if the fold those live in starts open.
///
/// Keyed on the stored credential rather than the URL, because every client's
/// URL carries a default (`http://localhost:8080` and friends) and so is never
/// empty; a stored secret is the only signal that someone deliberately set one
/// of these up.
fn unselected_client_configured(config: &Config, is_set: &impl Fn(&str) -> bool) -> bool {
    TorrentBackend::ALL
        .iter()
        .copied()
        .filter(|backend| *backend != config.torrent_backend)
        .any(|backend| {
            let client = config.torrent_client_for(backend);
            client.api_key_key.is_some_and(is_set) || client.password_key.is_some_and(is_set)
        })
}

/// An unset URL renders as an empty field, not `None`.
pub(super) fn url_or_empty(url: Option<&url::Url>) -> String {
    url.map(url::Url::to_string).unwrap_or_default()
}

/// What gluetun last actually reported for one endpoint, or `None` when
/// nothing has been observed yet — the settings page's short version of what
/// Diagnostics shows in full.
pub(super) fn gluetun_last_observed(
    endpoint: &sharerr_core::endpoint::AdvertisedEndpoint,
) -> Option<String> {
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
pub(super) async fn gluetun_last_error(
    state: &crate::state::ServeState,
    target: GluetunTarget,
) -> Option<String> {
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
    // A host, not a URL: `Url::parse(&format!("http://{host}:{port}"))` accepts
    // `https://seed.example` (host `https`, path `//seed.example`) and
    // `seed.example/sharerr` (the port lands in the path) without complaint,
    // and either yields a base nothing can announce to.
    if host.contains("://") || host.contains('/') {
        anyhow::bail!(
            "{host:?} should be a bare host name or address — no scheme or path. Use \
             `advertised_url` for a full URL"
        );
    }
    if host.chars().any(char::is_whitespace) {
        anyhow::bail!("{host:?} contains whitespace");
    }
    if host.contains(':') && !(host.starts_with('[') && host.ends_with(']')) {
        anyhow::bail!(
            "{host:?} should not carry a port — the tracker port is its own setting — and an \
             IPv6 address must be written in brackets"
        );
    }
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
pub(super) fn url_placeholder(source: MediaSource) -> &'static str {
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

/// One *arr app's section, as both the Settings page and the wizard render
/// it. `None` for a source with no URL or API key (the directory source).
pub(super) fn arr_section(
    kind: MediaSource,
    config: &Config,
    is_set: &impl Fn(&str) -> bool,
    primary: bool,
) -> Option<ArrSection> {
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
        docs_url: super::docs::for_source(kind).unwrap_or_default(),
        url_path,
        primary,
    })
}

/// The path-mapping rows plus a spare blank one, so "add a mapping" needs no
/// JavaScript — shared with the wizard's paths step.
pub(super) fn path_rows(config: &Config) -> Vec<PathRow> {
    config
        .path_map
        .iter()
        .map(PathRow::from)
        .chain(std::iter::once(PathRow::default()))
        .collect()
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

    // One section per app, from the same list everything else iterates, so a
    // hand-typed enumeration cannot silently leave an app unconfigurable.
    // `ARRS`, not `ALL`: the directory source has no URL or key and gets the
    // Libraries section below.
    let arrs = MediaSource::ARRS
        .iter()
        .copied()
        .filter_map(|kind| {
            let primary = matches!(kind, MediaSource::Sonarr | MediaSource::Radarr);
            arr_section(kind, &config, &is_set, primary)
        })
        .collect::<Vec<_>>();
    let secondary_arr_configured = arrs
        .iter()
        .any(|arr| !arr.primary && (!arr.url.is_empty() || arr.key_set));
    let library_sources_configured =
        arrs.iter().filter(|arr| !arr.url.is_empty()).count() + config.library.len();

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
        library_sources_configured,

        torrent_backend: config.torrent_backend.as_str(),
        unselected_client_configured: unselected_client_configured(&config, &is_set),

        qbit_url: config.qbittorrent.url.to_string(),
        qbit_api_key_set: is_set(secret_keys::QBITTORRENT_API_KEY),
        qbit_category: config.qbittorrent.category.clone(),
        qbit_tag: config.qbittorrent.tag.clone(),
        qbit_skip_checking: config.qbittorrent.skip_checking,

        transmission_url: config.transmission.url.to_string(),
        transmission_username: config.transmission.username.clone(),
        transmission_password_set: is_set(secret_keys::TRANSMISSION_PASSWORD),
        transmission_label: config.transmission.label.clone(),

        rtorrent_url: config.rtorrent.url.to_string(),
        rtorrent_username: config.rtorrent.username.clone(),
        rtorrent_password_set: is_set(secret_keys::RTORRENT_PASSWORD),
        rtorrent_label: config.rtorrent.label.clone(),

        seeding_upload_limit_kib: config
            .seeding
            .upload_limit_kib
            .map(|kib| kib.to_string())
            .unwrap_or_default(),
        seeding_ratio_limit: config
            .seeding
            .ratio_limit
            .map(|ratio| ratio.to_string())
            .unwrap_or_default(),

        tracker_advertised_host: config.tracker.advertised_host.clone().unwrap_or_default(),
        tracker_port: config
            .tracker
            .port
            .map(|p| p.to_string())
            .unwrap_or_default(),
        tracker_advertised_url: url_or_empty(config.tracker.advertised_url.as_ref()),
        tracker_token_set: is_set(secret_keys::TRACKER_TOKEN),
        tracker_token_previous_set: is_set(secret_keys::TRACKER_TOKEN_PREVIOUS),
        tracker_token_previous_last_used: state
            .serve
            .legacy_token_status()
            .snapshot()
            .await
            .map(super::peers::ago),
        lighthouse_enabled: config.lighthouse.enabled,
        lighthouse_mount: config.lighthouse.mount.as_str(),
        lighthouse_urls: config
            .lighthouse
            .urls
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
        lighthouse_url_count: config.lighthouse.urls.len(),
        gluetun_control_url: url_or_empty(config.gluetun.control_url.as_ref()),
        gluetun_enabled: config.gluetun.enabled,
        gluetun_api_key_set: is_set(secret_keys::GLUETUN_API_KEY),
        gluetun_poll_secs: config.gluetun.poll_secs,
        gluetun_last_observed: gluetun_last_observed(&state.serve.endpoint()),
        gluetun_last_error: gluetun_last_error(&state.serve, GluetunTarget::Tracker).await,

        gluetun_client_control_url: url_or_empty(config.gluetun_client.control_url.as_ref()),
        gluetun_client_enabled: config.gluetun_client.enabled,
        gluetun_client_api_key_set: is_set(secret_keys::GLUETUN_CLIENT_API_KEY),
        gluetun_client_poll_secs: config.gluetun_client.poll_secs,
        gluetun_client_last_observed: gluetun_last_observed(&state.serve.client_endpoint()),
        gluetun_client_last_error: gluetun_last_error(&state.serve, GluetunTarget::Client).await,
        gluetun_client_configured: config.gluetun_client.control_url.is_some(),

        revealed: None,

        sync_enabled: config.sync.enabled,
        checks_reachability: config.checks.reachability,
        sync_interval_secs: config.sync.interval_secs,

        notifications_webhook_set: is_set(secret_keys::NOTIFICATIONS_WEBHOOK_URL),
        notifications_kind: config.notifications.kind.as_str(),
        notifications_peer_quiet_secs: config.notifications.peer_quiet_secs,

        metrics_enabled: config.metrics.enabled,
        metrics_token_set: is_set(secret_keys::METRICS_TOKEN),

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

        path_map: path_rows(&config),
        path_map_count: config.path_map.len(),

        min_password_len: super::auth::MIN_PASSWORD_LEN,
        data_dir: config.data_dir.display().to_string(),
        bind: config.server.bind.to_string(),
        config_path: state.serve.config_path().display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::result_large_err)]

    use super::*;

    /// A field whose `<input>` can render `disabled` — no master key yet, or
    /// its config path is pinned by a `SHARERR_*` env var — submits nothing
    /// at all, so every such field must deserialize from an object that
    /// omits it entirely, the same as the form struct's derive would see
    /// from a real `axum_extra::extract::Form` decode of a request missing
    /// that key. `serde_json` stands in for the wire format here: serde's
    /// `#[serde(default)]` handling of an absent field is format-agnostic,
    /// so this exercises the exact same derived `Deserialize` impl a real
    /// urlencoded POST goes through, without needing a signed-in router
    /// fixture this crate has no other precedent for.
    ///
    /// Before the fix, each of these panicked with serde's own "missing
    /// field" error — which, through the real extractor, surfaced as a bare
    /// unstyled 422 instead of ever reaching `reject()`'s styled page.
    #[test]
    fn every_lockable_form_field_tolerates_being_entirely_absent() {
        serde_json::from_str::<GeneralForm>("{}").unwrap();
        serde_json::from_str::<ArrForm>(r#"{}"#).unwrap();
        serde_json::from_str::<QbitForm>(r#"{}"#).unwrap();
        serde_json::from_str::<RpcClientForm>(r#"{}"#).unwrap();
        serde_json::from_str::<TorrentBackendForm>(r#"{}"#).unwrap();
        serde_json::from_str::<TrackerForm>(r#"{}"#).unwrap();
        serde_json::from_str::<LighthouseForm>(r#"{}"#).unwrap();
        serde_json::from_str::<GluetunForm>(r#"{}"#).unwrap();
        serde_json::from_str::<SyncForm>(r#"{}"#).unwrap();
        serde_json::from_str::<NotificationsForm>(r#"{}"#).unwrap();
    }

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
    fn lighthouse_urls_are_one_per_line_and_blank_lines_are_dropped() {
        let urls =
            parse_lighthouse_urls("https://one.example\n\n  https://two.example  \n").unwrap();
        assert_eq!(urls, vec!["https://one.example/", "https://two.example/"]);

        assert_eq!(parse_lighthouse_urls("").unwrap(), Vec::<String>::new());
        assert_eq!(
            parse_lighthouse_urls("   \n  \n").unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_bad_lighthouse_url_names_its_line_rather_than_silently_dropping() {
        let err = parse_lighthouse_urls("https://good.example\nnot a url\n")
            .expect_err("the second line is not a URL");
        assert!(format!("{err:#}").contains("lighthouse URL 2"), "{err:#}");
    }

    #[test]
    fn a_blank_seeding_field_unsets_and_a_valid_one_parses() {
        assert_eq!(parse_upload_limit_kib("").unwrap(), None);
        assert_eq!(parse_upload_limit_kib("  ").unwrap(), None);
        assert_eq!(parse_upload_limit_kib("500").unwrap(), Some(500));

        assert_eq!(parse_ratio_limit("").unwrap(), None);
        assert_eq!(parse_ratio_limit("2.5").unwrap(), Some(2.5));
    }

    #[test]
    fn a_non_numeric_seeding_field_is_named_rather_than_silently_dropped() {
        let err = parse_upload_limit_kib("lots").expect_err("not a number");
        assert!(format!("{err:#}").contains("KiB/s"), "{err:#}");

        let err = parse_ratio_limit("lots").expect_err("not a number");
        assert!(format!("{err:#}").contains("ratio"), "{err:#}");

        let err = parse_ratio_limit("-1").expect_err("a ratio cannot be negative");
        assert!(format!("{err:#}").contains("positive"), "{err:#}");
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

    // -----------------------------------------------------------------------
    // Handler tests
    //
    // A `WebState` built on `state::fixtures::unconfigured()` — a temp `data_dir`
    // with no master key set, same fixture `state.rs`'s own tests use. Per
    // CLAUDE.md, no tier-1 fixture opens a real vault (a parallel test runner
    // cannot scope `SHARERR_MASTER_KEY` per test), so these call handlers
    // directly with hand-built extractors rather than through a router, and
    // stick to inputs that either avoid the vault entirely or deliberately
    // exercise the path where it will not open.
    // -----------------------------------------------------------------------

    use crate::web::web_state;

    #[tokio::test]
    async fn save_arr_writes_the_normalised_url_to_the_config_file() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let config_path = serve.config_path().to_path_buf();
        let state = web_state(serve);

        // Blank api_key with no clear flag never touches the vault — see
        // `apply_secret`'s early return — so this stays within the no-live-vault
        // rule while still exercising the config-writing half of `save_arr`.
        let response = save_arr(
            State(state),
            axum::extract::Path(MediaSource::Sonarr),
            Query(NextQuery::default()),
            Form(ArrForm {
                url: "sonarr:8989".to_owned(),
                api_key: String::new(),
                clear_api_key: None,
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::SEE_OTHER);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::LOCATION)
                .expect("a successful save redirects"),
            "/settings?saved=sonarr"
        );

        let written = std::fs::read_to_string(&config_path).expect("save_arr writes the file");
        assert!(written.contains("http://sonarr:8989/"), "{written}");
    }

    #[tokio::test]
    async fn save_arr_rejects_when_the_vault_will_not_open_rather_than_write_a_partial_config() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let config_path = serve.config_path().to_path_buf();
        let state = web_state(serve);

        // A non-blank api_key routes through `apply_secret`, which opens the
        // vault — impossible here with no master key set. `save_arr` must reject
        // before `write_config` ever runs, or a URL would land in `sharerr.toml`
        // while the API key silently failed to save beside it.
        let response = save_arr(
            State(state),
            axum::extract::Path(MediaSource::Sonarr),
            Query(NextQuery::default()),
            Form(ArrForm {
                url: "sonarr:8989".to_owned(),
                api_key: "some-api-key".to_owned(),
                clear_api_key: None,
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
        assert!(
            !config_path.exists(),
            "a rejected secret write must not leave a partial config file behind"
        );
    }

    #[tokio::test]
    async fn save_transmission_writes_url_username_and_label_to_the_config_file() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let config_path = serve.config_path().to_path_buf();
        let state = web_state(serve);

        // Blank password with no clear flag never touches the vault — see
        // `apply_secret`'s early return — same reasoning `save_arr`'s own
        // config-writing test relies on.
        let response = save_transmission(
            State(state),
            Query(NextQuery::default()),
            Form(RpcClientForm {
                url: "transmission:9091".to_owned(),
                username: "sam".to_owned(),
                password: String::new(),
                clear_password: None,
                label: "shared".to_owned(),
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::SEE_OTHER);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::LOCATION)
                .expect("a successful save redirects"),
            "/settings?saved=transmission"
        );

        let written =
            std::fs::read_to_string(&config_path).expect("save_transmission writes the file");
        assert!(written.contains("http://transmission:9091/"), "{written}");
        assert!(written.contains(r#"username = "sam""#), "{written}");
        assert!(written.contains(r#"label = "shared""#), "{written}");
    }

    #[tokio::test]
    async fn save_transmission_rejects_when_the_vault_will_not_open_rather_than_write_a_partial_config()
     {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let config_path = serve.config_path().to_path_buf();
        let state = web_state(serve);

        // A non-blank password routes through `apply_secret`, which opens the
        // vault — impossible here with no master key set. The handler must
        // reject before `write_config` ever runs, or the URL/username/label
        // would land in `sharerr.toml` while the password silently failed to
        // save beside it.
        let response = save_transmission(
            State(state),
            Query(NextQuery::default()),
            Form(RpcClientForm {
                url: "transmission:9091".to_owned(),
                username: "sam".to_owned(),
                password: "hunter2".to_owned(),
                clear_password: None,
                label: "shared".to_owned(),
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
        assert!(
            !config_path.exists(),
            "a rejected secret write must not leave a partial config file behind"
        );
    }

    #[tokio::test]
    async fn save_rtorrent_writes_url_username_and_label_to_the_config_file() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let config_path = serve.config_path().to_path_buf();
        let state = web_state(serve);

        let response = save_rtorrent(
            State(state),
            Query(NextQuery::default()),
            Form(RpcClientForm {
                url: "http://seedbox.example/RPC2".to_owned(),
                username: "sam".to_owned(),
                password: String::new(),
                clear_password: None,
                label: "shared".to_owned(),
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::SEE_OTHER);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::LOCATION)
                .expect("a successful save redirects"),
            "/settings?saved=rtorrent"
        );

        let written = std::fs::read_to_string(&config_path).expect("save_rtorrent writes the file");
        assert!(written.contains("http://seedbox.example/RPC2"), "{written}");
        assert!(written.contains(r#"username = "sam""#), "{written}");
        assert!(written.contains(r#"label = "shared""#), "{written}");
    }

    /// The URL is the exact RPC endpoint, not a base — `normalise_url` must
    /// not silently append a trailing slash the way a plain-origin URL would
    /// parse to, or the path a reverse proxy actually listens on would be lost.
    #[tokio::test]
    async fn save_rtorrent_preserves_the_exact_rpc_path() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let config_path = serve.config_path().to_path_buf();
        let state = web_state(serve);

        save_rtorrent(
            State(state),
            Query(NextQuery::default()),
            Form(RpcClientForm {
                url: "http://seedbox.example/plugins/httprpc/action.php".to_owned(),
                username: String::new(),
                password: String::new(),
                clear_password: None,
                label: String::new(),
            }),
        )
        .await;

        let written = std::fs::read_to_string(&config_path).expect("save_rtorrent writes the file");
        assert!(
            written.contains("http://seedbox.example/plugins/httprpc/action.php"),
            "{written}"
        );
    }

    #[tokio::test]
    async fn save_rtorrent_rejects_when_the_vault_will_not_open_rather_than_write_a_partial_config()
    {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let config_path = serve.config_path().to_path_buf();
        let state = web_state(serve);

        let response = save_rtorrent(
            State(state),
            Query(NextQuery::default()),
            Form(RpcClientForm {
                url: "http://seedbox.example/RPC2".to_owned(),
                username: "sam".to_owned(),
                password: "hunter2".to_owned(),
                clear_password: None,
                label: "shared".to_owned(),
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
        assert!(
            !config_path.exists(),
            "a rejected secret write must not leave a partial config file behind"
        );
    }

    #[tokio::test]
    async fn save_torrent_backend_writes_the_selected_client() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let config_path = serve.config_path().to_path_buf();
        let state = web_state(serve);

        let response = save_torrent_backend(
            State(state),
            Query(NextQuery::default()),
            Form(TorrentBackendForm {
                backend: "transmission".to_owned(),
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::SEE_OTHER);
        let written =
            std::fs::read_to_string(&config_path).expect("save_torrent_backend writes the file");
        assert!(
            written.contains(r#"torrent_backend = "transmission""#),
            "{written}"
        );
    }

    /// A value that did not come from the `<select>`'s own two `<option>`s —
    /// hand-crafted or stale from a future backend this build does not know —
    /// must be refused rather than written, so `sharerr.toml` never ends up
    /// naming a client [`sharerr_core::config::TorrentBackend`] cannot parse.
    #[tokio::test]
    async fn save_torrent_backend_rejects_a_value_that_is_not_a_known_client() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let config_path = serve.config_path().to_path_buf();
        let state = web_state(serve);

        let response = save_torrent_backend(
            State(state),
            Query(NextQuery::default()),
            Form(TorrentBackendForm {
                backend: "deluge".to_owned(),
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
        assert!(!config_path.exists());
    }

    /// The wizard is the only source of `next`, but the value still arrives
    /// through an ordinary query string that a crafted link could set to
    /// anything — a scheme-relative or absolute URL must not survive to
    /// become the `Location` header.
    #[test]
    fn next_is_only_honoured_when_it_is_this_apps_own_path() {
        assert_eq!(
            sanitize_next(Some("/wizard/paths".to_owned())),
            Some("/wizard/paths".to_owned())
        );
        assert_eq!(sanitize_next(Some("//evil.example".to_owned())), None);
        assert_eq!(sanitize_next(Some("https://evil.example".to_owned())), None);
        assert_eq!(sanitize_next(Some(String::new())), None);
        assert_eq!(sanitize_next(None), None);
    }

    #[tokio::test]
    async fn save_general_redirects_to_next_when_it_is_given_a_safe_one() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);

        let response = save_general(
            State(state),
            Query(NextQuery {
                next: Some("/wizard/services".to_owned()),
            }),
            Form(GeneralForm {
                tag: "sharerr".to_owned(),
            }),
        )
        .await;

        assert_eq!(
            response
                .headers()
                .get(axum::http::header::LOCATION)
                .expect("a successful save redirects"),
            "/wizard/services?saved=general"
        );
    }

    #[tokio::test]
    async fn save_general_falls_back_to_settings_when_next_is_not_this_apps_own_path() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);

        let response = save_general(
            State(state),
            Query(NextQuery {
                next: Some("https://evil.example".to_owned()),
            }),
            Form(GeneralForm {
                tag: "sharerr".to_owned(),
            }),
        )
        .await;

        assert_eq!(
            response
                .headers()
                .get(axum::http::header::LOCATION)
                .expect("a successful save redirects"),
            "/settings?saved=general"
        );
    }

    #[test]
    fn sanitize_next_accepts_only_a_plain_local_path() {
        let ok = |s: &str| sanitize_next(Some(s.to_owned())).as_deref() == Some(s);
        let refused = |s: &str| sanitize_next(Some(s.to_owned())).is_none();

        assert!(ok("/settings"));
        assert!(ok("/wizard/step?x=1&y=2#top"));
        assert!(ok("/"));

        assert!(refused("https://evil.example"));
        assert!(refused("//evil.example"));
        // Browsers normalise a backslash to a slash in special-scheme URLs.
        assert!(refused("/\\evil.example"));
        assert!(refused("/settings\\..\\x"));
        // A CR/LF is not a valid `HeaderValue`; the save must not become a 500.
        assert!(refused("/settings\r\nSet-Cookie: x=y"));
        assert!(refused("/settings x"));
        assert!(refused("/sett\u{e9}ings"));
        assert!(refused("settings"));
        assert!(refused(""));
        assert!(sanitize_next(None).is_none());
    }

    #[tokio::test]
    async fn save_general_rejects_a_blank_tag() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let config_path = serve.config_path().to_path_buf();
        let state = web_state(serve);

        let response = save_general(
            State(state),
            Query(NextQuery::default()),
            Form(GeneralForm {
                tag: "   ".to_owned(),
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
        assert!(!config_path.exists());
    }

    #[tokio::test]
    async fn generate_secret_rejects_a_field_that_is_not_a_known_secret() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);

        let response = generate_secret(
            State(state),
            axum::extract::Path("not-a-real-field".to_owned()),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn the_settings_page_renders_for_a_fresh_unconfigured_instance() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);

        let response = page(State(state), Query(PageQuery::default())).await;

        assert_eq!(response.status(), axum::http::StatusCode::OK);
    }

    #[tokio::test]
    async fn build_page_reports_no_secrets_set_and_no_config_error_for_a_fresh_instance() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();

        let rendered = build_page(&web_state(serve), None, None).await;

        assert!(rendered.config_error.is_none());
        assert!(!rendered.qbit_api_key_set);
        assert!(!rendered.tracker_token_set);
        // A spare blank row is always appended, even with none configured.
        assert_eq!(rendered.libraries.len(), 1);
        assert_eq!(rendered.path_map.len(), 1);
    }

    #[tokio::test]
    async fn save_arr_rejects_a_source_with_no_url_or_api_key() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);

        // The directory source parses as a `MediaSource` but is configured
        // through the Libraries section, not this handler.
        let response = save_arr(
            State(state),
            axum::extract::Path(MediaSource::Directory),
            Query(NextQuery::default()),
            Form(ArrForm::default()),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn save_arr_with_a_blank_url_unsets_the_section_rather_than_write_an_empty_one() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let config_path = serve.config_path().to_path_buf();
        let state = web_state(serve);

        let response = save_arr(
            State(state),
            axum::extract::Path(MediaSource::Sonarr),
            Query(NextQuery::default()),
            Form(ArrForm::default()),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::SEE_OTHER);
        let written = std::fs::read_to_string(&config_path).expect("save_arr writes the file");
        assert!(!written.contains("[sonarr]"), "{written}");
    }

    #[tokio::test]
    async fn save_qbittorrent_rejects_a_malformed_api_key() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);

        let response = save_qbittorrent(
            State(state),
            Query(NextQuery::default()),
            Form(QbitForm {
                url: "qbit:8080".to_owned(),
                api_key: "not-a-real-key".to_owned(),
                ..Default::default()
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn save_qbittorrent_requires_a_url() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let config_path = serve.config_path().to_path_buf();
        let state = web_state(serve);

        let response = save_qbittorrent(
            State(state),
            Query(NextQuery::default()),
            Form(QbitForm::default()),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
        assert!(!config_path.exists());
    }

    #[tokio::test]
    async fn save_qbittorrent_writes_category_tag_and_skip_checking() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let config_path = serve.config_path().to_path_buf();
        let state = web_state(serve);

        let response = save_qbittorrent(
            State(state),
            Query(NextQuery::default()),
            Form(QbitForm {
                url: "qbit:8080".to_owned(),
                category: "sharerr".to_owned(),
                tag: "shared".to_owned(),
                skip_checking: Some("on".to_owned()),
                ..Default::default()
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::SEE_OTHER);
        let written =
            std::fs::read_to_string(&config_path).expect("save_qbittorrent writes the file");
        assert!(written.contains(r#"category = "sharerr""#), "{written}");
        assert!(written.contains(r#"tag = "shared""#), "{written}");
        assert!(written.contains("skip_checking = true"), "{written}");
    }

    #[tokio::test]
    async fn save_transmission_requires_a_url() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let config_path = serve.config_path().to_path_buf();
        let state = web_state(serve);

        let response = save_transmission(
            State(state),
            Query(NextQuery::default()),
            Form(RpcClientForm::default()),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
        assert!(!config_path.exists());
    }

    #[tokio::test]
    async fn save_rtorrent_requires_a_url() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let config_path = serve.config_path().to_path_buf();
        let state = web_state(serve);

        let response = save_rtorrent(
            State(state),
            Query(NextQuery::default()),
            Form(RpcClientForm::default()),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
        assert!(!config_path.exists());
    }

    #[tokio::test]
    async fn save_torrent_backend_accepts_qbittorrent_and_rtorrent_too() {
        for backend in ["qbittorrent", "rtorrent"] {
            let (_dir, serve) = crate::state::fixtures::unconfigured();
            let config_path = serve.config_path().to_path_buf();
            let state = web_state(serve);

            let response = save_torrent_backend(
                State(state),
                Query(NextQuery::default()),
                Form(TorrentBackendForm {
                    backend: backend.to_owned(),
                }),
            )
            .await;

            assert_eq!(response.status(), axum::http::StatusCode::SEE_OTHER);
            let written = std::fs::read_to_string(&config_path)
                .expect("save_torrent_backend writes the file");
            assert!(
                written.contains(&format!(r#"torrent_backend = "{backend}""#)),
                "{written}"
            );
        }
    }

    #[tokio::test]
    async fn save_tracker_writes_host_port_and_advertised_url() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let config_path = serve.config_path().to_path_buf();
        let state = web_state(serve);

        let response = save_tracker(
            State(state),
            Query(NextQuery::default()),
            Form(TrackerForm {
                advertised_host: "sharerr.example".to_owned(),
                port: "51413".to_owned(),
                advertised_url: "https://sharerr.example".to_owned(),
                token: String::new(),
                clear_token: None,
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::SEE_OTHER);
        let written = std::fs::read_to_string(&config_path).expect("save_tracker writes the file");
        assert!(
            written.contains(r#"advertised_host = "sharerr.example""#),
            "{written}"
        );
        assert!(written.contains("port = 51413"), "{written}");
        assert!(
            written.contains(r#"advertised_url = "https://sharerr.example/""#),
            "{written}"
        );
    }

    #[tokio::test]
    async fn save_tracker_rejects_a_private_advertised_host() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let config_path = serve.config_path().to_path_buf();
        let state = web_state(serve);

        let response = save_tracker(
            State(state),
            Query(NextQuery::default()),
            Form(TrackerForm {
                advertised_host: "192.168.1.20".to_owned(),
                ..Default::default()
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
        assert!(!config_path.exists());
    }

    #[test]
    fn validate_advertised_host_wants_a_bare_host() {
        assert!(validate_advertised_host("seed.example").is_ok());
        assert!(validate_advertised_host("203.0.113.5").is_ok());
        assert!(validate_advertised_host("[2001:db8::1]").is_ok());

        assert!(validate_advertised_host("https://seed.example").is_err());
        assert!(validate_advertised_host("seed.example/sharerr").is_err());
        assert!(validate_advertised_host("seed.example:8477").is_err());
        assert!(validate_advertised_host("2001:db8::1").is_err());
        assert!(validate_advertised_host("seed example").is_err());
        assert!(validate_advertised_host("localhost").is_err());
        assert!(validate_advertised_host("192.168.1.5").is_err());
    }

    #[tokio::test]
    async fn save_tracker_rejects_a_port_out_of_range() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let config_path = serve.config_path().to_path_buf();
        let state = web_state(serve);

        let response = save_tracker(
            State(state),
            Query(NextQuery::default()),
            Form(TrackerForm {
                port: "not-a-port".to_owned(),
                ..Default::default()
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
        assert!(!config_path.exists());
    }

    #[tokio::test]
    async fn save_tracker_with_blank_fields_unsets_them() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let config_path = serve.config_path().to_path_buf();
        let state = web_state(serve);

        let response = save_tracker(
            State(state),
            Query(NextQuery::default()),
            Form(TrackerForm::default()),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::SEE_OTHER);
        assert!(config_path.exists());
    }

    // ------------------------------------------------------ token rotation
    //
    // `rotate_tracker_token_in`/`clear_tracker_token_in`/
    // `finalize_tracker_token_in` take a plain `&mut Vault` rather than a
    // `WebState`, specifically so they can be tested against a vault opened
    // directly with a hand-picked key — no `SHARERR_MASTER_KEY` in the real
    // process env, and so none of the risk `CLAUDE.md`'s testing-tiers note
    // warns about (a parallel test runner cannot scope a real env var per
    // test — `figment::Jail`'s own scoping only covers other `Jail` users,
    // not the many plain `unconfigured()`-based tests elsewhere in this same
    // file that assert on there being *no* master key).

    fn open_test_vault(dir: &tempfile::TempDir) -> Vault {
        Vault::open(
            dir.path().join("vault.bin"),
            &SecretString::from("test-key"),
        )
        .unwrap()
    }

    #[test]
    fn rotating_a_first_token_sets_it_with_no_previous() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = open_test_vault(&dir);

        rotate_tracker_token_in(&mut vault, "first-token").unwrap();

        assert_eq!(
            vault
                .get(secret_keys::TRACKER_TOKEN)
                .unwrap()
                .unwrap()
                .expose_secret(),
            "first-token"
        );
        assert!(
            vault
                .get(secret_keys::TRACKER_TOKEN_PREVIOUS)
                .unwrap()
                .is_none()
        );
    }

    /// The whole point of rotation: the value a second rotation replaces is
    /// preserved, not dropped, and a third rotation only ever keeps the
    /// *immediately* prior value — a single-generation grace window, not a
    /// chain.
    #[test]
    fn rotating_again_shifts_the_current_token_to_previous_and_only_keeps_one_generation() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = open_test_vault(&dir);

        rotate_tracker_token_in(&mut vault, "first-token").unwrap();
        rotate_tracker_token_in(&mut vault, "second-token").unwrap();

        assert_eq!(
            vault
                .get(secret_keys::TRACKER_TOKEN)
                .unwrap()
                .unwrap()
                .expose_secret(),
            "second-token"
        );
        assert_eq!(
            vault
                .get(secret_keys::TRACKER_TOKEN_PREVIOUS)
                .unwrap()
                .unwrap()
                .expose_secret(),
            "first-token"
        );

        rotate_tracker_token_in(&mut vault, "third-token").unwrap();

        assert_eq!(
            vault
                .get(secret_keys::TRACKER_TOKEN)
                .unwrap()
                .unwrap()
                .expose_secret(),
            "third-token"
        );
        assert_eq!(
            vault
                .get(secret_keys::TRACKER_TOKEN_PREVIOUS)
                .unwrap()
                .unwrap()
                .expose_secret(),
            "second-token",
            "the first token must not linger once a second rotation has happened"
        );
    }

    /// Retyping the same value the token already holds is not a rotation —
    /// there is nothing to preserve, and treating it as one would make an
    /// accidental double-submit look like a real rotation happened.
    #[test]
    fn rotating_to_the_same_value_is_a_no_op_for_the_previous_slot() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = open_test_vault(&dir);

        rotate_tracker_token_in(&mut vault, "same-token").unwrap();
        rotate_tracker_token_in(&mut vault, "same-token").unwrap();

        assert_eq!(
            vault
                .get(secret_keys::TRACKER_TOKEN)
                .unwrap()
                .unwrap()
                .expose_secret(),
            "same-token"
        );
        assert!(
            vault
                .get(secret_keys::TRACKER_TOKEN_PREVIOUS)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn clearing_the_token_removes_both_current_and_previous() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = open_test_vault(&dir);
        rotate_tracker_token_in(&mut vault, "first-token").unwrap();
        rotate_tracker_token_in(&mut vault, "second-token").unwrap();

        clear_tracker_token_in(&mut vault).unwrap();

        assert!(vault.get(secret_keys::TRACKER_TOKEN).unwrap().is_none());
        assert!(
            vault
                .get(secret_keys::TRACKER_TOKEN_PREVIOUS)
                .unwrap()
                .is_none(),
            "turning the requirement off must not leave a forgotten previous token"
        );
    }

    #[test]
    fn finalizing_removes_only_the_previous_token() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = open_test_vault(&dir);
        rotate_tracker_token_in(&mut vault, "first-token").unwrap();
        rotate_tracker_token_in(&mut vault, "second-token").unwrap();

        finalize_tracker_token_in(&mut vault).unwrap();

        assert_eq!(
            vault
                .get(secret_keys::TRACKER_TOKEN)
                .unwrap()
                .unwrap()
                .expose_secret(),
            "second-token",
            "finalizing must not touch the current token"
        );
        assert!(
            vault
                .get(secret_keys::TRACKER_TOKEN_PREVIOUS)
                .unwrap()
                .is_none()
        );
    }

    /// Neither the hand-typed path (`save_tracker`) nor the minted path
    /// (`generate_secret`) can touch the vault without one, same as every
    /// other secret-writing handler in this file — this is the regression
    /// check that rewiring both onto `rotate_tracker_token` did not lose
    /// that failure mode, without needing a real openable vault to prove it.
    #[tokio::test]
    async fn save_tracker_with_a_token_rejects_when_the_vault_will_not_open() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);

        let response = save_tracker(
            State(state),
            Query(NextQuery::default()),
            Form(TrackerForm {
                token: "typed-token".to_owned(),
                ..Default::default()
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn generate_secret_rejects_when_the_vault_will_not_open() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);

        let response =
            generate_secret(State(state), axum::extract::Path("tracker".to_owned())).await;

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn finalize_tracker_rejects_when_the_vault_cannot_open() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);

        let response = finalize_tracker(State(state)).await;

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn save_lighthouse_rejects_an_unknown_mount() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);

        let response = save_lighthouse(
            State(state),
            Form(LighthouseForm {
                mount: "not-a-mount".to_owned(),
                ..Default::default()
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn save_lighthouse_rejects_an_invalid_url_in_the_list() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);

        let response = save_lighthouse(
            State(state),
            Form(LighthouseForm {
                mount: "frontend".to_owned(),
                urls: "not a url".to_owned(),
                ..Default::default()
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn save_lighthouse_writes_enabled_mount_and_urls() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let config_path = serve.config_path().to_path_buf();
        let state = web_state(serve);

        let response = save_lighthouse(
            State(state),
            Form(LighthouseForm {
                enabled: Some("on".to_owned()),
                mount: "tracker".to_owned(),
                urls: "https://lighthouse.example".to_owned(),
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::SEE_OTHER);
        let written =
            std::fs::read_to_string(&config_path).expect("save_lighthouse writes the file");
        assert!(written.contains("enabled = true"), "{written}");
        assert!(written.contains(r#"mount = "tracker""#), "{written}");
        assert!(written.contains("https://lighthouse.example/"), "{written}");
    }

    #[tokio::test]
    async fn save_seeding_rejects_a_non_numeric_upload_limit() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);

        let response = save_seeding(
            State(state),
            Form(SeedingForm {
                upload_limit_kib: "lots".to_owned(),
                ratio_limit: String::new(),
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn save_seeding_rejects_a_negative_ratio() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);

        let response = save_seeding(
            State(state),
            Form(SeedingForm {
                upload_limit_kib: String::new(),
                ratio_limit: "-1".to_owned(),
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn save_seeding_writes_the_limits() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let config_path = serve.config_path().to_path_buf();
        let state = web_state(serve);

        let response = save_seeding(
            State(state),
            Form(SeedingForm {
                upload_limit_kib: "500".to_owned(),
                ratio_limit: "2.5".to_owned(),
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::SEE_OTHER);
        let written = std::fs::read_to_string(&config_path).expect("save_seeding writes the file");
        assert!(written.contains("upload_limit_kib = 500"), "{written}");
        assert!(written.contains("ratio_limit = 2.5"), "{written}");
    }

    #[tokio::test]
    async fn save_gluetun_writes_enabled_and_control_url() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let config_path = serve.config_path().to_path_buf();
        let state = web_state(serve);

        let response = save_gluetun(
            State(state),
            Form(GluetunForm {
                enabled: Some("on".to_owned()),
                control_url: "gluetun:8000".to_owned(),
                poll_secs: "60".to_owned(),
                ..Default::default()
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::SEE_OTHER);
        let written = std::fs::read_to_string(&config_path).expect("save_gluetun writes the file");
        assert!(written.contains("[gluetun]"), "{written}");
        assert!(written.contains("http://gluetun:8000/"), "{written}");
    }

    #[tokio::test]
    async fn save_gluetun_rejects_a_poll_interval_below_the_floor() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let config_path = serve.config_path().to_path_buf();
        let state = web_state(serve);

        let response = save_gluetun(
            State(state),
            Form(GluetunForm {
                poll_secs: "1".to_owned(),
                ..Default::default()
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
        assert!(!config_path.exists());
    }

    #[tokio::test]
    async fn save_gluetun_rejects_a_non_numeric_poll_interval() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);

        let response = save_gluetun(
            State(state),
            Form(GluetunForm {
                poll_secs: "soon".to_owned(),
                ..Default::default()
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn save_gluetun_client_writes_to_the_client_section() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let config_path = serve.config_path().to_path_buf();
        let state = web_state(serve);

        let response = save_gluetun_client(
            State(state),
            Form(GluetunForm {
                control_url: "gluetun-client:8000".to_owned(),
                ..Default::default()
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::SEE_OTHER);
        let written =
            std::fs::read_to_string(&config_path).expect("save_gluetun_client writes the file");
        assert!(written.contains("[gluetun_client]"), "{written}");
    }

    #[tokio::test]
    async fn save_sync_rejects_an_interval_below_the_floor() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);

        let response = save_sync(
            State(state),
            Form(SyncForm {
                enabled: None,
                interval_secs: "1".to_owned(),
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn save_sync_writes_enabled_and_interval() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let config_path = serve.config_path().to_path_buf();
        let state = web_state(serve);

        let response = save_sync(
            State(state),
            Form(SyncForm {
                enabled: Some("on".to_owned()),
                interval_secs: "900".to_owned(),
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::SEE_OTHER);
        let written = std::fs::read_to_string(&config_path).expect("save_sync writes the file");
        assert!(written.contains("enabled = true"), "{written}");
        assert!(written.contains("interval_secs = 900"), "{written}");
    }

    #[tokio::test]
    async fn save_notifications_rejects_an_invalid_webhook_url() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);

        let response = save_notifications(
            State(state),
            Form(NotificationsForm {
                webhook_url: "not a url".to_owned(),
                kind: "generic".to_owned(),
                peer_quiet_secs: "600".to_owned(),
                ..Default::default()
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn save_notifications_rejects_an_unknown_kind() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);

        let response = save_notifications(
            State(state),
            Form(NotificationsForm {
                kind: "carrier-pigeon".to_owned(),
                peer_quiet_secs: "600".to_owned(),
                ..Default::default()
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn save_notifications_rejects_a_non_numeric_peer_quiet_threshold() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);

        let response = save_notifications(
            State(state),
            Form(NotificationsForm {
                kind: "discord".to_owned(),
                peer_quiet_secs: "a while".to_owned(),
                ..Default::default()
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn save_notifications_writes_kind_and_peer_quiet_secs() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let config_path = serve.config_path().to_path_buf();
        let state = web_state(serve);

        let response = save_notifications(
            State(state),
            Form(NotificationsForm {
                kind: "apprise".to_owned(),
                peer_quiet_secs: "3600".to_owned(),
                ..Default::default()
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::SEE_OTHER);
        let written =
            std::fs::read_to_string(&config_path).expect("save_notifications writes the file");
        assert!(written.contains(r#"kind = "apprise""#), "{written}");
        assert!(written.contains("peer_quiet_secs = 3600"), "{written}");
    }

    #[tokio::test]
    async fn save_libraries_rejects_an_unparseable_row() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);

        let response = save_libraries(
            State(state),
            Form(LibrariesForm {
                path: vec!["/media/tv".to_owned()],
                kind: vec!["not-a-kind".to_owned()],
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn save_libraries_writes_a_valid_row() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let dir = serve.config().await.data_dir.clone();
        let library_path = dir.join("tv");
        std::fs::create_dir_all(&library_path).expect("make a real directory to point at");
        let config_path = serve.config_path().to_path_buf();
        let state = web_state(serve);

        let response = save_libraries(
            State(state),
            Form(LibrariesForm {
                path: vec![library_path.display().to_string()],
                kind: vec!["tv".to_owned()],
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::SEE_OTHER);
        assert!(config_path.exists());
    }

    #[tokio::test]
    async fn save_paths_rejects_a_short_row() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);

        let response = save_paths(
            State(state),
            Query(NextQuery::default()),
            Form(PathsForm {
                arr: vec!["/data/media".to_owned()],
                sharerr: vec![],
                qbit: vec![],
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn write_config_falls_back_to_a_replacement_when_the_config_failed_to_load() {
        let (_dir, serve) = crate::state::fixtures::unloadable();
        let config_path = serve.config_path().to_path_buf();
        let state = web_state(serve);

        let response = save_general(
            State(state),
            Query(NextQuery::default()),
            Form(GeneralForm {
                tag: "sharerr".to_owned(),
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::SEE_OTHER);
        let written =
            std::fs::read_to_string(&config_path).expect("the replacement file must be written");
        assert!(written.contains(r#"tag = "sharerr""#), "{written}");
    }

    #[tokio::test]
    async fn write_config_reports_a_config_file_that_will_not_open() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let config_path = serve.config_path().to_path_buf();
        // No config_error is recorded, but the file on disk is not valid TOML —
        // `ConfigFile::open` must fail, not panic, and no half-written file
        // should be left behind.
        std::fs::write(&config_path, "this is not [ valid toml").expect("seed a broken file");
        let state = web_state(serve);

        let response = save_general(
            State(state),
            Query(NextQuery::default()),
            Form(GeneralForm {
                tag: "sharerr".to_owned(),
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn secrets_present_is_empty_with_no_vault_configured() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let config = serve.config().await;

        assert!(secrets_present(&config).await.is_empty());
    }

    #[test]
    fn gluetun_last_observed_is_none_until_something_is_observed() {
        let endpoint = sharerr_core::endpoint::AdvertisedEndpoint::new(None);
        assert_eq!(gluetun_last_observed(&endpoint), None);

        let base = url::Url::parse("http://gluetun:8000").unwrap();
        endpoint.observe(base);
        assert!(gluetun_last_observed(&endpoint).is_some());
    }

    #[test]
    fn title_case_capitalises_only_the_first_letter() {
        assert_eq!(title_case("sonarr"), "Sonarr");
        assert_eq!(title_case(""), "");
    }

    #[test]
    fn url_placeholder_names_each_arrs_documented_default_port() {
        assert_eq!(url_placeholder(MediaSource::Sonarr), "http://sonarr:8989");
        assert_eq!(url_placeholder(MediaSource::Directory), "");
    }
}
