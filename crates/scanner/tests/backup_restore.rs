//! WBS 2.3.1's own literal acceptance test: "actually perform the restore
//! once against a copy; diff tenant/order counts before and after as the
//! pass condition."
//!
//! Drives the real `scripts/backup-database.sh` and `scripts/restore-database.sh`
//! (not a reimplementation of their logic) against a real `Store`-backed
//! SQLite database, with a concurrent writer thread still inserting orders
//! while the backup runs - the exact hazard `backup-database.sh`'s own header
//! comment investigates (a writer mid-commit while `.backup` runs). Excluded
//! from the default run like `e2e_stagenet.rs` is, since it shells out to
//! external scripts and requires `sqlite3` on PATH; run explicitly with:
//!
//! ```sh
//! cargo test --test backup_restore -- --ignored --nocapture
//! ```

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use scanner::store::{NewOrder, NewTenant, Store};
use uuid::Uuid;

/// Path to a script in the workspace's `scripts/` directory. Cargo runs this
/// test with the scanner crate as its working directory.
fn script(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("scripts")
        .join(name)
}

fn require_sqlite3() {
    let ok = Command::new("sqlite3")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    assert!(ok, "sqlite3 CLI must be on PATH for this test - it's what backup-database.sh/restore-database.sh themselves require");
}

fn seed_tenant_and_orders(store: &Store, n: u32) -> String {
    let created = store
        .create_tenant(
            NewTenant {
                key_custody_backend: "plain".into(),
                sealed_key_material: vec![0u8; 64],
                primary_address: "4backup_restore_test_addr".into(),
                network: "mainnet".into(),
                allowed_origins: vec!["https://merchant.example".into()],
                confirmations_required: None,
                order_expiry_seconds: None,
            },
            1_000,
        )
        .unwrap();

    for i in 0..n {
        store
            .create_order(NewOrder {
                confirmations_required_override: None,
                tenant_id: created.tenant.id.clone(),
                merchant_order_id: None,
                minor_index: i,
                address: format!("sub_{i}"),
                xmr_amount_piconero: 100,
                description: None,
                created_at: 1_000,
                expires_at: 2_000,
            })
            .unwrap();
    }

    created.tenant.id
}

/// Full drill: seed a live database, keep a writer hammering it, run the real
/// backup script mid-write, run the real restore script into a fresh
/// destination, and diff tenant/order/order_payments counts between source
/// and restored copy.
#[test]
#[ignore = "shells out to scripts/*.sh and requires the sqlite3 CLI - see module docs"]
fn backup_then_restore_preserves_tenants_and_orders_under_concurrent_writes() {
    require_sqlite3();

    let work_dir =
        std::env::temp_dir().join(format!("moneropay_backup_restore_test_{}", Uuid::new_v4()));
    std::fs::create_dir_all(&work_dir).unwrap();
    let src_db_path = work_dir.join("moneropay.db");
    let backup_dir = work_dir.join("backups");
    let restore_dest = work_dir.join("restored.db");

    // Seed the live database with an initial batch before the backup starts,
    // so the restore-side assertions have a known floor even if the
    // concurrent writer below happens to lose the race against .backup
    // entirely on a slow machine.
    let store = Store::open_file(src_db_path.to_str().unwrap()).unwrap();
    let tenant_id = seed_tenant_and_orders(&store, 20);

    // Concurrent writer: keeps inserting orders against the same live
    // database file for as long as the backup step is running - this is the
    // real hazard backup-database.sh's header comment reasons about (a
    // writer mid-commit while `.backup` steps through the source), not a
    // synthetic scenario.
    let stop = Arc::new(AtomicBool::new(false));
    let writer_store = Store::open_file(src_db_path.to_str().unwrap()).unwrap();
    let writer_tenant_id = tenant_id.clone();
    let writer_stop = stop.clone();
    let writer = std::thread::spawn(move || {
        let mut i = 1000u32;
        while !writer_stop.load(Ordering::Relaxed) {
            let _ = writer_store.create_order(NewOrder {
                confirmations_required_override: None,
                tenant_id: writer_tenant_id.clone(),
                merchant_order_id: None,
                minor_index: i,
                address: format!("sub_{i}"),
                xmr_amount_piconero: 100,
                description: None,
                created_at: 1_000,
                expires_at: 2_000,
            });
            i += 1;
        }
        i
    });

    // Give the writer a head start so the backup genuinely lands mid-stream
    // rather than possibly racing ahead of the first insert.
    std::thread::sleep(std::time::Duration::from_millis(50));

    let backup_output = Command::new(script("backup-database.sh"))
        .arg(&src_db_path)
        .arg(&backup_dir)
        .output()
        .expect("failed to run scripts/backup-database.sh");

    stop.store(true, Ordering::Relaxed);
    let final_writer_index = writer.join().unwrap();

    assert!(
        backup_output.status.success(),
        "backup-database.sh failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&backup_output.stdout),
        String::from_utf8_lossy(&backup_output.stderr)
    );

    // Read the source's own counts *after* the writer has stopped, so the
    // comparison below is against a stable, fully-settled source - the
    // backup itself may legitimately have captured fewer rows than this if
    // it ran before the writer finished, which is why we restore and compare
    // against the backup's own reported counts, not a live-source snapshot
    // taken at an arbitrary moment.
    let source_final = Store::open_file(src_db_path.to_str().unwrap()).unwrap();
    let source_tenant_count = source_final.count_tenants().unwrap();
    assert_eq!(source_tenant_count, 1);
    assert!(
        final_writer_index > 1000,
        "concurrent writer should have gotten at least one insert in before the backup completed"
    );

    // Locate the single backup file the script just produced.
    let backup_file = std::fs::read_dir(&backup_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().and_then(|e| e.to_str()) == Some("db"))
        .expect("backup-database.sh should have written exactly one .db file");

    // Restore it to a brand-new destination - the real WBS 2.3.1 acceptance
    // step: "restores cleanly on a fresh box."
    let restore_output = Command::new(script("restore-database.sh"))
        .arg(&backup_file)
        .arg(&restore_dest)
        .output()
        .expect("failed to run scripts/restore-database.sh");

    assert!(
        restore_output.status.success(),
        "restore-database.sh failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&restore_output.stdout),
        String::from_utf8_lossy(&restore_output.stderr)
    );

    // Open the *backup file itself* (independent of the restored copy) to
    // learn exactly what it captured - this is the real baseline the restore
    // must reproduce exactly, since the backup may have captured fewer rows
    // than the fully-settled source above.
    let backup_snapshot = Store::open_file(backup_file.to_str().unwrap()).unwrap();
    let backup_tenant_count = backup_snapshot.count_tenants().unwrap();
    let backup_order_count = count_orders(backup_file.to_str().unwrap());
    let backup_payment_count = count_order_payments(backup_file.to_str().unwrap());
    drop(backup_snapshot);

    // The literal WBS pass condition: diff tenant/order counts between the
    // backup that was taken and the restored copy on the "fresh box".
    let restored_tenant_count = Store::open_file(restore_dest.to_str().unwrap())
        .unwrap()
        .count_tenants()
        .unwrap();
    let restored_order_count = count_orders(restore_dest.to_str().unwrap());
    let restored_payment_count = count_order_payments(restore_dest.to_str().unwrap());

    assert_eq!(
        backup_tenant_count, 1,
        "the one seeded tenant must appear in the backup"
    );
    assert_eq!(
        restored_tenant_count, backup_tenant_count,
        "restored tenant count must exactly match the backup it was restored from"
    );
    assert_eq!(
        restored_order_count, backup_order_count,
        "restored order count must exactly match the backup it was restored from"
    );
    assert_eq!(
        restored_payment_count, backup_payment_count,
        "restored order_payments count must exactly match the backup it was restored from"
    );
    // Sanity floor: the backup must have captured at least the 20 orders
    // seeded before the writer even started, proving the .backup step really
    // did see committed data, not an empty/corrupt file.
    assert!(backup_order_count >= 20, "backup should contain at least the 20 orders seeded before the writer started, got {backup_order_count}");

    let _ = std::fs::remove_dir_all(&work_dir);
}

