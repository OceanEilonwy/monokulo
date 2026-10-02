//! One setting's declaration, and how its effective value is resolved.

use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;

use crate::value::{whole_number_message, SettingKind, SettingValue};

/// When a saved value reaches the running process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Applies {
    /// Straight away, through the section's `Live` value (and its
    /// `Reloadable`, if it has one).
    Live,
    /// On the next start. Saving it is allowed; the admin page then warns
    /// that a restart is needed.
    Restart,
}

/// Where a setting's value can come from, lowest precedence first: a value
/// from a later source wins over one from an earlier source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// The options file (TOML), which the admin page writes to.
    Toml,
    /// The database: runtime switches the admin page keeps.
    Database,
    /// A command-line option, `--` and the key with `-` for `.` and `_`
    /// ([`cli_flag`]).
    Cli,
    /// The setting's environment variable: secrets only.
    Env,
}

/// The sources a setting accepts: the options file and the command line,
/// unless it says otherwise. A setting is stored in at most one place (the
/// options file or the database), and only a secret comes from the
/// environment, alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Sources {
    pub toml: bool,
    pub database: bool,
    pub cli: bool,
    pub env: bool,
}

impl Sources {
    /// Configuration: the options file and the command line.
    pub const CONFIG: Sources = Sources::of(&[Source::Toml, Source::Cli]);

    /// Exactly `sources`.
    pub const fn of(sources: &[Source]) -> Sources {
        let mut out = Sources {
            toml: false,
            database: false,
            cli: false,
            env: false,
        };
        let mut i = 0;
        while i < sources.len() {
            match sources[i] {
                Source::Toml => out.toml = true,
                Source::Database => out.database = true,
                Source::Cli => out.cli = true,
                Source::Env => out.env = true,
            }
            i += 1;
        }
        out
    }

    /// Whether a value is kept somewhere (the options file or the
    /// database), so the admin page can change it.
    pub const fn stored(self) -> bool {
        self.toml || self.database
    }
}

/// How a setting is given from outside the admin page: `ENGINE_TOKEN`,
/// `--server-bind`, or `ENGINE_SERVER_BIND or --server-bind`, as its sources
/// allow.
pub fn outside_names(key: &str, env_var: &str, sources: Sources) -> String {
    match (sources.env, sources.cli) {
        (true, true) => format!("{env_var} or --{}", cli_flag(key)),
        (false, true) => format!("--{}", cli_flag(key)),
        _ => env_var.to_string(),
    }
}

/// A setting's command-line option, without the leading `--`:
/// `payment.reorg_check_depth` is `payment-reorg-check-depth`.
pub fn cli_flag(key: &str) -> String {
    key.replace(['.', '_'], "-")
}

/// Inclusive limits on a whole-number setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Bounds {
    pub min: i64,
    pub max: i64,
}

/// Limits for [`Setting::bounds`]: `bounds: range(1, 10_000)`. Inside
/// [`settings!`](crate::settings) this is written `check: range(1, 10_000)`.
///
/// Bounds are a field of their own rather than a `check` function because
/// the admin page needs the numbers too (for the input's `min`/`max`), and
/// a function pointer can't carry them.
pub const fn range(min: i64, max: i64) -> Option<Bounds> {
    Some(Bounds { min, max })
}

/// A default written as its raw form, for types that can only be built by
/// parsing: `default: parsed_default("http://127.0.0.1:8443")` for an
/// `HttpUrl`.
///
/// # Panics
///
/// If `raw` doesn't parse. That is a mistake in the declaration, the same
/// on every run; `Registry::build` evaluates every default, so the unit
/// test each crate has that builds its registry hits it first.
pub fn parsed_default<T: SettingValue>(raw: &'static str) -> T {
    match T::parse(raw) {
        Ok(value) => value,
        Err(e) => panic!("the default {raw:?} doesn't parse: {e}"),
    }
}

/// An extra rule for a setting's value, on top of its type's own. The error
/// is shown next to the field.
pub type Check<T> = fn(&T) -> Result<(), String>;

