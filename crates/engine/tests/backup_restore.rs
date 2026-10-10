//! The backup's acceptance test: "actually perform the restore once against
//! a copy; diff tenant/order counts before and after as the pass condition."
//!
//! Drives the real `deploy/backup/monokulo-backup.sh` and
//! `deploy/backup/monokulo-restore.sh` (not a reimplementation of their
//! logic) against a data folder holding a real `Store`-backed engine.db and a
//! monokulo.db, with a concurrent writer thread still inserting orders while
//! the backup runs - the exact hazard `monokulo-backup.sh`'s header reasons
//! about (a writer mid-commit while `.backup` runs). Needs the `sqlite3` CLI
//! on PATH, as the scripts themselves do, and fails without it rather than
//! skipping. Unix only: the scripts are shell scripts, which Windows can't
//! run.

#![cfg(unix)]
// An integration test crate: every function in it is test code, which
// fails by panicking.
#![expect(
    clippy::tests_outside_test_module,
    clippy::unwrap_used,
    reason = "an integration test crate is all test code"
)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use engine::store::{NewOrder, NewTenant, Store};
use uuid::Uuid;

/// Path to a script in the workspace's `deploy/backup/` directory. Cargo
/// runs this test with the engine crate as its working directory.
fn script(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("deploy/backup")
        .join(name)
}

/// A small monokulo.db beside the engine's: the scripts back up and restore
/// whichever of the two a data folder holds.
fn seed_monokulo_db(path: &std::path::Path) {
    let status = Command::new("sqlite3")
        .arg(path)
        .arg("PRAGMA journal_mode=WAL; CREATE TABLE users (id INTEGER PRIMARY KEY); CREATE TABLE wallets (id INTEGER PRIMARY KEY); INSERT INTO users VALUES (1), (2); INSERT INTO wallets VALUES (1);")
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
}

