#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use std::path::Path;

fn doc(text: &str) -> ConfigFile {
    ConfigFile {
        path: PathBuf::from("sharerr.toml"),
        doc: text.parse().expect("valid toml"),
        recovered: false,
    }
}

#[test]
fn editing_a_value_leaves_comments_and_neighbours_alone() {
    let mut file = doc(r#"
# Why this instance exists.
tag = "sharerr"

[qbittorrent]
# Whatever the container is reachable as.
url = "http://localhost:8080"
username = "admin"
"#);

    file.apply([Edit::str("qbittorrent.url", "http://qbit:9090")]);
    let out = file.to_toml();

    assert!(out.contains(r#"url = "http://qbit:9090""#));
    assert!(
        out.contains("# Why this instance exists."),
        "comments must survive a save:\n{out}"
    );
    assert!(
        out.contains("# Whatever the container is reachable as."),
        "{out}"
    );
    assert!(out.contains(r#"username = "admin""#), "{out}");
}

#[test]
fn a_missing_table_is_created_as_a_header() {
    let mut file = doc("tag = \"sharerr\"\n");
    file.apply([Edit::str("sonarr.url", "http://sonarr:8989")]);

    let out = file.to_toml();
    assert!(
        out.contains("[sonarr]"),
        "expected a real table header:\n{out}"
    );
    assert!(out.contains(r#"url = "http://sonarr:8989""#), "{out}");
}

#[test]
fn unset_removes_the_key_rather_than_blanking_it() {
    let mut file = doc("[sonarr]\nurl = \"http://sonarr:8989\"\n");
    file.apply([Edit::unset("sonarr.url")]);

    let out = file.to_toml();
    assert!(!out.contains("url ="), "{out}");
}

#[test]
fn a_cleared_text_input_unsets_instead_of_writing_an_empty_string() {
    // `url = ""` fails to parse as a Url, which would turn a user clearing a
    // field into a container that will not start.
    let mut file = doc("[tracker]\nadvertised_host = \"seed.example\"\n");
    file.apply([Edit::str_or_unset("tracker.advertised_host", "   ")]);

    assert!(
        !file.to_toml().contains("advertised_host"),
        "{}",
        file.to_toml()
    );
}

#[test]
fn every_scalar_kind_round_trips_through_validation() {
    let mut file = doc("");
    file.apply([
        Edit::str("tag", "shared"),
        Edit::bool("sync.enabled", false),
        Edit::int("sync.interval_secs", 1800),
        Edit::int("tracker.port", 19000),
    ]);

    let config = crate::settings::validate(&file.to_toml()).expect("valid");
    assert_eq!(config.tag, "shared");
    assert!(!config.sync.enabled);
    assert_eq!(config.sync.interval_secs, 1800);
    assert_eq!(config.tracker.port, Some(19000));
}

#[test]
fn a_str_list_writes_a_toml_array_and_survives_validation() {
    let mut file = doc("");
    file.apply([Edit::str_list(
        "lighthouse.urls",
        vec![
            "https://lighthouse.example.com".to_owned(),
            "https://second.example.com".to_owned(),
        ],
    )]);

    let out = file.to_toml();
    assert!(out.contains("https://lighthouse.example.com"), "{out}");
    assert!(out.contains("https://second.example.com"), "{out}");

    let config = crate::settings::validate(&out).expect("valid");
    assert_eq!(config.lighthouse.urls.len(), 2);
}

#[test]
fn a_float_writes_a_toml_float_and_survives_validation() {
    let mut file = doc("");
    file.apply([Edit::float("seeding.ratio_limit", 2.5)]);

    let out = file.to_toml();
    assert!(out.contains("2.5"), "{out}");

    let config = crate::settings::validate(&out).expect("valid");
    assert_eq!(config.seeding.ratio_limit, Some(2.5));
}

#[test]
fn path_map_is_replaced_wholesale_and_an_empty_list_removes_it() {
    let mut file = doc("[[path_map]]\narr = \"/old\"\nsharerr = \"/stale\"\n");

    let mappings = parse_path_map(&[
        (
            "/tv".to_owned(),
            "/media/tv".to_owned(),
            "/downloads/tv".to_owned(),
        ),
        (
            "/movies".to_owned(),
            "/media/movies".to_owned(),
            String::new(),
        ),
    ])
    .unwrap();
    file.set_path_map(&mappings);

    let config = crate::settings::validate(&file.to_toml()).expect("valid");
    assert_eq!(config.path_map.len(), 2);
    assert_eq!(
        config.path_map[0].qbit.as_deref(),
        Some(Path::new("/downloads/tv"))
    );
    assert_eq!(config.path_map[1].qbit, None, "a blank qbit stays absent");
    assert!(
        !file.to_toml().contains("/old"),
        "stale rows must not survive"
    );

    file.set_path_map(&[]);
    assert!(!file.to_toml().contains("path_map"));
}

/// `[[library]]` behaves exactly like `[[path_map]]`: replaced wholesale,
/// removed when empty, blank rows dropped, and a bad kind refused.
#[test]
fn library_rows_round_trip_and_reject_a_bad_kind() {
    let mut file = doc("[[library]]\npath = \"/stale\"\nkind = \"movie\"\n");

    let libraries = parse_libraries(&[
        ("/media/extras".to_owned(), "movie".to_owned()),
        ("/media/tapes".to_owned(), "tv".to_owned()),
        (String::new(), "movie".to_owned()),
    ])
    .unwrap();
    file.set_libraries(&libraries);

    let config = crate::settings::validate(&file.to_toml()).expect("valid");
    assert_eq!(config.library.len(), 2);
    assert_eq!(config.library[0].kind, LibraryKind::Movie);
    assert_eq!(config.library[1].path, Path::new("/media/tapes"));
    assert!(!file.to_toml().contains("/stale"), "stale rows must go");

    file.set_libraries(&[]);
    assert!(!file.to_toml().contains("library"));

    let err = parse_libraries(&[("/media/x".to_owned(), "anime".to_owned())])
        .expect_err("an unknown kind must be refused");
    assert!(format!("{err:#}").contains("library 1"), "{err:#}");
}

#[test]
fn a_blank_path_map_row_is_dropped_but_a_half_filled_one_is_an_error() {
    let dropped = parse_path_map(&[
        ("/tv".to_owned(), "/media/tv".to_owned(), String::new()),
        (String::new(), "  ".to_owned(), String::new()),
    ])
    .unwrap();
    assert_eq!(dropped.len(), 1);

    let err = parse_path_map(&[("/tv".to_owned(), String::new(), String::new())])
        .expect_err("a half-filled row is a mistake, not a deletion");
    assert!(format!("{err:#}").contains("path mapping 1"), "{err:#}");
}

#[test]
fn save_refuses_a_document_that_would_not_start() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sharerr.toml");
    std::fs::write(&path, "tag = \"original\"\n").unwrap();

    let mut file = ConfigFile::open(&path).unwrap();
    // `deny_unknown_fields` rejects this, and a bare TOML parse would not.
    file.doc["taag"] = value("typo");

    let err = file
        .save()
        .expect_err("an invalid document must not reach disk");
    assert!(format!("{err:#}").contains("taag"), "{err:#}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "tag = \"original\"\n",
        "the previous file must be untouched"
    );
    assert!(
        !path.with_extension("toml.tmp").exists(),
        "a rejected save should not leave a temp file"
    );
}

/// The inline `..` guard — see `write_validated`'s comment for why it
/// exists at all, and why it has the shape it has.
#[test]
fn open_refuses_a_path_containing_dot_dot() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("..").join("sharerr.toml");

    let err = ConfigFile::open(&path).expect_err("a `..` component must be refused");
    assert!(format!("{err:#}").contains(".."), "{err:#}");
}

/// The same guard, reached through `backup_path` — the one caller
/// (`web/settings.rs`) invokes it on a `replacing` document before
/// `write_validated` ever runs, so this needs its own guard rather than
/// inheriting one. Without it, a `..` path that happens to resolve to a
/// file that exists would report a backup `write_validated` would then
/// refuse to make.
#[test]
fn backup_path_refuses_a_path_containing_dot_dot_even_when_it_resolves_to_a_real_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("sharerr.toml"), "tag = \"x\"\n").unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let path = sub.join("..").join("sharerr.toml");
    assert!(path.exists(), "the `..` path must resolve to a real file");

    assert_eq!(ConfigFile::replacing(&path).backup_path(), None);
}

/// The same guard, reached through `write_validated` rather than `open` —
/// the path `replacing` never checks up front.
#[test]
fn write_validated_refuses_a_path_containing_dot_dot() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("..").join("sharerr.toml");

    let file = ConfigFile::replacing(&path);
    let err = file
        .write_validated("tag = \"x\"\n")
        .expect_err("a `..` component must be refused");
    assert!(format!("{err:#}").contains(".."), "{err:#}");
}

/// The guard checks the path as a `str`, so a path that is not UTF-8 is
/// refused rather than lossily re-encoded into a different filename.
#[cfg(unix)]
#[test]
fn open_refuses_a_path_that_is_not_utf8() {
    use std::os::unix::ffi::OsStrExt;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(std::ffi::OsStr::from_bytes(b"\xff.toml"));

    let err = ConfigFile::open(&path).expect_err("a non-UTF-8 path must be refused");
    assert!(format!("{err:#}").contains("UTF-8"), "{err:#}");
}

#[test]
fn an_absent_file_is_created_rather_than_erroring() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join("sharerr.toml");

    let mut file = ConfigFile::open(&path).unwrap();
    file.apply([Edit::str("tag", "fresh")]);
    let config = file.save().unwrap();

    assert_eq!(config.tag, "fresh");
    assert!(std::fs::read_to_string(&path).unwrap().contains("fresh"));
}

/// A file that parses has nothing to recover from, and must not be treated as
/// disposable — the whole point of `toml_edit` is that its comments survive.
#[test]
fn a_parseable_file_is_edited_in_place_and_never_moved_aside() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sharerr.toml");
    std::fs::write(&path, "# keep me\ntag = \"first\"\n").unwrap();

    let mut file = ConfigFile::open(&path).unwrap();
    assert_eq!(file.backup_path(), None);

    file.apply([Edit::str("tag", "second")]);
    file.save().unwrap();

    let written = std::fs::read_to_string(&path).unwrap();
    assert!(written.contains("# keep me"), "comments must survive");
    assert!(!path.with_extension("toml.invalid").exists());
}

