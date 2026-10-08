//! Every historic schema is constructed with its real migrations. Generated
//! data is inserted using columns that actually existed in that schema.
use super::*;
use crate::property_support::{config, TempFile};
use proptest::prelude::*;
use rusqlite::types::Value;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn insert(conn: &Connection, table: &str, values: &[(&str, Value)]) {
    let columns: Vec<String> = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap()
        .query_map([], |r| r.get(1))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    let selected: Vec<_> = values
        .iter()
        .filter(|(name, _)| columns.iter().any(|c| c == name))
        .collect();
    let names = selected
        .iter()
        .map(|(n, _)| *n)
        .collect::<Vec<_>>()
        .join(",");
    let placeholders = std::iter::repeat_n("?", selected.len())
        .collect::<Vec<_>>()
        .join(",");
    conn.execute(
        &format!("INSERT INTO {table}({names}) VALUES({placeholders})"),
        rusqlite::params_from_iter(selected.iter().map(|(_, v)| v)),
    )
    .unwrap();
}
fn text(s: &str) -> Value {
    Value::Text(s.to_owned())
}
fn number(n: i64) -> Value {
    Value::Integer(n)
}

fn seed(conn: &Connection, version: usize, amount: i64, payload: &str) {
    insert(
        conn,
        "tenants",
        &[
            ("id", text("tenant")),
            ("public_key", text("pk_old")),
            ("secret_token_hash", text("hash")),
            ("key_custody_backend", text("plain")),
            ("sealed_key_material", Value::Blob(vec![])),
            ("primary_address", text("address")),
            ("network", text("mainnet")),
            ("allowed_origins", text("[]")),
            ("created_at", number(1000)),
            ("created_at_utc", number(1000)),
            ("next_minor_index", number(2)),
            ("scanned_through_height", number(2)),
        ],
    );
    insert(
        conn,
        "orders",
        &[
            ("id", text("order")),
            ("tenant_id", text("tenant")),
            ("scan_tenant_id", text("tenant")),
            ("minor_index", number(1)),
            ("address", text("address-1")),
            ("fiat_currency", text("USD")),
            ("fiat_amount", text("1.00")),
            ("exchange_rate", text("0.01")),
            ("xmr_amount_piconero", number(amount)),
            ("amount_received_piconero", number(amount)),
            ("status", text("confirming")),
            ("created_at", number(1000)),
            ("created_at_utc", number(1000)),
            ("expires_at", number(2000)),
            ("expires_at_utc", number(2000)),
            ("updated_at", number(1001)),
            ("updated_at_utc", number(1001)),
        ],
    );
    insert(
        conn,
        "order_payments",
        &[
            ("id", number(1)),
            ("order_id", text("order")),
            ("txid", text("payment")),
            ("output_index", number(0)),
            ("amount_piconero", number(amount)),
            ("key_images_json", text("[]")),
            ("first_seen_at", number(1001)),
            ("first_seen_at_utc", number(1001)),
            ("block_height", number(3)),
        ],
    );
    insert(
        conn,
        "scanned_blocks",
        &[
            ("network", text("mainnet")),
            ("height", number(2)),
            ("block_hash", text("parent")),
        ],
    );
    insert(
        conn,
        "webhooks",
        &[
            ("id", text("webhook")),
            ("tenant_id", text("tenant")),
            ("url", text("https://merchant.example/hook")),
            ("headers_json", text("{}")),
            ("signing_secret", text("secret")),
            ("created_at", number(1000)),
            ("created_at_utc", number(1000)),
        ],
    );
    insert(
        conn,
        "webhook_deliveries",
        &[
            ("id", number(1)),
            ("webhook_id", text("webhook")),
            ("order_id", text("order")),
            ("event_type", text("order.confirming")),
            ("payload_json", text(payload)),
            ("attempt_count", number(2)),
            ("next_attempt_at", number(1100)),
            ("next_attempt_at_utc", number(1100)),
            ("last_error", text("retry")),
        ],
    );
    if version >= 18 {
        insert(
            conn,
            "partial_block_progress",
            &[
                ("network", text("mainnet")),
                ("tenant_id", text("tenant")),
                ("height", number(3)),
                ("block_hash", text("block")),
                ("window_generation", text("")),
                ("next_tx_index", number(1)),
            ],
        );
        insert(
            conn,
            "partial_block_matches",
            &[
                ("network", text("mainnet")),
                ("tenant_id", text("tenant")),
                ("order_id", text("order")),
                ("txid", text("staged")),
                ("output_index", number(1)),
                ("amount_piconero", number(1)),
                ("key_images_json", text("[]")),
                ("seen_at_utc", number(1001)),
            ],
        );
    }
    if version >= 19 {
        insert(
            conn,
            "reorg_jobs",
            &[
                ("network", text("mainnet")),
                ("fork_height", number(3)),
                ("phase", text("process")),
                ("candidate_max_id", number(1)),
                ("collect_after_height", number(3)),
                ("collect_after_id", number(1)),
                ("created_at_utc", number(1001)),
                ("updated_at_utc", number(1001)),
            ],
        );
        insert(
            conn,
            "reorg_work",
            &[
                ("network", text("mainnet")),
                ("payment_id", number(1)),
                ("attempts", number(2)),
                ("next_attempt_at_utc", number(1100)),
            ],
        );
        insert(
            conn,
            "scheduler_positions",
            &[
                ("network", text("mainnet")),
                ("position", text("settlement_tip")),
                ("value", text("3")),
            ],
        );
    }
}

