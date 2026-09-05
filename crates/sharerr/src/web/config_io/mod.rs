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
        // The same inline guard as `write_validated`; its comment explains
        // why this cannot be a shared helper.
        let Some(path_text) = path.to_str() else {
            return Err(anyhow::anyhow!("{} is not valid UTF-8", path.display()));
        };
        if path_text.contains("..") {
            return Err(anyhow::anyhow!("{path_text} must not contain '..'"));
        }
        let text = read_or_empty(std::path::Path::new(path_text))?;

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
    ///
    /// Carries the same inline `..` guard as `write_validated` — see that
    /// method's comment for why the shape (checked `&str` receiver, rebuilt
    /// `Path`) is load-bearing for CodeQL's `DotDotCheck` sanitizer. A `..` or
    /// non-UTF-8 path yields `None` here rather than an error: the caller only
    /// wants a name to show the operator, and `write_validated` would refuse
    /// to write such a path anyway.
    pub fn backup_path(&self) -> Option<PathBuf> {
        let path_text = self.path.to_str()?;
        if path_text.contains("..") {
            return None;
        }
        let path = std::path::Path::new(path_text);
        (self.recovered && path.exists()).then(|| invalid_path(path))
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
    /// this array back — see `sharerr_core::config::PeerImport`. `"peers"` is
    /// deliberately not registered in `sharerr_core::config::config_paths`:
    /// that list is "every path the web UI writes back", and this key is
    /// neither web-UI-editable nor ever written, only ever removed, by
    /// `sharerr::commands::serve`.
    pub fn clear_peers(&mut self) {
        self.apply([Edit::unset("peers")]);
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
        // Refuse a path containing `..`.
        //
        // This is the one check CodeQL's `rust/path-injection` query
        // recognises as a sanitizer (`TaintedPathExtensions.qll`'s
        // `DotDotCheck`). The query's source is the axum handler's `State`
        // parameter — its model treats every handler argument as remote
        // input — so `state.serve.config_path()`, set once at startup from
        // `--config`/`SHARERR_CONFIG` and never reassigned, counts as
        // "user-provided" here. `docs/SECURITY.md`'s "What is out of scope"
        // section explains why there is no real privilege boundary to
        // enforce: whoever sets that flag already controls the process. This
        // exists to satisfy the query, at a real cost — an operator-supplied
        // config path can no longer contain `..`, even a legitimate one (a
        // relative bind-mount a directory up), nor be non-UTF-8 — accepted as
        // a deliberate trade-off rather than left as a dismissed finding.
        //
        // The shape is load-bearing, because `DotDotCheck` is a barrier
        // *guard*: it only clears later reads of the receiver variable of
        // `.contains("..")`, on the false branch, inside this same function.
        // Hence the receiver is a `str` local (`path_text`), not the `Path`
        // and not a call chain; the check is inline, not a helper (a
        // `reject_traversal(path)?` call is invisible to it — that was the
        // previous attempt, and it never registered); the true branch is a
        // literal `return` rather than `bail!`, so dominance survives a
        // failed macro expansion; and every sink below reads a `Path` rebuilt
        // from `path_text` *after* the check rather than `self.path` itself.
        // It is a substring check, not a component-wise one, because that is
        // the exact shape the query recognises. `write_validated` runs
        // regardless of whether `self` came from `open` (already checked) or
        // `replacing` (never checked), so the guard belongs here too.
        let Some(path_text) = self.path.to_str() else {
            return Err(anyhow::anyhow!(
                "{} is not valid UTF-8",
                self.path.display()
            ));
        };
        if path_text.contains("..") {
            return Err(anyhow::anyhow!("{path_text} must not contain '..'"));
        }
        let path = std::path::Path::new(path_text);

        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }

        // Keep the original instead of overwriting it. It is still the only copy of
        // whatever the operator hand-wrote — comments, a URL they typed once and
        // would have to look up again — and the reason it did not load may be a
        // single character they can lift straight back out.
        //
        // Inlined rather than calling `self.backup_path()`: that re-reads
        // `self.path` from scratch, which would carry it around the guard above.
        if self.recovered && path.exists() {
            let aside = invalid_path(path);
            std::fs::rename(path, &aside)
                .with_context(|| format!("moving {} aside", path.display()))?;
            tracing::warn!(
                moved_to = %aside.display(),
                "replaced an unparseable config file"
            );
        }

        // tmp-then-rename, mirroring `Vault::persist`: a crash or a full disk
        // partway through leaves the previous config intact rather than truncated.
        // A truncated sharerr.toml is not merely lost settings — with
        // `deny_unknown_fields` it is a container that will not start.
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;

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
mod tests;
