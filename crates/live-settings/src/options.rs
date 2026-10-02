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

use crate::setting::{cli_flag, AnySetting, Applies};
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

/// The options file, and the text it had when it was last read or written:
/// a save refuses to overwrite edits made since.
pub struct OptionsFile {
    place: Place,
    loaded: Mutex<String>,
}

impl OptionsFile {
    /// The file at `path`, which needn't exist yet: the first save creates
    /// it (and its directory).
    pub fn at(path: impl Into<PathBuf>) -> Self {
        OptionsFile {
            place: Place::Disk(path.into()),
            loaded: Mutex::new(String::new()),
        }
    }

    /// A file held in memory, starting as `text`.
    pub fn in_memory(text: impl Into<String>) -> Self {
        OptionsFile {
            place: Place::Memory(Mutex::new(text.into())),
            loaded: Mutex::new(String::new()),
        }
    }

    /// Where it is and whether it can be written.
    pub fn info(&self) -> FileInfo {
        match &self.place {
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
        match &self.place {
            Place::Disk(path) => path.display().to_string(),
            Place::Memory(_) => "the options file".to_string(),
        }
    }

    fn read_text(&self) -> Result<String, StoreError> {
        match &self.place {
            Place::Disk(path) if !path.exists() => Ok(String::new()),
            Place::Disk(path) => std::fs::read_to_string(path)
                .map_err(|e| StoreError::new(format!("{} can't be read: {e}", path.display()))),
            Place::Memory(text) => Ok(text.lock().clone()),
        }
    }

    /// Replaces the file, by writing a new one beside it and renaming it
    /// over the old, so a crash never leaves half a file.
    fn write_text(&self, text: &str) -> Result<(), StoreError> {
        match &self.place {
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
        let values = parse(&text, declared).map_err(|problems| {
            StoreError::new(format!("{}: {}", self.name(), problems.join("; ")))
        })?;
        *self.loaded.lock() = text;
        Ok(values)
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

    /// Sets (or, for `None`, removes) each key in place. Refused if the
    /// file changed since it was last read or written.
    fn write_values(
        &self,
        changes: &[(&'static dyn AnySetting, Option<String>)],
    ) -> Result<(), StoreError> {
        // The page locks what this file holds when it can't be written;
        // this refuses a save that comes anyway (a hand-made form, the API).
        if let Place::Disk(path) = &self.place {
            if !crate::paths::writable(path) {
                return Err(StoreError::new(format!(
                    "{} can't be written by this process: change it by editing it, then reload it.",
                    path.display()
                )));
            }
        }
        let mut loaded = self.loaded.lock();
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
            match raw.as_deref().filter(|raw| !raw.is_empty()) {
                Some(raw) => set(&mut doc, setting.key(), toml_value(&setting.kind(), raw)),
                None => remove(&mut doc, setting.key()),
            }
        }
        let text = doc.to_string();
        self.write_text(&text)?;
        *loaded = text;
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
        if !to_file.is_empty() {
            self.file.write_values(&to_file)?;
        }
        if !to_database.is_empty() {
            self.runtime.write_all(to_database).await?;
        }
        Ok(())
    }

    fn file_info(&self) -> Option<FileInfo> {
        Some(self.file.info())
    }
}

/// The values in `text`, by key: one table per key prefix, or the whole
/// key as a dotted key. Anything that isn't a setting the file may hold,
/// or a value its setting refuses, is a problem, given with its line.
fn parse(
    text: &str,
    declared: &[&'static dyn AnySetting],
) -> Result<HashMap<String, String>, Vec<String>> {
    let doc =
        toml_edit::Document::parse(text).map_err(|e| vec![e.to_string().trim().to_string()])?;
    let mut values = HashMap::new();
    let mut problems = Vec::new();
    walk(
        text,
        doc.as_table(),
        "",
        declared,
        &mut values,
        &mut problems,
    );
    if problems.is_empty() {
        Ok(values)
    } else {
        Err(problems)
    }
}

fn walk(
    text: &str,
    table: &Table,
    prefix: &str,
    declared: &[&'static dyn AnySetting],
    values: &mut HashMap<String, String>,
    problems: &mut Vec<String>,
) {
    for (name, item) in table.iter() {
        let key = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}.{name}")
        };
        let at = line(text, item.span())
            .map(|n| format!("line {n}: "))
            .unwrap_or_default();
        match declared.iter().find(|s| s.key() == key) {
            Some(setting) if !setting.sources().toml => {
                let instead = if setting.sources().env {
                    format!(
                        "it is a secret: set {} in the environment",
                        setting.env_var()
                    )
                } else if setting.sources().database {
                    "the admin page keeps it".to_string()
                } else {
                    format!("give it as --{}", cli_flag(&key))
                };
                problems.push(format!("{at}{key} can't be in the options file: {instead}"));
            }
            Some(setting) => match raw(item).and_then(|raw| setting.normalise(&raw).map(|_| raw)) {
                Ok(raw) => {
                    values.insert(key, raw);
                }
                Err(e) => problems.push(format!("{at}{key}: {e}")),
            },
            None => match item {
                Item::Table(table) => walk(text, table, &key, declared, values, problems),
                Item::Value(Value::InlineTable(inline)) => walk(
                    text,
                    &inline.clone().into_table(),
                    &key,
                    declared,
                    values,
                    problems,
                ),
                _ => problems.push(format!("{at}there is no setting called {key}")),
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
        let entry = table.entry(part).or_insert_with(toml_edit::table);
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
    let mut out = format!(
        "# Options file for {program}, written by `{program} --init`.\n\
         #\n\
         # Every setting is listed with its default, commented out: uncomment a line\n\
         # and change it to use another value. The admin settings page saves here too,\n\
         # and reads the file again when you press Reload. A command-line option wins\n\
         # over this file: `{program} --help` lists them.\n"
    );
    let secrets: Vec<_> = declared.iter().filter(|s| s.sources().env).collect();
    if !secrets.is_empty() {
        out.push_str("#\n# Secrets are never kept here. Set them in the environment:\n");
        for secret in secrets {
            let required = if secret.required() { " (required)" } else { "" };
            out.push_str(&format!("#   {}{required}\n", secret.env_var()));
        }
    }
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
        if !prefix.is_empty() {
            out.push_str(&format!("[{prefix}]\n"));
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
            let default = setting.default_stored();
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
    out
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