/// The case editing cannot reach. A typo'd key is still a typo'd key after any
/// number of corrected sections are saved beside it, so `validate` rejects
/// every one — the operator would be stuck behind a page offering to help.
#[test]
fn replacing_writes_over_a_file_that_editing_could_never_repair() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sharerr.toml");
    let original = "taag = \"typo\"\n";
    std::fs::write(&path, original).unwrap();

    // Editing in place: the offending key travels with the document.
    let mut edited = ConfigFile::open(&path).unwrap();
    edited.apply([Edit::str("tag", "repaired")]);
    assert!(edited.save().is_err(), "editing cannot get past the typo");

    let mut file = ConfigFile::replacing(&path);
    file.apply([Edit::str("tag", "repaired")]);
    let config = file.save().unwrap();

    assert_eq!(config.tag, "repaired");
    assert!(std::fs::read_to_string(&path).unwrap().contains("repaired"));
    assert_eq!(
        std::fs::read_to_string(path.with_extension("toml.invalid")).unwrap(),
        original,
        "the only copy of what the operator wrote must be kept"
    );
}

/// Unparseable TOML takes the same route — `open` refuses it outright.
#[test]
fn an_unparseable_file_is_refused_by_open_and_handled_by_replacing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sharerr.toml");
    let original = "this is not toml at all {{{\n";
    std::fs::write(&path, original).unwrap();

    assert!(ConfigFile::open(&path).is_err());

    let mut file = ConfigFile::replacing(&path);
    file.apply([Edit::str("tag", "repaired")]);
    file.save().unwrap();

    assert!(std::fs::read_to_string(&path).unwrap().contains("repaired"));
    assert_eq!(
        std::fs::read_to_string(path.with_extension("toml.invalid")).unwrap(),
        original
    );
}

