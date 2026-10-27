//! The options file: settings kept in TOML, one table per key prefix
//! (`[payment]` holds `payment.reorg_check_depth`). It is read when the
//! process starts and when an admin reloads it; the admin page writes the
//! keys it saves back into it in place, keeping its comments and layout.
//!
//! Runtime switches stay in the database and secrets in the environment;
//! [`LayeredStore`] keeps the two stores behind the registry's one
//! [`SettingsStore`], each key in its own place.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use serde::Serialize;
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, Value};

use crate::setting::{cli_flag, AnySetting, Applies, Env};
use crate::store::{SettingsStore, StoreError};
use crate::value::SettingKind;

/// Where the options file is, and whether the admin page can write it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct FileInfo {
    pub path: String,
    pub exists: bool,
    pub writable: bool,
}

enum Place {
    Disk(PathBuf),
    /// No file: tests, and tools that only describe settings.
    Memory(Mutex<String>),
}

/// The file itself, shared by every handle on it: where it is, and the
/// text it had when it was last read or written, so a save refuses to
/// overwrite edits made since, whichever handle made the save.
struct Shared {
    place: Place,
    loaded: Mutex<String>,
}

/// The options file, or one table of it.
///
/// A handle is cheap to clone, and clones share the file. Services in one
/// process (monokulo, and an engine and a push relay embedded in it) share
/// one file this way: monokulo's handle leaves each embedded service's
/// table alone ([`OptionsFile::leaving`]), and each service's handle is
/// its table ([`OptionsFile::scoped`]), where it reads and writes its own
/// keys under their own names. Saving through any of them keeps what the
/// others saved.
#[derive(Clone)]
pub struct OptionsFile {
    shared: Arc<Shared>,
    /// The table this handle is, if it is one: its keys are named without
    /// it (`payment.x` for `engine.payment.x`).
    scope: Option<String>,
    /// Tables other handles own: their subtables are left alone here.
    leaves: Vec<String>,
    /// What to add when a key under one of these tables isn't a setting.
    hints: Vec<(String, String)>,
    /// Settings that were renamed, old key and new: a file still holding
    /// an old key is refused with the new name, so the fix is plain.
    renamed: &'static [(&'static str, &'static str)],
    /// Settings that were removed, key and when: a file still holding one
    /// is read without it, and [`OptionsFile::retired_warnings`] says so.
    retired: &'static [(&'static str, &'static str)],
}

/// Which file, and which part of it: not its text.
impl std::fmt::Debug for OptionsFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut out = f.debug_struct("OptionsFile");
        match &self.shared.place {
            Place::Disk(path) => out.field("path", path),
            Place::Memory(_) => out.field("path", &"in memory"),
        };
        out.field("scope", &self.scope)
            .field("leaves", &self.leaves)
            .finish_non_exhaustive()
    }
}

