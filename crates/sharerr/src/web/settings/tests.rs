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
    let urls = parse_lighthouse_urls("https://one.example\n\n  https://two.example  \n").unwrap();
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

// `fixtures::unconfigured()` gives a `ServeState` with no master key set,
// and this test's whole point is that `apply_secret` then fails to open
// the vault — but `master_key_from_env` reads the real process
// environment, which several other tests in this binary legitimately
// mutate via `figment::Jail`. Wrapped in `Jail` too (with `clear_env`) so
// this is guaranteed to run with no other Jail closure's env mutation
// active, rather than racing the parallel runner for a var it needs
// absent — see `secrets.rs`'s `opening_a_vault_without_a_master_key_fails_with_no_side_effects`
// for the same pattern with no async involved.
#[test]
fn save_arr_rejects_when_the_vault_will_not_open_rather_than_write_a_partial_config() {
    figment::Jail::expect_with(|jail| {
        jail.clear_env();
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let config_path = serve.config_path().to_path_buf();
        let state = web_state(serve);

        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            // A non-blank api_key routes through `apply_secret`, which opens
            // the vault — impossible here with no master key set. `save_arr`
            // must reject before `write_config` ever runs, or a URL would
            // land in `sharerr.toml` while the API key silently failed to
            // save beside it.
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
        });
        Ok(())
    });
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

    let written = std::fs::read_to_string(&config_path).expect("save_transmission writes the file");
    assert!(written.contains("http://transmission:9091/"), "{written}");
    assert!(written.contains(r#"username = "sam""#), "{written}");
    assert!(written.contains(r#"label = "shared""#), "{written}");
}

// Same race as `save_arr_rejects_when_the_vault_will_not_open_...` above —
// `Jail`-wrapped so no other Jail test's `SHARERR_MASTER_KEY` can be live
// while this one relies on it being absent.
#[test]
fn save_transmission_rejects_when_the_vault_will_not_open_rather_than_write_a_partial_config() {
    figment::Jail::expect_with(|jail| {
        jail.clear_env();
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let config_path = serve.config_path().to_path_buf();
        let state = web_state(serve);

        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            // A non-blank password routes through `apply_secret`, which opens
            // the vault — impossible here with no master key set. The handler
            // must reject before `write_config` ever runs, or the
            // URL/username/label would land in `sharerr.toml` while the
            // password silently failed to save beside it.
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
        });
        Ok(())
    });
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

// Same race as `save_arr_rejects_when_the_vault_will_not_open_...` above.
#[test]
fn save_rtorrent_rejects_when_the_vault_will_not_open_rather_than_write_a_partial_config() {
    figment::Jail::expect_with(|jail| {
        jail.clear_env();
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let config_path = serve.config_path().to_path_buf();
        let state = web_state(serve);

        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
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
        });
        Ok(())
    });
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
    let written = std::fs::read_to_string(&config_path).expect("save_qbittorrent writes the file");
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
        let written =
            std::fs::read_to_string(&config_path).expect("save_torrent_backend writes the file");
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
// Same race as `save_arr_rejects_when_the_vault_will_not_open_...` above:
// this asserts a rejection that depends on the vault failing to open,
// which needs `SHARERR_MASTER_KEY` to genuinely be absent, not merely
// absent from this fixture's own config.
#[test]
fn save_tracker_with_a_token_rejects_when_the_vault_will_not_open() {
    figment::Jail::expect_with(|jail| {
        jail.clear_env();
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);

        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
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
        });
        Ok(())
    });
}

// Same race as above.
#[test]
fn generate_secret_rejects_when_the_vault_will_not_open() {
    figment::Jail::expect_with(|jail| {
        jail.clear_env();
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);

        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let response =
                generate_secret(State(state), axum::extract::Path("tracker".to_owned())).await;

            assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
        });
        Ok(())
    });
}

// Same race as above.
#[test]
fn finalize_tracker_rejects_when_the_vault_cannot_open() {
    figment::Jail::expect_with(|jail| {
        jail.clear_env();
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);

        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let response = finalize_tracker(State(state)).await;
            assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
        });
        Ok(())
    });
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
    let written = std::fs::read_to_string(&config_path).expect("save_lighthouse writes the file");
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

// ------------------------------------------------ config export / import

