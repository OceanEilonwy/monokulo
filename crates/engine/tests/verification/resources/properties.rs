use super::*;
use crate::link::{Link, LinkCost, MAX_TIMEOUT, MIN_TIMEOUT};
use crate::property_support::config;
use crate::scanner::{next_page, next_scan_chunk};
use crate::work::ScanTuning;
use proptest::prelude::*;
use std::time::Duration;

proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn timeout_bounds_hold_for_extreme_measurements(secs in any::<u64>(), blocks in any::<u64>(), bytes in any::<u64>()) {
        let link = Link::new();
        link.record_small(Duration::from_secs(secs));
        link.record_blocks(blocks, bytes as usize, Duration::from_secs(secs), Duration::from_nanos(1));
        let timeout = link.timeout_for_blocks(blocks);
        prop_assert!((MIN_TIMEOUT..=MAX_TIMEOUT).contains(&timeout));
        prop_assert!((MIN_TIMEOUT..=MAX_TIMEOUT).contains(&crate::link::timeout_for(Duration::from_secs(secs))));
    }

    #[test]
    fn request_sizes_remain_bounded_across_numeric_inputs(
        cap in any::<u64>(), remaining in any::<u64>(), avg in any::<f64>(),
        rtt in any::<f64>(), work in any::<f64>(), rate in any::<f64>(), cpu in any::<f64>(),
    ) {
        let tuning = ScanTuning::DEFAULT;
        let link = Some(LinkCost { rtt_secs: rtt, ttfb_per_block_secs: work, rate_bytes_per_sec: rate });
        let chunk = next_scan_chunk(&tuning, cap, link, avg, remaining);
        let page = next_page(&tuning, cap, link, avg, Some(cpu), remaining);
        prop_assert!((1..=tuning.chunk_max_blocks.min(remaining.max(1))).contains(&chunk.blocks));
        prop_assert!((1..=(crate::scanner::PAGE_MAX_TXS).min(remaining.max(1))).contains(&page.blocks));
    }

    #[test]
    fn larger_resources_and_faster_links_do_not_reduce_request_size(
        cap in 1u64..1_000_000_000, extra in 0u64..1_000_000_000, avg in 1.0f64..1_000_000.0,
        rtt in 0.0f64..5.0, work in 0.0f64..1.0, rate in 1000.0f64..1_000_000_000.0,
        cpu in 0.000_001f64..10.0, remaining in 1u64..10_000,
    ) {
        let tuning = ScanTuning::DEFAULT;
        let slow = Some(LinkCost { rtt_secs: rtt, ttfb_per_block_secs: work, rate_bytes_per_sec: rate });
        let fast = Some(LinkCost { rtt_secs: rtt/2.0, ttfb_per_block_secs: work/2.0, rate_bytes_per_sec: rate*2.0 });
        prop_assert!(next_scan_chunk(&tuning, cap+extra, fast, avg, remaining).blocks >= next_scan_chunk(&tuning, cap, slow, avg, remaining).blocks);
        prop_assert!(next_page(&tuning, cap+extra, fast, avg, Some(cpu/2.0), remaining).blocks >= next_page(&tuning, cap, slow, avg, Some(cpu), remaining).blocks);
    }

    #[test]
    fn progress_histories_survive_clock_jumps_and_extreme_counters(
        events in prop::collection::vec((any::<i64>(), any::<u64>(), any::<u64>()), 1..256),
    ) {
        let mut progress = ScanProgress::default();
        let mut total = 0u64;
        for (now, bytes, scans) in events {
            progress.start_block(scans, now);
            progress.fetched_block(scans, bytes);
            progress.finish_block(scans, now);
            progress.spent(now, 0.5, 1.0, scans);
            progress.discarded(bytes, now);
            progress.cache_bytes(bytes, now);
            progress.want_headers_first(now, HeadersFirstReason::FailedRequest);
            total = total.saturating_add(bytes);
            let report = progress.report(now);
            prop_assert_eq!(progress.discarded_cache_bytes, total);
            prop_assert!(progress.recent.len() <= RECENT_BLOCKS);
            prop_assert!(progress.time.len() <= MAX_SAMPLES);
            prop_assert!(progress.discarded_recent.len() <= MAX_SAMPLES);
            prop_assert!(report.scan_secs_recent.is_finite());
            prop_assert!(progress.secs_per_tx_scan().is_none_or(f64::is_finite));
        }
    }
}

#[test]
fn stopped_and_backwards_clocks_cannot_grow_telemetry_without_bound() {
    let mut p = ScanProgress::default();
    for i in 0..MAX_SAMPLES * 2 {
        p.spent(-(i as i64), 1.0, 1.0, 1);
        p.discarded(1, -(i as i64));
    }
    assert_eq!(p.time.len(), MAX_SAMPLES);
    assert_eq!(p.discarded_recent.len(), MAX_SAMPLES);
}

#[test]
fn maximum_timeout_and_wall_clock_boundaries_do_not_panic() {
    assert_eq!(crate::link::timeout_for(Duration::MAX), MAX_TIMEOUT);
    let mut p = ScanProgress::default();
    p.start_block(1, i64::MIN);
    p.finish_block(1, i64::MAX);
    p.want_headers_first(i64::MAX, HeadersFirstReason::FailedRequest);
    p.discarded(u64::MAX, i64::MIN);
    p.discarded(u64::MAX, i64::MAX);
    assert_eq!(p.discarded_cache_bytes, u64::MAX);
    let _ = p.report(i64::MIN);
    let _ = p.report(i64::MAX);
}

fn persisted_config(config: proptest::test_runner::Config) -> proptest::test_runner::Config {
    crate::property_support::persist(
        config,
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/proptest-regressions/resource_properties.txt"
        ),
    )
}
