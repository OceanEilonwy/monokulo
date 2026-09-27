//! Throttled logging for errors that repeat every tick (admin_settings_v2.md
//! task 7.13). A node or key-custody backend that is down produces the same
//! error every second; logging each one buries everything else. `throttled`
//! logs a given kind of message at most once per interval, with a count of
//! how many were held back since.

use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

/// At most one message per key per this long.
pub const INTERVAL: Duration = Duration::from_secs(60);

struct Seen {
    last_logged: Instant,
    held_back: u64,
}

static SEEN: LazyLock<Mutex<HashMap<String, Seen>>> = LazyLock::new(Default::default);

/// Logs `message` to stderr unless one with the same `key` was logged less
/// than `INTERVAL` ago. `key` names the kind of problem and what it is about
/// (for example `"scan-failed:tenant_abc"`), not the full message, so
/// messages that differ only in detail are grouped. Returns whether it
/// logged.
pub fn throttled(key: &str, message: impl std::fmt::Display) -> bool {
    throttled_at(key, message, Instant::now())
}

fn throttled_at(key: &str, message: impl std::fmt::Display, now: Instant) -> bool {
    let mut seen = SEEN.lock();
    // Bounded: a key per tenant is fine, a key per transaction would not be.
    if seen.len() > 10_000 {
        seen.retain(|_, s| now.duration_since(s.last_logged) < INTERVAL);
    }
    match seen.get_mut(key) {
        Some(s) if now.duration_since(s.last_logged) < INTERVAL => {
            s.held_back += 1;
            false
        }
        Some(s) => {
            if s.held_back > 0 {
                eprintln!("{message} (and {} more like this in the last {:?})", s.held_back, now.duration_since(s.last_logged));
            } else {
                eprintln!("{message}");
            }
            s.last_logged = now;
            s.held_back = 0;
            true
        }
        None => {
            eprintln!("{message}");
            seen.insert(key.to_string(), Seen { last_logged: now, held_back: 0 });
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repeated_message_is_logged_once_per_interval() {
        let start = Instant::now();
        let key = "test:repeated";
        assert!(throttled_at(key, "first", start));
        assert!(!throttled_at(key, "again", start + Duration::from_secs(1)));
        assert!(!throttled_at(key, "again", start + Duration::from_secs(30)));
        assert!(throttled_at(key, "later", start + INTERVAL + Duration::from_secs(1)));
        assert!(throttled_at("test:other", "a different kind", start + Duration::from_secs(2)));
    }
}
