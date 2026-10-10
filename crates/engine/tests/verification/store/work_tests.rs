use super::*;
use crate::store::{NewOrder, NewTenant};

fn file_store() -> (Store, String) {
    let path = std::env::temp_dir().join(format!("scanner_work_{}.db", uuid::Uuid::new_v4()));
    let path = path.to_string_lossy().into_owned();
    (Store::create_file(&path).unwrap(), path)
}

fn cleanup(path: &str) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{path}{suffix}"));
    }
}

fn tenant(store: &Store, network: &str) -> String {
    store
        .create_tenant(
            &NewTenant {
                key_custody_backend: "plain".into(),
                sealed_key_material: vec![0u8; 64],
                primary_address: format!("4{}", uuid::Uuid::new_v4().simple()),
                network: network.into(),
                confirmations_required: Some(10),
                order_expiry_seconds: None,
            },
            100,
        )
        .unwrap()
        .tenant
        .id
        .into_string()
}

fn order(store: &Store, tenant_id: &str, expires_at: i64) -> String {
    let index = store
        .allocate_minor_index(&TenantId::new(tenant_id.to_owned()))
        .unwrap();
    store
        .create_order(&NewOrder {
            idempotency_key: None,
            confirmations_required_override: None,
            tenant_id: tenant_id.into(),
            merchant_order_id: None,
            minor_index: index,
            address: format!("addr-{}", uuid::Uuid::new_v4().simple()),
            xmr_amount_piconero: 100,
            description: None,
            created_at: 100,
            expires_at,
        })
        .unwrap()
        .id
        .into_string()
}

fn pay(store: &Store, order_id: &str, txid: &str, height: Option<i64>) -> i64 {
    store
        .record_payment_match(
            &OrderId::new(order_id.to_owned()),
            txid,
            0,
            10,
            "[\"ki\"]",
            100,
            height,
            None,
        )
        .unwrap();
    store
        .get_all_payments(&OrderId::new(order_id.to_owned()))
        .unwrap()
        .into_iter()
        .find(|p| p.txid == txid)
        .unwrap()
        .id
}

fn work(store: &Store, network: &str) -> Vec<i64> {
    store
        .due_reorg_candidates(
            shared::network::parse_network(network).unwrap(),
            i64::MAX,
            1000,
        )
        .unwrap()
        .into_iter()
        .map(|c| c.payment.id)
        .collect()
}

/// Collection covers confirmed payments at or above the fork and every
/// unconfirmed one, on this network only, in pages that survive a
/// restart, and never a payment recorded after the job opened.
#[test]
fn a_reorg_job_collects_its_candidates_in_pages_that_survive_a_restart() {
    let (store, path) = fixture();
    let (main_order, other_order) = (&store.1.clone(), &store.2.clone());
    let s = &store.0;
    let below = pay(s, main_order, "below", Some(9));
    let at = pay(s, main_order, "at", Some(10));
    let above: Vec<i64> = (0..5)
        .map(|i| pay(s, main_order, &format!("above{i}"), Some(11 + i)))
        .collect();
    let unconfirmed = pay(s, main_order, "pool", None);
    let _other_network = pay(s, other_order, "other", Some(12));
    assert_eq!(
        s.open_reorg_job(monero::Network::Mainnet, 10, 1000)
            .unwrap(),
        OpenedReorg::Created
    );
    let late = pay(s, main_order, "late", Some(12));

    // Two candidates per page, restarting the process between pages.
    assert!(matches!(
        s.collect_reorg_candidates(monero::Network::Mainnet, 2, 1001)
            .unwrap(),
        ReorgPhase::CollectConfirmed {
            after_height: _,
            after_id: _
        }
    ));
    let s = reopen(store, &path);
    let mut phase = s
        .collect_reorg_candidates(monero::Network::Mainnet, 2, 1002)
        .unwrap();
    while phase != ReorgPhase::Process {
        phase = s
            .collect_reorg_candidates(monero::Network::Mainnet, 2, 1003)
            .unwrap();
    }
    let mut collected = work(&s, "mainnet");
    collected.sort_unstable();
    let mut expected = vec![at, unconfirmed];
    expected.extend(&above);
    expected.sort_unstable();
    assert_eq!(collected, expected);
    assert!(!collected.contains(&below) && !collected.contains(&late));
    assert!(work(&s, "stagenet").is_empty());
    drop(s);
    cleanup(&path);
}

/// A deeper fork found while a job is open lowers its fork and collects
/// again from there; a shallower one changes nothing.
#[test]
fn a_deeper_fork_widens_the_open_job_and_a_shallower_one_does_not() {
    let (store, path) = fixture();
    let s = &store.0;
    let deep = pay(s, &store.1, "deep", Some(5));
    let shallow = pay(s, &store.1, "shallow", Some(10));
    s.open_reorg_job(monero::Network::Mainnet, 8, 1000).unwrap();
    while s
        .collect_reorg_candidates(monero::Network::Mainnet, 10, 1000)
        .unwrap()
        != ReorgPhase::Process
    {}
    assert_eq!(work(s, "mainnet"), vec![shallow]);
    assert_eq!(
        s.open_reorg_job(monero::Network::Mainnet, 9, 1001).unwrap(),
        OpenedReorg::Covered
    );
    assert_eq!(
        s.open_reorg_job(monero::Network::Mainnet, 4, 1002).unwrap(),
        OpenedReorg::Deepened { from: 8 }
    );
    while s
        .collect_reorg_candidates(monero::Network::Mainnet, 10, 1003)
        .unwrap()
        != ReorgPhase::Process
    {}
    let mut collected = work(s, "mainnet");
    collected.sort_unstable();
    assert_eq!(collected, vec![deep, shallow]);
    drop(store);
    cleanup(&path);
}

/// A failed candidate backs off behind the others; a completed one is
/// gone; the job finishes only once nothing is left.
#[test]
fn a_failed_candidate_waits_behind_the_others_and_blocks_the_rewind_until_done() {
    let (store, path) = fixture();
    let s = &store.0;
    let first = pay(s, &store.1, "first", Some(10));
    let second = pay(s, &store.1, "second", Some(11));
    for h in 8..=12 {
        s.set_scanned_block(monero::Network::Mainnet, h, &format!("old{h}"))
            .unwrap();
    }
    s.open_reorg_job(monero::Network::Mainnet, 10, 1000)
        .unwrap();
    while s
        .collect_reorg_candidates(monero::Network::Mainnet, 10, 1000)
        .unwrap()
        != ReorgPhase::Process
    {}
    assert!(s.settlement_frozen(monero::Network::Mainnet).unwrap());
    assert!(!s.settlement_frozen(monero::Network::Stagenet).unwrap());

    // The first retries are due at once; the third failure waits.
    for _ in 0..3 {
        s.defer_reorg_candidate(monero::Network::Mainnet, first, 1000)
            .unwrap();
    }
    assert_eq!(
        (
            reorg_retry_delay(1),
            reorg_retry_delay(2),
            reorg_retry_delay(3)
        ),
        (0, 0, 1)
    );
    let due: Vec<i64> = s
        .due_reorg_candidates(monero::Network::Mainnet, 1000, 10)
        .unwrap()
        .iter()
        .map(|c| c.payment.id)
        .collect();
    assert_eq!(due, vec![second], "the failed one waits");
    assert!(matches!(
        s.finish_reorg(monero::Network::Mainnet, 10, Some((9, "old9"))),
        Err(StoreError::NotFound)
    ));
    s.complete_reorg_candidate(monero::Network::Mainnet, second)
        .unwrap();
    let later = s
        .due_reorg_candidates(monero::Network::Mainnet, 1000 + reorg_retry_delay(3), 10)
        .unwrap();
    assert_eq!(later.len(), 1);
    assert_eq!((later[0].payment.id, later[0].attempts), (first, 3));
    s.complete_reorg_candidate(monero::Network::Mainnet, first)
        .unwrap();

    assert!(
        matches!(
            s.finish_reorg(monero::Network::Mainnet, 9, Some((8, "old8"))),
            Err(StoreError::NotFound)
        ),
        "wrong fork"
    );
    s.finish_reorg(monero::Network::Mainnet, 10, Some((9, "old9")))
        .unwrap();
    assert_eq!(
        s.max_scanned_height(monero::Network::Mainnet).unwrap(),
        Some(9)
    );
    assert!(s.reorg_job(monero::Network::Mainnet).unwrap().is_none());
    assert!(!s.settlement_frozen(monero::Network::Mainnet).unwrap());
    drop(store);
    cleanup(&path);
}

