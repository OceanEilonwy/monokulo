//! Generic env > database > code-default settings resolution.
//!
//! Both `scanner` and `monokulo` store their own runtime-configurable
//! settings in a per-service `settings` table (`key TEXT PRIMARY KEY, value
//! TEXT NOT NULL`) rather than a boot-time-only config file/environment-only
//! model - the admin settings page each now has needs a real place to
//! persist a change to. An environment variable, where set, still wins
//! outright over whatever is in the database - the precedence an operator
//! already relying on env-var-based deployment (a container orchestrator's
//! own secrets/config injection, for instance) needs to keep working
//! unchanged even after this exists.
//!
//! This module is the shared resolution logic only - each crate owns its own
//! `settings` table, its own list of known setting names/env-var names/
//! defaults, and its own `Store`/`Db` methods for reading/writing rows.
//! There's nothing engine- or control-plane-specific about "check an env var,
//! then a database row, then fall back to a default" for it to live in
//! either crate over the other.

/// Where a setting's current *effective* value actually came from - shown on
/// an admin settings page so an operator can tell "this is from an
/// environment variable; changing it here has no effect until that variable
/// is unset" apart from "this is a real, currently-effective database value"
/// or "neither is set, this is just the code default".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingSource {
    Env,
    Database,
    Default,
}

/// Resolves one setting's effective *raw string* value and where it came
/// from. `env_var` wins outright if set to a non-empty value; otherwise
/// `db_value` (a row already read from the caller's own `settings` table)
/// wins if present; otherwise `default`.
pub fn resolve_raw(env_var: &str, db_value: Option<&str>, default: &str) -> (String, SettingSource) {
    if let Some(v) = env_value(env_var) {
        if !v.trim().is_empty() {
            return (v, SettingSource::Env);
        }
    }
    match db_value {
        Some(v) => (v.to_string(), SettingSource::Database),
        None => (default.to_string(), SettingSource::Default),
    }
}

/// Same precedence as [`resolve_raw`], parsed into `T`. An environment
/// variable that's set but fails to parse panics with a clear message -
/// the same "refuse to boot loudly rather than silently misbehave"
/// discipline `scanner`'s own former TOML config validation already applied
/// to a malformed value an operator explicitly set. A *database* value that
/// fails to parse (e.g. a stale row from before a setting's accepted shape
/// changed) is treated as absent instead - falls through to the default,
/// with a warning on stderr - since a stored value nobody is actively
/// setting right now shouldn't be able to block a boot.
pub fn resolve_parsed<T>(env_var: &str, db_value: Option<&str>, default: T) -> T
where
    T: std::str::FromStr,
{
    if let Some(raw) = env_value(env_var) {
        if !raw.trim().is_empty() {
            return raw
                .parse()
                .unwrap_or_else(|_| panic!("{env_var} is set to {raw:?}, which is not a valid value for this setting"));
        }
    }
    if let Some(raw) = db_value {
        match raw.parse() {
            Ok(v) => return v,
            Err(_) => eprintln!("settings: stored value {raw:?} is no longer valid for this setting, using the default instead"),
        }
    }
    default
}

/// Which of `env_var`/`db_value` the *next* call to [`resolve_raw`]/
/// [`resolve_parsed`] would actually use - for an admin page to show "this
/// field is overridden by an environment variable" without needing to
/// duplicate the precedence rule itself.
pub fn source(env_var: &str, db_value: Option<&str>) -> SettingSource {
    match env_value(env_var) {
        Some(v) if !v.trim().is_empty() => SettingSource::Env,
        _ => {
            if db_value.is_some() {
                SettingSource::Database
            } else {
                SettingSource::Default
            }
        }
    }
}

/// The one place settings read the environment. With the `test-support`
/// feature (tests only), a [`test_env::EnvOverride`] on the current thread
/// takes precedence over the real process environment.
fn env_value(env_var: &str) -> Option<String> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(value) = test_env::overridden(env_var) {
        return value;
    }
    std::env::var(env_var).ok()
}

/// Per-thread environment overrides for tests that need a setting to come
/// from "the environment".
///
/// `std::env::set_var` changes the real process environment, which every
/// test running in parallel in the same binary shares. A test setting
/// `SCANNER_PAYMENT_CONFIRMATIONS_REQUIRED` would make another test that
/// happens to read that setting at the same moment see the wrong value,
/// and fail only now and then. An override here is visible only on the
/// thread that set it - under `#[tokio::test]`'s default single-threaded
/// runtime that is the whole test, handlers included - and is undone when
/// its guard is dropped.
#[cfg(any(test, feature = "test-support"))]
pub mod test_env {
    use std::cell::RefCell;
    use std::collections::HashMap;

    thread_local! {
        static OVERRIDES: RefCell<HashMap<String, Option<String>>> = RefCell::new(HashMap::new());
    }

    /// `Some(Some(value))`: set to `value`; `Some(None)`: forced unset;
    /// `None`: not overridden, use the real environment.
    pub(super) fn overridden(env_var: &str) -> Option<Option<String>> {
        OVERRIDES.with(|overrides| overrides.borrow().get(env_var).cloned())
    }

    /// Restores the previous override (or none) when dropped.
    #[must_use = "the override is undone as soon as this guard is dropped"]
    pub struct EnvOverride {
        env_var: String,
        previous: Option<Option<String>>,
    }

    /// Makes `env_var` read as `value` (or as unset, for `None`) for
    /// settings resolved on this thread, until the guard is dropped.
    pub fn set(env_var: &str, value: Option<&str>) -> EnvOverride {
        let previous = OVERRIDES.with(|overrides| overrides.borrow_mut().insert(env_var.to_string(), value.map(str::to_string)));
        EnvOverride { env_var: env_var.to_string(), previous }
    }

