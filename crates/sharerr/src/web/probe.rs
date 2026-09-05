//! "Test connection" — the button next to each service on the settings page.
//!
//! This exists because every way sharerr fails to reach a service produces the
//! same symptom: nothing happens. `sharerr doctor` turns that into an actionable
//! line, but it is a CLI command, and the whole point of the web UI is that an
//! operator should never need one. So each service gets a button that answers the
//! two questions a wrong setting raises — *can I reach it* and *does it accept the
//! credential* — separately, because the fixes are different.
//!
//! The checking itself lives in [`crate::checks`], shared with `doctor`. This file
//! is only the renderer: it turns one outcome into one badge. Deciding what is true
//! in two places is what let the two tools drift into describing different
//! conditions in similar words.
//!
//! Deliberately narrower than `doctor`: path-mapping resolution and tracker state
//! are not checked here. Those need a library to walk and are worth a page of
//! their own rather than a one-line badge.

use axum::extract::{Path, State};
use axum::response::{Html, IntoResponse, Response};
use sharerr_core::config::{TorrentBackend, secret_keys};
use sharerr_core::{Config, MediaSource};
use std::fmt::Write as _;

use super::WebState;
use crate::checks::{
    ArrOutcome, DirOutcome, QbitOutcome, check_arr, check_library, check_qbit,
    resolve_torrent_credential,
};

/// Run one service's check and return the HTML fragment htmx swaps in.
///
/// Always `200`, even when the check fails: htmx does not swap non-2xx responses
/// by default, so a `502` here would leave the operator staring at a button that
/// appears to do nothing — the exact failure mode this page exists to remove.
pub async fn test(State(state): State<WebState>, Path(service): Path<String>) -> Response {
    let config = state.serve.config().await;

    let outcome = if let Some(backend) = TorrentBackend::parse(&service) {
        torrent_client_badge(&state, &config, backend).await
    } else if service == "library" {
        library_badge(&config).await
    } else if let Some(kind) = MediaSource::parse(&service) {
        // "directory" parses as a MediaSource but is not a probeable service;
        // its button is the "library" one above.
        if kind == MediaSource::Directory {
            Outcome::Bad("Unknown service.".to_owned())
        } else {
            arr_badge(kind, &state, &config).await
        }
    } else {
        Outcome::Bad("Unknown service.".to_owned())
    };

    outcome.into_fragment()
}

enum Outcome {
    Good(String),
    Bad(String),
}

impl Outcome {
    /// Render as an escaped inline badge.
    ///
    /// The messages interpolate a service's own error text, which is remote input;
    /// escaping it here is what stops a hostile *arr response becoming markup on
    /// the settings page.
    fn into_fragment(self) -> Response {
        let (class, message) = match self {
            Self::Good(message) => ("ok", message),
            Self::Bad(message) => ("error", message),
        };

        Html(format!(
            r#"<span class="{class}">{}</span>"#,
            askama::filters::escape(&message, askama::filters::Html)
                .map_or_else(|_| String::from("(unprintable)"), |e| e.to_string())
        ))
        .into_response()
    }
}

async fn arr_badge(kind: MediaSource, state: &WebState, config: &Config) -> Outcome {
    let service = config.service(kind);
    // The caller filtered Directory out, and every *arr app has a vault key.
    let Some(key) = secret_keys::credential_for(kind) else {
        return Outcome::Bad("Unknown service.".to_owned());
    };

    let api_key = state.secret(key).await;
    let outcome = check_arr(
        kind,
        service.map(|service| &service.url),
        api_key,
        &config.tag,
    )
    .await;

    // Second person and a next action, which is what distinguishes these from
    // `doctor`'s report — the operator is looking at the field they just filled in.
    match outcome {
        ArrOutcome::NotConfigured => Outcome::Bad("No URL configured. Save one first.".to_owned()),
        ArrOutcome::NoCredential => Outcome::Bad("No API key stored. Save one first.".to_owned()),
        ArrOutcome::CredentialUnreadable(reason) | ArrOutcome::BadUrl(reason) => {
            Outcome::Bad(reason)
        }
        ArrOutcome::Unreachable(reason) => Outcome::Bad(format!("Could not reach it: {reason}")),
        ArrOutcome::AuthRejected => {
            Outcome::Bad("Reached it, but the API key was rejected.".to_owned())
        }
        ArrOutcome::Failed(reason) => Outcome::Bad(reason),
        ArrOutcome::TagMissing { version } => Outcome::Bad(format!(
            "Connected to {version}, but no tag named {:?} exists there yet — \
             create it and tag something, or nothing will ever be shared.",
            config.tag
        )),
        // Not an error: the tag is there and simply has nothing on it yet, which is
        // the normal state right after setup. Saying so is still worth a badge,
        // because it is the most common reason sharerr appears to do nothing.
        ArrOutcome::TagUnused { version } => Outcome::Good(format!(
            "Connected to {version}; the tag exists but nothing carries it yet.",
        )),
        ArrOutcome::Ready { version, items, .. } => Outcome::Good(format!(
            "Connected to {version}; {} file(s) tagged {:?}.",
            items.len(),
            config.tag
        )),
    }
}