/// One setting, typed. The value type is fixed here, so every reader gets
/// the same type and parsing lives in one place.
///
/// Usually declared with [`settings!`](crate::settings) rather than by
/// hand.
pub struct Setting<T: SettingValue> {
    /// The stored key, and the field name on the admin page.
    pub key: &'static str,
    /// Its environment variable: a secret's, and empty for every other
    /// setting.
    pub env_var: &'static str,
    /// The value when neither the environment nor the store has one. A
    /// function, because most value types can't be built in a `const`.
    pub default: fn() -> T,
    /// An extra rule on top of the type's own, checked whenever a value is
    /// parsed.
    pub check: Option<Check<T>>,
    /// Whole-number limits, checked whenever a value is parsed and shown to
    /// the admin page as the input's `min`/`max`. See [`range`].
    pub bounds: Option<Bounds>,
    /// What the setting is for, shown under it on the admin page.
    pub description: &'static str,
    /// An example value. It must itself be valid; `Registry::build` checks.
    pub example: Option<&'static str>,
    pub applies: Applies,
    /// Where its value may come from. Without `Database` it is never
    /// stored, a save refuses it and the admin page shows it locked.
    pub sources: Sources,
    /// A setting that can't be saved and has no usable default: the
    /// process can't start without it ([`Setting::require`]). `default` is
    /// a placeholder, never used for anything that runs.
    pub required: bool,
    /// Whether the admin page may change it. A setting the page shouldn't
    /// touch (where the database is) can still be in the options file.
    pub editable: bool,
}

/// Where a setting's effective value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingSource {
    /// The options file.
    Toml,
    /// The database (a runtime switch).
    Database,
    /// A command-line option, which wins over anything stored.
    Cli,
    /// An environment variable, which wins over everything else.
    Env,
    /// Nothing sets it, or what sets it is invalid.
    Default,
}

/// A value that was set but couldn't be used, so the default is in effect
/// instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Problem {
    /// Whether the bad value came from the environment or the command
    /// line, where the admin page can't fix it.
    pub from_env: bool,
    pub message: String,
}

/// A resolved value and where it came from.
pub(crate) struct Resolved<T> {
    pub value: T,
    pub source: SettingSource,
    pub problem: Option<Problem>,
}

impl<T: SettingValue> Setting<T> {
    /// Parses `raw` with the type's rules, then this setting's bounds and
    /// check.
    pub fn parse(&self, raw: &str) -> Result<T, String> {
        // A number that doesn't parse is told this setting's range, not
        // the type's.
        let value = T::parse(raw).map_err(|error| match (self.bounds, self.kind()) {
            (Some(_), SettingKind::Integer { min, max }) => whole_number_message(min, max),
            _ => error,
        })?;
        self.validate(&value)?;
        Ok(value)
    }

    fn validate(&self, value: &T) -> Result<(), String> {
        if let (Some(bounds), Some(n)) = (self.bounds, value.as_integer()) {
            if n < i128::from(bounds.min) || n > i128::from(bounds.max) {
                return Err(whole_number_message(Some(bounds.min), Some(bounds.max)));
            }
        }
        match self.check {
            Some(check) => check(value),
            None => Ok(()),
        }
    }

    pub fn default_value(&self) -> T {
        (self.default)()
    }

    /// The admin page control, with this setting's bounds applied.
    pub fn kind(&self) -> SettingKind {
        match (T::kind(), self.bounds) {
            (SettingKind::Integer { min, max }, Some(b)) => SettingKind::Integer {
                min: Some(min.map_or(b.min, |m| m.max(b.min))),
                max: Some(max.map_or(b.max, |m| m.min(b.max))),
            },
            (kind, _) => kind,
        }
    }

    /// The value from the command line and the environment alone (no
    /// store), and what is wrong with it, if anything: for a setting read
    /// before the store opens, or before logging starts. The registry
    /// reports the problem when it is built.
    pub fn read_unstored(&self, env: &Env) -> (T, Option<Problem>) {
        let resolved = self.resolve(None, env);
        (resolved.value, resolved.problem)
    }

