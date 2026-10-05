//! Oracles shared by ordinary properties and optional coverage-guided fuzzers.
//! Enabled only for tests or the `fuzzing` feature. No network or global state.
use crate::work::{retry::Retry, scheduler::Scheduler, ScanTuning, Tier, TierOutcome, Wait};
use std::time::Duration;

pub fn schedule(data: &[u8]) {
    let budget = Duration::from_nanos(u64::from(data.first().copied().unwrap_or(0)) * 100);
    let mut now = Duration::from_nanos(u64::from(data.get(1).copied().unwrap_or(0)) * 100);
    let Ok(mut machine) = Scheduler::new(&ScanTuning::DEFAULT, budget) else {
        return;
    };
    let mut previous = None;
    let mut closed = [false; 5];
    let mut count = [0u32; 5];
    for pair in data.get(2..).unwrap_or_default().chunks(2).take(1024) {
        let Some(effect) = machine.request(now) else {
            break;
        };
        assert!(
            !closed[effect.tier().index()],
            "a closed tier was scheduled again"
        );
        assert!(
            machine.request(now).is_none(),
            "two effects were issued concurrently"
        );
        if let Some(old) = previous {
            assert!(
                !machine.complete(old, Some(TierOutcome::Idle)),
                "a stale completion was accepted"
            );
        }
        let outcome = match pair[0] % 4 {
            0 => None,
            1 => Some(TierOutcome::Idle),
            2 => Some(TierOutcome::Blocked(Wait::NodeFailed)),
            _ => Some(TierOutcome::Failed),
        };
        assert!(
            machine.complete(effect, outcome),
            "the current completion was rejected"
        );
        count[effect.tier().index()] += 1;
        closed[effect.tier().index()] = outcome.is_some();
        previous = Some(effect);
        now = now.saturating_add(Duration::from_nanos(u64::from(
            pair.get(1).copied().unwrap_or(0),
        )));
    }
    // Always finish a generated prefix; every tier must get its progress floor.
    while let Some(effect) = machine.request(now) {
        assert!(
            !closed[effect.tier().index()],
            "a closed tier was scheduled again"
        );
        assert!(
            machine.complete(effect, Some(TierOutcome::Idle)),
            "the current completion was rejected"
        );
        count[effect.tier().index()] += 1;
        closed[effect.tier().index()] = true;
    }
    for tier in Tier::ALL {
        assert!(count[tier.index()] > 0, "a tier was starved");
        assert_eq!(
            machine.steps()[tier],
            count[tier.index()],
            "completed units were miscounted"
        );
    }
}

pub fn resources(values: [u64; 8]) {
    let tuning = ScanTuning::DEFAULT;
    let link = Some(crate::link::LinkCost {
        rtt_secs: f64::from_bits(values[3]),
        ttfb_per_block_secs: f64::from_bits(values[4]),
        rate_bytes_per_sec: f64::from_bits(values[5]),
    });
    let cap = values[0];
    let remaining = values[1];
    let avg = f64::from_bits(values[2]);
    let chunk = crate::scanner::next_scan_chunk(&tuning, cap, link, avg, remaining);
    let page = crate::scanner::next_page(
        &tuning,
        cap,
        link,
        avg,
        Some(f64::from_bits(values[6])),
        remaining,
    );
    assert!(
        (1..=tuning.chunk_max_blocks.min(remaining.max(1))).contains(&chunk.blocks),
        "block request exceeded its bounds"
    );
    assert!(
        (1..=crate::scanner::PAGE_MAX_TXS.min(remaining.max(1))).contains(&page.blocks),
        "transaction page exceeded its bounds"
    );
    let retry = Retry::failed(
        Some(Retry {
            failures: values[7] as u32,
            last_failure: Duration::ZERO,
            retry_at: Duration::ZERO,
        }),
        Duration::from_secs(values[0]),
    );
    assert!(
        retry.retry_at >= retry.last_failure,
        "retry deadline wrapped backwards"
    );
    let timeout = crate::link::timeout_for(Duration::from_secs(values[7]));
    assert!(
        (crate::link::MIN_TIMEOUT..=crate::link::MAX_TIMEOUT).contains(&timeout),
        "timeout escaped its limits"
    );
}