/// When the fork is at or below the oldest stored block, the rewind
/// leaves the ancestor anchored instead of an empty window (which would
/// read as "never scanned" and re-seed at the tip).
#[test]
fn a_rewind_that_empties_the_window_keeps_the_ancestor_and_clamps_cursors() {
    let (store, path) = fixture();
    let s = &store.0;
    s.set_scanned_block(monero::Network::Mainnet, 20, "old20")
        .unwrap();
    s.set_scanned_block(monero::Network::Mainnet, 21, "old21")
        .unwrap();
    s.execute_raw_for_test("UPDATE tenants SET scanned_through_height = 21")
        .unwrap();
    s.open_reorg_job(monero::Network::Mainnet, 20, 1000)
        .unwrap();
    while s
        .collect_reorg_candidates(monero::Network::Mainnet, 10, 1000)
        .unwrap()
        != ReorgPhase::Process
    {}
    s.finish_reorg(monero::Network::Mainnet, 20, Some((19, "new19")))
        .unwrap();
    assert_eq!(
        s.scanned_blocks_between(monero::Network::Mainnet, 0, 100)
            .unwrap(),
        vec![(19, "new19".to_owned())]
    );
    let cursors: Vec<Option<u64>> = s
        .list_active_tenants()
        .unwrap()
        .into_iter()
        .filter(|t| t.network == "mainnet")
        .map(|t| t.scanned_through_height)
        .collect();
    assert!(cursors.iter().all(|c| *c == Some(19)), "{cursors:?}");
    drop(store);
    cleanup(&path);
}

fn staged_reorg_cleanup(network: monero::Network, fork: u64, restart: bool) {
    let (mut store, path) = file_store();
    let other = if network == monero::Network::Mainnet {
        monero::Network::Stagenet
    } else {
        monero::Network::Mainnet
    };
    let mut staged = Vec::new();
    for (net, height) in [
        (network, fork - 1),
        (network, fork),
        (network, fork + 1),
        (other, fork + 1),
    ] {
        let t = TenantId::new(tenant(&store, shared::network::network_str(net)));
        let o = order(&store, t.as_str(), 100_000);
        let hash = format!("{net:?}-{height}");
        store
            .save_block_checkpoint(
                net,
                &t,
                &BlockCheckpoint {
                    height,
                    hash: hash.clone(),
                    next_tx: 1,
                },
            )
            .unwrap();
        store.conn.execute("INSERT INTO partial_block_matches(network,tenant_id,order_id,txid,output_index,amount_piconero,key_images_json,seen_at_utc,output_key) VALUES(?1,?2,?3,'staged-tx',0,17,'[]',1000,'output-key')", params![shared::network::SqlNetwork(net),t,o]).unwrap();
        staged.push((net, t, height, hash));
    }
    store.open_reorg_job(network, fork, 1000).unwrap();
    while store.collect_reorg_candidates(network, 1, 1000).unwrap() != ReorgPhase::Process {}
    if restart {
        drop(store);
        store = Store::open_file(&path).unwrap();
    }
    store
        .finish_reorg(network, fork, Some((fork - 1, "ancestor")))
        .unwrap();
    // Completion is durable and only discards staging on the replaced branch.
    drop(store);
    let store = Store::open_file(&path).unwrap();
    assert!(store.reorg_job(network).unwrap().is_none());
    for (net, t, height, hash) in staged {
        let discarded = net == network && height >= fork;
        assert_eq!(
            store.block_checkpoint(net, &t).unwrap().is_none(),
            discarded,
            "BOUNDARY: reorg-staging-cleanup"
        );
        let count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM partial_block_matches WHERE network=?1 AND tenant_id=?2",
                params![shared::network::SqlNetwork(net), t],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            count,
            i64::from(!discarded),
            "BOUNDARY: reorg-staging-matches"
        );
        let matches = store.take_staged_payments(net, &t, &hash).unwrap();
        assert_eq!(matches.len(), usize::from(!discarded));
        if !discarded {
            assert_eq!(matches[0].amount_piconero, 17);
        }
    }
    drop(store);
    cleanup(&path);
}

#[test]
fn every_reorg_staging_cleanup_boundary_survives_reopen() {
    for net in [
        monero::Network::Mainnet,
        monero::Network::Testnet,
        monero::Network::Stagenet,
    ] {
        for fork in [1, 3, 1000] {
            for restart in [false, true] {
                staged_reorg_cleanup(net, fork, restart);
            }
        }
    }
    println!("ENGINE_BOUNDARY_HITS {{\"reorg-staging-reopen-schedules\":18,\"reorg-staging-invalidated-at-or-above-fork\":36,\"reorg-staging-preserved-below-fork-or-other-network\":36}}");
}

#[path = "work_properties.rs"]
mod properties;

/// An open order is due at its deadline; recomputing it schedules the
/// next point its status can move; a terminal one is unscheduled.
#[test]
fn open_orders_are_due_by_deadline_and_height_and_terminal_ones_never() {
    let (store, path) = fixture();
    let s = &store.0;
    let expiring = order(s, &store.3, 5_000);
    assert_eq!(
        s.due_order_ids(monero::Network::Mainnet, 4_999, 0, 10)
            .unwrap(),
        Vec::<String>::new()
    );
    assert_eq!(
        s.due_order_ids(monero::Network::Mainnet, 5_000, 0, 10)
            .unwrap(),
        vec![expiring.clone()]
    );

    // Fully paid at height 50 with ten confirmations needed: due again
    // each block until it settles, then never.
    s.record_payment_match(
        &OrderId::new(expiring.clone()),
        "tx",
        0,
        100,
        "[\"ki\"]",
        1_000,
        Some(50),
        None,
    )
    .unwrap();
    s.recompute_order_status(&OrderId::new(expiring.clone()), 52, 1_000)
        .unwrap();
    assert!(s
        .due_order_ids(monero::Network::Mainnet, 1_000, 52, 10)
        .unwrap()
        .is_empty());
    assert_eq!(
        s.due_order_ids(monero::Network::Mainnet, 1_000, 53, 10)
            .unwrap(),
        vec![expiring.clone()]
    );
    assert!(
        s.due_order_ids(monero::Network::Mainnet, 9_999, 52, 10)
            .unwrap()
            .is_empty(),
        "no deadline once fully paid"
    );
    s.recompute_order_status(&OrderId::new(expiring.clone()), 59, 1_000)
        .unwrap();
    assert!(
        !s.due_order_ids(monero::Network::Mainnet, i64::MAX, u64::MAX, 10)
            .unwrap()
            .contains(&OrderId::new(expiring)),
        "settled"
    );
    drop(store);
    cleanup(&path);
}

