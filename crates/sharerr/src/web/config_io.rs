//! Read-modify-write of `sharerr.toml` on behalf of the web UI.
//!
//! Two things make this more than `toml::to_string(&config)`.
//!
//! **Comments survive.** `docker/config/sharerr.toml` is mostly prose explaining
//! why path mapping exists and what each field costs. A serde round-trip through
//! `Config` would silently delete all of it the first time anyone pressed Save, so
//! edits are applied to a parsed [`DocumentMut`] and everything untouched — key
//! order, comments, spacing — stays exactly as the operator left it.
//!
//! **Nothing reaches disk unvalidated.** `Config` is `deny_unknown_fields`, and a
//! document that fails `extract` is a startup failure. Since the UI is how the
//! operator would fix such a failure, writing one would lock them out of their own
//! instance. Every save is parsed back through [`crate::settings::validate`] first.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
#[cfg(test)]
use sharerr_core::Config;
use sharerr_core::config::{LibraryConfig, LibraryKind, PathMapping};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

/// One field the UI is allowed to write, addressed by its dotted config path.
///
/// `&'static str` rather than a runtime string: every writable field is known at
/// compile time, and a typo'd path would otherwise write a key that `Config`'s
/// `deny_unknown_fields` rejects only at the next startup.
#[derive(Debug, Clone)]
pub struct Edit {
    pub path: &'static str,
    pub value: Setting,
}

#[derive(Debug, Clone)]
pub enum Setting {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    /// A TOML array of strings, e.g. `urls = ["a", "b"]`.
    StrList(Vec<String>),
    /// Remove the key entirely, falling back to the compiled default.
    ///
    /// Distinct from writing an empty string: `sonarr` is `Option<ServiceConfig>`,
    /// and `url = ""` is a parse error where an absent table means "not configured".
    Unset,
}

impl Edit {
    pub fn str(path: &'static str, v: impl Into<String>) -> Self {
        Self {
            path,
            value: Setting::Str(v.into()),
        }
    }

    pub fn int(path: &'static str, v: i64) -> Self {
        Self {
            path,
            value: Setting::Int(v),
        }
    }

    pub fn float(path: &'static str, v: f64) -> Self {
        Self {
            path,
            value: Setting::Float(v),
        }
    }

    pub fn bool(path: &'static str, v: bool) -> Self {
        Self {
            path,
            value: Setting::Bool(v),
        }
    }

    pub fn str_list(path: &'static str, v: Vec<String>) -> Self {
        Self {
            path,
            value: Setting::StrList(v),
        }
    }

    pub fn unset(path: &'static str) -> Self {
        Self {
            path,
            value: Setting::Unset,
        }
    }

    /// Write the value, or remove the key when the trimmed input is blank.
    ///
    /// This is what nearly every optional text input in the UI wants: a user who
    /// clears a field means "not configured", not "configured as empty".
    pub fn str_or_unset(path: &'static str, v: &str) -> Self {
        let trimmed = v.trim();
        if trimmed.is_empty() {
            Self::unset(path)
        } else {
            Self::str(path, trimmed)
        }
    }
}

/// A `sharerr.toml` open for editing.
#[derive(Debug)]
pub struct ConfigFile {
    path: PathBuf,
    doc: DocumentMut,
    /// Set when the file on disk did not parse and `doc` is a blank replacement.
    /// [`Self::write_validated`] moves the original aside rather than
    /// overwriting it.
    recovered: bool,
}

impl ConfigFile {
    /// Open the file for editing, or start an empty document if it is absent.
    ///
    /// A missing config file is legal today — a deployment can be configured
    /// entirely through `SHARERR_*` — and must stay legal, so this is not an error.
    /// Unparseable TOML *is* one: there is a document here and it cannot be edited.
    /// [`Self::replacing`] is the way past that.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let text = read_or_empty(&path)?;

        let doc = text
            .parse::<DocumentMut>()
            .with_context(|| format!("{} is not valid TOML", path.display()))?;