impl OptionsFile {
    /// The file at `path`, which needn't exist yet: the first save creates
    /// it (and its directory).
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self::new(Place::Disk(path.into()))
    }

    /// A file held in memory, starting as `text`.
    pub fn in_memory(text: impl Into<String>) -> Self {
        Self::new(Place::Memory(Mutex::new(text.into())))
    }

    fn new(place: Place) -> Self {
        OptionsFile {
            shared: Arc::new(Shared {
                place,
                loaded: Mutex::new(String::new()),
            }),
            scope: None,
            leaves: Vec::new(),
            hints: Vec::new(),
            renamed: &[],
            retired: &[],
        }
    }

    /// The table `table` of the same file, as a file of its own: its
    /// subtables are read and written as keys without the table's name.
    /// Values directly in `table` (not in a subtable) belong to the file's
    /// other reader and are left alone.
    pub fn scoped(&self, table: &str) -> Self {
        OptionsFile {
            shared: self.shared.clone(),
            scope: Some(table.to_string()),
            leaves: Vec::new(),
            hints: Vec::new(),
            renamed: &[],
            retired: &[],
        }
    }

    /// This handle, leaving the subtables of `table` to another handle on
    /// the same file (one made with [`Self::scoped`]): they aren't read as
    /// this handle's settings, nor refused as unknown. Called once for
    /// each such table.
    pub fn leaving(mut self, table: &str) -> Self {
        self.leaves.push(table.to_string());
        self
    }

    /// This handle, adding `hint` to the problem reported for a key under
    /// `table` that isn't a setting: why such keys don't belong here.
    /// Called once for each such table.
    pub fn with_hint(mut self, table: &str, hint: &str) -> Self {
        self.hints.push((table.to_string(), hint.to_string()));
        self
    }

    /// This handle, refusing each `(old, new)` key in `renamed` with the
    /// name it has now rather than as an unknown key.
    pub fn with_renamed(mut self, renamed: &'static [(&'static str, &'static str)]) -> Self {
        self.renamed = renamed;
        self
    }

    /// This handle, reading past each `(key, since)` in `retired` rather
    /// than refusing it as unknown: a setting removed on `since`, which an
    /// options file written before then may still hold. Its value goes
    /// nowhere; [`Self::retired_warnings`] names it.
    pub fn with_retired(mut self, retired: &'static [(&'static str, &'static str)]) -> Self {
        self.retired = retired;
        self
    }

    /// How [`parse`] reads the file for this handle.
    fn reading(&self) -> Reading<'_> {
        Reading {
            scope: self.scope.as_deref(),
            leaves: &self.leaves,
            hints: &self.hints,
            renamed: self.renamed,
            retired: self.retired,
        }
    }

    /// Where it is and whether it can be written.
    pub fn info(&self) -> FileInfo {
        match &self.shared.place {
            Place::Disk(path) => FileInfo {
                path: path.display().to_string(),
                exists: path.exists(),
                writable: crate::paths::writable(path),
            },
            Place::Memory(_) => FileInfo {
                path: "(in memory)".to_string(),
                exists: true,
                writable: true,
            },
        }
    }

    fn name(&self) -> String {
        match &self.shared.place {
            Place::Disk(path) => path.display().to_string(),
            Place::Memory(_) => "the options file".to_string(),
        }
    }

    fn read_text(&self) -> Result<String, StoreError> {
        match &self.shared.place {
            Place::Disk(path) if !path.exists() => Ok(String::new()),
            Place::Disk(path) => std::fs::read_to_string(path)
                .map_err(|e| StoreError::new(format!("{} can't be read: {e}", path.display()))),
            Place::Memory(text) => Ok(text.lock().clone()),
        }
    }

    /// Replaces the file, by writing a new one beside it and renaming it
    /// over the old, so a crash never leaves half a file.
    fn write_text(&self, text: &str) -> Result<(), StoreError> {
        match &self.shared.place {
            Place::Disk(path) => write_atomically(path, text)
                .map_err(|e| StoreError::new(format!("{} can't be written: {e}", path.display()))),
            Place::Memory(memory) => {
                *memory.lock() = text.to_string();
                Ok(())
            }
        }
    }

    /// The file's values, by key, checked against `declared`: every key must
    /// be a setting the file may hold, and every value valid. All problems
    /// are reported together, each with its line.
    fn read_values(
        &self,
        declared: &[&'static dyn AnySetting],
    ) -> Result<HashMap<String, String>, StoreError> {
        let text = self.read_text()?;
        let Parsed { values, .. } =
            parse(&text, declared, &self.reading()).map_err(|problems| {
                StoreError::new(format!("{}: {}", self.name(), problems.join("; ")))
            })?;
        *self.shared.loaded.lock() = text;
        Ok(values)
    }

    /// One line for each retired setting ([`Self::with_retired`]) the file
    /// still holds, saying it does nothing now and can be deleted: for the
    /// process to log at start. Nothing when the file can't be read; its
    /// read says why.
    pub fn retired_warnings(&self, declared: &[&'static dyn AnySetting]) -> Vec<String> {
        self.read_text()
            .ok()
            .and_then(|text| parse(&text, declared, &self.reading()).ok())
            .map(|parsed| parsed.retired)
            .unwrap_or_default()
    }

    /// The file's values, by key, checked against `declared`, for what a
    /// process needs before its registry exists (where its database is, its
    /// thread count). A problem is the error to stop on.
    pub fn read(
        &self,
        declared: &[&'static dyn AnySetting],
    ) -> Result<HashMap<String, String>, StoreError> {
        self.read_values(declared)
    }

    /// Sets (or, for `None`, removes) each key in place, and gives back the
    /// text it replaced, for [`Self::restore`]. Refused if the file changed
    /// since it was last read or written.
    fn write_values(
        &self,
        changes: &[(&'static dyn AnySetting, Option<String>)],
    ) -> Result<String, StoreError> {
        // The page locks what this file holds when it can't be written;
        // this refuses a save that comes anyway (a hand-made form, the API).
        if let Place::Disk(path) = &self.shared.place {
            if !crate::paths::writable(path) {
                return Err(StoreError::new(format!(
                    "{} can't be written by this process: change it by editing it, then reload it.",
                    path.display()
                )));
            }
        }
        let mut loaded = self.shared.loaded.lock();
        let current = self.read_text()?;
        if current != *loaded {
            return Err(StoreError::new(format!(
                "{} has changed since it was loaded: reload it, then save again.",
                self.name()
            )));
        }
        let mut doc: DocumentMut = current
            .parse()
            .map_err(|e| StoreError::new(format!("{}: {e}", self.name())))?;
        for (setting, raw) in changes {
            let key = match &self.scope {
                Some(table) => format!("{table}.{}", setting.key()),
                None => setting.key().to_string(),
            };
            // An empty value is a value (`key = ""`, or `[]`), kept as the
            // database keeps it; only taking the value away removes the key.
            match raw.as_deref() {
                Some(raw) => set(&mut doc, &key, toml_value(&setting.kind(), raw)),
                None => remove(&mut doc, &key),
            }
        }
        let text = doc.to_string();
        self.write_text(&text)?;
        *loaded = text;
        Ok(current)
    }

    /// Puts back the text a [`Self::write_values`] replaced, when the rest
    /// of its save failed.
    fn restore(&self, previous: &str) -> Result<(), StoreError> {
        let mut loaded = self.shared.loaded.lock();
        self.write_text(previous)?;
        *loaded = previous.to_string();
        Ok(())
    }
}

/// Writes `text` to the file `path` names (through a symlink, which stays),
/// keeping its permissions: a new file beside it, renamed over it.
fn write_atomically(path: &Path, text: &str) -> std::io::Result<()> {
    let path = crate::paths::resolved(path);
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "options.toml".to_string());
    let temporary = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let written = std::fs::write(&temporary, text).and_then(|()| {
        if let Ok(old) = std::fs::metadata(&path) {
            std::fs::set_permissions(&temporary, old.permissions())?;
        }
        std::fs::rename(&temporary, &path)
    });
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

/// The options file and the database behind one store: each setting is
/// read from and saved to the one its sources name.
pub struct LayeredStore {
    file: OptionsFile,
    runtime: Arc<dyn SettingsStore>,
    declared: Vec<&'static dyn AnySetting>,
}

impl LayeredStore {
    pub fn new(
        file: OptionsFile,
        runtime: Arc<dyn SettingsStore>,
        declared: &[&'static dyn AnySetting],
    ) -> Self {
        LayeredStore {
            file,
            runtime,
            declared: declared.to_vec(),
        }
    }

    fn setting(&self, key: &str) -> Option<&'static dyn AnySetting> {
        self.declared.iter().copied().find(|s| s.key() == key)
    }
}

#[async_trait::async_trait]
impl SettingsStore for LayeredStore {
    async fn read_all(&self) -> Result<HashMap<String, String>, StoreError> {
        let mut values = self.file.read_values(&self.declared)?;
        // Only runtime switches come from the database; a row for anything
        // else is left from an older release and means nothing now.
        let runtime = self.runtime.read_all().await?;
        values.extend(
            runtime
                .into_iter()
                .filter(|(key, _)| self.setting(key).is_some_and(|s| s.sources().database)),
        );
        Ok(values)
    }

    async fn write_all(
        &self,
        changes: Vec<(&'static str, Option<String>)>,
    ) -> Result<(), StoreError> {
        let mut to_file = Vec::new();
        let mut to_database = Vec::new();
        for (key, value) in changes {
            match self.setting(key) {
                Some(setting) if setting.sources().toml => to_file.push((setting, value)),
                Some(_) => to_database.push((key, value)),
                None => return Err(StoreError::new(format!("there is no setting called {key}"))),
            }
        }
        // The file first, then the database; when the database refuses,
        // the file goes back to what it was, so the save is all or nothing.
        let previous = if to_file.is_empty() {
            None
        } else {
            Some(self.file.write_values(&to_file)?)
        };
        if !to_database.is_empty() {
            if let Err(e) = self.runtime.write_all(to_database).await {
                if let Some(previous) = previous {
                    if let Err(restore) = self.file.restore(&previous) {
                        return Err(StoreError::new(format!(
                            "{e}; and {} could not be put back ({restore}), so it holds the new values: reload it.",
                            self.file.name()
                        )));
                    }
                }
                return Err(e);
            }
        }
        Ok(())
    }

    fn file_info(&self) -> Option<FileInfo> {
        Some(self.file.info())
    }
}

/// How a handle reads the file: the whole of it, or one table, and what it
/// leaves to another handle.
struct Reading<'a> {
    scope: Option<&'a str>,
    leaves: &'a [String],
    hints: &'a [(String, String)],
    renamed: &'static [(&'static str, &'static str)],
    retired: &'static [(&'static str, &'static str)],
}

/// What [`parse`] found in a file.
struct Parsed {
    values: HashMap<String, String>,
    /// A warning for each retired setting the file holds.
    retired: Vec<String>,
}

/// The values in `text`, by key: one table per key prefix, or the whole
/// key as a dotted key, and a warning for each retired setting it holds.
/// Anything else that isn't a setting the file may hold, or a value its
/// setting refuses, is a problem, given with its line and its full key.
fn parse(
    text: &str,
    declared: &[&'static dyn AnySetting],
    reading: &Reading<'_>,
) -> Result<Parsed, Vec<String>> {
    let doc =
        toml_edit::Document::parse(text).map_err(|e| vec![e.to_string().trim().to_string()])?;
    let mut values = HashMap::new();
    let mut problems = Vec::new();
    let mut retired = Vec::new();
    let mut walking = Walk {
        text,
        declared,
        reading,
        values: &mut values,
        problems: &mut problems,
        retired: &mut retired,
    };
    match reading.scope {
        None => walking.table(doc.as_table(), ""),
        Some(scope) => {
            // Only the table's subtables are this handle's: a value
            // directly in it is the other reader's (`engine.url`).
            if let Some(table) = doc.get(scope).and_then(Item::as_table_like) {
                for (name, item) in table.iter() {
                    if is_table(item) {
                        walking.item(name, item, "");
                    }
                }
            }
        }
    }
    if problems.is_empty() {
        Ok(Parsed { values, retired })
    } else {
        Err(problems)
    }
}

fn is_table(item: &Item) -> bool {
    matches!(item, Item::Table(_) | Item::Value(Value::InlineTable(_)))
}

struct Walk<'a, 'r> {
    text: &'a str,
    declared: &'a [&'static dyn AnySetting],
    reading: &'a Reading<'r>,
    values: &'a mut HashMap<String, String>,
    problems: &'a mut Vec<String>,
    retired: &'a mut Vec<String>,
}

impl Walk<'_, '_> {
    fn table(&mut self, table: &dyn toml_edit::TableLike, prefix: &str) {
        for (name, item) in table.iter() {
            self.item(name, item, prefix);
        }
    }

    /// `name` in the table at `prefix` (a key relative to the handle's
    /// scope).
    fn item(&mut self, name: &str, item: &Item, prefix: &str) {
        let key = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}.{name}")
        };
        // As the reader of the file sees it.
        let shown = match self.reading.scope {
            Some(scope) => format!("{scope}.{key}"),
            None => key.clone(),
        };
        let at = line(self.text, item.span())
            .map(|n| format!("line {n}: "))
            .unwrap_or_default();
        match self.declared.iter().find(|s| s.key() == key) {
            Some(setting) if !setting.sources().toml => {
                let instead = if setting.sources().env {
                    format!(
                        "it is a secret: set {} in the environment",
                        setting.env_var()
                    )
                } else if setting.sources().database {
                    "the admin page keeps it".to_string()
                } else {
                    format!("give it as --{}", cli_flag(&shown))
                };
                self.problems.push(format!(
                    "{at}{shown} can't be in the options file: {instead}"
                ));
            }
            Some(setting) => match raw(item).and_then(|raw| setting.normalise(&raw).map(|_| raw)) {
                Ok(raw) => {
                    self.values.insert(key, raw);
                }
                Err(e) => self.problems.push(format!("{at}{shown}: {e}")),
            },
            // Another handle's table.
            None if is_table(item) && self.reading.leaves.iter().any(|table| prefix == table) => {}
            None => match item {
                Item::Table(table) => self.table(table, &key),
                Item::Value(Value::InlineTable(inline)) => self.table(inline, &key),
                _ => {
                    if let Some((_, since)) =
                        self.reading.retired.iter().find(|(key, _)| *key == shown)
                    {
                        self.retired.push(format!(
                            "{at}{shown} was retired on {since} and does nothing now: delete it from the options file"
                        ));
                        return;
                    }
                    if let Some((_, new)) =
                        self.reading.renamed.iter().find(|(old, _)| *old == shown)
                    {
                        self.problems.push(format!(
                            "{at}{shown} is now called {new}: rename it in the options file"
                        ));
                        return;
                    }
                    let hint = self
                        .reading
                        .hints
                        .iter()
                        .find(|(table, _)| shown.starts_with(&format!("{table}.")))
                        .map(|(_, hint)| format!(": {hint}"))
                        .unwrap_or_default();
                    self.problems
                        .push(format!("{at}there is no setting called {shown}{hint}"));
                }
            },
        }
    }
}

/// The line a span starts on.
fn line(text: &str, span: Option<std::ops::Range<usize>>) -> Option<usize> {
    let start = span?.start;
    Some(text.get(..start)?.matches('\n').count() + 1)
}

/// An item as the raw text its setting parses: a string as itself, numbers
/// and booleans as written, a list of strings comma separated, anything
/// else as JSON.
fn raw(item: &Item) -> Result<String, String> {
    match item {
        Item::Value(value) => Ok(value_raw(value)),
        Item::Table(table) => Ok(table_json(table).to_string()),
        _ => Err("Enter a value or a table.".to_string()),
    }
}

fn value_raw(value: &Value) -> String {
    match value {
        Value::String(s) => s.value().clone(),
        Value::Integer(n) => n.value().to_string(),
        Value::Float(f) => f.value().to_string(),
        Value::Boolean(b) => b.value().to_string(),
        Value::Datetime(d) => d.value().to_string(),
        Value::Array(array) if array.iter().all(|v| v.is_str()) => array
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join(","),
        other => value_json(other).to_string(),
    }
}

fn value_json(value: &Value) -> serde_json::Value {
    match value {
        Value::String(s) => serde_json::Value::String(s.value().clone()),
        Value::Integer(n) => serde_json::Value::from(*n.value()),
        Value::Float(f) => serde_json::Value::from(*f.value()),
        Value::Boolean(b) => serde_json::Value::Bool(*b.value()),
        Value::Datetime(d) => serde_json::Value::String(d.value().to_string()),
        Value::Array(array) => serde_json::Value::Array(array.iter().map(value_json).collect()),
        Value::InlineTable(table) => serde_json::Value::Object(
            table
                .iter()
                .map(|(k, v)| (k.to_string(), value_json(v)))
                .collect(),
        ),
    }
}

fn table_json(table: &Table) -> serde_json::Value {
    serde_json::Value::Object(
        table
            .iter()
            .filter_map(|(k, item)| {
                let value = match item {
                    Item::Value(v) => value_json(v),
                    Item::Table(t) => table_json(t),
                    _ => return None,
                };
                Some((k.to_string(), value))
            })
            .collect(),
    )
}

/// A stored value as TOML, by the setting's kind.
fn toml_value(kind: &SettingKind, raw: &str) -> Value {
    match kind {
        SettingKind::Integer { .. } => raw
            .trim()
            .parse::<i64>()
            .map(Value::from)
            .unwrap_or_else(|_| Value::from(raw)),
        SettingKind::Bool => Value::from(raw.trim() == "true"),
        SettingKind::ChoiceList { .. } => {
            let mut array = Array::new();
            for item in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                array.push(item);
            }
            Value::Array(array)
        }
        SettingKind::Json => serde_json::from_str(raw)
            .ok()
            .and_then(|json| json_value(&json))
            .unwrap_or_else(|| Value::from(raw)),
        _ => Value::from(raw),
    }
}

fn json_value(json: &serde_json::Value) -> Option<Value> {
    Some(match json {
        serde_json::Value::Null => return None,
        serde_json::Value::Bool(b) => Value::from(*b),
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(i) => Value::from(i),
            None => Value::from(n.as_f64()?),
        },
        serde_json::Value::String(s) => Value::from(s.as_str()),
        serde_json::Value::Array(items) => {
            let mut array = Array::new();
            for item in items.iter().filter_map(json_value) {
                array.push(item);
            }
            Value::Array(array)
        }
        serde_json::Value::Object(map) => {
            let mut table = InlineTable::new();
            for (k, v) in map {
                if let Some(v) = json_value(v) {
                    table.insert(k, v);
                }
            }
            Value::InlineTable(table)
        }
    })
}

