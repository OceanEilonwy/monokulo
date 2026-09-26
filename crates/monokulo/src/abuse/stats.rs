//! Challenge counts over the last hour, for operators on the status page
//! (step 9e): how many challenges were issued, solved and refused (a proof
//! or wait token that didn't redeem, or a request past the hard limit).
//! Sixty one-minute buckets in memory; nothing is stored.

use std::sync::Mutex;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HourCounts {
    pub issued: u64,
    pub solved: u64,
    pub refused: u64,
}

#[derive(Clone, Copy)]
pub enum Event {
    Issued,
    Solved,
    Refused,
}

pub struct ChallengeStats {
    /// `(minute, counts)`, indexed by `minute % 60`.
    buckets: Mutex<[(i64, HourCounts); 60]>,
}

impl Default for ChallengeStats {
    fn default() -> Self {
        ChallengeStats { buckets: Mutex::new([(i64::MIN, HourCounts::default()); 60]) }
    }
}

impl ChallengeStats {
    pub fn record(&self, event: Event, now: i64) {
        let minute = now.div_euclid(60);
        let mut buckets = self.buckets.lock().unwrap();
        let bucket = &mut buckets[minute.rem_euclid(60) as usize];
        if bucket.0 != minute {
            *bucket = (minute, HourCounts::default());
        }
        match event {
            Event::Issued => bucket.1.issued += 1,
            Event::Solved => bucket.1.solved += 1,
            Event::Refused => bucket.1.refused += 1,
        }
    }

    pub fn last_hour(&self, now: i64) -> HourCounts {
        let minute = now.div_euclid(60);
        let buckets = self.buckets.lock().unwrap();
        buckets.iter().filter(|(m, _)| *m <= minute && minute.saturating_sub(*m) < 60).fold(HourCounts::default(), |sum, (_, c)| HourCounts {
            issued: sum.issued + c.issued,
            solved: sum.solved + c.solved,
            refused: sum.refused + c.refused,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_cover_exactly_the_last_hour() {
        let stats = ChallengeStats::default();
        stats.record(Event::Issued, 1_000_000);
        stats.record(Event::Issued, 1_000_000 + 30 * 60);
        stats.record(Event::Solved, 1_000_000 + 30 * 60);
        stats.record(Event::Refused, 1_000_000 + 59 * 60);
        assert_eq!(stats.last_hour(1_000_000 + 59 * 60), HourCounts { issued: 2, solved: 1, refused: 1 });
        assert_eq!(stats.last_hour(1_000_000 + 61 * 60), HourCounts { issued: 1, solved: 1, refused: 1 });
        assert_eq!(stats.last_hour(1_000_000 + 200 * 60), HourCounts::default());
    }
}
