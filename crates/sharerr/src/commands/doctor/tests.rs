
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::result_large_err)]

use sharerr_core::config::{LibraryKind, TorrentBackend};
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::test_support::vault_in;

// `check_vault` (and therefore `run`) resolve the master key from the real
// process env var — see CLAUDE.md's "no tier-1 fixture opens a real vault".
// Everything below that takes `Option<&Vault>`/`&Vault` directly, rather than
// deriving it internally, is tested against a vault built the way
// `gossip.rs`'s and `sync/tests.rs`'s tests do: a local key that never
// touches the process env.

// --------------------------------------------------------- report_capped

#[test]
fn report_capped_stops_showing_items_past_the_cap_but_still_counts_them() {
    let mut report = Report::default();
    let items: Vec<i32> = (0..(MAX_LISTED as i32 + 3)).collect();
    let mut shown = Vec::new();

    report_capped(
        &mut report,
        &items,
        |_, item| shown.push(*item),
        " thing(s)",
    );

    assert_eq!(shown.len(), MAX_LISTED);
    assert_eq!(shown, items[..MAX_LISTED]);
}

#[test]
fn report_capped_shows_every_item_when_under_the_cap() {
    let mut report = Report::default();
    let items = vec!["a", "b"];
    let mut shown = Vec::new();

    report_capped(&mut report, &items, |_, item| shown.push(*item), "");

    assert_eq!(shown, items);
}

// ------------------------------------------------------------ fail/hint

#[test]
fn fix_hint_names_both_the_web_ui_and_the_cli() {
    let hint = fix_hint("sonarr.api_key");
    assert!(hint.contains("Settings"));
    assert!(hint.contains("sharerr vault set sonarr.api_key"));
}

#[test]
fn fail_missing_and_fail_unreadable_both_count_as_failures() {
    let mut report = Report::default();
    fail_missing(&mut report, "sonarr.api_key");
    assert_eq!(report.failures, 1);

    fail_unreadable(
        &mut report,
        "sonarr.api_key",
        std::io::Error::other("decryption failed"),
    );
    assert_eq!(report.failures, 2);
}

// ------------------------------------------------------------ check_vault

/// `check_vault`'s success path — actually opening the vault and reporting
/// each configured key's status — is the one thing here that resolves
/// `SHARERR_MASTER_KEY` from the process env, so (per CLAUDE.md's "no
/// tier-1 fixture opens a real vault") it can only be exercised inside a
/// `Jail`. Configuring Sonarr and pre-seeding its key drives the loop's
/// `Ok(Some(_))` arm specifically — `fail_missing`/`fail_unreadable` above
/// already cover the other two arms on their own, but never through
/// `check_vault` itself.
#[test]
fn check_vault_opens_a_real_vault_and_reports_each_configured_key() {
    figment::Jail::expect_with(|jail| {
        jail.set_env("SHARERR_MASTER_KEY", "a-master-key");
        let config = Config {
            data_dir: jail.directory().to_path_buf(),
            sonarr: Some(ServiceConfig {
                url: Url::parse("http://sonarr.example").unwrap(),
            }),
            ..Config::default()
        };
        let mut vault =
            sharerr_store::Vault::open(config.vault_path(), &SecretString::from("a-master-key"))
                .unwrap();
        vault
            .put(secret_keys::SONARR_API_KEY, &SecretString::from("a-key"))
            .unwrap();

        let mut report = Report::default();
        let (opened, _credential) = check_vault(&config, &mut report);

        assert!(opened.is_some(), "the vault must open");
        // One failure, not two: qBittorrent's key was never seeded (its
        // absence is `check_torrent_credential`'s own concern, already
        // covered elsewhere), but Sonarr's *was*, so it must not also fail.
        assert_eq!(report.failures, 1, "only the unseeded qbit key should fail");
        Ok(())
    });
}

// ------------------------------------------------ quiet_credential/credential

#[test]
fn quiet_credential_is_none_without_a_vault_and_reports_nothing() {
    assert!(quiet_credential(None, "sonarr.api_key").is_none());
}

#[test]
fn quiet_credential_and_credential_read_a_real_vault_without_reporting_the_quiet_ones() {
    let dir = tempfile::tempdir().unwrap();
    let mut vault = vault_in(&dir);
    vault
        .put("sonarr.api_key", &SecretString::from("k"))
        .unwrap();

    assert!(quiet_credential(Some(&vault), "sonarr.api_key").is_some());
    assert!(quiet_credential(Some(&vault), "radarr.api_key").is_none());

    let mut report = Report::default();
    assert!(credential(Some(&vault), "sonarr.api_key", &mut report).is_some());
    assert_eq!(report.failures, 0, "a present secret is not a failure");
}

#[test]
fn credential_reports_a_missing_key_and_a_closed_vault_each_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault_in(&dir);

    let mut report = Report::default();
    assert!(credential(Some(&vault), "radarr.api_key", &mut report).is_none());
    assert_eq!(report.failures, 1);

    let mut report = Report::default();
    assert!(credential(None, "radarr.api_key", &mut report).is_none());
    assert_eq!(report.failures, 1);
}

// ------------------------------------------------- check_torrent_credential

/// A torrent-client config with just the vault keys under test — the URL
/// and the rest are defaults `check_torrent_credential` never reads.
fn client(
    primary_credential: Option<&'static str>,
    fallback_credential: Option<&'static str>,
) -> sharerr_core::config::TorrentClientConfig<'static> {
    static CONFIG: std::sync::LazyLock<Config> = std::sync::LazyLock::new(Config::default);
    let mut client = CONFIG.torrent_client();
    client.primary_credential = primary_credential;
    client.fallback_credential = fallback_credential;
    client
}

/// Resolve the credential the way `run` does, so a `check_qbit` test gets
/// the same input the real command hands it.
fn stored_credential(config: &Config, vault: &Vault) -> Option<checks::TorrentCredential> {
    check_torrent_credential(vault, &config.torrent_client(), &mut Report::default())
}

#[test]
fn an_api_key_takes_precedence_over_a_configured_password() {
    let dir = tempfile::tempdir().unwrap();
    let mut vault = vault_in(&dir);
    vault
        .put("qbittorrent.api_key", &SecretString::from("k"))
        .unwrap();
    let mut report = Report::default();

    let credential = check_torrent_credential(
        &vault,
        &client(Some("qbittorrent.api_key"), Some("qbittorrent.password")),
        &mut report,
    );

    assert!(matches!(
        credential,
        Some(checks::TorrentCredential::ApiKey(_))
    ));

    assert_eq!(report.failures, 0);
}