    /// The value from the command line and the environment alone, for
    /// start-up. A required setting that is unset or invalid is an error,
    /// which the process reports and exits on. Any other invalid value
    /// gives the default (the registry reports it when it is built).
    pub fn require(&self, env: &Env) -> Result<T, String> {
        let (value, problem) = self.read_unstored(env);
        match problem {
            Some(problem) if self.required => Err(problem.message),
            _ => Ok(value),
        }
    }

    /// How to set it from outside: `ENGINE_TOKEN or --server-token`.
    fn outside_names(&self) -> String {
        outside_names(self.key, self.env_var, self.sources)
    }

    /// The environment over the command line over the stored value (the
    /// options file's or the database's) over the default, for the sources
    /// this setting accepts. An invalid value from any of them gives the
    /// default and is reported as a problem, which stops the process at
    /// start (`Registry::build`). A required setting that is unset is a
    /// problem too.
    pub(crate) fn resolve(&self, stored: Option<&str>, env: &Env) -> Resolved<T> {
        let stored = if self.sources.stored() { stored } else { None };
        let mut problem = None;
        let outside = [
            (
                self.sources.env,
                SettingSource::Env,
                env.get(self.env_var),
                self.env_var.to_string(),
            ),
            (
                self.sources.cli,
                SettingSource::Cli,
                env.cli(self.key),
                format!("--{}", cli_flag(self.key)),
            ),
        ];
        let given = outside
            .into_iter()
            .find_map(|(accepted, source, raw, name)| {
                Some((source, raw.filter(|_| accepted)?, name))
            });
        if let Some((source, raw, name)) = given {
            match self.parse(&raw) {
                Ok(value) => {
                    return Resolved {
                        value,
                        source,
                        problem: None,
                    }
                }
                Err(e) => {
                    problem = Some(Problem {
                        from_env: true,
                        message: format!("{name} is set to an invalid value. {e}"),
                    })
                }
            }
        } else if let Some(raw) = stored {
            let (source, place) = if self.sources.toml {
                (SettingSource::Toml, "In the options file, it")
            } else {
                (SettingSource::Database, "The saved value")
            };
            match self.parse(raw) {
                Ok(value) => {
                    return Resolved {
                        value,
                        source,
                        problem: None,
                    }
                }
                Err(e) => {
                    problem = Some(Problem {
                        from_env: false,
                        message: format!("{place} is invalid. {e}"),
                    })
                }
            }
        } else if self.required {
            problem = Some(Problem {
                from_env: true,
                message: format!("{} must be set. {}", self.outside_names(), self.description),
            });
        }
        Resolved {
            value: self.default_value(),
            source: SettingSource::Default,
            problem,
        }
    }
}

/// Any setting, whatever its value type: what the registry, the admin page
/// and `ALL` lists work with. Implemented only by [`Setting`].
///
/// There is deliberately no way to read a value through this trait; code
/// reads settings through its section's `Live` value.
pub trait AnySetting: private::Resolve + Send + Sync {
    fn key(&self) -> &'static str;
    fn env_var(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn example(&self) -> Option<&'static str>;
    fn applies(&self) -> Applies;
    fn kind(&self) -> SettingKind;
    /// Where its value may come from. Without `Database`: never stored,
    /// shown locked.
    fn sources(&self) -> Sources;
    /// Whether the process can't start without it.
    fn required(&self) -> bool;
    /// Whether the admin page may change it.
    fn editable(&self) -> bool;
}

impl<T: SettingValue> AnySetting for Setting<T> {
    fn key(&self) -> &'static str {
        self.key
    }

    fn env_var(&self) -> &'static str {
        self.env_var
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn example(&self) -> Option<&'static str> {
        self.example
    }

    fn applies(&self) -> Applies {
        self.applies
    }

    fn kind(&self) -> SettingKind {
        Setting::kind(self)
    }

    fn sources(&self) -> Sources {
        self.sources
    }

    fn required(&self) -> bool {
        self.required
    }

    fn editable(&self) -> bool {
        self.editable
    }
}

/// The untyped operations the registry needs. In a private module so
/// nothing outside the crate can call them or implement `AnySetting`.
pub(crate) mod private {
    use super::*;