#[tokio::test]
async fn export_config_serves_the_effective_config_as_a_downloadable_attachment() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let live = serve.config().await;
    let state = web_state(serve);

    let response = export_config(State(state)).await;

    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let headers = response.headers().clone();
    assert_eq!(
        headers.get(header::CONTENT_TYPE).unwrap(),
        "application/toml"
    );
    assert_eq!(
        headers.get(header::CONTENT_DISPOSITION).unwrap(),
        "attachment; filename=\"sharerr-config.toml\""
    );

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    // Round-trips through the same validation every import goes through,
    // and produces exactly what was live at the time of export.
    let reparsed = crate::settings::validate(&text).unwrap();
    assert_eq!(reparsed.tag, live.tag);
    assert_eq!(reparsed.data_dir, live.data_dir);
}

#[tokio::test]
async fn import_config_replaces_the_file_and_takes_effect_immediately() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let config_path = serve.config_path().to_path_buf();
    let state = web_state(serve);

    let response = import_config(
        State(state.clone()),
        Form(ImportConfigForm {
            toml_text: "tag = \"restored-tag\"\n".to_owned(),
        }),
    )
    .await;

    assert_eq!(response.status(), axum::http::StatusCode::SEE_OTHER);
    let written = std::fs::read_to_string(&config_path).unwrap();
    assert!(written.contains(r#"tag = "restored-tag""#), "{written}");
    // Live immediately, not only after a restart — matching every other
    // settings save.
    assert_eq!(state.serve.config().await.tag, "restored-tag");
}

#[tokio::test]
async fn import_config_rejects_invalid_text_without_touching_the_file() {
    let (_dir, serve) = crate::state::fixtures::ready().await;
    let config_path = serve.config_path().to_path_buf();
    let before = std::fs::read_to_string(&config_path).unwrap_or_default();
    let state = web_state(serve);

    let response = import_config(
        State(state),
        Form(ImportConfigForm {
            toml_text: "this is not valid toml [[[".to_owned(),
        }),
    )
    .await;

    assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(
        std::fs::read_to_string(&config_path).unwrap_or_default(),
        before,
        "a rejected import must not touch the file at all"
    );
}

#[tokio::test]
async fn import_config_backs_up_a_file_that_previously_would_not_load() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let config_path = serve.config_path().to_path_buf();
    std::fs::write(&config_path, "this is not [ valid toml").unwrap();
    let state = web_state(serve);

    let response = import_config(
        State(state),
        Form(ImportConfigForm {
            toml_text: "tag = \"recovered\"\n".to_owned(),
        }),
    )
    .await;

    assert_eq!(response.status(), axum::http::StatusCode::SEE_OTHER);
    let backup = config_path.with_extension("toml.invalid");
    assert!(
        backup.is_file(),
        "the unparseable original must be kept, not discarded"
    );
    assert!(
        std::fs::read_to_string(&config_path)
            .unwrap()
            .contains(r#"tag = "recovered""#)
    );
}

// ------------------------------------------ handlers with an open vault
//
// The tests above stop at the vault's door. These open one for real,
// through `figment::Jail` (CLAUDE.md's one sanctioned way), so the
// secret-writing half of a save — `apply_secret`, the tracker-token
// rotation tail — actually runs.

/// Drive `body` against a `WebState` whose vault opens; `Jail` scopes the
/// master key to this closure and serialises against every other Jail
/// test in the binary. Not async, hence the runtime built here.
fn with_open_vault<F, Fut>(body: F)
where
    F: FnOnce(WebState, std::path::PathBuf) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    figment::Jail::expect_with(|jail| {
        jail.set_env("SHARERR_MASTER_KEY", "settings-tests-master-key");
        let config = sharerr_core::Config {
            data_dir: jail.directory().to_path_buf(),
            ..Default::default()
        };
        let path = jail.directory().join("sharerr.toml");
        // A save reloads the config from this file, and a reload that
        // forgot `data_dir` would move the vault path out from under the
        // very secret the save just wrote.
        std::fs::write(
            &path,
            format!("data_dir = {:?}\n", jail.directory().display().to_string()),
        )
        .unwrap();
        let serve = std::sync::Arc::new(crate::state::ServeState::new(config, path.clone(), None));
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(body(web_state(serve), path));
        Ok(())
    });
}

async fn stored(state: &WebState, key: &str) -> Option<String> {
    state
        .serve
        .open_vault()
        .await
        .unwrap()
        .get(key)
        .unwrap()
        .map(|secret| secret.expose_secret().to_owned())
}