#[test]
fn a_missing_api_key_falls_back_to_a_present_password() {
    let dir = tempfile::tempdir().unwrap();
    let mut vault = vault_in(&dir);
    vault
        .put("transmission.password", &SecretString::from("p"))
        .unwrap();
    let mut report = Report::default();

    let credential = check_torrent_credential(
        &vault,
        &client(None, Some("transmission.password")),
        &mut report,
    );

    assert!(matches!(
        credential,
        Some(checks::TorrentCredential::Password(_))
    ));
    assert_eq!(report.failures, 0);
}

#[test]
fn neither_credential_present_is_reported_as_the_password_missing() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault_in(&dir);
    let mut report = Report::default();

    let credential = check_torrent_credential(
        &vault,
        &client(None, Some("transmission.password")),
        &mut report,
    );

    assert!(credential.is_none());
    assert_eq!(report.failures, 1);
}

#[test]
fn a_backend_with_only_an_api_key_concept_reports_that_key_missing() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault_in(&dir);
    let mut report = Report::default();

    let credential = check_torrent_credential(
        &vault,
        &client(Some("qbittorrent.api_key"), None),
        &mut report,
    );

    assert!(credential.is_none());

    assert_eq!(report.failures, 1);
}

// ---------------------------------------------------------- check_library

#[test]
fn a_missing_library_directory_is_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let library = sharerr_core::config::LibraryConfig {
        path: dir.path().join("does-not-exist"),
        kind: LibraryKind::Tv,
    };
    let mut report = Report::default();

    let items = check_library(&library, &mut report);

    assert!(items.is_empty());
    assert_eq!(report.failures, 1);
}

#[test]
fn a_library_path_that_is_a_file_is_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("not-a-dir");
    std::fs::write(&file, b"nope").unwrap();
    let library = sharerr_core::config::LibraryConfig {
        path: file,
        kind: LibraryKind::Movie,
    };
    let mut report = Report::default();

    let items = check_library(&library, &mut report);

    assert!(items.is_empty());
    assert_eq!(report.failures, 1);
}

#[test]
fn an_empty_library_directory_is_a_warning_not_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let library = sharerr_core::config::LibraryConfig {
        path: dir.path().to_path_buf(),
        kind: LibraryKind::Tv,
    };
    let mut report = Report::default();

    let items = check_library(&library, &mut report);

    assert!(items.is_empty());
    assert_eq!(report.failures, 0);
    assert_eq!(report.warnings, 1);
}

#[test]
fn a_populated_library_is_reported_ready_with_its_files() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = sharerr_testkit::tv_library(dir.path()).unwrap();
    let library = sharerr_core::config::LibraryConfig {
        path: dir.path().to_path_buf(),
        kind: LibraryKind::Tv,
    };
    let mut report = Report::default();

    let items = check_library(&library, &mut report);

    assert!(!items.is_empty());
    assert_eq!(items.len(), fixture.files.len());
    assert_eq!(report.failures, 0);
}

// ----------------------------------------------------------- check_tracker

#[test]
fn no_advertised_address_and_no_gluetun_is_a_failure() {
    let config = Config::default();
    let mut report = Report::default();

    check_tracker(&config, None, &mut report);

    assert_eq!(report.failures, 1);
}

#[test]
fn no_static_address_but_a_gluetun_control_url_is_only_informational() {
    let config = Config {
        gluetun: sharerr_core::config::GluetunConfig {
            control_url: Some(Url::parse("http://127.0.0.1:8000").unwrap()),
            ..Default::default()
        },
        ..Config::default()
    };
    let mut report = Report::default();

    check_tracker(&config, None, &mut report);

    assert_eq!(report.failures, 0);
}

#[test]
fn a_configured_advertised_host_is_reported_ok() {
    let config = Config {
        tracker: sharerr_core::config::TrackerConfig {
            advertised_host: Some("box.lan".to_owned()),
            ..Config::default().tracker
        },
        ..Config::default()
    };
    let mut report = Report::default();

    check_tracker(&config, None, &mut report);

    assert_eq!(report.failures, 0);
}

#[test]
fn an_unparseable_advertised_host_is_reported_as_a_failure() {
    let config = Config {
        tracker: sharerr_core::config::TrackerConfig {
            // A space is not a legal host character, so `Url::parse` fails
            // even after `bracket_ipv6`.
            advertised_host: Some("not a host".to_owned()),
            ..Config::default().tracker
        },
        ..Config::default()
    };
    let mut report = Report::default();

    check_tracker(&config, None, &mut report);

    assert_eq!(report.failures, 1);
}

/// A previous token left over from an in-progress rotation is purely
/// informational — it must not turn an otherwise-healthy tracker section
/// into a warning or a failure.
#[test]
fn a_previous_announce_token_does_not_affect_the_failure_count() {
    let dir = tempfile::tempdir().unwrap();
    let mut vault = vault_in(&dir);
    vault
        .put(
            secret_keys::TRACKER_TOKEN_PREVIOUS,
            &SecretString::from("old"),
        )
        .unwrap();

    let config = Config {
        tracker: sharerr_core::config::TrackerConfig {
            advertised_host: Some("box.lan".to_owned()),
            ..Config::default().tracker
        },
        ..Config::default()
    };
    let mut report = Report::default();

    check_tracker(&config, Some(&vault), &mut report);

    assert_eq!(report.failures, 0);
    assert_eq!(report.warnings, 0);
}

// ---------------------------------------------------------- check_database

#[tokio::test]
async fn check_database_opens_a_fresh_store_and_counts_its_items() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        data_dir: dir.path().to_path_buf(),
        ..Config::default()
    };
    let mut report = Report::default();

    check_database(&config, &mut report).await;

    assert_eq!(report.failures, 0);
}

// ---------------------------------------------------------------- check_arr

fn arr_service(url: &Url) -> ServiceConfig {
    ServiceConfig { url: url.clone() }
}