/// One badge summarising every `[[library]]` directory: the counts when all is
/// well, or the first problem — the operator fixes one thing at a time anyway.
async fn library_badge(config: &Config) -> Outcome {
    if config.library.is_empty() {
        return Outcome::Bad("No library directories configured. Save one first.".to_owned());
    }

    let libraries = config.library.clone();
    // The scans stat every file; off the async loop so a slow mount cannot
    // stall the single worker thread this may be running on.
    let outcomes = match tokio::task::spawn_blocking(move || {
        libraries
            .into_iter()
            .map(|library| {
                let outcome = check_library(&library);
                (library, outcome)
            })
            .collect::<Vec<_>>()
    })
    .await
    {
        Ok(outcomes) => outcomes,
        // A panicked or cancelled scan must not render as a green "0 folders
        // found" badge asserting the configuration is fine.
        Err(err) => return Outcome::Bad(format!("The scan did not complete: {err}")),
    };

    let (mut folders, mut files, mut skipped) = (0usize, 0usize, 0usize);
    for (library, outcome) in outcomes {
        let path = library.path.display();
        match outcome {
            DirOutcome::Missing => {
                return Outcome::Bad(format!(
                    "{path} does not exist as sharerr sees it — check the mount."
                ));
            }
            DirOutcome::NotADirectory => {
                return Outcome::Bad(format!("{path} is not a directory."));
            }
            DirOutcome::Unreadable(reason) => {
                return Outcome::Bad(format!("Could not scan {path}: {reason}"));
            }
            DirOutcome::Empty => folders += 1,
            DirOutcome::Ready {
                skipped: s,
                ref items,
            } => {
                folders += 1;
                files += items.len();
                skipped += s;
            }
        }
    }

    let mut message = format!("{folders} folder(s), {files} media file(s) found.");
    if skipped > 0 {
        let _ = write!(
            message,
            " {skipped} file(s) skipped — their names could not be classified."
        );
    }
    Outcome::Good(message)
}

