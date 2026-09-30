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
    /// The environment variable that overrides the stored value.
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
}

/// Where a setting's effective value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingSource {
    /// An environment variable, which wins over anything saved.
    Env,
    /// A saved value.
    Database,
    /// Neither is set, or the one that is set is invalid.
    Default,
}

/// A value that was set but couldn't be used, so the default is in effect
/// instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Problem {
    /// Whether the bad value came from the environment, where the admin
    /// page can't fix it.
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
        let value = T::parse(raw)?;
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

    /// Environment over stored over default. An invalid environment or
    /// stored value falls back to the default for this key only, and is
    /// reported as a problem.
    pub(crate) fn resolve(&self, stored: Option<&str>, env: &Env) -> Resolved<T> {
        let mut problem = None;
        if let Some(raw) = env.get(self.env_var) {
            match self.parse(&raw) {
                Ok(value) => {
                    return Resolved {
                        value,
                        source: SettingSource::Env,
                        problem: None,
                    }
                }
                Err(e) => {
                    problem = Some(Problem {
                        from_env: true,
                        message: format!(
                            "{} is set to an invalid value, so the default is used. {e}",
                            self.env_var
                        ),
                    })
                }
            }
        } else if let Some(raw) = stored {
            match self.parse(raw) {
                Ok(value) => {
                    return Resolved {
                        value,
                        source: SettingSource::Database,
                        problem: None,
                    }
                }
                Err(e) => {
                    problem = Some(Problem {
                        from_env: false,
                        message: format!("The saved value is invalid, so the default is used. {e}"),
                    })
                }
            }
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

        fn check_declaration(&self) -> Result<(), String> {
            if self.key.is_empty() || self.env_var.is_empty() {
                return Err("a setting needs a key and an environment variable".to_string());
            }
            if let Some(bounds) = self.bounds {
                if !matches!(T::kind(), SettingKind::Integer { .. }) {
                    return Err("range(..) only applies to whole-number settings".to_string());
                }
                if bounds.min > bounds.max {
                    return Err(format!("range({}, {}) is empty", bounds.min, bounds.max));
                }
            }
            let default = self.default_value();
            self.validate(&default)
                .map_err(|e| format!("the default is rejected by its own rules: {e}"))?;
            match T::parse(&default.to_stored()) {
                Ok(back) if back == default => {}
                _ => {
                    return Err("the default doesn't survive being stored and read back".to_string())
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
#[derive(Debug, Clone, Default)]
pub struct Env {
    fixed: Option<Arc<HashMap<String, String>>>,
}

impl Env {
    /// The real process environment.
    pub fn process() -> Self {
        Env { fixed: None }
    }

    /// A fixed set of variables, with nothing else set.
    pub fn fixed<K: Into<String>, V: Into<String>>(vars: impl IntoIterator<Item = (K, V)>) -> Self {
        Env {
            fixed: Some(Arc::new(
                vars.into_iter()
                    .map(|(k, v)| (k.into(), v.into()))
                    .collect(),
            )),
        }
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
#[derive(Debug, Clone)]
pub struct Snapshot {
    stored: HashMap<String, String>,
    env: Env,
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