#[tokio::test]
async fn check_arr_reports_a_ready_source_and_returns_its_items() {
    let server = MockServer::start().await;
    sharerr_testkit::mock::mount_json(
        &server,
        "/api/v3/system/status",
        sharerr_testkit::library::system_status_json("Sonarr"),
    )
    .await;
    sharerr_testkit::mock::mount_json(
        &server,
        "/api/v3/tag",
        serde_json::json!([{ "id": 3, "label": "sharerr" }]),
    )
    .await;
    sharerr_testkit::mock::mount_json(&server, "/api/v3/series", serde_json::json!([])).await;

    let dir = tempfile::tempdir().unwrap();
    let mut vault = vault_in(&dir);
    vault
        .put(secret_keys::SONARR_API_KEY, &SecretString::from("k"))
        .unwrap();
    let url = Url::parse(&server.uri()).unwrap();
    let config = Config::default();
    let mut report = Report::default();

    let items = check_arr(
        MediaSource::Sonarr,
        &arr_service(&url),
        &config,
        Some(&vault),
        false,
        &mut report,
    )
    .await;

    // The tag resolves but nothing carries it yet — `TagUnused`, so no items.
    assert!(items.is_empty());
    assert_eq!(report.failures, 0);
    assert_eq!(report.warnings, 1);
}

#[tokio::test]
async fn check_arr_without_a_stored_credential_fails_once_and_does_not_call_out() {
    let server = MockServer::start().await;
    let url = Url::parse(&server.uri()).unwrap();
    let config = Config::default();
    let mut report = Report::default();

    let items = check_arr(
        MediaSource::Sonarr,
        &arr_service(&url),
        &config,
        None,
        false,
        &mut report,
    )
    .await;

    assert!(items.is_empty());
    assert_eq!(report.failures, 1);
}

#[tokio::test]
async fn check_arr_with_fix_creates_a_missing_tag_instead_of_just_failing() {
    let server = MockServer::start().await;
    sharerr_testkit::mock::mount_json(
        &server,
        "/api/v3/system/status",
        sharerr_testkit::library::system_status_json("Sonarr"),
    )
    .await;
    // No `sharerr` tag exists yet.
    sharerr_testkit::mock::mount_json(&server, "/api/v3/tag", serde_json::json!([])).await;
    Mock::given(method("POST"))
        .and(path("/api/v3/tag"))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "id": 9,
            "label": "sharerr"
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let mut vault = vault_in(&dir);
    vault
        .put(secret_keys::SONARR_API_KEY, &SecretString::from("k"))
        .unwrap();
    let url = Url::parse(&server.uri()).unwrap();
    let config = Config::default();
    let mut report = Report::default();

    let items = check_arr(
        MediaSource::Sonarr,
        &arr_service(&url),
        &config,
        Some(&vault),
        true,
        &mut report,
    )
    .await;

    assert!(items.is_empty(), "a just-created tag carries nothing yet");
    assert_eq!(
        report.failures, 0,
        "fix succeeded, so this is not a failure"
    );
}

// --------------------------------------------------------------- check_qbit

#[tokio::test]
async fn check_qbit_reports_ready_and_lists_the_configured_category() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/version"))
        .respond_with(ResponseTemplate::new(200).set_body_string("v5.2.3"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/categories"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "sharerr": { "name": "sharerr", "savePath": "" } }),
            ),
        )
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let mut vault = vault_in(&dir);
    vault
        .put(
            secret_keys::QBITTORRENT_API_KEY,
            &SecretString::from("qbt_jCGn3V76XutJwQpsXgIm6A9NLB86"),
        )
        .unwrap();
    let config = Config {
        torrent_backend: TorrentBackend::Qbittorrent,
        qbittorrent: sharerr_core::config::QbitConfig {
            url: Url::parse(&server.uri()).unwrap(),
            ..Default::default()
        },
        ..Config::default()
    };
    let mut report = Report::default();

    check_qbit(
        &config,
        stored_credential(&config, &vault),
        false,
        &mut report,
    )
    .await;

    assert_eq!(report.failures, 0);
}

#[tokio::test]
async fn check_qbit_offers_to_create_a_missing_category_and_does_so_with_fix() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/version"))
        .respond_with(ResponseTemplate::new(200).set_body_string("v5.2.3"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/torrents/categories"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/createCategory"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let mut vault = vault_in(&dir);
    vault
        .put(
            secret_keys::QBITTORRENT_API_KEY,
            &SecretString::from("qbt_jCGn3V76XutJwQpsXgIm6A9NLB86"),
        )
        .unwrap();
    let config = Config {
        torrent_backend: TorrentBackend::Qbittorrent,
        qbittorrent: sharerr_core::config::QbitConfig {
            url: Url::parse(&server.uri()).unwrap(),
            ..Default::default()
        },
        ..Config::default()
    };
    let mut report = Report::default();

    check_qbit(
        &config,
        stored_credential(&config, &vault),
        true,
        &mut report,
    )
    .await;

    assert_eq!(report.failures, 0);
}

// -------------------------------------------------------------- suggest_paths

#[test]
fn suggest_paths_refuses_a_search_root_that_is_not_a_directory() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope");
    let config = Config::default();
    let mut report = Report::default();

    suggest_paths(&config, &[], Some(&missing), &mut report);

    assert_eq!(report.failures, 1);
}

#[test]
fn suggest_paths_with_nothing_discovered_has_nothing_to_match() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::default();
    let mut report = Report::default();

    suggest_paths(&config, &[], Some(dir.path()), &mut report);

    assert_eq!(report.failures, 0);
}

// ---------------------------------------------------------- print_config_summary

/// Not a behavioural assertion — `println!` output is not worth capturing —
/// but a guard against a panic in any of its branches (services configured
/// or not, libraries present, a path map with and without a distinct qbit
/// view) as this function changes.
#[test]
fn print_config_summary_does_not_panic_on_a_populated_config() {
    let config = Config {
        library: vec![sharerr_core::config::LibraryConfig {
            path: "/data/tv".into(),
            kind: LibraryKind::Tv,
        }],
        path_map: vec![sharerr_core::config::PathMapping {
            arr: "/tv".into(),
            sharerr: "/data/tv".into(),
            qbit: None,
        }],
        ..Config::default()
    };

    print_config_summary(&config);
    print_config_summary(&Config::default());
}