/// While a reorg is open on its network an order can't newly settle; the
/// recompute obligation stays and it settles after the rewind.
#[test]
fn a_settlement_waits_for_an_open_reorg_on_its_network() {
    let (store, path) = fixture();
    let s = &store.0;
    let o = order(s, &store.3, 5_000);
    s.record_payment_match(
        &OrderId::new(o.clone()),
        "tx",
        0,
        100,
        "[\"ki\"]",
        1_000,
        Some(50),
        None,
    )
    .unwrap();
    s.open_reorg_job(monero::Network::Mainnet, 70, 1_000)
        .unwrap();
    let (_, frozen) = s
        .recompute_order_status(&OrderId::new(o.clone()), 59, 1_000)
        .unwrap();
    assert_eq!(frozen, crate::status::OrderStatus::Confirming);
    assert_eq!(
        s.pending_payment_recomputes_page(monero::Network::Mainnet, "", 10_000)
            .unwrap(),
        vec![o.clone()]
    );
    assert_eq!(
        s.due_order_ids(monero::Network::Mainnet, 1_000, 0, 10)
            .unwrap(),
        vec![o.clone()],
        "due again at once"
    );

    while s
        .collect_reorg_candidates(monero::Network::Mainnet, 10, 1000)
        .unwrap()
        != ReorgPhase::Process
    {}
    s.finish_reorg(monero::Network::Mainnet, 70, Some((69, "h69")))
        .unwrap();
    let (_, settled) = s
        .recompute_order_status(&OrderId::new(o), 59, 1_000)
        .unwrap();
    assert_eq!(settled, crate::status::OrderStatus::Paid);
    assert!(s
        .pending_payment_recomputes_page(monero::Network::Mainnet, "", 10_000)
        .unwrap()
        .is_empty());
    drop(store);
    cleanup(&path);
}

/// The scanner's hot queries are answered from indexes, never by
/// scanning a whole table: the plans are checked here so an edit can't
/// silently turn one back into a full scan.
#[test]
fn the_scanners_hot_queries_use_their_indexes() {
    let store = Store::open_in_memory().unwrap();
    let plan = |sql: &str| -> String {
        let mut stmt = store
            .conn
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap();
        let rows = stmt.query_map([], |row| row.get::<_, String>(3)).unwrap();
        rows.map(|row| row.unwrap()).collect::<Vec<_>>().join(" | ")
    };
    let window = format!(
        "SELECT minor_index FROM orders WHERE id IN ({}) ORDER BY minor_index",
        crate::store::scan_window_orders("o.scan_tenant_id = 'x'")
    )
    .replace(":since_minus_grace", "0");
    let in_scope = format!(
        "SELECT t.id, t.scanned_through_height FROM tenants t
         WHERE t.network = 'mainnet' AND t.disabled_at_utc IS NULL AND t.id > '' AND {} ORDER BY t.id LIMIT 32",
        crate::store::tenant_in_scope("t.id")
    )
    .replace(":since_minus_grace", "0");
    for (what, sql, index) in [
        ("scan window, open half", window.as_str(), "_status_"),
        ("scan window, closed half", window.as_str(), "orders_scan_tenant_closed_idx"),
        ("tenant in scope, open half", in_scope.as_str(), "_status_"),
        ("tenant in scope, closed half", in_scope.as_str(), "orders_scan_tenant_closed_idx"),
        (
            "catch-up groups",
            "SELECT DISTINCT scanned_through_height FROM tenants WHERE network = 'mainnet' AND disabled_at_utc IS NULL \
             AND scanned_through_height IS NOT NULL AND scanned_through_height < 10 AND scanned_through_height > 1 \
             ORDER BY scanned_through_height LIMIT 1",
            "tenants_network_cursor_idx",
        ),
        (
            "group members",
            "SELECT id FROM tenants WHERE network = 'mainnet' AND disabled_at_utc IS NULL AND scanned_through_height = 5 ORDER BY id",
            "tenants_network_cursor_idx",
        ),
        (
            "void recheck page",
            "SELECT op.* FROM order_payments op WHERE op.voided_at_utc IS NOT NULL AND op.superseded_by IS NULL AND op.voided_at_utc >= 0 AND op.id > 0 \
             ORDER BY op.id LIMIT 16",
            "order_payments_voided_idx",
        ),
        (
            "the engine page's groups of stores",
            "SELECT scanned_through_height, COUNT(*) FROM tenants WHERE network = 'mainnet' AND disabled_at_utc IS NULL \
             AND scanned_through_height IS NOT NULL GROUP BY scanned_through_height ORDER BY scanned_through_height DESC LIMIT 32",
            "tenants_network_cursor_idx",
        ),
        (
            "the order-event stream",
            "SELECT e.seq, e.event_id, e.tenant_id, t.public_key, e.order_id, e.event_type, e.payload_json, \
             e.created_at_utc FROM order_events e JOIN tenants t ON t.id = e.tenant_id \
             WHERE e.seq > 5 ORDER BY e.seq LIMIT 256",
            "INTEGER PRIMARY KEY",
        ),
        (
            "pruning order events",
            "SELECT seq FROM order_events WHERE created_at_utc < 5",
            "order_events_created_idx",
        ),
        (
            "due by time",
            "SELECT o.id FROM orders o WHERE o.next_due_at_utc IS NOT NULL AND o.next_due_at_utc <= 5 ORDER BY o.next_due_at_utc, o.id LIMIT 64",
            "orders_due_at_idx",
        ),
    ] {
        let plan = plan(sql);
        assert!(plan.contains(index), "{what}: expected {index} in the plan, got {plan}");
        let full_scan = plan.split(" | ").any(|step| step.starts_with("SCAN ") && !step.contains("json_each"));
        assert!(!full_scan, "{what}: a full scan in {plan}");
    }
}

#[test]
fn scheduler_positions_are_per_network_typed_and_survive_a_restart() {
    use position::{CatchUpGroup, ScanRange, VoidRecheck};
    let (store, path) = fixture();
    store
        .0
        .set_scheduler_position::<CatchUpGroup>(monero::Network::Mainnet, &42)
        .unwrap();
    store
        .0
        .set_scheduler_position::<CatchUpGroup>(monero::Network::Mainnet, &43)
        .unwrap();
    store
        .0
        .set_scheduler_position::<ScanRange>(monero::Network::Mainnet, &"tn_x".to_owned())
        .unwrap();
    let s = reopen(store, &path);
    assert_eq!(
        s.scheduler_position::<CatchUpGroup>(monero::Network::Mainnet)
            .unwrap(),
        Some(43)
    );
    assert_eq!(
        s.scheduler_position::<ScanRange>(monero::Network::Mainnet)
            .unwrap()
            .as_deref(),
        Some("tn_x")
    );
    assert_eq!(
        s.scheduler_position::<CatchUpGroup>(monero::Network::Stagenet)
            .unwrap(),
        None
    );
    assert_eq!(
        s.scheduler_position::<VoidRecheck>(monero::Network::Mainnet)
            .unwrap(),
        None
    );
    // A hand-edited, unreadable value starts that rotation over.
    s.execute_raw_for_test(
        "UPDATE scheduler_positions SET value = 'x' WHERE position = 'catch_up_group'",
    )
    .unwrap();
    assert_eq!(
        s.scheduler_position::<CatchUpGroup>(monero::Network::Mainnet)
            .unwrap(),
        None
    );
    drop(s);
    cleanup(&path);
}