/// Sets `key` (one table per prefix) to `value`, keeping everything else.
fn set(doc: &mut DocumentMut, key: &str, value: Value) {
    let mut parts: Vec<&str> = key.split('.').collect();
    let Some(name) = parts.pop() else {
        return;
    };
    let mut table: &mut dyn toml_edit::TableLike = doc.as_table_mut();
    for part in parts {
        // A new table only gets a header if it holds values itself:
        // `[engine.payment]`, not an empty `[engine]` above it.
        let entry = table.entry(part).or_insert_with(|| {
            let mut new = Table::new();
            new.set_implicit(true);
            Item::Table(new)
        });
        let Some(next) = entry.as_table_like_mut() else {
            return;
        };
        table = next;
    }
    match table.get_mut(name) {
        // In place, so the comment above it stays.
        Some(Item::Value(existing)) => {
            let decor = existing.decor().clone();
            *existing = value;
            *existing.decor_mut() = decor;
        }
        _ => {
            table.insert(name, Item::Value(value));
        }
    }
}

fn remove(doc: &mut DocumentMut, key: &str) {
    let mut parts: Vec<&str> = key.split('.').collect();
    let Some(name) = parts.pop() else {
        return;
    };
    let mut table: &mut dyn toml_edit::TableLike = doc.as_table_mut();
    for part in parts {
        let Some(next) = table.get_mut(part).and_then(Item::as_table_like_mut) else {
            return;
        };
        table = next;
    }
    table.remove(name);
}