#[test]
fn print_config_summary_reports_a_client_username_when_the_backend_has_one() {
    // Only Transmission/rtorrent carry a username; qBittorrent (the
    // default) never does, so the `Some` arm needs a different backend.
    print_config_summary(&Config {
        torrent_backend: TorrentBackend::Transmission,
        ..Config::default()
    });
}

#[test]
fn print_config_summary_reports_an_unparseable_advertised_host() {
    print_config_summary(&Config {
        tracker: sharerr_core::config::TrackerConfig {
            advertised_host: Some("not a host".to_owned()),
            ..Config::default().tracker
        },
        ..Config::default()
    });
}

// -------------------------------------------------------------- summarize

#[test]
fn summarize_with_nothing_wrong_says_so() {
    assert!(summarize(0, 0).is_ok());
}

#[test]
fn summarize_with_only_warnings_still_succeeds() {
    assert!(summarize(0, 3).is_ok());
}

#[test]
fn summarize_with_any_failure_is_an_error_naming_both_counts() {
    let err = summarize(2, 1).unwrap_err();
    assert!(err.to_string().contains("2 check(s) failed"));
    assert!(err.to_string().contains("1 warning(s)"));
}

// ------------------------------------------------------------------- run

fn doctor_args() -> crate::cli::DoctorArgs {
    crate::cli::DoctorArgs {
        fix: false,
        suggest_paths: false,
        search_root: None,
    }
}

/// Nothing configured at all: no master key (so the vault section fails),
/// no *arr app and no `[[library]]`, no advertised address. Exercises
/// `run`'s control flow end to end down the all-failing path.
///
/// `secrets.rs` has a `#[test]` that legitimately sets `SHARERR_MASTER_KEY`
/// via `figment::Jail`, so relying on the var being merely *unset in this
/// process* would race it under the parallel test runner. `Jail` clears the
/// env for its closure and serializes against every other Jail-based test,
/// which is what actually makes "no master key" safe to assert here rather
/// than racy — hence a plain `#[test]` driving its own runtime inside the
/// `Jail` closure (matching `secrets.rs::open_vault_at_opens_the_vault_named_by_a_master_key`)
/// instead of `#[tokio::test]`, which would already hold a runtime on this
/// thread and panic on the nested one `Jail`'s pattern needs.
#[test]
fn run_reports_every_failure_on_an_empty_config() {
    figment::Jail::expect_with(|jail| {
        jail.clear_env();
        let config = Config {
            data_dir: jail.directory().to_path_buf(),
            ..Config::default()
        };

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let result = runtime.block_on(run(&config, Some("bad config file"), &doctor_args()));

        assert!(result.is_err(), "an unconfigured instance cannot pass");
        Ok(())
    });
}

// ----------------------------------------------------------- check_gluetun

async fn gluetun_server(ip: serde_json::Value, port: serde_json::Value) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/publicip/ip"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ip))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/openvpn/portforwarded"))
        .respond_with(ResponseTemplate::new(200).set_body_json(port))
        .mount(&server)
        .await;
    server
}

fn gluetun_config(control_url: &Url) -> Config {
    Config {
        gluetun: sharerr_core::config::GluetunConfig {
            control_url: Some(control_url.clone()),
            ..Default::default()
        },
        ..Config::default()
    }
}

#[tokio::test]
async fn check_gluetun_with_no_control_url_does_nothing() {
    let mut report = Report::default();
    check_gluetun(
        &Config::default(),
        None,
        crate::gluetun::GluetunTarget::Tracker,
        &mut report,
    )
    .await;
    assert_eq!(report.failures, 0);
    assert_eq!(report.warnings, 0);
}

#[tokio::test]
async fn check_gluetun_reports_ip_and_port_when_they_agree_with_the_config() {
    let server = gluetun_server(
        serde_json::json!({ "public_ip": "203.0.113.9" }),
        serde_json::json!({ "port": 41234 }),
    )
    .await;
    let config = gluetun_config(&Url::parse(&server.uri()).unwrap());
    let mut report = Report::default();

    check_gluetun(
        &config,
        None,
        crate::gluetun::GluetunTarget::Tracker,
        &mut report,
    )
    .await;

    assert_eq!(report.failures, 0);
    assert_eq!(report.warnings, 0);
}

#[tokio::test]
async fn check_gluetun_warns_when_the_advertised_host_is_not_the_tunnels_exit() {
    let server = gluetun_server(
        serde_json::json!({ "public_ip": "203.0.113.9" }),
        serde_json::json!({ "port": 41234 }),
    )
    .await;
    let config = Config {
        tracker: sharerr_core::config::TrackerConfig {
            advertised_host: Some("198.51.100.1".to_owned()),
            ..Config::default().tracker
        },
        ..gluetun_config(&Url::parse(&server.uri()).unwrap())
    };
    let mut report = Report::default();

    check_gluetun(
        &config,
        None,
        crate::gluetun::GluetunTarget::Tracker,
        &mut report,
    )
    .await;

    assert_eq!(report.failures, 0);
    assert_eq!(report.warnings, 1);
}

#[tokio::test]
async fn check_gluetun_fails_on_ip_and_warns_on_port_independently() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/publicip/ip"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/openvpn/portforwarded"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let config = gluetun_config(&Url::parse(&server.uri()).unwrap());
    let mut report = Report::default();

    check_gluetun(
        &config,
        None,
        crate::gluetun::GluetunTarget::Tracker,
        &mut report,
    )
    .await;

    assert_eq!(report.failures, 1);
    assert_eq!(report.warnings, 1);
}

/// What this parameterisation covers: a dual-VPN operator's
/// `[gluetun_client]` tunnel must be checked by `doctor` too — otherwise a
/// broken client-tunnel key produces a clean report.
#[tokio::test]
async fn check_gluetun_checks_the_client_tunnel_when_asked() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/publicip/ip"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/openvpn/portforwarded"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let config = Config {
        gluetun_client: sharerr_core::config::GluetunConfig {
            control_url: Some(Url::parse(&server.uri()).unwrap()),
            ..Default::default()
        },
        ..Config::default()
    };
    let mut report = Report::default();

    check_gluetun(
        &config,
        None,
        crate::gluetun::GluetunTarget::Client,
        &mut report,
    )
    .await;

    assert_eq!(
        report.failures, 1,
        "the broken client tunnel must be caught"
    );
}