/// Runs `op` with each of its SQL statements failed in turn: every
/// failure is reported (never a panic, never swallowed) and leaves the
/// database exactly as it was; then `op` runs clean, and its result is
/// returned. An operation its callers run inside their transaction is
/// given one here too.
fn sweep<T>(store: &Store, op: impl Fn(&Store) -> Result<T>) -> T {
    for fault in 0.. {
        let before = store.dump_for_test();
        let seen = store.fail_nth_access(Some(fault));
        let result = op(store);
        store.fail_nth_access(None);
        if seen.load(std::sync::atomic::Ordering::Relaxed) <= fault {
            return result.unwrap();
        }
        assert!(
            result.is_err(),
            "the failure of statement {fault} was swallowed"
        );
        assert_eq!(
            store.dump_for_test(),
            before,
            "the failure of statement {fault} left a partial write"
        );
    }
    unreachable!()
}

/// A tip observation and its obligations commit together, including first
/// use on legacy state, and never enqueue another network's orders.
#[test]
fn tip_decreases_queue_recomputes_atomically_and_only_for_their_network() {
    let store = Store::open_in_memory().unwrap();
    let main = tenant(&store, "mainnet");
    let stage = tenant(&store, "stagenet");
    let main_order = order(&store, &main, 10_000);
    let stage_order = order(&store, &stage, 10_000);
    pay(&store, &main_order, "main", Some(3));
    pay(&store, &stage_order, "stage", Some(3));
    // First observation also repairs legacy databases with no position.
    sweep(&store, |s| {
        s.observe_settlement_tip(monero::Network::Mainnet, 20)
    });
    assert_eq!(
        store
            .pending_payment_recomputes_page(monero::Network::Mainnet, "", 10)
            .unwrap(),
        vec![OrderId::new(main_order.clone())]
    );
    store
        .execute_raw_for_test("DELETE FROM pending_payment_recomputes")
        .unwrap();
    sweep(&store, |s| {
        s.observe_settlement_tip(monero::Network::Mainnet, 21)
    });
    assert!(store
        .pending_payment_recomputes_page(monero::Network::Mainnet, "", 10)
        .unwrap()
        .is_empty());
    sweep(&store, |s| {
        s.observe_settlement_tip(monero::Network::Mainnet, 6)
    });
    assert_eq!(
        store
            .pending_payment_recomputes_page(monero::Network::Mainnet, "", 10)
            .unwrap(),
        vec![OrderId::new(main_order)]
    );
    assert!(store
        .pending_payment_recomputes_page(monero::Network::Stagenet, "", 10)
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .scheduler_position::<position::SettlementTip>(monero::Network::Mainnet)
            .unwrap(),
        Some(6)
    );
}

#[test]
fn branch_tracking_and_recollection_commit_whole_and_preserve_the_rescan_anchor() {
    let store = Store::open_in_memory().unwrap();
    let tenant_id = tenant(&store, "mainnet");
    let order_id = order(&store, &tenant_id, 10_000);
    let first = pay(&store, &order_id, "first", Some(11));
    let second = pay(&store, &order_id, "second", Some(12));
    store
        .set_scanned_block(monero::Network::Mainnet, 10, "a10")
        .unwrap();
    store
        .open_reorg_job(monero::Network::Mainnet, 11, 200)
        .unwrap();
    sweep(&store, |s| {
        s.restart_reorg_for_branch(monero::Network::Mainnet, 12, "b12", 200)
    });
    assert_eq!(
        store.reorg_branch(monero::Network::Mainnet).unwrap(),
        Some((12, "b12".to_owned()))
    );
    assert_eq!(store.reorg_branch(monero::Network::Stagenet).unwrap(), None);
    while store
        .collect_reorg_candidates(monero::Network::Mainnet, 10, 200)
        .unwrap()
        != ReorgPhase::Process
    {}
    store
        .complete_reorg_candidate(monero::Network::Mainnet, first)
        .unwrap();
    sweep(&store, |s| {
        s.extend_reorg_branch(monero::Network::Mainnet, 13, "b13")
    });
    assert_eq!(
        store
            .reorg_job(monero::Network::Mainnet)
            .unwrap()
            .unwrap()
            .phase,
        ReorgPhase::Process
    );
    assert_eq!(
        store
            .reorg_work_remaining(monero::Network::Mainnet)
            .unwrap()
            .0,
        1
    );
    // Another branch, at the same fork point: the completed payment must
    // be collected again, and retries on the discarded branch must clear.
    store
        .defer_reorg_candidate(monero::Network::Mainnet, second, 200)
        .unwrap();
    sweep(&store, |s| {
        s.restart_reorg_for_branch(monero::Network::Mainnet, 13, "c13", 201)
    });
    assert_eq!(
        store
            .reorg_work_remaining(monero::Network::Mainnet)
            .unwrap()
            .0,
        0
    );
    while store
        .collect_reorg_candidates(monero::Network::Mainnet, 10, 201)
        .unwrap()
        != ReorgPhase::Process
    {}
    let candidates = store
        .due_reorg_candidates(monero::Network::Mainnet, 201, 10)
        .unwrap();
    assert_eq!(candidates.len(), 2);
    assert!(candidates.iter().all(|candidate| candidate.attempts == 0));
    for candidate in candidates {
        store
            .complete_reorg_candidate(monero::Network::Mainnet, candidate.payment.id)
            .unwrap();
    }
    sweep(&store, |s| {
        s.finish_reorg(monero::Network::Mainnet, 11, Some((10, "a10")))
    });
    assert_eq!(
        store
            .get_scanned_block_hash(monero::Network::Mainnet, 13)
            .unwrap(),
        None,
        "branch tracking must not claim an unscanned block was scanned"
    );
    assert_eq!(
        store.reorg_branch(monero::Network::Mainnet).unwrap(),
        Some((13, "c13".to_owned()))
    );
    sweep(&store, |s| s.clear_reorg_branch(monero::Network::Mainnet));
    assert_eq!(store.reorg_branch(monero::Network::Mainnet).unwrap(), None);
    assert!(!store.settlement_frozen(monero::Network::Mainnet).unwrap());
}