/// Replacing still validates. Writing a document that `Config` rejects would
/// swap one unloadable file for another, having already destroyed the first.
#[test]
fn a_replacement_that_would_not_load_leaves_the_original_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sharerr.toml");
    let original = "not toml {{{\n";
    std::fs::write(&path, original).unwrap();

    let mut file = ConfigFile::replacing(&path);
    file.apply([Edit::str("taag", "typo")]);

    assert!(file.save().is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    assert!(!path.with_extension("toml.invalid").exists());
}

/// There is nothing to keep when there was no file, and offering to preserve
/// one would be a lie on the page that says so.
#[test]
fn replacing_an_absent_file_has_no_backup_to_name() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sharerr.toml");

    assert_eq!(ConfigFile::replacing(&path).backup_path(), None);
}

#[test]
fn a_save_round_trips_through_reopening() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sharerr.toml");
    std::fs::write(&path, "# keep me\ntag = \"first\"\n").unwrap();

    let mut file = ConfigFile::open(&path).unwrap();
    file.apply([Edit::str("tag", "second")]);
    file.save().unwrap();

    let reopened = ConfigFile::open(&path).unwrap();
    assert!(reopened.to_toml().contains("# keep me"));
    assert!(reopened.to_toml().contains(r#"tag = "second""#));
}

/// The two transforms are inverses over every writable path, which is what
/// lets `load_or_recover` derive `SHARERR_DATA_DIR` from the same constant
/// the UI writes through — a renamed field breaks this test instead of
/// silently breaking recovery.
#[test]
fn env_var_names_round_trip_through_the_override_scan() {
    use sharerr_core::config::config_paths;

    let vars = config_paths::ALL
        .iter()
        .map(|path| (config_paths::env_var(path), "x".to_owned()));
    let found = collect_overrides(vars);

    for path in config_paths::ALL {
        assert_eq!(
            found.get(*path).map(String::as_str),
            Some(config_paths::env_var(path).as_str()),
            "{path:?} did not survive the env round-trip"
        );
    }
}

#[test]
fn env_overrides_map_variables_back_to_config_paths() {
    let vars = [
        ("SHARERR_QBITTORRENT__URL", "http://pinned:8080"),
        ("SHARERR_TAG", "pinned"),
        // Lowercase still configures the instance, so it must still be detected.
        ("sharerr_tracker__advertised_host", "seed.example"),
        // Not config fields — these must not appear as locked settings.
        ("SHARERR_CONFIG", "/config/sharerr.toml"),
        ("SHARERR_MASTER_KEY", "hunter2"),
        ("PATH", "/usr/bin"),
    ]
    .map(|(k, v)| (k.to_owned(), v.to_owned()));

    let found = collect_overrides(vars.into_iter());

    assert_eq!(
        found.get("qbittorrent.url").map(String::as_str),
        Some("SHARERR_QBITTORRENT__URL")
    );
    assert_eq!(found.get("tag").map(String::as_str), Some("SHARERR_TAG"));
    assert_eq!(
        found.get("tracker.advertised_host").map(String::as_str),
        Some("sharerr_tracker__advertised_host"),
        "the name is reported as the operator actually spelled it"
    );
    assert!(
        !found.contains_key("config"),
        "SHARERR_CONFIG is not a setting"
    );
    assert!(!found.contains_key("master_key"));
    assert_eq!(found.len(), 3);
}

#[test]
fn the_double_underscore_convention_survives_a_round_trip() {
    // The inverse of figment's `.split("__")`. Spelled out by hand rather than
    // generated, so this would still catch a change to the mapping rule.
    let vars = [("SHARERR_SYNC__INTERVAL_SECS".to_owned(), "600".to_owned())];
    assert!(collect_overrides(vars.into_iter()).contains_key("sync.interval_secs"));
}