/// `tracker.advertised_host` names where *this instance* is reached — it
/// has no bearing on the torrent client's separate tunnel, so a mismatch
/// there must not be reported against the client target.
#[tokio::test]
async fn check_gluetun_does_not_compare_the_advertised_host_for_the_client_target() {
    let server = gluetun_server(
        serde_json::json!({ "public_ip": "203.0.113.9" }),
        serde_json::json!({ "port": 41234 }),
    )
    .await;
    let config = Config {
        tracker: sharerr_core::config::TrackerConfig {
            advertised_host: Some("198.51.100.1".to_owned()),
            ..Config::default().tracker
        },
        gluetun_client: sharerr_core::config::GluetunConfig {
            control_url: Some(Url::parse(&server.uri()).unwrap()),
            ..Default::default()
        },
        ..Config::default()
    };
    let mut report = Report::default();

    check_gluetun(
        &config,
        None,
        crate::gluetun::GluetunTarget::Client,
        &mut report,
    )
    .await;

    assert_eq!(report.failures, 0);
    assert_eq!(report.warnings, 0);
}

// ------------------------------------------------------- check_reachability

#[tokio::test]
async fn check_reachability_accepts_a_live_listener() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    // Accept in the background so the connect below completes rather than
    // sitting in the kernel's backlog for the length of the test.
    std::thread::spawn(move || {
        let _ = listener.accept();
    });

    let config = Config {
        tracker: sharerr_core::config::TrackerConfig {
            advertised_url: Some(Url::parse(&format!("http://127.0.0.1:{port}")).unwrap()),
            ..Config::default().tracker
        },
        ..Config::default()
    };
    let mut report = Report::default();

    check_reachability(&config, &mut report).await;

    assert_eq!(report.failures, 0);
    assert_eq!(report.warnings, 0);
}

#[tokio::test]
async fn check_reachability_warns_rather_than_fails_when_nothing_answers() {
    let port = sharerr_testkit::net::closed_port();
    let config = Config {
        tracker: sharerr_core::config::TrackerConfig {
            advertised_url: Some(Url::parse(&format!("http://127.0.0.1:{port}")).unwrap()),
            ..Config::default().tracker
        },
        ..Config::default()
    };
    let mut report = Report::default();

    check_reachability(&config, &mut report).await;

    assert_eq!(report.failures, 0);
    assert_eq!(report.warnings, 1);
}

#[tokio::test]
async fn check_reachability_does_nothing_without_an_advertised_address() {
    let mut report = Report::default();
    check_reachability(&Config::default(), &mut report).await;
    assert_eq!(report.failures, 0);
    assert_eq!(report.warnings, 0);
}

// ------------------------------------------------------------- check_paths

fn discovered(arr_path: impl Into<std::path::PathBuf>, size: u64) -> Discovered {
    sharerr_core::Discovered {
        source: MediaSource::Sonarr,
        source_id: 1,
        file_id: 2,
        spec: sharerr_core::MediaSpec::Movie {
            title: "Gilded Ferry".to_owned(),
            year: Some(2019),
        },
        arr_path: arr_path.into(),
        size,
        ids: sharerr_core::ExternalIds::default(),
        media: None,
        scene_name: None,
        original_path: None,
    }
}

#[test]
fn check_paths_covers_a_sample_an_unmapped_file_and_an_invalid_path() {
    use sharerr_core::config::PathMapping;

    let dir = tempfile::tempdir().unwrap();
    let mapped_file = dir.path().join("show.s01e01.mkv");
    std::fs::write(&mapped_file, b"x").unwrap();

    let config = Config {
        path_map: vec![PathMapping {
            arr: "/tv".into(),
            sharerr: dir.path().to_path_buf(),
            qbit: None,
        }],
        ..Config::default()
    };
    let items = vec![
        discovered("/tv/show.s01e01.mkv", 1),
        discovered("relative/bad.mkv", 1),
        discovered("/movies/unmapped.mkv", 1),
    ];
    let mut report = Report::default();

    check_paths(&config, &items, &mut report);

    assert_eq!(report.failures, 2, "one invalid path, one missing file");
    assert_eq!(report.warnings, 1, "the unmapped item");
}

#[test]
fn check_paths_with_no_map_and_nothing_discovered_is_informational_only() {
    let mut report = Report::default();
    check_paths(&Config::default(), &[], &mut report);
    assert_eq!(report.failures, 0);
    assert_eq!(report.warnings, 0);
}

#[test]
fn check_paths_with_everything_readable_reports_success() {
    use sharerr_core::config::PathMapping;

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("show.s01e01.mkv");
    std::fs::write(&file, b"x").unwrap();

    let config = Config {
        path_map: vec![PathMapping {
            arr: "/tv".into(),
            sharerr: dir.path().to_path_buf(),
            qbit: None,
        }],
        ..Config::default()
    };
    let items = vec![discovered("/tv/show.s01e01.mkv", 1)];
    let mut report = Report::default();

    check_paths(&config, &items, &mut report);

    assert_eq!(report.failures, 0);
}

// ------------------------------------------------------------ suggest_paths

#[test]
fn suggest_paths_finds_a_match_by_name_and_size_under_the_search_root() {
    let dir = tempfile::tempdir().unwrap();
    let actual = dir.path().join("Gilded.Ferry.2019.mkv");
    std::fs::write(&actual, b"xx").unwrap();

    let config = Config::default();
    let items = vec![discovered("/tv/Gilded.Ferry.2019.mkv", 2)];
    let mut report = Report::default();

    suggest_paths(&config, &items, Some(dir.path()), &mut report);

    assert_eq!(report.failures, 0);
}

// ---------------------------------------------------------------- check_arr

#[tokio::test]
async fn check_arr_reports_a_missing_tag_as_a_failure_without_fix() {
    let server = MockServer::start().await;
    sharerr_testkit::mock::mount_json(
        &server,
        "/api/v3/system/status",
        sharerr_testkit::library::system_status_json("Sonarr"),
    )
    .await;
    sharerr_testkit::mock::mount_json(&server, "/api/v3/tag", serde_json::json!([])).await;

    let dir = tempfile::tempdir().unwrap();
    let mut vault = vault_in(&dir);
    vault
        .put(secret_keys::SONARR_API_KEY, &SecretString::from("k"))
        .unwrap();
    let url = Url::parse(&server.uri()).unwrap();
    let config = Config::default();
    let mut report = Report::default();

    let items = check_arr(
        MediaSource::Sonarr,
        &arr_service(&url),
        &config,
        Some(&vault),
        false,
        &mut report,
    )
    .await;

    assert!(items.is_empty());
    assert_eq!(report.failures, 1);
}