/// Every durable operation of the scheduler, failed statement by
/// statement, along one reorg's life and a block scan's.
#[test]
fn every_store_operation_fails_whole_and_then_works() {
    let store = Store::open_in_memory().unwrap();
    let tenant_id = tenant(&store, "mainnet");
    let other = tenant(&store, "mainnet");
    let order_id = order(&store, &tenant_id, 10_000);
    // `other` has no order: nothing in scope, ever.
    for h in 1..=12u64 {
        store
            .set_scanned_block(monero::Network::Mainnet, h, &format!("a{h}"))
            .unwrap();
    }
    store
        .execute_raw_for_test("UPDATE tenants SET scanned_through_height = 12")
        .unwrap();
    store
        .record_payment_match(
            &OrderId::new(order_id.clone()),
            "tx_confirmed",
            0,
            50,
            "[\"ki1\"]",
            150,
            Some(11),
            None,
        )
        .unwrap();
    store
        .record_payment_match(
            &OrderId::new(order_id.clone()),
            "tx_pool",
            0,
            50,
            "[\"ki2\"]",
            150,
            None,
            None,
        )
        .unwrap();
    store
        .record_payment_match(
            &OrderId::new(order_id.clone()),
            "tx_voided",
            0,
            50,
            "[\"ki3\"]",
            150,
            Some(10),
            None,
        )
        .unwrap();
    store
        .void_payment(&OrderId::new(order_id.clone()), "tx_voided", 0, 160)
        .unwrap();
    let voided_id = store
        .get_all_payments(&OrderId::new(order_id.clone()))
        .unwrap()
        .iter()
        .find(|p| p.txid == "tx_voided")
        .unwrap()
        .id;

    // A reorg's life.
    sweep(&store, |s| {
        s.open_reorg_job(monero::Network::Mainnet, 11, 200)
    });
    assert!(sweep(&store, |s| s.settlement_frozen(monero::Network::Mainnet)));
    assert_eq!(
        sweep(&store, |s| s.collect_reorg_candidates(
            monero::Network::Mainnet,
            1,
            200
        )),
        ReorgPhase::CollectConfirmed {
            after_height: 11,
            after_id: 1
        }
    );
    sweep(&store, |s| {
        s.collect_reorg_candidates(monero::Network::Mainnet, 1, 200)
    });
    sweep(&store, |s| {
        s.collect_reorg_candidates(monero::Network::Mainnet, 1, 200)
    });
    assert_eq!(
        sweep(&store, |s| s.collect_reorg_candidates(
            monero::Network::Mainnet,
            64,
            200
        )),
        ReorgPhase::Process
    );
    let due = sweep(&store, |s| {
        s.due_reorg_candidates(monero::Network::Mainnet, 200, 10)
    });
    assert_eq!(due.len(), 2);
    sweep(&store, |s| {
        s.defer_reorg_candidate(monero::Network::Mainnet, due[0].payment.id, 200)
    });
    assert_eq!(
        sweep(&store, |s| s.reorg_work_remaining(monero::Network::Mainnet)).0,
        2
    );
    sweep(&store, |s| {
        s.complete_reorg_candidate(monero::Network::Mainnet, due[0].payment.id)
    });
    sweep(&store, |s| {
        s.complete_reorg_candidate(monero::Network::Mainnet, due[1].payment.id)
    });
    sweep(&store, |s| {
        s.finish_reorg(monero::Network::Mainnet, 11, Some((10, "a10")))
    });
    assert!(sweep(&store, |s| s.reorg_job(monero::Network::Mainnet)).is_none());

    // Positions, pages and lookups.
    sweep(&store, |s| {
        s.set_scheduler_position::<position::VoidRecheck>(monero::Network::Mainnet, &5)
    });
    assert_eq!(
        sweep(&store, |s| s.scheduler_position::<position::VoidRecheck>(
            monero::Network::Mainnet
        )),
        Some(5)
    );
    assert_eq!(
        sweep(&store, |s| s.scanned_blocks_between(
            monero::Network::Mainnet,
            9,
            10
        ))
        .len(),
        2
    );
    sweep(&store, |s| {
        s.due_order_ids(monero::Network::Mainnet, 20_000, 12, 10)
    });
    assert_eq!(
        sweep(&store, |s| s.voided_payments_page(
            monero::Network::Mainnet,
            0,
            0,
            10
        ))
        .len(),
        1
    );
    assert!(sweep(&store, |s| s.payment_by_id(voided_id)).is_some());
    assert_eq!(
        sweep(&store, |s| s.scan_group_cursors(
            monero::Network::Mainnet,
            20,
            None,
            10
        )),
        vec![10]
    );
    assert_eq!(
        sweep(&store, |s| s.tenants_at_cursor(
            monero::Network::Mainnet,
            10,
            &[],
            10
        ))
        .len(),
        2
    );
    assert_eq!(
        sweep(&store, |s| s.scan_windows(
            &[
                TenantId::new(tenant_id.clone()),
                TenantId::new(other.clone())
            ],
            150,
            0
        ))
        .len(),
        1
    );
    assert_eq!(
        sweep(&store, |s| s.active_tenants_page(
            monero::Network::Mainnet,
            150,
            0,
            "",
            10
        ))
        .len(),
        1
    );

    // A block scan: checkpointed, staged, replaced, taken, committed.
    let checkpoint = BlockCheckpoint {
        height: 11,
        hash: "b11".into(),
        next_tx: 3,
    };
    sweep(&store, |s| {
        s.in_transaction(|s| {
            s.save_block_checkpoint(
                monero::Network::Mainnet,
                &TenantId::new(tenant_id.clone()),
                &checkpoint,
            )
        })
    });
    sweep(&store, |s| {
        s.stage_partial_match(&crate::store::StagedMatch {
            network: monero::Network::Mainnet,
            tenant_id: &TenantId::new(tenant_id.clone()),
            order_id: &OrderId::new(order_id.clone()),
            txid: "tx_staged",
            output_index: 0,
            amount: 70,
            key_images_json: "[]",
            seen_at: 170,
            output_key: None,
        })
    });
    let replaced = BlockCheckpoint {
        height: 11,
        hash: "c11".into(),
        next_tx: 1,
    };
    sweep(&store, |s| {
        s.in_transaction(|s| {
            s.save_block_checkpoint(
                monero::Network::Mainnet,
                &TenantId::new(tenant_id.clone()),
                &replaced,
            )
        })
    });
    assert!(
        sweep(&store, |s| s.in_transaction(|s| s.take_staged_payments(
            monero::Network::Mainnet,
            &TenantId::new(tenant_id.clone()),
            "c11"
        )))
        .is_empty(),
        "the staged match went with the old block"
    );
    assert_eq!(
        sweep(&store, |s| s.block_checkpoint(
            monero::Network::Mainnet,
            &TenantId::new(tenant_id.clone())
        )),
        None
    );
    let scanned = [crate::work::ScannedBlock::for_test(
        &TenantId::new(tenant_id),
        11,
    )];
    assert_eq!(
        sweep(&store, |s| s.advance_scanned_cursors(
            monero::Network::Mainnet,
            11,
            &scanned
        ))
        .len(),
        1
    );
    assert_eq!(
        sweep(&store, |s| s.advance_idle_cursors(
            monero::Network::Mainnet,
            10,
            11,
            i64::MAX / 2,
            0
        )),
        1
    );
}