/// The options file `--init` writes: every setting the file may hold,
/// grouped by key prefix, each with what it is for, what it takes, when it
/// applies and its default, commented out, so the file changes nothing
/// until a line is uncommented. Secrets are named, with their variables.
pub fn render_init(program: &str, declared: &[&'static dyn AnySetting]) -> String {
    render_init_nested(program, declared, &[])
}

/// A service that runs inside this process and keeps its options in this
/// process's file, under `[table.…]`: an embedded engine's
/// `[engine.payment]`, an embedded push relay's `[relay.server]`.
#[derive(Clone, Copy)]
pub struct Nested<'a> {
    pub table: &'a str,
    /// What the table is, written above it.
    pub about: &'a str,
    pub settings: &'a [&'static dyn AnySetting],
    /// The defaults in effect for it: the host's own
    /// ([`Env::with_defaults`]) where it gives one.
    pub defaults: &'a Env,
}

/// [`render_init`], followed by the settings of each `nested` service under
/// its table, introduced by its `about`, each showing the default in
/// effect under its `defaults`. The secrets of every one of them are named
/// at the top.
pub fn render_init_nested(
    program: &str,
    declared: &[&'static dyn AnySetting],
    nested: &[Nested<'_>],
) -> String {
    let mut out = format!(
        "# Options file for {program}, written by `{program} --init`.\n\
         #\n\
         # Every setting is listed with its default, commented out: uncomment a line\n\
         # and change it to use another value. The admin settings page saves here too,\n\
         # and reads the file again when you press Reload. A command-line option wins\n\
         # over this file: `{program} --help` lists them.\n"
    );
    let secrets: Vec<_> = declared
        .iter()
        .chain(nested.iter().flat_map(|service| service.settings))
        .filter(|s| s.sources().env)
        .collect();
    if !secrets.is_empty() {
        out.push_str("#\n# Secrets are never kept here. Set them in the environment:\n");
        for secret in secrets {
            let required = if secret.required() { " (required)" } else { "" };
            out.push_str(&format!("#   {}{required}\n", secret.env_var()));
        }
    }
    push_tables(&mut out, None, declared, &Env::default());
    for service in nested {
        out.push_str("\n#\n");
        for line in wrap(service.about, 76) {
            out.push_str(&format!("# {line}\n"));
        }
        out.push_str("#\n");
        push_tables(
            &mut out,
            Some(service.table),
            service.settings,
            service.defaults,
        );
    }
    out
}

/// Every setting of `declared` the file may hold, one table per key
/// prefix, each under `table` if given, commented out with the default in
/// effect under `defaults`.
fn push_tables(
    out: &mut String,
    table: Option<&str>,
    declared: &[&'static dyn AnySetting],
    defaults: &Env,
) {
    let in_file: Vec<_> = declared.iter().filter(|s| s.sources().toml).collect();
    // Keys without a prefix come before the first table, as TOML needs.
    let mut groups: Vec<(&str, Vec<&&'static dyn AnySetting>)> = Vec::new();
    for setting in &in_file {
        let prefix = setting.key().split_once('.').map_or("", |(p, _)| p);
        match groups.iter_mut().find(|(p, _)| *p == prefix) {
            Some((_, members)) => members.push(setting),
            None => groups.push((prefix, vec![setting])),
        }
    }
    groups.sort_by_key(|(prefix, _)| !prefix.is_empty());
    for (prefix, members) in groups {
        out.push('\n');
        match (table, prefix.is_empty()) {
            (None, true) => {}
            (None, false) => out.push_str(&format!("[{prefix}]\n")),
            (Some(table), true) => out.push_str(&format!("[{table}]\n")),
            (Some(table), false) => out.push_str(&format!("[{table}.{prefix}]\n")),
        }
        for (i, setting) in members.into_iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            let name = setting
                .key()
                .strip_prefix(&format!("{prefix}."))
                .unwrap_or(setting.key());
            for line in wrap(setting.description(), 76) {
                out.push_str(&format!("# {line}\n"));
            }
            let facts = facts(*setting);
            if !facts.is_empty() {
                out.push_str(&format!("# {facts}\n"));
            }
            let default = setting.default_stored(defaults);
            let shown = if default.is_empty() {
                match setting.example() {
                    Some(example) => {
                        out.push_str("# Unset by default; for example:\n");
                        toml_value(&setting.kind(), example).to_string()
                    }
                    None => "\"\"".to_string(),
                }
            } else {
                toml_value(&setting.kind(), &default).to_string()
            };
            out.push_str(&format!("# {name} = {}\n", shown.trim()));
        }
    }
}

/// What a setting takes and when it applies, in a sentence or two.
fn facts(setting: &dyn AnySetting) -> String {
    let mut facts = Vec::new();
    match setting.kind() {
        SettingKind::Integer {
            min: Some(min),
            max: Some(max),
        } => facts.push(format!("A whole number from {min} to {max}.")),
        SettingKind::Integer { .. } => facts.push("A whole number.".to_string()),
        SettingKind::Bool => facts.push("true or false.".to_string()),
        SettingKind::Choice { choices } => facts.push(format!("One of: {}.", choices.join(", "))),
        SettingKind::ChoiceList { choices } => {
            facts.push(format!("A list of any of: {}.", choices.join(", ")))
        }
        _ => {}
    }
    if setting.applies() == Applies::Restart {
        facts.push("Takes effect after a restart.".to_string());
    }
    if !setting.editable() {
        facts.push("The admin page doesn't change it.".to_string());
    }
    facts.join(" ")
}

/// `text` in lines of at most `width` characters, broken between words.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.len() + 1 + word.len() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// Writes the `--init` file at `path`, refusing to replace one that exists.
pub fn write_init(path: &Path, text: &str) -> Result<(), String> {
    if path.exists() {
        return Err(format!(
            "{} already exists: move it away first, or name another file with --options.",
            path.display()
        ));
    }
    write_atomically(path, text).map_err(|e| format!("{} can't be written: {e}", path.display()))
}
