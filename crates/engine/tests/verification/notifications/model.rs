//! Independent pending-permit model against actual notification waits.
#![expect(
    clippy::unwrap_used,
    reason = "isolated fuzz runtime and observable wait assertions"
)]
use crate::node_events::{NodeWakes, MIN_GAP};
use std::sync::atomic::Ordering;
use std::time::Duration;

pub(crate) fn explore(data: &[u8]) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .unwrap()
        .block_on(async {
            let mut networks: [NodeWakes; 3] = std::array::from_fn(|_| NodeWakes::default());
            let mut pending = [[false; 3]; 3];
            let mut counters = [[0u64; 2]; 3];
            for event in data.chunks(3).take(128) {
                let action = event[0] % 8;
                let network = usize::from(event.get(1).copied().unwrap_or(0) % 3);
                let value = event.get(2).copied().unwrap_or(0);
                let wakes = &networks[network];
                match action {
                    0 | 1 => {
                        for _ in 0..=value {
                            if action == 0 {
                                wakes.pool_changed();
                                pending[network][0] = true;
                            } else {
                                wakes.chain_changed();
                                pending[network][1] = true;
                                pending[network][2] = true;
                            }
                        }
                    }
                    2..=4 => {
                        let kind = usize::from(action - 2);
                        let interval = Duration::from_millis(u64::from(value));
                        let started = tokio::time::Instant::now();
                        let expected = pending[network][kind];
                        let actual = match kind {
                            0 => wakes.pool_or(interval).await,
                            1 => wakes.chain_or(interval).await,
                            _ => wakes.proof_or(interval).await,
                        };
                        assert_eq!(
                            actual, expected,
                            "notification was lost, duplicated, or crossed kinds/networks"
                        );
                        assert_eq!(
                            started.elapsed(),
                            if expected {
                                MIN_GAP.min(interval)
                            } else {
                                interval
                            },
                            "wait violated polling bound or burst minimum gap"
                        );
                        pending[network][kind] = false;
                        if kind < 2 {
                            counters[network][kind] += u64::from(actual);
                        }
                    }
                    5 => {
                        let mut future = Box::pin(wakes.pool_or(Duration::from_millis(100)));
                        assert!(
                            futures_util::poll!(future.as_mut()).is_pending(),
                            "wait skipped the minimum gap"
                        );
                        if !pending[network][0] && value & 1 != 0 {
                            tokio::time::advance(MIN_GAP).await;
                            assert!(
                                futures_util::poll!(future.as_mut()).is_pending(),
                                "unsignalled wait finished before polling deadline"
                            );
                            wakes.pool_changed();
                            pending[network][0] = true;
                        }
                        drop(future); // cancellation before gap or after waiter registration
                    }
                    6 => {
                        networks[network] = NodeWakes::default();
                        pending[network] = [false; 3];
                        counters[network] = [0; 2];
                    }
                    _ => {
                        let frame = data.get(usize::from(value)..).unwrap_or_default();
                        let expected = frame.iter().position(|&b| b == b':').and_then(|at| {
                            let topic = &frame[..at];
                            if topic == b"json-minimal-txpool_add" {
                                Some(crate::node_events::Announcement::Pool)
                            } else if topic == b"json-minimal-chain_main" {
                                Some(crate::node_events::Announcement::Chain)
                            } else {
                                None
                            }
                        });
                        assert_eq!(
                            crate::node_events::announcement(frame),
                            expected,
                            "topic decoder accepted a prefix or wrong topic"
                        );
                    }
                }
                for (i, wakes) in networks.iter().enumerate() {
                    assert_eq!(
                        wakes.pool_passes_woken.load(Ordering::Relaxed),
                        counters[i][0],
                        "pool wake counter differs from completed waits"
                    );
                    assert_eq!(
                        wakes.rounds_woken.load(Ordering::Relaxed),
                        counters[i][1],
                        "chain wake counter differs from completed waits"
                    );
                }
            }
        });
}