fn tracker_form(token: &str, clear: bool) -> TrackerForm {
    TrackerForm {
        advertised_host: "sharerr.example".to_owned(),
        port: "51413".to_owned(),
        advertised_url: String::new(),
        token: token.to_owned(),
        clear_token: clear.then(|| "on".to_owned()),
    }
}

#[test]
fn generate_secret_mints_a_tracker_token_and_stores_it() {
    with_open_vault(|state, _| async move {
        let response = generate_secret(
            State(state.clone()),
            axum::extract::Path("tracker".to_owned()),
        )
        .await;
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let minted = stored(&state, secret_keys::TRACKER_TOKEN)
            .await
            .expect("the minted token is in the vault");
        assert_eq!(minted.len(), crate::secrets::KEY_BYTES * 2, "hex");
    });
}

/// The tracker form's three vault paths in sequence: a typed token
/// rotates in, a second one shifts the first to the previous slot,
/// finalize retires that slot, and the clear checkbox removes it all.
#[test]
fn save_tracker_rotates_finalizes_and_clears_the_token_through_the_vault() {
    with_open_vault(|state, _| async move {
        let response = save_tracker(
            State(state.clone()),
            Query(NextQuery::default()),
            Form(tracker_form("first-token", false)),
        )
        .await;
        assert_eq!(response.status(), axum::http::StatusCode::SEE_OTHER);
        assert_eq!(
            stored(&state, secret_keys::TRACKER_TOKEN).await.as_deref(),
            Some("first-token")
        );

        save_tracker(
            State(state.clone()),
            Query(NextQuery::default()),
            Form(tracker_form("second-token", false)),
        )
        .await;
        assert_eq!(
            stored(&state, secret_keys::TRACKER_TOKEN_PREVIOUS)
                .await
                .as_deref(),
            Some("first-token")
        );

        let response = finalize_tracker(State(state.clone())).await;
        assert_eq!(response.status(), axum::http::StatusCode::SEE_OTHER);
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::LOCATION)
                .unwrap(),
            "/settings?saved=tracker"
        );
        assert!(
            stored(&state, secret_keys::TRACKER_TOKEN_PREVIOUS)
                .await
                .is_none()
        );
        assert_eq!(
            stored(&state, secret_keys::TRACKER_TOKEN).await.as_deref(),
            Some("second-token")
        );

        let response = save_tracker(
            State(state.clone()),
            Query(NextQuery::default()),
            Form(tracker_form("", true)),
        )
        .await;
        assert_eq!(response.status(), axum::http::StatusCode::SEE_OTHER);
        assert!(stored(&state, secret_keys::TRACKER_TOKEN).await.is_none());
    });
}

#[tokio::test]
async fn save_tracker_rejects_a_token_that_cannot_be_a_url_segment() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let state = web_state(serve);

    let response = save_tracker(
        State(state),
        Query(NextQuery::default()),
        Form(tracker_form("ab/cd+ef==", false)),
    )
    .await;

    assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
}

#[test]
fn save_metrics_writes_the_flag_and_stores_then_clears_the_token() {
    with_open_vault(|state, config_path| async move {
        let response = save_metrics(
            State(state.clone()),
            Form(MetricsForm {
                enabled: Some("on".to_owned()),
                token: "scrape-me".to_owned(),
                clear_token: None,
            }),
        )
        .await;
        assert_eq!(response.status(), axum::http::StatusCode::SEE_OTHER);
        let written = std::fs::read_to_string(&config_path).unwrap();
        assert!(written.contains("[metrics]"), "{written}");
        assert!(written.contains("enabled = true"), "{written}");
        assert_eq!(
            stored(&state, secret_keys::METRICS_TOKEN).await.as_deref(),
            Some("scrape-me")
        );

        let response = save_metrics(
            State(state.clone()),
            Form(MetricsForm {
                enabled: None,
                token: String::new(),
                clear_token: Some("on".to_owned()),
            }),
        )
        .await;
        assert_eq!(response.status(), axum::http::StatusCode::SEE_OTHER);
        assert!(stored(&state, secret_keys::METRICS_TOKEN).await.is_none());
    });
}

// ------------------------------------------ the remaining plain saves

#[tokio::test]
async fn save_checks_writes_the_reachability_flag() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let config_path = serve.config_path().to_path_buf();
    let state = web_state(serve);

    let response = save_checks(
        State(state),
        Form(ChecksForm {
            reachability: Some("on".to_owned()),
        }),
    )
    .await;

    assert_eq!(response.status(), axum::http::StatusCode::SEE_OTHER);
    let written = std::fs::read_to_string(&config_path).unwrap();
    assert!(written.contains("reachability = true"), "{written}");
}