/// Catch-up groups are counted as they are listed: one per distinct
/// cursor below the mark, enabled tenants only, on the one network.
#[test]
fn catch_up_groups_are_counted_as_listed() {
    let store = Store::open_in_memory().unwrap();
    let ids: Vec<_> = std::iter::repeat_with(|| tenant(&store, "mainnet"))
        .take(5)
        .collect();
    tenant(&store, "stagenet");
    for (id, cursor) in ids.iter().zip([3, 3, 7, 20, 9]) {
        store
            .execute_raw_for_test(&format!(
                "UPDATE tenants SET scanned_through_height = {cursor} WHERE id = '{id}'"
            ))
            .unwrap();
    }
    store
        .execute_raw_for_test(&format!(
            "UPDATE tenants SET disabled_at_utc = 1 WHERE id = '{}'",
            ids[4]
        ))
        .unwrap();
    store
        .execute_raw_for_test(
            "UPDATE tenants SET scanned_through_height = 1 WHERE network = 'stagenet'",
        )
        .unwrap();
    let listed = store
        .scan_group_cursors(monero::Network::Mainnet, 20, None, 10)
        .unwrap();
    assert_eq!(listed, [3, 7]);
    assert_eq!(
        store
            .count_scan_groups(monero::Network::Mainnet, 20)
            .unwrap(),
        listed.len() as u64
    );
    assert_eq!(
        store
            .count_scan_groups(monero::Network::Mainnet, 3)
            .unwrap(),
        0
    );
}

/// A value past SQLite's range is refused before it reaches the query,
/// and a corrupted row fails its page: both errors, never a wrapped
/// number read as a height.
#[test]
fn out_of_range_values_and_corrupt_rows_are_errors() {
    let store = Store::open_in_memory().unwrap();
    let tenant_id = tenant(&store, "mainnet");
    assert!(matches!(
        store.scan_group_cursors(monero::Network::Mainnet, u64::MAX, None, 10),
        Err(StoreError::Sqlite(rusqlite::Error::ToSqlConversionFailure(
            _
        )))
    ));
    let order_id = order(&store, &tenant_id, 10_000);
    store
        .record_payment_match(
            &OrderId::new(order_id),
            "tx",
            0,
            1,
            "[]",
            100,
            Some(7),
            None,
        )
        .unwrap();
    store
        .open_reorg_job(monero::Network::Mainnet, 5, 100)
        .unwrap();
    while store
        .collect_reorg_candidates(monero::Network::Mainnet, 10, 100)
        .unwrap()
        != ReorgPhase::Process
    {}
    store
        .execute_raw_for_test("UPDATE reorg_work SET attempts = -3")
        .unwrap();
    assert!(matches!(
        store.due_reorg_candidates(monero::Network::Mainnet, 100, 10),
        Err(StoreError::Sqlite(
            rusqlite::Error::IntegralValueOutOfRange(_, -3)
        ))
    ));
}

/// A job row whose phase isn't one this build knows (a hand-edited or
/// newer row) is an error, not a guess.
#[test]
fn an_unknown_reorg_phase_is_an_error() {
    let store = Store::open_in_memory().unwrap();
    store
        .open_reorg_job(monero::Network::Mainnet, 5, 100)
        .unwrap();
    store.execute_raw_for_test("PRAGMA ignore_check_constraints = ON; UPDATE reorg_jobs SET phase = 'later'; PRAGMA ignore_check_constraints = OFF").unwrap();
    let error = store.reorg_job(monero::Network::Mainnet).unwrap_err();
    assert!(error.to_string().contains("unknown reorg phase"), "{error}");
}

/// Collecting for a job that has finished collecting changes nothing;
/// deferring a candidate that is already gone changes nothing.
#[test]
fn late_collects_and_defers_are_no_ops() {
    let store = Store::open_in_memory().unwrap();
    store
        .open_reorg_job(monero::Network::Mainnet, 5, 100)
        .unwrap();
    while store
        .collect_reorg_candidates(monero::Network::Mainnet, 10, 100)
        .unwrap()
        != ReorgPhase::Process
    {}
    let before = store.dump_for_test();
    assert_eq!(
        store
            .collect_reorg_candidates(monero::Network::Mainnet, 10, 100)
            .unwrap(),
        ReorgPhase::Process
    );
    store
        .defer_reorg_candidate(monero::Network::Mainnet, 12345, 100)
        .unwrap();
    assert_eq!(store.dump_for_test(), before);
}

/// An order due by both time and height is listed once, and the list
/// stops at the limit.
#[test]
fn due_orders_are_listed_once_up_to_the_limit() {
    let store = Store::open_in_memory().unwrap();
    let tenant_id = tenant(&store, "mainnet");
    let orders: Vec<String> = std::iter::repeat_with(|| order(&store, &tenant_id, 10_000))
        .take(3)
        .collect();
    store
        .execute_raw_for_test("UPDATE orders SET next_due_at_utc = 50, next_due_height = 7")
        .unwrap();
    let due = store
        .due_order_ids(monero::Network::Mainnet, 100, 10, 10)
        .unwrap();
    assert_eq!(due.len(), 3, "each once: {due:?}");
    assert!(orders
        .iter()
        .all(|o| due.contains(&OrderId::new(o.clone()))));
    assert_eq!(
        store
            .due_order_ids(monero::Network::Mainnet, 100, 10, 2)
            .unwrap()
            .len(),
        2
    );
}

/// The engine page's facts: each thing counted once, for its own
/// network only, and the groups listed highest first up to the limit.
#[test]
fn the_engine_page_s_facts_count_each_thing_once_on_its_network() {
    let store = Store::open_in_memory().unwrap();
    let (a, b) = (tenant(&store, "mainnet"), tenant(&store, "mainnet"));
    let elsewhere = tenant(&store, "stagenet");
    for h in 1..=12u64 {
        store
            .set_scanned_block(monero::Network::Mainnet, h, &format!("a{h}"))
            .unwrap();
    }
    store
        .execute_raw_for_test(&format!(
            "UPDATE tenants SET scanned_through_height = CASE id WHEN '{a}' THEN 12 WHEN '{b}' THEN 10 ELSE 3 END"
        ))
        .unwrap();
    let (both, by_height) = (order(&store, &a, 10_000), order(&store, &a, 10_000));
    let other = order(&store, &elsewhere, 10_000);
    store
        .execute_raw_for_test(&format!(
            "UPDATE orders SET next_due_at_utc = NULL, next_due_height = NULL;
             UPDATE orders SET next_due_at_utc = 50, next_due_height = 7 WHERE id IN ('{both}', '{other}');
             UPDATE orders SET next_due_height = 7 WHERE id = '{by_height}'"
        ))
        .unwrap();
    store
        .execute_raw_for_test("DELETE FROM pending_payment_recomputes")
        .unwrap();
    pay(&store, &both, "t1", None);
    pay(&store, &other, "t2", None);
    store
        .save_block_checkpoint(
            monero::Network::Mainnet,
            &TenantId::new(b),
            &BlockCheckpoint {
                height: 11,
                hash: "a11".to_owned(),
                next_tx: 3,
            },
        )
        .unwrap();
    store
        .open_reorg_job(monero::Network::Mainnet, 11, 100)
        .unwrap();

    let facts = store
        .activity_facts(monero::Network::Mainnet, 100, Some(10), 32)
        .unwrap();
    assert_eq!(
        facts,
        ActivityFacts {
            high_water: Some(12),
            groups: vec![(12, 1), (10, 1)],
            all_groups: 2,
            checkpoints: vec![11],
            reorg: Some((11, true, 0)),
            recomputes_pending: 1,
            orders_due: 2,
        }
    );
    let one = store
        .activity_facts(monero::Network::Mainnet, 100, Some(10), 1)
        .unwrap();
    assert_eq!((one.groups, one.all_groups), (vec![(12, 1)], 2));
    let none_due = store
        .activity_facts(monero::Network::Mainnet, 40, Some(6), 32)
        .unwrap();
    assert_eq!(none_due.orders_due, 0, "not yet due by time or height");
}