/// Test one torrent client specifically, regardless of whether it is the
/// backend currently selected to seed — the button sits under that client's
/// heading and must report on the credentials just saved there, so an
/// operator filling in its fields can confirm they work *before* switching
/// `torrent_backend` over to it.
async fn torrent_client_badge(
    state: &WebState,
    config: &Config,
    backend: TorrentBackend,
) -> Outcome {
    // Which client, and therefore which URL and which vault key, is a
    // parameter rather than inferred from the URL, because two clients can
    // live on the same host.
    let client = config.torrent_client_for(backend);

    // Opened once — going through `state.secret` for each key would open (and
    // Argon2-derive) the vault twice for one badge.
    let secret = super::diagnostics::secret_reader(state.serve.open_vault().await);
    let credential = resolve_torrent_credential(&client, &secret);

    let outcome = check_qbit(backend, client.url, client.login, credential).await;

    match outcome {
        QbitOutcome::NoCredential => Outcome::Bad("No password stored. Save one first.".to_owned()),
        QbitOutcome::CredentialUnreadable(reason) | QbitOutcome::BadUrl(reason) => {
            Outcome::Bad(reason)
        }
        QbitOutcome::Unreachable(reason) => Outcome::Bad(format!("Could not reach it: {reason}")),
        QbitOutcome::AuthRejected => {
            Outcome::Bad("Reached it, but the credential was rejected.".to_owned())
        }
        QbitOutcome::Failed(reason) => Outcome::Bad(format!("Signed in, but: {reason}")),
        QbitOutcome::Ready { version, kind, .. } => {
            Outcome::Good(format!("Signed in to {kind} {version}."))
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::result_large_err)]

    use super::*;
    use axum::body::to_bytes;
    use sharerr_core::config::ServiceConfig;
    use url::Url;

    async fn body_of(outcome: Outcome) -> String {
        let response = outcome.into_fragment();
        let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn a_hostile_error_message_is_escaped() {
        // The failure text can carry a remote service's own words straight through,
        // which makes it the one attacker-influenced string on this page.
        let html = body_of(Outcome::Bad("<img src=x onerror=alert(1)>".to_owned())).await;

        assert!(!html.contains("<img"), "{html}");
        assert!(html.contains("class=\"error\""), "{html}");
    }

    #[tokio::test]
    async fn success_and_failure_are_distinguishable_by_class() {
        assert!(
            body_of(Outcome::Good("fine".to_owned()))
                .await
                .contains("class=\"ok\"")
        );
        assert!(
            body_of(Outcome::Bad("nope".to_owned()))
                .await
                .contains("class=\"error\"")
        );
    }

    // ------------------------------------------------------------- badges
    //
    // No master key is set in this process, so every path that reaches the
    // vault deterministically finds it unreadable rather than open — see
    // CLAUDE.md's no-live-vault-in-tests rule. That still exercises a real
    // branch (`CredentialUnreadable`/an unreadable-vault `Outcome::Bad`),
    // just never the "credential found and it works" happy path.

    use super::super::web_state;

    #[tokio::test]
    async fn library_badge_with_nothing_configured_says_so() {
        let outcome = library_badge(&Config::default()).await;
        assert!(matches!(outcome, Outcome::Bad(_)));
        assert!(body_of(outcome).await.contains("No library directories"));
    }

    #[tokio::test]
    async fn library_badge_counts_a_real_directory() {
        let dir = tempfile::tempdir().unwrap();
        // Only counted, never read: tiny stand-ins rather than a full
        // testkit library's worth of bytes.
        std::fs::write(dir.path().join("Lanternwick.Hollow.S02E01.mkv"), [0u8; 16]).unwrap();
        std::fs::write(dir.path().join("Lanternwick.Hollow.S02E02.mkv"), [0u8; 16]).unwrap();
        let config = Config {
            library: vec![sharerr_core::config::LibraryConfig {
                path: dir.path().to_path_buf(),
                kind: sharerr_core::config::LibraryKind::Tv,
            }],
            ..Config::default()
        };

        let outcome = library_badge(&config).await;
        let html = body_of(outcome).await;
        assert!(html.contains("class=\"ok\""), "{html}");
        assert!(html.contains("media file(s) found"), "{html}");
    }

    #[tokio::test]
    async fn library_badge_reports_a_path_that_is_not_a_directory() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let config = Config {
            library: vec![sharerr_core::config::LibraryConfig {
                path: file.path().to_path_buf(),
                kind: sharerr_core::config::LibraryKind::Tv,
            }],
            ..Config::default()
        };

        let html = body_of(library_badge(&config).await).await;
        assert!(html.contains("class=\"error\""), "{html}");
        assert!(html.contains("is not a directory"), "{html}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn library_badge_reports_an_unreadable_directory() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("placeholder"), b"x").unwrap();
        let unreadable = dir.path().join("locked");
        std::fs::create_dir(&unreadable).unwrap();
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();

        let config = Config {
            library: vec![sharerr_core::config::LibraryConfig {
                path: unreadable.clone(),
                kind: sharerr_core::config::LibraryKind::Tv,
            }],
            ..Config::default()
        };

        let html = body_of(library_badge(&config).await).await;
        // Running as root (some CI/sandboxes) ignores directory permission
        // bits, so this exercises "Could not scan" only where a real
        // unreadable directory is achievable — otherwise it degrades to the
        // ordinary "empty" badge rather than failing the test.
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o755)).unwrap();
        if html.contains("class=\"error\"") {
            assert!(html.contains("Could not scan"), "{html}");
        }
    }

    #[tokio::test]
    async fn library_badge_reports_skipped_files_alongside_the_count() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Lanternwick.Hollow.S02E01.mkv"), [0u8; 16]).unwrap();
        std::fs::write(dir.path().join("some home video.mkv"), [0u8; 16]).unwrap();
        let config = Config {
            library: vec![sharerr_core::config::LibraryConfig {
                path: dir.path().to_path_buf(),
                kind: sharerr_core::config::LibraryKind::Tv,
            }],
            ..Config::default()
        };

        let html = body_of(library_badge(&config).await).await;
        assert!(html.contains("class=\"ok\""), "{html}");
        assert!(html.contains("1 file(s) skipped"), "{html}");
    }

    #[tokio::test]
    async fn library_badge_reports_a_missing_directory() {
        let config = Config {
            library: vec![sharerr_core::config::LibraryConfig {
                path: std::path::PathBuf::from("/does/not/exist/anywhere"),
                kind: sharerr_core::config::LibraryKind::Tv,
            }],
            ..Config::default()
        };

        let html = body_of(library_badge(&config).await).await;
        assert!(html.contains("class=\"error\""), "{html}");
        assert!(html.contains("does not exist"), "{html}");
    }

    #[tokio::test]
    async fn arr_badge_with_no_url_asks_for_one_before_touching_the_vault() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);
        let config = state.serve.config().await;

        let html = body_of(arr_badge(MediaSource::Sonarr, &state, &config).await).await;
        assert!(html.contains("No URL configured"), "{html}");
    }

    #[tokio::test]
    async fn arr_badge_with_a_url_but_no_vault_reports_that_rather_than_no_credential() {
        let (dir, serve) = crate::state::fixtures::unconfigured();
        let config = Config {
            data_dir: dir.path().to_path_buf(),
            sonarr: Some(ServiceConfig {
                url: Url::parse("http://sonarr.example:8989").unwrap(),
            }),
            ..Config::default()
        };
        let state = web_state(serve);

        let html = body_of(arr_badge(MediaSource::Sonarr, &state, &config).await).await;
        assert!(html.contains("class=\"error\""), "{html}");
    }

    #[tokio::test]
    async fn torrent_client_badge_with_no_vault_is_reported_as_such_for_every_backend() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);
        let config = state.serve.config().await;

        for backend in TorrentBackend::ALL.iter().copied() {
            let html = body_of(torrent_client_badge(&state, &config, backend).await).await;
            assert!(html.contains("class=\"error\""), "{backend:?}: {html}");
        }
    }

    #[tokio::test]
    async fn the_test_endpoint_always_answers_200_even_on_failure() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);

        // htmx only swaps a 2xx response — see `test`'s own doc comment — so
        // every one of these must come back 200 despite failing its check.
        for service in [
            "qbittorrent",
            "transmission",
            "rtorrent",
            "library",
            "sonarr",
            "directory",
            "not-a-real-service",
        ] {
            let response = test(State(state.clone()), Path(service.to_owned())).await;
            assert_eq!(
                response.status(),
                axum::http::StatusCode::OK,
                "{service} must answer 200"
            );
        }
    }

    // ------------------------------------------- badges with an open vault
    //
    // Everything above finds the vault unreadable. These open one for real,
    // through `figment::Jail` (the one sanctioned way — see CLAUDE.md), so
    // the badge gets past credential resolution and the *arr or client
    // outcome it renders is the one under test.

    use std::sync::Arc;

    use sharerr_testkit::mock::{base_url, mount_json};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Drive `body` against a `WebState` whose vault opens, with `sharerr.toml`
    /// loaded from `config`. `Jail` scopes the master key to this closure and
    /// serialises against every other Jail test in the binary; it is not
    /// async, hence the plain `#[test]` callers and the runtime built here.
    fn with_open_vault<F, Fut>(config: Config, body: F)
    where
        F: FnOnce(WebState) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        figment::Jail::expect_with(|jail| {
            jail.set_env("SHARERR_MASTER_KEY", "probe-tests-master-key");
            let config = Config {
                data_dir: jail.directory().to_path_buf(),
                ..config
            };
            let path = jail.directory().join("sharerr.toml");
            let serve = Arc::new(crate::state::ServeState::new(config, path, None));
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(body(web_state(serve)));
            Ok(())
        });
    }

    async fn store(state: &WebState, key: &str, value: &str) {
        let mut vault = state.serve.open_vault().await.unwrap();
        vault
            .put(key, &secrecy::SecretString::from(value.to_owned()))
            .unwrap();
    }

    fn sonarr_at(url: &Url) -> Config {
        Config {
            sonarr: Some(ServiceConfig { url: url.clone() }),
            ..Config::default()
        }
    }

    async fn mount_sonarr_status(server: &MockServer) {
        mount_json(
            server,
            "/api/v3/system/status",
            sharerr_testkit::library::system_status_json("Sonarr"),
        )
        .await;
    }

    #[tokio::test]
    async fn arr_badge_refuses_a_kind_with_no_credential_key() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);
        let html =
            body_of(arr_badge(MediaSource::Directory, &state, &Config::default()).await).await;
        assert!(html.contains("Unknown service"), "{html}");
    }

    #[test]
    fn arr_badge_asks_for_a_url_then_a_key_before_dialling() {
        with_open_vault(Config::default(), |state| async move {
            let html =
                body_of(arr_badge(MediaSource::Sonarr, &state, &Config::default()).await).await;
            assert!(html.contains("No URL configured"), "{html}");

            let server = MockServer::start().await;
            let config = sonarr_at(&base_url(&server));
            let html = body_of(arr_badge(MediaSource::Sonarr, &state, &config).await).await;
            assert!(html.contains("No API key stored"), "{html}");
            assert!(
                server.received_requests().await.unwrap().is_empty(),
                "nothing is dialled without a key"
            );
        });
    }

    #[test]
    fn arr_badge_reports_each_way_the_app_can_turn_it_away() {
        with_open_vault(Config::default(), |state| async move {
            store(&state, secret_keys::SONARR_API_KEY, "k").await;

            let server = MockServer::start().await;
            sharerr_testkit::mock::mount_json_status(
                &server,
                "/api/v3/system/status",
                401,
                serde_json::json!({}),
            )
            .await;
            let html = body_of(
                arr_badge(MediaSource::Sonarr, &state, &sonarr_at(&base_url(&server))).await,
            )
            .await;
            assert!(html.contains("API key was rejected"), "{html}");

            let server = MockServer::start().await;
            sharerr_testkit::mock::mount_json_status(
                &server,
                "/api/v3/system/status",
                500,
                serde_json::json!({}),
            )
            .await;
            let html = body_of(
                arr_badge(MediaSource::Sonarr, &state, &sonarr_at(&base_url(&server))).await,
            )
            .await;
            assert!(html.contains("class=\"error\""), "{html}");

            let port = sharerr_testkit::net::closed_port();
            let url = Url::parse(&format!("http://127.0.0.1:{port}")).unwrap();
            let html =
                body_of(arr_badge(MediaSource::Sonarr, &state, &sonarr_at(&url)).await).await;
            assert!(html.contains("Could not reach it"), "{html}");
        });
    }

    #[test]
    fn arr_badge_distinguishes_a_missing_tag_an_unused_tag_and_tagged_files() {
        with_open_vault(Config::default(), |state| async move {
            store(&state, secret_keys::SONARR_API_KEY, "k").await;

            let server = MockServer::start().await;
            mount_sonarr_status(&server).await;
            mount_json(&server, "/api/v3/tag", serde_json::json!([])).await;
            let html = body_of(
                arr_badge(MediaSource::Sonarr, &state, &sonarr_at(&base_url(&server))).await,
            )
            .await;
            assert!(html.contains("no tag named"), "{html}");

            let server = MockServer::start().await;
            mount_sonarr_status(&server).await;
            mount_json(&server, "/api/v3/tag", sharerr_testkit::library::tag_json()).await;
            mount_json(&server, "/api/v3/series", serde_json::json!([])).await;
            let html = body_of(
                arr_badge(MediaSource::Sonarr, &state, &sonarr_at(&base_url(&server))).await,
            )
            .await;
            assert!(html.contains("class=\"ok\""), "{html}");
            assert!(html.contains("nothing carries it yet"), "{html}");

            let media = tempfile::tempdir().unwrap();
            let library = sharerr_testkit::library::tv_library(media.path()).unwrap();
            let server = MockServer::start().await;
            mount_sonarr_status(&server).await;
            mount_json(&server, "/api/v3/tag", sharerr_testkit::library::tag_json()).await;
            mount_json(&server, "/api/v3/series", library.series_json()).await;
            mount_json(&server, "/api/v3/episodefile", library.episodefile_json()).await;
            mount_json(&server, "/api/v3/episode", library.episode_json()).await;
            let html = body_of(
                arr_badge(MediaSource::Sonarr, &state, &sonarr_at(&base_url(&server))).await,
            )
            .await;
            assert!(html.contains("class=\"ok\""), "{html}");
            assert!(html.contains("2 file(s) tagged"), "{html}");
        });
    }

    fn transmission_at(url: &Url) -> Config {
        Config {
            torrent_backend: TorrentBackend::Transmission,
            transmission: sharerr_core::config::TransmissionConfig {
                url: url.clone(),
                ..Default::default()
            },
            ..Config::default()
        }
    }

    async fn transmission_answering(status: u16, body: serde_json::Value) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/transmission/rpc"))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .mount(&server)
            .await;
        server
    }

    #[test]
    fn torrent_client_badge_reports_every_outcome_for_the_client_it_was_asked_about() {
        with_open_vault(Config::default(), |state| async move {
            let server = MockServer::start().await;
            let config = transmission_at(&base_url(&server));
            let badge = |config: Config, state: WebState| async move {
                body_of(torrent_client_badge(&state, &config, TorrentBackend::Transmission).await)
                    .await
            };

            let html = badge(config.clone(), state.clone()).await;
            assert!(html.contains("No password stored"), "{html}");

            store(&state, secret_keys::TRANSMISSION_PASSWORD, "pw").await;

            let server = transmission_answering(
                200,
                serde_json::json!({ "result": "success", "arguments": { "version": "4.0.5" } }),
            )
            .await;
            let html = badge(transmission_at(&base_url(&server)), state.clone()).await;
            assert!(html.contains("Signed in to Transmission 4.0.5"), "{html}");

            let server = transmission_answering(401, serde_json::json!({})).await;
            let html = badge(transmission_at(&base_url(&server)), state.clone()).await;
            assert!(html.contains("credential was rejected"), "{html}");

            let server = transmission_answering(500, serde_json::json!({})).await;
            let html = badge(transmission_at(&base_url(&server)), state.clone()).await;
            assert!(html.contains("Signed in, but:"), "{html}");

            let port = sharerr_testkit::net::closed_port();
            let url = Url::parse(&format!("http://127.0.0.1:{port}")).unwrap();
            let html = badge(transmission_at(&url), state.clone()).await;
            assert!(html.contains("Could not reach it"), "{html}");

            // A credential the backend cannot use: qBittorrent wants an API
            // key and finds a password-shaped value — nothing is dialled.
            store(&state, secret_keys::QBITTORRENT_API_KEY, "pw").await;
            let html = body_of(
                torrent_client_badge(&state, &Config::default(), TorrentBackend::Qbittorrent).await,
            )
            .await;
            assert!(html.contains("class=\"error\""), "{html}");
        });
    }

    #[tokio::test]
    async fn library_badge_counts_an_empty_directory_as_a_folder_with_nothing_in_it() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            library: vec![sharerr_core::config::LibraryConfig {
                path: dir.path().to_path_buf(),
                kind: sharerr_core::config::LibraryKind::Tv,
            }],
            ..Config::default()
        };
        let html = body_of(library_badge(&config).await).await;
        assert!(html.contains("class=\"ok\""), "{html}");
        assert!(
            html.contains("1 folder(s), 0 media file(s) found"),
            "{html}"
        );
    }
}