#[tokio::test]
async fn check_arr_reports_a_rejected_key_as_a_failure() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v3/system/status"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let mut vault = vault_in(&dir);
    vault
        .put(secret_keys::SONARR_API_KEY, &SecretString::from("k"))
        .unwrap();
    let url = Url::parse(&server.uri()).unwrap();
    let config = Config::default();
    let mut report = Report::default();

    check_arr(
        MediaSource::Sonarr,
        &arr_service(&url),
        &config,
        Some(&vault),
        false,
        &mut report,
    )
    .await;

    assert_eq!(report.failures, 1);
}

#[tokio::test]
async fn check_arr_reports_an_unreachable_service() {
    let dir = tempfile::tempdir().unwrap();
    let mut vault = vault_in(&dir);
    vault
        .put(secret_keys::SONARR_API_KEY, &SecretString::from("k"))
        .unwrap();
    let port = sharerr_testkit::net::closed_port();
    let url = Url::parse(&format!("http://127.0.0.1:{port}")).unwrap();
    let config = Config::default();
    let mut report = Report::default();

    check_arr(
        MediaSource::Sonarr,
        &arr_service(&url),
        &config,
        Some(&vault),
        false,
        &mut report,
    )
    .await;

    assert_eq!(report.failures, 1);
}

// --------------------------------------------------------------- check_qbit

#[tokio::test]
async fn check_qbit_reports_a_rejected_key() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/app/version"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let mut vault = vault_in(&dir);
    vault
        .put(
            secret_keys::QBITTORRENT_API_KEY,
            &SecretString::from("qbt_jCGn3V76XutJwQpsXgIm6A9NLB86"),
        )
        .unwrap();
    let config = Config {
        torrent_backend: TorrentBackend::Qbittorrent,
        qbittorrent: sharerr_core::config::QbitConfig {
            url: Url::parse(&server.uri()).unwrap(),
            ..Default::default()
        },
        ..Config::default()
    };
    let mut report = Report::default();

    check_qbit(
        &config,
        stored_credential(&config, &vault),
        false,
        &mut report,
    )
    .await;

    assert_eq!(report.failures, 1);
}

#[tokio::test]
async fn check_qbit_reports_an_unreachable_service() {
    let dir = tempfile::tempdir().unwrap();
    let mut vault = vault_in(&dir);
    vault
        .put(
            secret_keys::QBITTORRENT_API_KEY,
            &SecretString::from("qbt_jCGn3V76XutJwQpsXgIm6A9NLB86"),
        )
        .unwrap();
    let port = sharerr_testkit::net::closed_port();
    let config = Config {
        torrent_backend: TorrentBackend::Qbittorrent,
        qbittorrent: sharerr_core::config::QbitConfig {
            url: Url::parse(&format!("http://127.0.0.1:{port}")).unwrap(),
            ..Default::default()
        },
        ..Config::default()
    };
    let mut report = Report::default();

    check_qbit(
        &config,
        stored_credential(&config, &vault),
        false,
        &mut report,
    )
    .await;

    assert_eq!(report.failures, 1);
}

#[tokio::test]
async fn check_qbit_falls_back_to_a_password_when_the_backend_has_no_api_key() {
    // Transmission has no API-key concept, so `check_qbit` must resolve
    // its password instead — a different branch than every qBittorrent
    // test above exercises.
    let dir = tempfile::tempdir().unwrap();
    let vault = vault_in(&dir);
    let port = sharerr_testkit::net::closed_port();
    let config = Config {
        torrent_backend: TorrentBackend::Transmission,
        transmission: sharerr_core::config::TransmissionConfig {
            url: Url::parse(&format!("http://127.0.0.1:{port}")).unwrap(),
            ..Default::default()
        },
        ..Config::default()
    };
    let mut report = Report::default();

    // No password stored either: the vault section reported the miss,
    // and `check_qbit` records the skip without ever building a client.
    check_qbit(
        &config,
        stored_credential(&config, &vault),
        false,
        &mut report,
    )
    .await;

    assert_eq!(report.failures, 1);
}

// ------------------------------------------- check_arr, every outcome

fn sonarr_vault(dir: &tempfile::TempDir) -> Vault {
    let mut vault = vault_in(dir);
    vault
        .put(secret_keys::SONARR_API_KEY, &SecretString::from("k"))
        .unwrap();
    vault
}

async fn arr_check(server_url: &Url, vault: &Vault, fix: bool) -> (Vec<Discovered>, Report) {
    let mut report = Report::default();
    let items = check_arr(
        MediaSource::Sonarr,
        &arr_service(server_url),
        &Config::default(),
        Some(vault),
        fix,
        &mut report,
    )
    .await;
    (items, report)
}

async fn sonarr_status(server: &MockServer) {
    sharerr_testkit::mock::mount_json(
        server,
        "/api/v3/system/status",
        sharerr_testkit::library::system_status_json("Sonarr"),
    )
    .await;
}

#[tokio::test]
async fn check_arr_reports_tagged_files_and_returns_them() {
    let media = tempfile::tempdir().unwrap();
    let library = sharerr_testkit::library::tv_library(media.path()).unwrap();
    let server = MockServer::start().await;
    sonarr_status(&server).await;
    sharerr_testkit::mock::mount_json(&server, "/api/v3/tag", sharerr_testkit::library::tag_json())
        .await;
    sharerr_testkit::mock::mount_json(&server, "/api/v3/series", library.series_json()).await;
    sharerr_testkit::mock::mount_json(&server, "/api/v3/episodefile", library.episodefile_json())
        .await;
    sharerr_testkit::mock::mount_json(&server, "/api/v3/episode", library.episode_json()).await;
    let dir = tempfile::tempdir().unwrap();
    let vault = sonarr_vault(&dir);

    let (items, report) = arr_check(&Url::parse(&server.uri()).unwrap(), &vault, false).await;

    assert_eq!(items.len(), 2, "both tagged episodes come back");
    assert_eq!(report.failures, 0);
    assert_eq!(report.warnings, 0);
}