/// The one backup folder `monokulo-backup.sh` wrote into `backup_dir`.
fn backup_folder(backup_dir: &std::path::Path) -> PathBuf {
    let folders: Vec<PathBuf> = std::fs::read_dir(backup_dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    assert_eq!(
        folders.len(),
        1,
        "monokulo-backup.sh should have written exactly one backup folder: {folders:?}"
    );
    assert!(
        folders[0]
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("monokulo-"),
        "a backup folder is named monokulo-<timestamp>: {folders:?}"
    );
    folders[0].clone()
}

fn require_sqlite3() {
    let ok = Command::new("sqlite3")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(
        ok,
        "the sqlite3 CLI must be on PATH for this test, as monokulo-backup.sh and \
         monokulo-restore.sh themselves require it: install it with \
         `sudo apt install sqlite3` (Debian, Ubuntu), `sudo dnf install sqlite` (Fedora), \
         `sudo pacman -S sqlite` (Arch) or `brew install sqlite` (macOS)"
    );
}

fn seed_tenant_and_orders(store: &Store, n: u32) -> String {
    let created = store
        .create_tenant(
            &NewTenant {
                key_custody_backend: "plain".into(),
                sealed_key_material: vec![0u8; 64],
                primary_address: "4backup_restore_test_addr".into(),
                network: "mainnet".into(),
                confirmations_required: None,
                order_expiry_seconds: None,
            },
            1_000,
        )
        .unwrap();

    for i in 0..n {
        store
            .create_order(&NewOrder {
                idempotency_key: None,
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

    created.tenant.id.into_string()
}

/// Full drill: seed a live database, keep a writer hammering it, run the real
/// backup script mid-write, run the real restore script into a fresh
/// destination, and diff `tenant/order/order_payments` counts between source
/// and restored copy.
#[test]
fn backup_then_restore_preserves_tenants_and_orders_under_concurrent_writes() {
    require_sqlite3();

    let work_dir =
        std::env::temp_dir().join(format!("monokulo_backup_restore_test_{}", Uuid::new_v4()));
    let data_dir = work_dir.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let src_db_path = data_dir.join("engine.db");
    seed_monokulo_db(&data_dir.join("monokulo.db"));
    let backup_dir = work_dir.join("backups");
    let restore_data = work_dir.join("restored");
    let restore_dest = restore_data.join("engine.db");

    // Seed the live database with an initial batch before the backup starts,
    // so the restore-side assertions have a known floor even if the
    // concurrent writer below happens to lose the race against .backup
    // entirely on a slow machine.
    let store = Store::open_file(src_db_path.to_str().unwrap()).unwrap();
    let tenant_id = seed_tenant_and_orders(&store, 20);

    // Concurrent writer: keeps inserting orders against the same live
    // database file for as long as the backup step is running - this is the
    // real hazard monokulo-backup.sh's header comment reasons about (a
    // writer mid-commit while `.backup` steps through the source), not a
    // synthetic scenario.
    let stop = Arc::new(AtomicBool::new(false));
    let writer_store = Store::open_file(src_db_path.to_str().unwrap()).unwrap();
    let writer_tenant_id = tenant_id;
    let writer_stop = Arc::clone(&stop);
    // Counts the inserts that committed, not the attempts: an attempt that
    // failed (the database locked by the backup, say) proves nothing about
    // a backup taken mid-write.
    let writer = std::thread::spawn(move || {
        let mut i = 1000u32;
        let mut inserted = 0u32;
        while !writer_stop.load(Ordering::Relaxed) {
            let created = writer_store.create_order(&NewOrder {
                idempotency_key: None,
                confirmations_required_override: None,
                tenant_id: shared::ids::TenantId::new(writer_tenant_id.clone()),
                merchant_order_id: None,
                minor_index: i,
                address: format!("sub_{i}"),
                xmr_amount_piconero: 100,
                description: None,
                created_at: 1_000,
                expires_at: 2_000,
            });
            if created.is_ok() {
                inserted += 1;
            }
            i += 1;
        }
        inserted
    });

    // Give the writer a head start so the backup genuinely lands mid-stream
    // rather than possibly racing ahead of the first insert.
    std::thread::sleep(std::time::Duration::from_millis(50));

    let backup_output = Command::new(script("monokulo-backup.sh"))
        .arg(&data_dir)
        .arg(&backup_dir)
        .output()
        .expect("failed to run deploy/backup/monokulo-backup.sh");

    stop.store(true, Ordering::Relaxed);
    let inserted_concurrently = writer.join().unwrap();

    assert!(
        backup_output.status.success(),
        "monokulo-backup.sh failed:\nstdout: {}\nstderr: {}",
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
        inserted_concurrently > 0,
        "concurrent writer should have gotten at least one insert in before the backup completed"
    );

    // Locate the backup folder the script just produced: both databases and
    // their checksums.
    let folder = backup_folder(&backup_dir);
    let backup_file = folder.join("engine.db");
    assert!(folder.join("monokulo.db").is_file() && folder.join("SHA256SUMS").is_file());

    // Restore it into a brand-new data folder - the acceptance step:
    // "restores cleanly on a fresh box."
    let restore_output = Command::new(script("monokulo-restore.sh"))
        .arg(&folder)
        .arg(&restore_data)
        .output()
        .expect("failed to run deploy/backup/monokulo-restore.sh");

    assert!(
        restore_output.status.success(),
        "monokulo-restore.sh failed:\nstdout: {}\nstderr: {}",
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
    assert_eq!(
        count(restore_data.join("monokulo.db").to_str().unwrap(), "users"),
        2,
        "monokulo.db is restored beside engine.db"
    );

    let _ = std::fs::remove_dir_all(&work_dir);
}

/// Drill the refusal-to-overwrite-without---force path directly: a restore
/// aimed at an existing destination must fail loudly rather than silently
/// clobber it, and must succeed once `--force` is supplied.
#[test]
fn restore_refuses_to_overwrite_an_existing_destination_without_force() {
    require_sqlite3();

    let work_dir = std::env::temp_dir().join(format!(
        "monokulo_backup_restore_force_test_{}",
        Uuid::new_v4()
    ));
    let data_dir = work_dir.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let src_db_path = data_dir.join("engine.db");
    let backup_dir = work_dir.join("backups");
    let restore_data = work_dir.join("restored");
    std::fs::create_dir_all(&restore_data).unwrap();
    let restore_dest = restore_data.join("engine.db");

    let store = Store::open_file(src_db_path.to_str().unwrap()).unwrap();
    seed_tenant_and_orders(&store, 3);
    drop(store);

    let backup_output = Command::new(script("monokulo-backup.sh"))
        .arg(&data_dir)
        .arg(&backup_dir)
        .output()
        .unwrap();
    assert!(backup_output.status.success());
    let folder = backup_folder(&backup_dir);

    // Something is already sitting at the destination.
    std::fs::write(&restore_dest, b"not a real database, must not be clobbered").unwrap();

    let refused = Command::new(script("monokulo-restore.sh"))
        .arg(&folder)
        .arg(&restore_data)
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

    let forced = Command::new(script("monokulo-restore.sh"))
        .arg(&folder)
        .arg(&restore_data)
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

fn count(db_path: &str, table: &str) -> i64 {
    let output = Command::new("sqlite3")
        .arg(db_path)
        .arg(format!("SELECT COUNT(*) FROM {table};"))
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap()
}

fn count_orders(db_path: &str) -> i64 {
    count(db_path, "orders")
}

fn count_order_payments(db_path: &str) -> i64 {
    count(db_path, "order_payments")
}