        Ok(Self {
            path,
            doc,
            recovered: false,
        })
    }

    /// Start a fresh document that will *replace* whatever is on disk.
    ///
    /// For the one case editing cannot fix: a `sharerr.toml` that failed to load.
    /// Editing it in place does not help, because the reason it failed is still
    /// there — save a corrected `tag` beside a typo'd `taag` and the document is
    /// still rejected, forever. Since a file that did not load is not in effect
    /// anyway, the honest move is to write out what the running process actually
    /// has and let the operator carry on.
    ///
    /// [`Self::write_validated`] renames the original to `sharerr.toml.invalid`
    /// first, so the only copy of what they hand-wrote — including the one stray
    /// character that probably caused this — survives for them to consult.
    pub fn replacing(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            doc: DocumentMut::new(),
            recovered: true,
        }
    }

    /// Where [`Self::write_validated`] will move the current file, when it is
    /// replacing one.
    pub fn backup_path(&self) -> Option<PathBuf> {
        (self.recovered && self.path.exists()).then(|| invalid_path(&self.path))
    }

    /// Apply edits in order. Nothing is written until [`Self::write_validated`].
    pub fn apply(&mut self, edits: impl IntoIterator<Item = Edit>) {
        for edit in edits {
            apply_one(&mut self.doc, edit);
        }
    }

    /// Replace the whole `path_map` array.
    ///
    /// Wholesale rather than per-row because the UI submits the entire table on
    /// every save, and an empty list legitimately means "all three views agree" —
    /// which has to remove the array rather than leave stale rows behind.
    pub fn set_path_map(&mut self, mappings: &[PathMapping]) {
        if mappings.is_empty() {
            self.doc.remove("path_map");
            return;
        }

        let mut tables = ArrayOfTables::new();
        for mapping in mappings {
            let mut table = Table::new();
            table["arr"] = value(mapping.arr.to_string_lossy().as_ref());
            table["sharerr"] = value(mapping.sharerr.to_string_lossy().as_ref());
            if let Some(qbit) = &mapping.qbit {
                table["qbit"] = value(qbit.to_string_lossy().as_ref());
            }
            tables.push(table);
        }

        self.doc["path_map"] = Item::ArrayOfTables(tables);
    }

    /// Replace the whole `library` array — wholesale for the same reason as
    /// [`Self::set_path_map`]: the form submits every row on every save, and an
    /// empty list means "no directories", which must remove the array.
    pub fn set_libraries(&mut self, libraries: &[LibraryConfig]) {
        if libraries.is_empty() {
            self.doc.remove("library");
            return;
        }

        let mut tables = ArrayOfTables::new();
        for library in libraries {
            let mut table = Table::new();
            table["path"] = value(library.path.to_string_lossy().as_ref());
            table["kind"] = value(library.kind.as_str());
            tables.push(table);
        }

        self.doc["library"] = Item::ArrayOfTables(tables);
    }

    /// Remove a drained `[[peers]]` bootstrap block. One-directional, unlike
    /// [`Self::set_path_map`]/[`Self::set_libraries`]: nothing ever writes
    /// this array back — see `sharerr_core::config::PeerImport`.
    pub fn clear_peers(&mut self) {
        self.doc.remove("peers");
    }

    /// Render the document as it would be written.
    pub fn to_toml(&self) -> String {
        self.doc.to_string()
    }

    /// Validate, then replace the file atomically.
    ///
    /// Returns the `Config` the new file produces, so the caller can swap it into
    /// the running server without re-reading from disk and racing itself.
    ///
    /// Test-only: the settings page validates in `prepare_config` (before the
    /// vault is touched) and writes through [`Self::write_validated`], so
    /// nothing in the binary needs the single-step form any more.
    #[cfg(test)]
    pub fn save(&self) -> Result<Config> {
        let text = self.to_toml();
        let config = crate::settings::validate(&text)?;
        self.write_validated(&text)?;
        Ok(config)
    }

    /// The write half of `save`, for a caller that has already
    /// serialised and validated the document (the settings page does so
    /// *before* touching the vault) and must not pay for — or drift from —
    /// a second pass. `text` must be this document's own `to_toml()` output.
    pub fn write_validated(&self, text: &str) -> Result<()> {
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }

        // Keep the original instead of overwriting it. It is still the only copy of
        // whatever the operator hand-wrote — comments, a URL they typed once and
        // would have to look up again — and the reason it did not load may be a
        // single character they can lift straight back out.
        if let Some(aside) = self.backup_path() {
            std::fs::rename(&self.path, &aside)
                .with_context(|| format!("moving {} aside", self.path.display()))?;
            tracing::warn!(
                moved_to = %aside.display(),
                "replaced an unparseable config file"
            );
        }

        // tmp-then-rename, mirroring `Vault::persist`: a crash or a full disk
        // partway through leaves the previous config intact rather than truncated.
        // A truncated sharerr.toml is not merely lost settings — with
        // `deny_unknown_fields` it is a container that will not start.
        let tmp = self.path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("replacing {}", self.path.display()))?;

        Ok(())
    }
}

/// Read the file, treating "not there" as "empty" and anything else as an error.
fn read_or_empty(path: &std::path::Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(source) => {
            Err(anyhow::Error::new(source)).with_context(|| format!("reading {}", path.display()))
        }
    }
}

/// Where an unparseable config is moved before a fresh one replaces it.
fn invalid_path(path: &std::path::Path) -> PathBuf {
    path.with_extension("toml.invalid")
}

fn apply_one(doc: &mut DocumentMut, edit: Edit) {
    let mut segments = edit.path.split('.').peekable();
    let mut table = doc.as_table_mut();

    while let Some(segment) = segments.next() {
        if segments.peek().is_none() {
            match edit.value {
                Setting::Str(s) => table[segment] = value(s),
                Setting::Int(i) => table[segment] = value(i),
                Setting::Float(f) => table[segment] = value(f),
                Setting::Bool(b) => table[segment] = value(b),
                Setting::StrList(list) => {
                    table[segment] = value(list.into_iter().collect::<toml_edit::Array>());
                }
                Setting::Unset => {
                    table.remove(segment);
                }
            }
            return;
        }

        // An intermediate segment that is missing — or is somehow not a table —
        // becomes one. `implicit(false)` keeps `[sonarr]` written as a real header
        // rather than a dotted `sonarr.url =`, matching the shape of the example
        // config an operator is used to reading.
        let entry = table
            .entry(segment)
            .or_insert_with(|| Item::Table(header()));
        if !entry.is_table() {
            *entry = Item::Table(header());
        }

        // Just proven to be a table, but `as_table_mut` is still the only way to
        // get the borrow — and returning on `None` keeps this free of `expect`,
        // which the workspace lints against.
        match entry.as_table_mut() {
            Some(next) => table = next,
            None => return,
        }
    }
}