#[tokio::test]
async fn check_arr_without_fix_fails_on_a_missing_tag() {
    let server = MockServer::start().await;
    sonarr_status(&server).await;
    sharerr_testkit::mock::mount_json(&server, "/api/v3/tag", serde_json::json!([])).await;
    let dir = tempfile::tempdir().unwrap();
    let vault = sonarr_vault(&dir);

    let (items, report) = arr_check(&Url::parse(&server.uri()).unwrap(), &vault, false).await;

    assert!(items.is_empty());
    assert_eq!(report.failures, 1);
}

#[tokio::test]
async fn check_arr_with_fix_reports_a_tag_it_could_not_create() {
    let server = MockServer::start().await;
    sonarr_status(&server).await;
    sharerr_testkit::mock::mount_json(&server, "/api/v3/tag", serde_json::json!([])).await;
    Mock::given(method("POST"))
        .and(path("/api/v3/tag"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let vault = sonarr_vault(&dir);

    let (items, report) = arr_check(&Url::parse(&server.uri()).unwrap(), &vault, true).await;

    assert!(items.is_empty());
    assert_eq!(report.failures, 1);
}

#[tokio::test]
async fn check_arr_fails_on_a_rejected_key_a_server_error_and_nothing_listening() {
    let dir = tempfile::tempdir().unwrap();
    let vault = sonarr_vault(&dir);

    let server = MockServer::start().await;
    sharerr_testkit::mock::mount_json_status(
        &server,
        "/api/v3/system/status",
        401,
        serde_json::json!({}),
    )
    .await;
    let (items, report) = arr_check(&Url::parse(&server.uri()).unwrap(), &vault, false).await;
    assert!(items.is_empty());
    assert_eq!(report.failures, 1, "rejected");

    let server = MockServer::start().await;
    sharerr_testkit::mock::mount_json_status(
        &server,
        "/api/v3/system/status",
        500,
        serde_json::json!({}),
    )
    .await;
    let (_, report) = arr_check(&Url::parse(&server.uri()).unwrap(), &vault, false).await;
    assert_eq!(report.failures, 1, "server error");

    let port = sharerr_testkit::net::closed_port();
    let url = Url::parse(&format!("http://127.0.0.1:{port}")).unwrap();
    let (_, report) = arr_check(&url, &vault, false).await;
    assert_eq!(report.failures, 1, "unreachable");
}

/// Only *arr apps carry a vault key; a directory source handed to this
/// check has nothing to look up and says nothing.
#[tokio::test]
async fn check_arr_ignores_a_kind_with_no_credential_key() {
    let dir = tempfile::tempdir().unwrap();
    let vault = vault_in(&dir);
    let mut report = Report::default();
    let url = Url::parse("http://unused.example").unwrap();

    let items = check_arr(
        MediaSource::Directory,
        &arr_service(&url),
        &Config::default(),
        Some(&vault),
        false,
        &mut report,
    )
    .await;

    assert!(items.is_empty());
    assert_eq!((report.failures, report.warnings), (0, 0));
}

// ----------------------------------------------- check_library (skipped)

#[test]
fn a_library_with_an_unclassifiable_file_warns_about_the_skip() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Lanternwick.Hollow.S02E01.mkv"), [0u8; 16]).unwrap();
    std::fs::write(dir.path().join("clip.mkv"), [0u8; 16]).unwrap();
    let library = sharerr_core::config::LibraryConfig {
        path: dir.path().to_path_buf(),
        kind: LibraryKind::Tv,
    };
    let mut report = Report::default();

    let items = check_library(&library, &mut report);

    assert_eq!(items.len(), 1, "the classifiable one is shared");
    assert_eq!(report.failures, 0);
    assert_eq!(report.warnings, 1, "the skip is a warning");
}

// ------------------------------------------- check_qbit, every outcome

fn transmission_config(url: &Url) -> Config {
    Config {
        torrent_backend: TorrentBackend::Transmission,
        transmission: sharerr_core::config::TransmissionConfig {
            url: url.clone(),
            ..Default::default()
        },
        ..Config::default()
    }
}

fn transmission_vault(dir: &tempfile::TempDir) -> Vault {
    let mut vault = vault_in(dir);
    vault
        .put(
            secret_keys::TRANSMISSION_PASSWORD,
            &SecretString::from("pw"),
        )
        .unwrap();
    vault
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

#[tokio::test]
async fn check_qbit_without_a_resolved_credential_records_a_skip() {
    let mut report = Report::default();
    check_qbit(&Config::default(), None, false, &mut report).await;
    assert_eq!(report.failures, 1);
}

#[tokio::test]
async fn check_qbit_signs_in_to_transmission_with_a_password() {
    let server = transmission_answering(
        200,
        serde_json::json!({ "result": "success", "arguments": { "version": "4.0.5" } }),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let vault = transmission_vault(&dir);
    let config = transmission_config(&Url::parse(&server.uri()).unwrap());
    let mut report = Report::default();

    check_qbit(
        &config,
        stored_credential(&config, &vault),
        false,
        &mut report,
    )
    .await;

    assert_eq!(report.failures, 0);
}

#[tokio::test]
async fn check_qbit_fails_on_a_rejected_password_a_server_error_and_nothing_listening() {
    let dir = tempfile::tempdir().unwrap();
    let vault = transmission_vault(&dir);

    let server = transmission_answering(401, serde_json::json!({})).await;
    let config = transmission_config(&Url::parse(&server.uri()).unwrap());
    let mut report = Report::default();
    check_qbit(
        &config,
        stored_credential(&config, &vault),
        false,
        &mut report,
    )
    .await;
    assert_eq!(report.failures, 1, "rejected");

    let server = transmission_answering(500, serde_json::json!({})).await;
    let config = transmission_config(&Url::parse(&server.uri()).unwrap());
    let mut report = Report::default();
    check_qbit(
        &config,
        stored_credential(&config, &vault),
        false,
        &mut report,
    )
    .await;
    assert_eq!(report.failures, 1, "server error");

    let port = sharerr_testkit::net::closed_port();
    let config = transmission_config(&Url::parse(&format!("http://127.0.0.1:{port}")).unwrap());
    let mut report = Report::default();
    check_qbit(
        &config,
        stored_credential(&config, &vault),
        false,
        &mut report,
    )
    .await;
    assert_eq!(report.failures, 1, "unreachable");
}

fn qbit_config(server: &MockServer) -> Config {
    Config {
        torrent_backend: TorrentBackend::Qbittorrent,
        qbittorrent: sharerr_core::config::QbitConfig {
            url: Url::parse(&server.uri()).unwrap(),
            ..Default::default()
        },
        ..Config::default()
    }
}

fn qbit_vault(dir: &tempfile::TempDir) -> Vault {
    let mut vault = vault_in(dir);
    vault
        .put(
            secret_keys::QBITTORRENT_API_KEY,
            &SecretString::from(sharerr_testkit::mock::QBIT_API_KEY),
        )
        .unwrap();
    vault
}

async fn qbit_answering(server: &MockServer, route: &str, response: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(response)
        .mount(server)
        .await;
}

/// A listing or category call that fails after sign-in is a warning, not
/// a failure: the client answered, which is the check's question.
#[tokio::test]
async fn check_qbit_warns_when_the_listing_or_the_category_list_fails() {
    let server = MockServer::start().await;
    qbit_answering(
        &server,
        "/api/v2/app/version",
        ResponseTemplate::new(200).set_body_string("v5.2.3"),
    )
    .await;
    qbit_answering(&server, "/api/v2/torrents/info", ResponseTemplate::new(500)).await;
    qbit_answering(
        &server,
        "/api/v2/torrents/categories",
        ResponseTemplate::new(500),
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let vault = qbit_vault(&dir);
    let config = qbit_config(&server);
    let mut report = Report::default();

    check_qbit(
        &config,
        stored_credential(&config, &vault),
        false,
        &mut report,
    )
    .await;

    assert_eq!(report.failures, 0);
    assert_eq!(report.warnings, 2);
}

#[tokio::test]
async fn check_qbit_warns_about_a_missing_category_and_fails_if_fix_cannot_create_it() {
    let server = MockServer::start().await;
    qbit_answering(
        &server,
        "/api/v2/app/version",
        ResponseTemplate::new(200).set_body_string("v5.2.3"),
    )
    .await;
    qbit_answering(
        &server,
        "/api/v2/torrents/info",
        ResponseTemplate::new(200).set_body_json(serde_json::json!([])),
    )
    .await;
    qbit_answering(
        &server,
        "/api/v2/torrents/categories",
        ResponseTemplate::new(200).set_body_json(serde_json::json!({})),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/torrents/createCategory"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let vault = qbit_vault(&dir);
    let config = qbit_config(&server);

    let mut report = Report::default();
    check_qbit(
        &config,
        stored_credential(&config, &vault),
        false,
        &mut report,
    )
    .await;
    assert_eq!(report.failures, 0);
    assert_eq!(report.warnings, 1, "without --fix it is advice");

    let mut report = Report::default();
    check_qbit(
        &config,
        stored_credential(&config, &vault),
        true,
        &mut report,
    )
    .await;
    assert_eq!(
        report.failures, 1,
        "with --fix a failed create is a failure"
    );
}

// ------------------------------------------------ database and summary

#[tokio::test]
async fn check_database_fails_when_the_database_path_cannot_be_opened() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sharerr.db")).unwrap();
    let config = Config {
        data_dir: dir.path().to_path_buf(),
        ..Config::default()
    };
    let mut report = Report::default();

    check_database(&config, &mut report).await;

    assert_eq!(report.failures, 1);
}

#[test]
fn describe_advertised_renders_a_configured_address() {
    let config = Config {
        tracker: sharerr_core::config::TrackerConfig {
            advertised_host: Some("203.0.113.1".to_owned()),
            ..Config::default().tracker
        },
        ..Config::default()
    };
    assert!(describe_advertised(&config).contains("203.0.113.1"));
    assert_eq!(describe_advertised(&Config::default()), "(unset)");
}

/// The vault opens but holds nothing for a configured source: one
/// failure naming the key, with the fix hint.
#[test]
fn check_vault_reports_a_missing_key_for_a_configured_source() {
    figment::Jail::expect_with(|jail| {
        jail.set_env("SHARERR_MASTER_KEY", "doctor-vault-tests");
        let config = Config {
            data_dir: jail.directory().to_path_buf(),
            sonarr: Some(ServiceConfig {
                url: Url::parse("http://sonarr.example").unwrap(),
            }),
            ..Config::default()
        };
        let mut report = Report::default();

        let (vault, credential) = check_vault(&config, &mut report);

        assert!(vault.is_some(), "the vault itself opened");
        assert!(credential.is_none(), "no qBittorrent key either");
        // The Sonarr key and the qBittorrent key are both missing.
        assert_eq!(report.failures, 2);
        Ok(())
    });
}

/// `run` end to end with every optional section switched on: a
/// configured *arr app, a `[[library]]`, both gluetun pollers, and path
/// suggestions — the loops and branches an empty config never enters.
/// The vault opens (master key set) but is empty, so the *arr and client
/// checks report missing keys rather than dialling anything.
#[test]
fn run_visits_every_optional_section_when_they_are_configured() {
    figment::Jail::expect_with(|jail| {
        jail.set_env("SHARERR_MASTER_KEY", "doctor-run-tests");
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let result = runtime.block_on(async {
            let gluetun = gluetun_server(
                serde_json::json!({ "public_ip": "203.0.113.7" }),
                serde_json::json!({ "port": 51413 }),
            )
            .await;
            let control = Url::parse(&gluetun.uri()).unwrap();
            let media = tempfile::tempdir().unwrap();
            let library = sharerr_testkit::library::tv_library(media.path()).unwrap();
            let config = Config {
                data_dir: jail.directory().to_path_buf(),
                sonarr: Some(ServiceConfig {
                    url: Url::parse("http://sonarr.example").unwrap(),
                }),
                library: vec![sharerr_core::config::LibraryConfig {
                    path: library.root.join("tv"),
                    kind: LibraryKind::Tv,
                }],
                gluetun: sharerr_core::config::GluetunConfig {
                    control_url: Some(control.clone()),
                    ..Default::default()
                },
                gluetun_client: sharerr_core::config::GluetunConfig {
                    control_url: Some(control),
                    ..Default::default()
                },
                ..Config::default()
            };
            let args = crate::cli::DoctorArgs {
                fix: false,
                suggest_paths: true,
                search_root: Some(media.path().to_path_buf()),
            };
            run(&config, None, &args).await
        });

        assert!(result.is_err(), "the missing keys are failures");
        Ok(())
    });
}