    /// A resolved value as strings.
    pub struct ResolvedView {
        /// The stored form (unmasked). Compared to detect a pending
        /// restart; never shown.
        pub stored_form: String,
        /// What people see (masked for secrets).
        pub shown: String,
        pub source: SettingSource,
        pub problem: Option<Problem>,
    }

    pub trait Resolve {
        /// Parses and checks `raw`, returning the form to store.
        fn normalise(&self, raw: &str) -> Result<String, String>;
        fn resolve_view(&self, stored: Option<&str>, env: &Env) -> ResolvedView;
        fn default_shown(&self) -> String;
        /// The default in its stored form (unmasked), for the options
        /// file `--init` writes.
        fn default_stored(&self) -> String;
        /// Checks the declaration itself: the default and example are
        /// valid, and bounds are only on whole numbers.
        fn check_declaration(&self) -> Result<(), String>;
    }

    impl<T: SettingValue> Resolve for Setting<T> {
        fn normalise(&self, raw: &str) -> Result<String, String> {
            self.parse(raw).map(|v| v.to_stored())
        }

        fn resolve_view(&self, stored: Option<&str>, env: &Env) -> ResolvedView {
            let resolved = self.resolve(stored, env);
            ResolvedView {
                stored_form: resolved.value.to_stored(),
                shown: resolved.value.render(),
                source: resolved.source,
                problem: resolved.problem,
            }
        }

        fn default_shown(&self) -> String {
            self.default_value().render()
        }

        fn default_stored(&self) -> String {
            self.default_value().to_stored()
        }

        fn check_declaration(&self) -> Result<(), String> {
            let sources = self.sources;
            if self.key.is_empty() {
                return Err("a setting needs a key".to_string());
            }
            if !(sources.toml || sources.database || sources.cli || sources.env) {
                return Err("a setting needs at least one source".to_string());
            }
            let secret = T::kind() == SettingKind::Secret;
            if secret && sources != Sources::of(&[Source::Env]) {
                return Err(
                    "a secret comes from its environment variable alone: the process list and an options file can be read by others"
                        .to_string(),
                );
            }
            if !secret && sources.env {
                return Err("only a secret comes from the environment".to_string());
            }
            if sources.env == self.env_var.is_empty() {
                return Err(
                    "a setting has an environment variable exactly when it comes from the environment"
                        .to_string(),
                );
            }
            if sources.toml && sources.database {
                return Err(
                    "a setting is stored in the options file or the database, not both".to_string(),
                );
            }
            if self.required && sources.stored() {
                return Err("only a setting that isn't stored can be required".to_string());
            }
            if let Some(bounds) = self.bounds {
                if !matches!(T::kind(), SettingKind::Integer { .. }) {
                    return Err("range(..) only applies to whole-number settings".to_string());
                }
                if bounds.min > bounds.max {
                    return Err(format!("range({}, {}) is empty", bounds.min, bounds.max));
                }
            }
            // A required setting's default is a placeholder, never used.
            if !self.required {
                let default = self.default_value();
                self.validate(&default)
                    .map_err(|e| format!("the default is rejected by its own rules: {e}"))?;
                match T::parse(&default.to_stored()) {
                    Ok(back) if back == default => {}
                    _ => {
                        return Err(
                            "the default doesn't survive being stored and read back".to_string()
                        )
                    }
                }
            }
            if let Some(example) = self.example {
                self.parse(example)
                    .map_err(|e| format!("the example {example:?} is invalid: {e}"))?;
            }
            Ok(())
        }
    }
}

/// The environment variables settings are resolved against.
///
/// Normally the real process environment. Tests use [`Env::fixed`] instead
/// of `std::env::set_var`, which would change the environment of every test
/// running at the same time in the same binary.
#[derive(Clone, Default)]
pub struct Env {
    fixed: Option<Arc<HashMap<String, String>>>,
    /// Values from the command line, by setting key.
    cli: Arc<HashMap<String, String>>,
}

