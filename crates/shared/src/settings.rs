//! The one place settings read the process environment, and per-thread
//! environment overrides for tests. The settings themselves (environment,
//! then database, then default; validation; saving) are declared and
//! resolved with the `live-settings` crate in each service.

/// The one place settings read the environment. With the `test-support`
/// feature (tests only), a [`test_env::EnvOverride`] on the current thread
/// takes precedence over the real process environment.
pub fn env_value(env_var: &str) -> Option<String> {
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
        let previous = OVERRIDES.with(|overrides| {
            overrides
                .borrow_mut()
                .insert(env_var.to_string(), value.map(str::to_string))
        });
        EnvOverride {
            env_var: env_var.to_string(),
            previous,
        }
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
            assert_eq!(
                env_value("SETTINGS_TEST_THREAD_OVERRIDE").as_deref(),
                Some("from-override")
            );
            // Invisible on any other thread.
            let elsewhere = std::thread::spawn(|| env_value("SETTINGS_TEST_THREAD_OVERRIDE"))
                .join()
                .unwrap();
            assert_eq!(elsewhere, None);
            {
                let _unset = test_env::set("SETTINGS_TEST_THREAD_OVERRIDE", None);
                assert_eq!(env_value("SETTINGS_TEST_THREAD_OVERRIDE"), None);
            }
            assert_eq!(
                env_value("SETTINGS_TEST_THREAD_OVERRIDE").as_deref(),
                Some("from-override"),
                "the inner guard restores the outer override"
            );
        }
        assert_eq!(env_value("SETTINGS_TEST_THREAD_OVERRIDE"), None);
    }
}
