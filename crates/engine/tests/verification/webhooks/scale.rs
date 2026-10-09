//! Thousands of real HTTP deliveries retain per-order FIFO, stable signed
//! payloads, bounded concurrency, and recovery behind failing merchants.
use super::*;

async fn backlog(healthy: usize, failing: usize, orders: usize, events: usize) {
    let good = Server::new(204).await;
    let bad = Server::new(503).await;
    let mut world = World::new(true);
    let failed = world.seed(&bad.url, failing, orders, events, "{}");
    let successful = world.seed(&good.url, healthy, orders, events, "{}");
    let successful_ids: HashSet<i64> = successful.iter().flatten().flatten().copied().collect();
    let failed_ids: HashSet<i64> = failed.iter().flatten().flatten().copied().collect();
    let bound = (healthy + failing) * orders * events + events + 10;
    let initial_now = crate::now_unix();
    for tick in 0..bound {
        world
            .tick(initial_now, 20, Duration::from_secs(10))
            .await
            .unwrap();
        let rows = world.rows();
        if rows
            .iter()
            .filter(|r| successful_ids.contains(&r.id))
            .all(|r| r.delivered.is_some())
        {
            break;
        }
        assert!(
            tick + 1 < bound,
            "healthy tail starved behind failing merchants"
        );
    }
    let before = world.rows();
    assert!(
        before
            .iter()
            .any(|r| failed_ids.contains(&r.id) && r.attempts > 0 && r.status == Some(503)),
        "the failing endpoint never received work"
    );
    assert!(before
        .iter()
        .filter(|r| failed_ids.contains(&r.id))
        .all(|r| r.delivered.is_none() && r.gave_up.is_none()));
    // Reopening must retain the backlog, delivery state and retry deadlines.
    world.restart();
    bad.state.status.store(204, Ordering::SeqCst);
    let retry_now = before.iter().map(|r| r.due).max().unwrap() + 1;
    for tick in 0..bound {
        world
            .tick(retry_now, 20, Duration::from_secs(10))
            .await
            .unwrap();
        if world.rows().iter().all(|r| r.delivered.is_some()) {
            break;
        }
        assert!(tick + 1 < bound, "failed backlog never recovered");
    }
    let final_rows = world.rows();
    assert_eq!(final_rows.len(), (healthy + failing) * orders * events);
    assert!(final_rows
        .iter()
        .all(|r| r.delivered.is_some() && r.gave_up.is_none()));
    // Already delivered work must not be resent after a worker restart/tick.
    assert!(final_rows
        .iter()
        .filter(|r| successful_ids.contains(&r.id))
        .all(|r| r.attempts == 1));
    for (server, expected_orders) in [(&good, healthy * orders), (&bad, failing * orders)] {
        assert!(server.state.peak.load(Ordering::SeqCst) <= 16);
        let captured = server.state.captured.lock();
        let mut seen: HashMap<(usize, usize), usize> = HashMap::new();
        let mut payloads: HashMap<String, Vec<u8>> = HashMap::new();
        for captured in captured.iter() {
            let event = captured.headers["x-monokulo-event-id"].to_str().unwrap();
            let parts: Vec<usize> = event
                .strip_prefix("event_")
                .unwrap()
                .split('_')
                .map(|n| n.parse().unwrap())
                .collect();
            let next = seen.entry((parts[0], parts[1])).or_default();
            // An event may retry, but never overtake an earlier event.
            assert!(parts[2] == *next || (*next > 0 && parts[2] + 1 == *next));
            if parts[2] == *next {
                *next += 1;
            }
            let sig = captured.headers["x-monokulo-signature"].to_str().unwrap();
            let signed_at: i64 = sig[2..sig.find(',').unwrap()].parse().unwrap();
            assert!(crate::webhook_sign::verify_signature(
                "whsec_property",
                &captured.body,
                sig,
                signed_at
            ));
            for name in [
                "x-monokulo-signature",
                "x-monokulo-event",
                "x-monokulo-event-id",
                "content-type",
            ] {
                assert_eq!(captured.headers.get_all(name).iter().count(), 1);
            }
            assert_eq!(captured.headers["x-monokulo-event"], "order.paid");
            if let Some(previous) = payloads.insert(event.to_owned(), captured.body.clone()) {
                assert_eq!(previous, captured.body);
            }
        }
        assert_eq!(seen.len(), expected_orders);
        assert!(seen.values().all(|&n| n == events));
    }
    println!(
        "ENGINE_SCALE_RESULT webhook_tenants={} deliveries={} recovered={}",
        healthy + failing,
        final_rows.len(),
        failed_ids.len()
    );
}
proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn merchant_backlogs_drain_and_recover_without_reordering(healthy in 1usize..17,failing in 1usize..9,orders in 1usize..5,events in 1usize..4) {
        runtime().block_on(backlog(healthy,failing,orders,events));
    }
}
#[test]
#[ignore = "large deterministic scale package; `cargo xtask engine scale` runs it"]
fn thirteen_thousand_webhooks_recover_without_starving_healthy_merchants() {
    runtime().block_on(backlog(1025, 128, 4, 3));
}

fn persisted_config(config: proptest::test_runner::Config) -> proptest::test_runner::Config {
    crate::property_support::persist(
        config,
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/proptest-regressions/webhook_scale_properties.txt"
        ),
    )
}