fn header() -> Table {
    let mut table = Table::new();
    table.set_implicit(false);
    table
}

/// Which config paths are currently pinned by a `SHARERR_*` environment variable,
/// mapped to the variable doing the pinning.
///
/// This exists because figment layers env *over* the file
/// ([`crate::settings::load`]), so a value the operator saves in the UI is
/// silently discarded on reload if the matching variable is set. The UI renders
/// those fields locked and names the variable, rather than accepting a save that
/// goes nowhere.
///
/// Scanned once and memoised: the process environment cannot change after
/// start, and this is otherwise re-read on every settings and wizard render.
pub fn env_overrides() -> BTreeMap<String, String> {
    static OVERRIDES: std::sync::OnceLock<BTreeMap<String, String>> = std::sync::OnceLock::new();
    OVERRIDES
        .get_or_init(|| collect_overrides(std::env::vars()))
        .clone()
}

/// The env-scanning logic, split out so it is testable without mutating the
/// process environment — which no parallel test can do safely.
fn collect_overrides(vars: impl Iterator<Item = (String, String)>) -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();

    for (name, _) in vars {
        // Figment lowercases env keys before matching, so `sharerr_tag` configures
        // the instance just as `SHARERR_TAG` does. Detection has to be equally
        // case-blind or it would report a field as free that is in fact pinned.
        let upper = name.to_uppercase();
        let Some(rest) = upper.strip_prefix("SHARERR_") else {
            continue;
        };
        if rest.is_empty() || crate::settings::NON_CONFIG_ENV.contains(&rest) {
            continue;
        }

        // The inverse of figment's `.split("__")`: `QBITTORRENT__URL` addresses
        // `qbittorrent.url`.
        found.insert(rest.to_lowercase().replace("__", "."), name);
    }

    found
}

/// Parse the path-mapping rows a form submitted.
///
/// Rows whose `arr` and `sharerr` are both blank are dropped, so the UI can render
/// a spare empty row for adding one without it becoming a phantom mapping. A row
/// with only one side filled is an error rather than a silent drop — it is a
/// half-finished edit, and discarding it would look like the save failed.
pub fn parse_path_map(rows: &[(String, String, String)]) -> Result<Vec<PathMapping>> {
    let mut mappings = Vec::new();

    for (index, (arr, sharerr, qbit)) in rows.iter().enumerate() {
        let (arr, sharerr, qbit) = (arr.trim(), sharerr.trim(), qbit.trim());

        if arr.is_empty() && sharerr.is_empty() && qbit.is_empty() {
            continue;
        }
        if arr.is_empty() || sharerr.is_empty() {
            bail!(
                "path mapping {} needs both an *arr path and a sharerr path",
                index + 1
            );
        }

        mappings.push(PathMapping {
            arr: PathBuf::from(arr),
            sharerr: PathBuf::from(sharerr),
            // Absent means "same as sharerr", which the resolver already handles.
            // Storing a copy instead would freeze today's value into the file and
            // stop it tracking a later edit to the sharerr path.
            qbit: (!qbit.is_empty()).then(|| PathBuf::from(qbit)),
        });
    }

    Ok(mappings)
}

/// Parse the library rows a form submitted: `(path, kind)` per row.
///
/// Rows with a blank path are dropped — the spare empty row the page renders for
/// adding one — and an unknown kind is an error rather than a guess, because the
/// kind decides which feed category every file in the directory lands in.
pub fn parse_libraries(rows: &[(String, String)]) -> Result<Vec<LibraryConfig>> {
    let mut libraries = Vec::new();

    for (index, (path, kind)) in rows.iter().enumerate() {
        let path = path.trim();
        if path.is_empty() {
            continue;
        }
        let Some(kind) = LibraryKind::parse(kind.trim()) else {
            bail!(
                "library {} has no valid kind — pick tv, movie, music or book",
                index + 1
            );
        };
        let path = PathBuf::from(path);
        // A relative path would scan fine against the working directory and
        // then fail at share time, when the path resolver refuses it.
        if !path.is_absolute() {
            bail!("library {} must be an absolute path", path.display());
        }
        libraries.push(LibraryConfig { path, kind });
    }

    if let Some((a, b)) = crate::library::overlapping_roots(&libraries) {
        bail!(
            "library directories overlap: {} and {} — a file reachable from both \
             would be shared under whichever kind came last",
            a.display(),
            b.display()
        );
    }

    Ok(libraries)
}

#[cfg(test)]
mod tests {
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
}