/// More time-due orders than a page: the height-due ones still get
/// their half of it, so confirming orders are recomputed every block
/// however many orders are held back by time.
#[test]
fn height_due_orders_get_half_the_page_whatever_the_time_due_backlog() {
    let store = Store::open_in_memory().unwrap();
    let tenant_id = tenant(&store, "mainnet");
    let by_time: Vec<String> = std::iter::repeat_with(|| order(&store, &tenant_id, 10_000))
        .take(8)
        .collect();
    let by_height: Vec<String> = std::iter::repeat_with(|| order(&store, &tenant_id, 10_000))
        .take(3)
        .collect();
    for id in &by_time {
        store
            .execute_raw_for_test(&format!(
                "UPDATE orders SET next_due_at_utc = 50 WHERE id = '{id}'"
            ))
            .unwrap();
    }
    for id in &by_height {
        store
            .execute_raw_for_test(&format!(
                "UPDATE orders SET next_due_height = 7 WHERE id = '{id}'"
            ))
            .unwrap();
    }
    let due = store
        .due_order_ids(monero::Network::Mainnet, 100, 10, 6)
        .unwrap();
    assert_eq!(due.len(), 6);
    let heights = due
        .iter()
        .filter(|id| by_height.contains(&id.to_string()))
        .count();
    assert_eq!(
        heights, 3,
        "every height-due order, within its half: {due:?}"
    );
    // With room to spare, the time-due ones take what the others left.
    let due = store
        .due_order_ids(monero::Network::Mainnet, 100, 10, 20)
        .unwrap();
    assert_eq!(due.len(), 11);
}

/// A reorg is only finished once it is processing and has nothing
/// left, at the fork it was opened for.
#[test]
fn finishing_a_reorg_early_or_at_another_fork_is_refused() {
    let store = Store::open_in_memory().unwrap();
    store
        .open_reorg_job(monero::Network::Mainnet, 5, 100)
        .unwrap();
    assert!(
        matches!(
            store.finish_reorg(monero::Network::Mainnet, 5, None),
            Err(StoreError::NotFound)
        ),
        "still collecting"
    );
    while store
        .collect_reorg_candidates(monero::Network::Mainnet, 10, 100)
        .unwrap()
        != ReorgPhase::Process
    {}
    assert!(
        matches!(
            store.finish_reorg(monero::Network::Mainnet, 4, None),
            Err(StoreError::NotFound)
        ),
        "another fork"
    );
    store
        .finish_reorg(monero::Network::Mainnet, 5, None)
        .unwrap();
    assert!(
        matches!(
            store.finish_reorg(monero::Network::Mainnet, 5, None),
            Err(StoreError::NotFound)
        ),
        "no job"
    );
}

// -- What is scanned for, and what is looked at again: the tests of the
// queries these pages replaced, held to the pages themselves. -----------

fn tenant_id(id: &str) -> TenantId {
    TenantId::new(id.to_owned())
}

fn order_id(id: &str) -> OrderId {
    OrderId::new(id.to_owned())
}

/// The stores in scope on `network`, by id.
fn in_scope(store: &Store, network: monero::Network, now: i64, grace: i64) -> Vec<String> {
    store
        .active_tenants_page(network, now, grace, "", 1000)
        .unwrap()
        .into_iter()
        .map(|(id, _)| id.into_string())
        .collect()
}

fn set_status(store: &Store, order: &str, status: &str) {
    store
        .conn
        .execute(
            "UPDATE orders SET status = ?2 WHERE id = ?1",
            params![order, status],
        )
        .unwrap();
}

/// A store is scanned for while it has an order that can still receive
/// a payment: in any open status, and in none of the settled ones. One
/// store per status, so its order's status is the only variable.
#[test]
fn a_store_is_in_scope_for_each_open_order_status_and_no_settled_one() {
    let store = Store::open_in_memory().unwrap();
    let mut stores = Vec::new();
    for (status, open) in [
        ("pending", true),
        ("unconfirmed", true),
        ("confirming", true),
        ("partial", true),
        ("paid", false),
        ("overpaid", false),
        ("expired", false),
    ] {
        let tenant = tenant(&store, "mainnet");
        let order = order(&store, &tenant, 2_000);
        set_status(&store, &order, status);
        stores.push((status, open, tenant));
    }
    let active = in_scope(&store, monero::Network::Mainnet, i64::MAX, 0);
    for (status, open, tenant) in &stores {
        assert_eq!(active.contains(tenant), *open, "{status}");
    }
}

/// An order that expired keeps its store in scope, and stays in the
/// store's scan window, for the grace period after it closed and no
/// longer: a payment that lands just after the deadline is still found.
#[test]
fn an_expired_order_stays_in_scope_for_the_grace_period_and_no_longer() {
    let store = Store::open_in_memory().unwrap();
    let tenant = tenant(&store, "mainnet");
    let order = order(&store, &tenant, 2_000);
    // Closed at its deadline, as `recompute_order_status` records it.
    store
        .conn
        .execute(
            "UPDATE orders SET status = 'expired', closed_at_utc = expires_at_utc WHERE id = ?1",
            params![order],
        )
        .unwrap();
    let minor: u32 = store
        .conn
        .query_row(
            "SELECT minor_index FROM orders WHERE id = ?1",
            params![order],
            |row| row.get(0),
        )
        .unwrap();
    // The store's page and its window say the same.
    let scanned_for = |now: i64, grace: i64| {
        let listed = in_scope(&store, monero::Network::Mainnet, now, grace).contains(&tenant);
        let windows = store
            .scan_windows(&[tenant_id(&tenant)], now, grace)
            .unwrap();
        match windows.get(&tenant_id(&tenant)) {
            Some(window) => assert_eq!(window, &vec![minor]),
            None => assert!(windows.is_empty()),
        }
        assert_eq!(
            listed,
            !windows.is_empty(),
            "at {now} with {grace}s of grace"
        );
        listed
    };
    // Exactly at the boundary (closed at `now - grace`): inclusive.
    assert!(scanned_for(2_000, 0));
    // One second past, with no grace at all.
    assert!(!scanned_for(2_001, 0));
    // A real grace window: still within it, then past it.
    assert!(scanned_for(2_500, 600));
    assert!(!scanned_for(2_601, 600));
}

/// A store with no orders isn't scanned for. One drops out once its
/// only order settles, and a fresh order brings it straight back: the
/// page is read fresh each time, so there is no "inactive" left over to
/// undo. One of several orders still open keeps it in.
#[test]
fn a_store_leaves_scope_when_its_orders_settle_and_returns_with_a_new_one() {
    let store = Store::open_in_memory().unwrap();
    let active =
        |tenant: &String| in_scope(&store, monero::Network::Mainnet, i64::MAX, 0).contains(tenant);
    let settle = |order: &str, txid: &str| {
        store
            .record_payment_match(&order_id(order), txid, 0, 100, "[]", 1_500, Some(50), None)
            .unwrap();
        let (_, status) = store
            .recompute_order_status(&order_id(order), 59, 1_600)
            .unwrap();
        assert_eq!(status, crate::status::OrderStatus::Paid);
    };

    let tenant_a = tenant(&store, "mainnet");
    assert!(!active(&tenant_a), "no orders at all");
    let first = order(&store, &tenant_a, 100_000);
    assert!(active(&tenant_a));
    settle(&first, "tx_a");
    assert!(!active(&tenant_a), "its only order is settled");
    order(&store, &tenant_a, 100_000);
    assert!(active(&tenant_a), "a fresh order");

    let tenant_b = tenant(&store, "mainnet");
    let settled = order(&store, &tenant_b, 100_000);
    order(&store, &tenant_b, 100_000);
    settle(&settled, "tx_b");
    assert!(active(&tenant_b), "its other order is still open");
}