    impl Drop for EnvOverride {
        fn drop(&mut self) {
            OVERRIDES.with(|overrides| {
                let mut overrides = overrides.borrow_mut();
                match self.previous.take() {
                    Some(previous) => overrides.insert(self.env_var.clone(), previous),
                    None => overrides.remove(&self.env_var),
                };
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_override_wins_over_the_real_environment_and_is_undone_on_drop() {
        std::env::remove_var("SETTINGS_TEST_THREAD_OVERRIDE");
        {
            let _guard = test_env::set("SETTINGS_TEST_THREAD_OVERRIDE", Some("from-override"));
            assert_eq!(resolve_raw("SETTINGS_TEST_THREAD_OVERRIDE", Some("from-db"), "d"), ("from-override".to_string(), SettingSource::Env));
            // Invisible on any other thread.
            let elsewhere = std::thread::spawn(|| resolve_raw("SETTINGS_TEST_THREAD_OVERRIDE", Some("from-db"), "d")).join().unwrap();
            assert_eq!(elsewhere.1, SettingSource::Database);
            {
                let _unset = test_env::set("SETTINGS_TEST_THREAD_OVERRIDE", None);
                assert_eq!(source("SETTINGS_TEST_THREAD_OVERRIDE", Some("from-db")), SettingSource::Database);
            }
            assert_eq!(source("SETTINGS_TEST_THREAD_OVERRIDE", None), SettingSource::Env, "the inner guard restores the outer override");
        }
        assert_eq!(source("SETTINGS_TEST_THREAD_OVERRIDE", None), SettingSource::Default);
    }

    // `std::env::set_var`/`remove_var` mutate real process-wide state, so
    // every test below uses its own never-reused variable name - this suite
    // runs on the default multi-threaded test runner, and two tests sharing
    // a name would race each other's set/remove calls.

    #[test]
    fn env_wins_over_a_present_database_value() {
        std::env::set_var("SETTINGS_TEST_ENV_WINS", "from-env");
        let (value, src) = resolve_raw("SETTINGS_TEST_ENV_WINS", Some("from-db"), "from-default");
        std::env::remove_var("SETTINGS_TEST_ENV_WINS");
        assert_eq!(value, "from-env");
        assert_eq!(src, SettingSource::Env);
    }

    #[test]
    fn an_empty_env_var_is_treated_as_unset() {
        std::env::set_var("SETTINGS_TEST_EMPTY_ENV", "");
        let (value, src) = resolve_raw("SETTINGS_TEST_EMPTY_ENV", Some("from-db"), "from-default");
        std::env::remove_var("SETTINGS_TEST_EMPTY_ENV");
        assert_eq!(value, "from-db", "an empty env var must not shadow a real database value");
        assert_eq!(src, SettingSource::Database);
    }

    #[test]
    fn database_wins_over_the_default_when_no_env_var_is_set() {
        std::env::remove_var("SETTINGS_TEST_DB_WINS");
        let (value, src) = resolve_raw("SETTINGS_TEST_DB_WINS", Some("from-db"), "from-default");
        assert_eq!(value, "from-db");
        assert_eq!(src, SettingSource::Database);
    }

    #[test]
    fn the_default_is_used_when_neither_env_nor_database_have_a_value() {
        std::env::remove_var("SETTINGS_TEST_DEFAULT_WINS");
        let (value, src) = resolve_raw("SETTINGS_TEST_DEFAULT_WINS", None, "from-default");
        assert_eq!(value, "from-default");
        assert_eq!(src, SettingSource::Default);
    }

    #[test]
    fn resolve_parsed_parses_the_winning_source_into_the_requested_type() {
        std::env::remove_var("SETTINGS_TEST_PARSED_DB");
        let value: u32 = resolve_parsed("SETTINGS_TEST_PARSED_DB", Some("42"), 7);
        assert_eq!(value, 42);

        std::env::set_var("SETTINGS_TEST_PARSED_ENV", "99");
        let value: u32 = resolve_parsed("SETTINGS_TEST_PARSED_ENV", Some("42"), 7);
        std::env::remove_var("SETTINGS_TEST_PARSED_ENV");
        assert_eq!(value, 99);

        let value: u32 = resolve_parsed("SETTINGS_TEST_PARSED_DEFAULT", None, 7);
        assert_eq!(value, 7);
    }

    #[test]
    #[should_panic(expected = "SETTINGS_TEST_BAD_ENV")]
    fn resolve_parsed_panics_on_a_malformed_env_var_rather_than_silently_falling_back() {
        std::env::set_var("SETTINGS_TEST_BAD_ENV", "not-a-number");
        let _: u32 = resolve_parsed("SETTINGS_TEST_BAD_ENV", Some("42"), 7);
    }

    #[test]
    fn resolve_parsed_falls_back_past_a_malformed_database_value_instead_of_panicking() {
        std::env::remove_var("SETTINGS_TEST_BAD_DB");
        let value: u32 = resolve_parsed("SETTINGS_TEST_BAD_DB", Some("not-a-number"), 7);
        assert_eq!(value, 7);
    }

    #[test]
    fn source_reports_which_precedence_tier_would_actually_be_used() {
        std::env::remove_var("SETTINGS_TEST_SOURCE");
        assert_eq!(source("SETTINGS_TEST_SOURCE", None), SettingSource::Default);
        assert_eq!(source("SETTINGS_TEST_SOURCE", Some("x")), SettingSource::Database);
        std::env::set_var("SETTINGS_TEST_SOURCE", "y");
        assert_eq!(source("SETTINGS_TEST_SOURCE", Some("x")), SettingSource::Env);
        std::env::remove_var("SETTINGS_TEST_SOURCE");
    }
}