pub fn inputs(data: &[u8]) {
    let text = String::from_utf8_lossy(data);
    if let Ok(cpus) = crate::threads::parse_cpu_list(&text) {
        assert!(cpus.len() <= 1024, "CPU list exceeds allocation limit");
        assert!(
            cpus.windows(2).all(|w| w[0] < w[1]),
            "CPU list is not sorted and unique"
        );
        let canonical = cpus
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(
            crate::threads::parse_cpu_list(&canonical),
            Ok(cpus),
            "CPU list round trip changed its meaning"
        );
    }
    if let Ok(node) = serde_json::from_slice::<crate::settings::MoneroNodeSetting>(data) {
        if let Ok(encoded) = serde_json::to_vec(&node) {
            assert_eq!(
                serde_json::from_slice::<crate::settings::MoneroNodeSetting>(&encoded).ok(),
                Some(node.clone()),
                "node setting serialization changed its meaning"
            );
        }
        let _ = crate::settings::check_node(&Some(live_settings::Json(node)));
    }
    let _ = crate::engine_settings::MONERO_NODE_MAINNET.parse(&text);
    let _ = crate::engine_settings::SERVER_CPUS.parse(&text);
    let _ = crate::engine_settings::PAYMENT_CONFIRMATIONS_REQUIRED.parse(&text);
    let _ = crate::engine_settings::PAYMENT_REORG_CHECK_DEPTH.parse(&text);
    let _ = crate::webhook_delivery::is_reserved_webhook_header(&text);
    // These identifiers deliberately preserve arbitrary strings. Test the
    // application serialization contract, including NUL and Unicode.
    let id = crate::store::OrderId::new(text.as_ref());
    if let Ok(encoded) = serde_json::to_string(&id) {
        assert_eq!(
            serde_json::from_str::<crate::store::OrderId>(&encoded).ok(),
            Some(id),
            "identifier serialization changed its meaning"
        );
    }
    let _ = url::Url::parse(&text);
    let _ = crate::webhook_sign::verify_signature("fuzz-secret", data, &text, 0);
}

#[cfg(test)]
#[path = "input_properties.rs"]
#[cfg_attr(coverage_nightly, coverage(off))]
mod properties;

/// Exercise production dispatch with bounded arrivals, closures and service.
///
/// Actual channel admission and abandoned callers are checked separately
/// against the real worker in queue properties.
pub fn queue(data: &[u8]) {
    use crate::store::{
        db::{Class, QUEUE_CAPACITY},
        dispatch::Dispatch,
    };
    let mut policy = Dispatch::default();
    let mut buffers: [std::collections::VecDeque<usize>; 3] =
        std::array::from_fn(|_| std::collections::VecDeque::new());
    let mut accepted: [Vec<usize>; 3] = std::array::from_fn(|_| Vec::new());
    let mut completed: [Vec<usize>; 3] = std::array::from_fn(|_| Vec::new());
    let mut closed = [false; 3];
    let mut previous = 2;
    let serve = |policy: &mut Dispatch,
                 buffers: &mut [std::collections::VecDeque<usize>; 3],
                 completed: &mut [Vec<usize>; 3],
                 previous: &mut usize| {
        let expected = (1..=3)
            .map(|offset| (*previous + offset) % 3)
            .find(|&i| !buffers[i].is_empty());
        let actual = policy
            .order()
            .into_iter()
            .find(|class| !buffers[*class as usize].is_empty());
        assert_eq!(
            actual.map(|c| c as usize),
            expected,
            "dispatch violated class rotation"
        );
        if let Some(class) = actual {
            let i = class as usize;
            if let Some(id) = buffers[i].pop_front() {
                completed[i].push(id);
            }
            policy.served(class);
            *previous = i;
        }
    };
    for (id, event) in data.chunks(2).take(2048).enumerate() {
        let class = usize::from(event.get(1).copied().unwrap_or(0)) % Class::ALL.len();
        match event[0] % 3 {
            0 => {
                if !closed[class] && buffers[class].len() < QUEUE_CAPACITY {
                    buffers[class].push_back(id);
                    accepted[class].push(id);
                }
            }
            1 => serve(&mut policy, &mut buffers, &mut completed, &mut previous),
            _ => closed[class] = true,
        }
        assert!(
            buffers.iter().all(|q| q.len() <= QUEUE_CAPACITY),
            "class queue exceeded capacity"
        );
    }
    while buffers.iter().any(|q| !q.is_empty()) {
        serve(&mut policy, &mut buffers, &mut completed, &mut previous);
    }
    assert_eq!(
        completed, accepted,
        "accepted jobs were lost, reordered, or executed twice"
    );
}

/// Exercise mempool ownership, eviction, scan windows and cache budgets.
pub fn mempool(data: &[u8]) {
    crate::work::explore_mempool(data);
}

/// Check status derivation and metamorphic guarantees against aggregate evidence.
pub fn status(data: &[u8]) {
    crate::status::exploration::explore(data);
}

/// Run real scanner/database histories against an independent money model.
pub fn history(data: &[u8]) {
    crate::work::history::explore(data);
}