/// Names only: a variable's value may be a secret (an admin token).
impl std::fmt::Debug for Env {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut cli: Vec<&String> = self.cli.keys().collect();
        cli.sort();
        match &self.fixed {
            None => f
                .debug_struct("Env")
                .field("cli", &cli)
                .finish_non_exhaustive(),
            Some(vars) => {
                let mut names: Vec<&String> = vars.keys().collect();
                names.sort();
                f.debug_struct("Env")
                    .field("vars", &names)
                    .field("cli", &cli)
                    .finish()
            }
        }
    }
}

impl Env {
    /// The real process environment.
    pub fn process() -> Self {
        Env::default()
    }

    /// A fixed set of variables, with nothing else set.
    pub fn fixed<K: Into<String>, V: Into<String>>(vars: impl IntoIterator<Item = (K, V)>) -> Self {
        Env {
            fixed: Some(Arc::new(
                vars.into_iter()
                    .map(|(k, v)| (k.into(), v.into()))
                    .collect(),
            )),
            cli: Arc::default(),
        }
    }

    /// This fixed environment with `var` set to `value` unless it is set
    /// already: for a test that needs a required secret without naming it.
    /// The process environment is left as it is.
    pub fn or_var(self, var: &str, value: &str) -> Self {
        match &self.fixed {
            Some(vars) if !vars.contains_key(var) => {
                let mut vars = (**vars).clone();
                vars.insert(var.to_string(), value.to_string());
                Env {
                    fixed: Some(Arc::new(vars)),
                    cli: self.cli,
                }
            }
            _ => self,
        }
    }

    /// This environment with values given on the command line, by setting
    /// key ([`crate::cli::values`]); they win over everything else.
    pub fn with_cli(mut self, values: HashMap<String, String>) -> Self {
        self.cli = Arc::new(values);
        self
    }

    /// The command-line value for the setting `key`. Unlike a variable, a
    /// blank one is a value: it was typed on purpose.
    pub fn cli(&self, key: &str) -> Option<String> {
        self.cli.get(key).cloned()
    }

    /// The variable's value. Unset and blank are the same: a blank
    /// variable must not hide a saved value.
    pub fn get(&self, var: &str) -> Option<String> {
        let value = match &self.fixed {
            Some(vars) => vars.get(var).cloned(),
            None => std::env::var(var).ok(),
        }?;
        if value.trim().is_empty() {
            None
        } else {
            Some(value)
        }
    }
}

/// Every setting's effective value at one moment: the stored values plus
/// the environment, with defaults behind them. Sections are built from it.
///
/// Reading a setting through it applies environment over stored over
/// default. An invalid value falls back to that setting's default and
/// nothing else, so one bad value doesn't take its neighbours with it.
#[derive(Clone)]
pub struct Snapshot {
    stored: HashMap<String, String>,
    env: Env,
}

/// Keys only: stored values include secrets (an admin token, collector
/// headers), and a `{:?}` in a log line or an assertion must not print
/// them.
impl std::fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut keys: Vec<&String> = self.stored.keys().collect();
        keys.sort();
        f.debug_struct("Snapshot")
            .field("stored_keys", &keys)
            .field("env", &self.env)
            .finish()
    }
}

impl Snapshot {
    pub fn new(stored: HashMap<String, String>, env: Env) -> Self {
        Snapshot { stored, env }
    }

    /// Nothing stored and nothing in the environment: every setting at its
    /// default.
    pub fn defaults() -> Self {
        Snapshot {
            stored: HashMap::new(),
            env: Env::fixed(Vec::<(String, String)>::new()),
        }
    }

    /// The effective value of `setting`.
    pub fn get<T: SettingValue>(&self, setting: &Setting<T>) -> T {
        setting
            .resolve(self.stored.get(setting.key).map(String::as_str), &self.env)
            .value
    }

    /// Where `setting`'s effective value comes from.
    pub fn source<T: SettingValue>(&self, setting: &Setting<T>) -> SettingSource {
        setting
            .resolve(self.stored.get(setting.key).map(String::as_str), &self.env)
            .source
    }

    pub(crate) fn stored(&self) -> &HashMap<String, String> {
        &self.stored
    }

    pub(crate) fn env(&self) -> &Env {
        &self.env
    }
}