/// Drill the refusal-to-overwrite-without---force path directly: a restore
/// aimed at an existing destination must fail loudly rather than silently
/// clobber it, and must succeed once `--force` is supplied.
#[test]
#[ignore = "shells out to scripts/*.sh and requires the sqlite3 CLI - see module docs"]
fn restore_refuses_to_overwrite_an_existing_destination_without_force() {
    require_sqlite3();

    let work_dir = std::env::temp_dir().join(format!(
        "moneropay_backup_restore_force_test_{}",
        Uuid::new_v4()
    ));
    std::fs::create_dir_all(&work_dir).unwrap();
    let src_db_path = work_dir.join("moneropay.db");
    let backup_dir = work_dir.join("backups");
    let restore_dest = work_dir.join("restored.db");

    let store = Store::open_file(src_db_path.to_str().unwrap()).unwrap();
    seed_tenant_and_orders(&store, 3);
    drop(store);

    let backup_output = Command::new(script("backup-database.sh"))
        .arg(&src_db_path)
        .arg(&backup_dir)
        .output()
        .unwrap();
    assert!(backup_output.status.success());
    let backup_file = std::fs::read_dir(&backup_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().and_then(|e| e.to_str()) == Some("db"))
        .unwrap();

    // Something is already sitting at the destination.
    std::fs::write(&restore_dest, b"not a real database, must not be clobbered").unwrap();

    let refused = Command::new(script("restore-database.sh"))
        .arg(&backup_file)
        .arg(&restore_dest)
        .output()
        .unwrap();
    assert!(
        !refused.status.success(),
        "restore must refuse to overwrite an existing destination without --force"
    );
    assert_eq!(
        std::fs::read(&restore_dest).unwrap(),
        b"not a real database, must not be clobbered",
        "the pre-existing destination file must be untouched after a refused restore"
    );

    let forced = Command::new(script("restore-database.sh"))
        .arg(&backup_file)
        .arg(&restore_dest)
        .arg("--force")
        .output()
        .unwrap();
    assert!(
        forced.status.success(),
        "restore with --force should succeed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&forced.stdout),
        String::from_utf8_lossy(&forced.stderr)
    );
    let restored_tenant_count = Store::open_file(restore_dest.to_str().unwrap())
        .unwrap()
        .count_tenants()
        .unwrap();
    assert_eq!(restored_tenant_count, 1);

    let _ = std::fs::remove_dir_all(&work_dir);
}

fn count_orders(db_path: &str) -> i64 {
    let output = Command::new("sqlite3")
        .arg(db_path)
        .arg("SELECT COUNT(*) FROM orders;")
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap()
}

fn count_order_payments(db_path: &str) -> i64 {
    let output = Command::new("sqlite3")
        .arg(db_path)
        .arg("SELECT COUNT(*) FROM order_payments;")
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap()
}