/// Each network's scan sees its own stores and orders only: another
/// chain's heights and transactions are unrelated to it.
#[test]
fn stores_in_scope_and_due_orders_are_those_of_one_network() {
    let store = Store::open_in_memory().unwrap();
    let main_tenant = tenant(&store, "mainnet");
    let stage_tenant = tenant(&store, "stagenet");
    let main_order = order(&store, &main_tenant, 5_000);
    let stage_order = order(&store, &stage_tenant, 5_000);

    assert_eq!(
        in_scope(&store, monero::Network::Mainnet, i64::MAX, 0),
        vec![main_tenant]
    );
    assert_eq!(
        in_scope(&store, monero::Network::Stagenet, i64::MAX, 0),
        vec![stage_tenant]
    );
    // Both orders reach their deadline at once; each is due on its own
    // network.
    let due = |network| {
        store
            .due_order_ids(network, 5_000, 0, 10)
            .unwrap()
            .into_iter()
            .map(OrderId::into_string)
            .collect::<Vec<_>>()
    };
    assert_eq!(due(monero::Network::Mainnet), vec![main_order]);
    assert_eq!(due(monero::Network::Stagenet), vec![stage_order]);
}

/// A payment a reorg pushed back into the pool has no height, and SQL
/// makes `NULL >= n` false: collecting by height alone would never look
/// at it again, and it could be proven double-spent and still never be
/// voided. It is collected, as something above every height. So is a
/// voided payment, mined or not: the transaction that proved it
/// double-spent can itself be reorged out, and un-voiding depends on
/// looking again. Never another network's.
#[test]
fn a_reorg_collects_payments_back_in_the_pool_and_voided_ones() {
    let store = Store::open_in_memory().unwrap();
    let tenant = tenant(&store, "mainnet");
    let order = order(&store, &tenant, 100_000);
    let other_tenant = self::tenant(&store, "stagenet");
    let other_order = self::order(&store, &other_tenant, 100_000);

    let below = pay(&store, &order, "below", Some(49));
    let pooled_again = pay(&store, &order, "pooled_again", Some(50));
    store
        .update_payment_block_height(&order_id(&order), "pooled_again", 0, None)
        .unwrap();
    let voided_mined = pay(&store, &order, "voided_mined", Some(60));
    let voided_pooled = pay(&store, &order, "voided_pooled", None);
    for txid in ["voided_mined", "voided_pooled"] {
        store
            .void_payment(&order_id(&order), txid, 0, 1_600)
            .unwrap();
    }
    pay(&store, &other_order, "other_mined", Some(60));
    pay(&store, &other_order, "other_pooled", None);

    store
        .open_reorg_job(monero::Network::Mainnet, 50, 2_000)
        .unwrap();
    while store
        .collect_reorg_candidates(monero::Network::Mainnet, 2, 2_001)
        .unwrap()
        != ReorgPhase::Process
    {}
    let mut collected = work(&store, "mainnet");
    collected.sort_unstable();
    let mut expected = vec![pooled_again, voided_mined, voided_pooled];
    expected.sort_unstable();
    assert_eq!(collected, expected);
    assert!(!collected.contains(&below));
    assert!(work(&store, "stagenet").is_empty());
}

/// The recheck of voided payments reads them from a cutoff: bounded by
/// how recently a payment was voided, not by every void ever. Pages move
/// on by payment id, and keep to their network.
#[test]
fn voided_payments_are_paged_from_a_cutoff_on_their_own_network() {
    let store = Store::open_in_memory().unwrap();
    let tenant = tenant(&store, "mainnet");
    let order = order(&store, &tenant, 100_000);
    let old = pay(&store, &order, "tx_old", Some(50));
    let recent = pay(&store, &order, "tx_recent", Some(50));
    pay(&store, &order, "tx_never_voided", Some(50));
    store
        .void_payment(&order_id(&order), "tx_old", 0, 1_000)
        .unwrap();
    store
        .void_payment(&order_id(&order), "tx_recent", 0, 5_000)
        .unwrap();
    let page = |network, cutoff: i64, after: i64, limit: usize| {
        store
            .voided_payments_page(network, cutoff, after, limit)
            .unwrap()
            .into_iter()
            .map(|payment| payment.id)
            .collect::<Vec<_>>()
    };
    let mainnet = monero::Network::Mainnet;
    assert_eq!(page(mainnet, 3_000, 0, 10), vec![recent]);
    assert_eq!(
        page(mainnet, 0, 0, 10),
        vec![old, recent],
        "a cutoff at or before every void returns all of them"
    );
    assert!(
        page(mainnet, 5_001, 0, 10).is_empty(),
        "a cutoff after every void returns nothing"
    );
    // One at a time, each page after the last payment of the one before.
    assert_eq!(page(mainnet, 0, 0, 1), vec![old]);
    assert_eq!(page(mainnet, 0, old, 1), vec![recent]);
    assert!(page(mainnet, 0, recent, 1).is_empty());
    assert!(page(monero::Network::Stagenet, 0, 0, 10).is_empty());
}

/// (store, mainnet order, stagenet order, mainnet tenant).
fn fixture() -> ((Store, String, String, String), String) {
    let (store, path) = file_store();
    let main_tenant = tenant(&store, "mainnet");
    let other_tenant = tenant(&store, "stagenet");
    let main_order = order(&store, &main_tenant, 100_000);
    let other_order = order(&store, &other_tenant, 100_000);
    ((store, main_order, other_order, main_tenant), path)
}

fn reopen(store: (Store, String, String, String), path: &str) -> Store {
    drop(store);
    Store::open_file(path).unwrap()
}

#[test]
fn unsigned_values_cross_into_sqlite_checked_both_ways() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    let read = |sql: &str| conn.query_row(sql, [], |row| row.get::<_, Unsigned<u64>>(0));
    assert_eq!(read("SELECT 42").unwrap(), Unsigned(42));
    // A negative value read back is an error, not a wrapped huge height.
    assert!(matches!(
        read("SELECT -1"),
        Err(rusqlite::Error::IntegralValueOutOfRange(0, -1))
    ));
    // A value too big for SQLite's signed integers is refused on the way in.
    let wrote = conn.query_row("SELECT ?1", [Unsigned(u64::MAX)], |row| {
        row.get::<_, i64>(0)
    });
    assert!(matches!(
        wrote,
        Err(rusqlite::Error::ToSqlConversionFailure(_))
    ));
    let wrote = conn.query_row("SELECT ?1", [Unsigned(7usize)], |row| row.get::<_, i64>(0));
    assert_eq!(wrote.unwrap(), 7);
}
