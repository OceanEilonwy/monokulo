//! Throttled logging for errors that repeat every tick (admin_settings_v2.md
//! task 7.13). A node or key-custody backend that is down produces the same
//! error every second; logging each one buries everything else.
//! [`throttled!`](crate::throttled) logs a given kind of event at most once
//! per interval, with a `suppressed` field counting how many were held back
//! since.

use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

/// Re-exported for [`throttled!`](crate::throttled), so callers need no
/// `tracing` dependency of their own for it.
pub use tracing;

/// At most one event per key per this long.
pub const INTERVAL: Duration = Duration::from_secs(60);

struct Seen {
    last_logged: Instant,
    held_back: u64,
}

#[cfg(not(any(test, feature = "test-support")))]
static SEEN: LazyLock<Mutex<HashMap<String, Seen>>> = LazyLock::new(Default::default);

// Under test, each thread (each test) has its own throttle state, so one
// test's event can't hold back another's: every test sees the first event
// of each kind logged, whatever ran before it in the same binary.
#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static SEEN: LazyLock<Mutex<HashMap<String, Seen>>> = LazyLock::new(Default::default);
}

/// Logs a `tracing` event unless one with the same key was logged less than
/// [`INTERVAL`] ago. The key names the kind of problem and what it is about
/// (for example `format!("scan-failed:{tenant}")`), not the full message, so
/// events that differ only in detail are grouped. The event gets two extra
/// fields: `throttle_key` and `suppressed`, the number held back since the
/// last one.
///
/// ```ignore
/// shared::throttled!(format!("tick-failed:{network:?}"), warn, network = ?network, error = %e, "scan tick failed");
/// ```
#[macro_export]
macro_rules! throttled {
    ($key:expr, $level:ident, $($rest:tt)+) => {{
        let key = $key;
        if let Some(suppressed) = $crate::log::admit(&key) {
            $crate::log::tracing::$level!(throttle_key = %key, suppressed, $($rest)+);
        }
    }};
}

/// Whether an event with `key` may be logged now, and if so how many were
/// held back since the last one.
pub fn admit(key: &str) -> Option<u64> {
    admit_at(key, Instant::now())
}

fn admit_at(key: &str, now: Instant) -> Option<u64> {
    #[cfg(not(any(test, feature = "test-support")))]
    return admit_in(&SEEN, key, now);
    #[cfg(any(test, feature = "test-support"))]
    return SEEN.with(|seen| admit_in(seen, key, now));
}

fn admit_in(seen: &Mutex<HashMap<String, Seen>>, key: &str, now: Instant) -> Option<u64> {
    let mut seen = seen.lock();
    // Bounded: a key per tenant is fine, a key per transaction would not be.
    if seen.len() > 10_000 {
        seen.retain(|_, s| now.duration_since(s.last_logged) < INTERVAL);
    }
    match seen.get_mut(key) {
        Some(s) if now.duration_since(s.last_logged) < INTERVAL => {
            s.held_back += 1;
            None
        }
        Some(s) => {
            let held_back = s.held_back;
            s.last_logged = now;
            s.held_back = 0;
            Some(held_back)
        }
        None => {
            seen.insert(key.to_string(), Seen { last_logged: now, held_back: 0 });
            Some(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repeated_event_is_admitted_once_per_interval_with_the_count_held_back() {
        let start = Instant::now();
        let key = "test:repeated";
        assert_eq!(admit_at(key, start), Some(0));
        assert_eq!(admit_at(key, start + Duration::from_secs(1)), None);
        assert_eq!(admit_at(key, start + Duration::from_secs(30)), None);
        assert_eq!(admit_at(key, start + INTERVAL + Duration::from_secs(1)), Some(2));
        assert_eq!(admit_at("test:other", start + Duration::from_secs(2)), Some(0));
    }

    #[test]
    fn the_macro_builds_with_fields_and_a_formatted_message() {
        let network = "stagenet";
        let e = "timeout";
        crate::throttled!(format!("test:macro:{network}"), warn, network, error = %e, "scan tick failed on {network}");
        crate::throttled!("test:macro:plain", error, "no fields");
    }
}