#[tokio::test]
async fn save_lighthouse_with_no_urls_unsets_the_list() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let config_path = serve.config_path().to_path_buf();
    let state = web_state(serve);

    let response = save_lighthouse(
        State(state),
        Form(LighthouseForm {
            enabled: None,
            mount: "tracker".to_owned(),
            urls: "\n\n".to_owned(),
        }),
    )
    .await;

    assert_eq!(response.status(), axum::http::StatusCode::SEE_OTHER);
    let written = std::fs::read_to_string(&config_path).unwrap();
    assert!(!written.contains("urls"), "{written}");
}

#[tokio::test]
async fn save_seeding_with_blank_fields_unsets_both_limits() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let config_path = serve.config_path().to_path_buf();
    let state = web_state(serve);

    let response = save_seeding(
        State(state),
        Form(SeedingForm {
            upload_limit_kib: String::new(),
            ratio_limit: String::new(),
        }),
    )
    .await;

    assert_eq!(response.status(), axum::http::StatusCode::SEE_OTHER);
    let written = std::fs::read_to_string(&config_path).unwrap();
    assert!(!written.contains("upload_limit_kib"), "{written}");
    assert!(!written.contains("ratio_limit"), "{written}");
}

#[tokio::test]
async fn save_sync_rejects_a_non_numeric_interval() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let state = web_state(serve);

    let response = save_sync(
        State(state),
        Form(SyncForm {
            enabled: Some("on".to_owned()),
            interval_secs: "soon".to_owned(),
        }),
    )
    .await;

    assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn save_paths_writes_the_rows_and_drops_the_spare_blank_one() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let config_path = serve.config_path().to_path_buf();
    let state = web_state(serve);

    let response = save_paths(
        State(state),
        Query(NextQuery::default()),
        Form(PathsForm {
            arr: vec!["/data/media".to_owned(), String::new()],
            sharerr: vec!["/media".to_owned(), String::new()],
            qbit: vec!["/downloads".to_owned(), String::new()],
        }),
    )
    .await;

    assert_eq!(response.status(), axum::http::StatusCode::SEE_OTHER);
    let written = std::fs::read_to_string(&config_path).unwrap();
    assert!(written.contains("/data/media"), "{written}");
    assert!(written.contains("/downloads"), "{written}");
}

/// qBittorrent has its own save; handing it to the RPC-style one is a
/// programming error the handler refuses rather than half-applies.
#[tokio::test]
async fn save_rpc_client_refuses_the_backend_that_has_its_own_save() {
    let (_dir, serve) = crate::state::fixtures::unconfigured();
    let state = web_state(serve);

    let response = save_rpc_client(
        &state,
        None,
        RpcClientForm::default(),
        TorrentBackend::Qbittorrent,
    )
    .await;

    assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
}

// ------------------------------------------------------- build_page

#[tokio::test]
async fn build_page_names_the_backup_path_when_the_config_failed_to_load() {
    let (_dir, serve) = crate::state::fixtures::unloadable();
    // The notice only names a backup when there is a file to move aside.
    std::fs::write(serve.config_path(), "taag = 1\n").unwrap();

    let rendered = build_page(&web_state(serve), None, None).await;

    assert!(rendered.config_error.is_some());
    let notice = rendered
        .config_notice
        .expect("a save will move the file aside");
    assert!(notice.contains("will be kept as"), "{notice}");
}

#[tokio::test]
async fn build_page_lists_each_configured_library_before_the_spare_row() {
    let (dir, serve) = crate::state::fixtures::unconfigured();
    serve
        .replace_config(sharerr_core::Config {
            data_dir: dir.path().to_path_buf(),
            library: vec![sharerr_core::config::LibraryConfig {
                path: "/media/tv".into(),
                kind: sharerr_core::config::LibraryKind::Tv,
            }],
            ..Default::default()
        })
        .await;

    let rendered = build_page(&web_state(serve), None, None).await;

    assert_eq!(rendered.libraries.len(), 2);
    assert_eq!(rendered.libraries[0].path, "/media/tv");
    assert_eq!(rendered.libraries[0].kind, "tv");
    assert!(rendered.libraries[1].path.is_empty());
}
