//! The current time, in the two units the services store: Unix seconds
//! (every `*_at` column and field) and Unix nanoseconds (log records). One
//! clock for both services, so no copy can drift to another unit or panic.

/// Seconds since the Unix epoch. A clock set before 1970 reads as 0 rather
/// than panicking in a request or a loop.
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Nanoseconds since the Unix epoch, as log records carry them; saturates
/// rather than wrapping.
pub fn now_unix_nanos() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seconds_and_nanoseconds_agree() {
        let (seconds, nanos) = (now_unix(), now_unix_nanos());
        assert!(seconds > 1_700_000_000);
        assert!((nanos / 1_000_000_000 - seconds).abs() <= 1);
    }
}