fn upgrade(version: usize, amount: i64, payload: &str, fault: Option<usize>) -> usize {
    let path = TempFile::new();
    let conn = Connection::open(&path.0).unwrap();
    configure_connection(&conn).unwrap();
    shared::migrations::apply(&conn, &MIGRATIONS[..version]).unwrap();
    seed(&conn, version, amount, payload);
    let accesses = Arc::new(AtomicUsize::new(0));
    let denied = Arc::new(AtomicBool::new(false));
    if let Some(at) = fault {
        let (hook_accesses, hook_denied) = (Arc::clone(&accesses), Arc::clone(&denied));
        conn.authorizer(Some(move |_: rusqlite::hooks::AuthContext<'_>| {
            if hook_accesses.fetch_add(1, Ordering::SeqCst) == at {
                hook_denied.store(true, Ordering::SeqCst);
                rusqlite::hooks::Authorization::Deny
            } else {
                rusqlite::hooks::Authorization::Allow
            }
        }))
        .unwrap();
        let result = shared::migrations::apply(&conn, MIGRATIONS);
        conn.authorizer(
            None::<fn(rusqlite::hooks::AuthContext<'_>) -> rusqlite::hooks::Authorization>,
        )
        .unwrap();
        assert_eq!(result.is_err(), denied.load(Ordering::SeqCst));
        let versions: Vec<i64> = conn
            .prepare("SELECT version FROM schema_migrations ORDER BY version")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(versions, (1..=versions.len() as i64).collect::<Vec<_>>());
    }
    drop(conn);
    let store = Store::open_file(&path.0).unwrap();
    let tenant = TenantId::new("tenant");
    let order = OrderId::new("order");
    let saved = store.get_order(&tenant, &order).unwrap().unwrap();
    assert_eq!(saved.xmr_amount_piconero, amount as u64);
    assert_eq!(saved.amount_received_piconero, amount as u64);
    assert_eq!(saved.minor_index, 1);
    let payments = store.get_all_payments(&order).unwrap();
    assert_eq!(payments.len(), 1);
    assert_eq!(payments[0].amount_piconero, amount as u64);
    assert_eq!(payments[0].block_height, Some(3));
    let delivery = store.due_webhook_deliveries_for_test(1200, 10).unwrap();
    assert_eq!(delivery.len(), 1);
    assert_eq!(delivery[0].payload_json, payload);
    assert_eq!(delivery[0].attempt_count, 2);
    assert_eq!(
        store
            .pending_payment_recomputes_page(monero::Network::Mainnet, "", 10)
            .unwrap(),
        vec![order]
    );
    if version >= 18 {
        assert_eq!(
            store
                .block_checkpoint(monero::Network::Mainnet, &tenant)
                .unwrap()
                .unwrap()
                .next_tx,
            1
        );
    }
    if version >= 19 {
        assert_eq!(
            store
                .reorg_work_remaining(monero::Network::Mainnet)
                .unwrap()
                .0,
            1
        );
        store
            .complete_reorg_candidate(monero::Network::Mainnet, 1)
            .unwrap();
        store
            .finish_reorg(monero::Network::Mainnet, 3, Some((2, "parent")))
            .unwrap();
        assert!(store
            .block_checkpoint(monero::Network::Mainnet, &tenant)
            .unwrap()
            .is_none());
        assert!(store.reorg_job(monero::Network::Mainnet).unwrap().is_none());
    }
    assert_eq!(
        store
            .conn_for_test()
            .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert!(!store
        .conn_for_test()
        .prepare("PRAGMA foreign_key_check")
        .unwrap()
        .exists([])
        .unwrap());
    drop(store);
    let reopened = Store::open_file(&path.0).unwrap();
    assert_eq!(
        reopened
            .get_all_payments(&OrderId::new("order"))
            .unwrap()
            .len(),
        1
    );
    accesses.load(Ordering::SeqCst)
}

proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn upgrades_preserve_money_queues_and_interrupted_work(version in 1usize..=MIGRATIONS.len(), amount in 1i64..=i64::MAX, payload in any::<String>(), fault in 0usize..400) {
        upgrade(version,amount,&payload,Some(fault));
    }
}

