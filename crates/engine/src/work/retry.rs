//! Pure retry policy shared by the scanner and deterministic exploration.
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retry {
    pub failures: u32,
    pub last_failure: Duration,
    pub retry_at: Duration,
}

impl Retry {
    pub const FORGET_AFTER: Duration = Duration::from_secs(3600);

    pub fn failed(previous: Option<Self>, now: Duration) -> Self {
        let failures = previous.map_or(1, |p| p.failures.saturating_add(1));
        let delay = Self::delay(failures);
        Self {
            failures,
            last_failure: now,
            retry_at: now.saturating_add(delay),
        }
    }

    pub fn delay(failures: u32) -> Duration {
        if failures <= 2 {
            Duration::ZERO
        } else {
            Duration::from_secs((1u64 << failures.saturating_sub(2).min(6)).min(60))
        }
    }

    pub fn waiting(self, now: Duration) -> bool {
        now < self.retry_at
    }
    pub fn forgotten(self, now: Duration) -> bool {
        now.saturating_sub(self.last_failure) >= Self::FORGET_AFTER
    }
}