#[test]
fn every_historic_schema_upgrades_with_existing_money_and_work() {
    for version in 1..=MIGRATIONS.len() {
        upgrade(version, 42, "legacy payload", None);
    }
}

#[test]
fn every_reached_upgrade_fault_recovers_without_partial_schema() {
    // The last migration is a short representative transaction; deny every
    // authorizer boundary, including bookkeeping and commit operations.
    let count = upgrade(MIGRATIONS.len() - 1, 42, "fault sweep", Some(usize::MAX));
    for at in 0..count {
        upgrade(MIGRATIONS.len() - 1, 42, "fault sweep", Some(at));
    }
}

#[test]
fn migration_crash_child() {
    let Ok(path) = std::env::var("MONOKULO_MIGRATION_CHILD_PATH") else {
        return;
    };
    let conn = Connection::open(path).unwrap();
    configure_connection(&conn).unwrap();
    conn.authorizer(Some(|context: rusqlite::hooks::AuthContext<'_>| {
        if matches!(
            context.action,
            rusqlite::hooks::AuthAction::Insert {
                table_name: "schema_migrations"
            }
        ) {
            // DDL has completed but the migration/version transaction has not.
            crash_checkpoint("migration.before_commit");
        }
        rusqlite::hooks::Authorization::Allow
    }))
    .unwrap();
    shared::migrations::apply(&conn, MIGRATIONS).unwrap();
    crash_checkpoint("migration.after_commit");
}

async fn crash_upgrade(version: usize, amount: i64, payload: &str, point: &str) {
    let path = TempFile::new();
    let conn = Connection::open(&path.0).unwrap();
    configure_connection(&conn).unwrap();
    shared::migrations::apply(&conn, &MIGRATIONS[..version]).unwrap();
    seed(&conn, version, amount, payload);
    drop(conn);
    let mut child = crate::property_support::CrashChild(Some(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "store::properties::migration_crash_child",
                "--nocapture",
            ])
            .env("MONOKULO_MIGRATION_CHILD_PATH", &path.0)
            .env("MONOKULO_PROPERTY_CRASH_PATH", &path.0)
            .env("MONOKULO_PROPERTY_CRASH_POINT", point)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap(),
    ));
    child.rendezvous(&path.0, point).await;
    let _ = child.finish();
    let conn = Connection::open(&path.0).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        count,
        if point.ends_with("before_commit") {
            version as i64
        } else {
            MIGRATIONS.len() as i64
        }
    );
    drop(conn);
    let store = Store::open_file(&path.0).unwrap();
    assert_eq!(
        store.get_all_payments(&OrderId::new("order")).unwrap()[0].amount_piconero,
        amount as u64
    );
    assert_eq!(
        store.due_webhook_deliveries_for_test(1200, 10).unwrap()[0].payload_json,
        payload
    );
    assert_eq!(
        store
            .conn_for_test()
            .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
}

proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn historic_upgrades_recover_from_process_death(version in 1usize..MIGRATIONS.len(), amount in 1i64..=i64::MAX, payload in any::<String>(), after in any::<bool>()) {
        crate::property_support::runtime().block_on(crash_upgrade(version,amount,&payload,if after {"migration.after_commit"} else {"migration.before_commit"}));
    }
}

#[test]
fn migration_process_death_before_and_after_commit_preserves_money_and_schema() {
    crate::property_support::runtime().block_on(async {
        for version in 1..MIGRATIONS.len() {
            for point in ["migration.before_commit", "migration.after_commit"] {
                crash_upgrade(version, 42, "crash payload", point).await;
            }
        }
    });
}

fn persisted_config(config: proptest::test_runner::Config) -> proptest::test_runner::Config {
    crate::property_support::persist(
        config,
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/proptest-regressions/store/migration_properties.txt"
        ),
    )
}
