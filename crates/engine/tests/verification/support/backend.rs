//! Owns the active observer and executor; a reopen replaces both.
#![expect(
    clippy::unwrap_used,
    reason = "verification connection setup must fail loudly on malformed state"
)]
use crate::store::{db::Class, Db, SharedStore, Store, StoreError};
use std::sync::Arc;

pub(crate) struct Backend {
    db: Option<Db>,
    store: Option<SharedStore>,
    path: String,
    worker: bool,
}
impl Backend {
    pub(crate) fn new(path: &str, store: SharedStore, worker: bool) -> Self {
        let db = if worker {
            Db::open(path, &store.lock()).unwrap()
        } else {
            Db::over_shared(Arc::clone(&store))
        };
        Self {
            db: Some(db),
            store: Some(store),
            path: path.into(),
            worker,
        }
    }
    pub(crate) fn db(&self) -> &Db {
        self.db.as_ref().unwrap()
    }
    pub(crate) fn store(&self) -> &SharedStore {
        self.store.as_ref().unwrap()
    }
    pub(crate) async fn reopen(&mut self) {
        // TEMP state proves operations use a new executor connection; observing
        // a second connection alone would not meet this positive control.
        self.db()
            .run(Class::Admin, |s| {
                s.conn_for_test().execute_batch(
                    "CREATE TEMP TABLE verification_connection_probe (id INTEGER)",
                )?;
                Ok::<_, StoreError>(())
            })
            .await
            .unwrap();
        replace(&mut self.db, &mut self.store, &self.path, self.worker);
        let next = self;
        let temporary: i64 = next.db().run(Class::Admin, |s| {
            s.conn_for_test().query_row("SELECT count(*) FROM sqlite_temp_master WHERE name='verification_connection_probe'", [], |r| r.get(0)).map_err(StoreError::from)
        }).await.unwrap();
        assert_eq!(temporary, 0, "BOUNDARY: executor-connection-replaced");
    }
}

/// Shared connection replacement; callers retain their independent domain
/// oracle checks. Direct histories can use this without an async executor probe.
pub(crate) fn replace(
    db: &mut Option<Db>,
    observer: &mut Option<SharedStore>,
    path: &str,
    worker: bool,
) {
    let before = snapshot(&observer.as_ref().unwrap().lock());
    let old = Arc::downgrade(observer.as_ref().unwrap());
    drop(db.take());
    drop(observer.take());
    assert!(old.upgrade().is_none(), "BOUNDARY: active-store-replaced");
    let store = Store::open_file(path).unwrap().into_shared();
    assert_eq!(
        snapshot(&store.lock()),
        before,
        "BOUNDARY: reopen-durable-state"
    );
    *db = Some(if worker {
        Db::open(path, &store.lock()).unwrap()
    } else {
        Db::over_shared(Arc::clone(&store))
    });
    *observer = Some(store);
}

/// Snapshot persisted tables, including money, work, proof and delivery state.
/// Sorting observed rows does not derive expected ledger/status decisions.
fn snapshot(store: &Store) -> Vec<(String, Vec<String>)> {
    let conn = store.conn_for_test();
    let mut tables = conn.prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap();
    let names = tables
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    names
        .into_iter()
        .map(|name| {
            let mut statement = conn
                .prepare(&format!("SELECT * FROM \"{}\"", name.replace('"', "\"\"")))
                .unwrap();
            let columns = statement.column_count();
            let mut rows = statement
                .query_map([], |r| {
                    (0..columns)
                        .map(|i| r.get::<_, rusqlite::types::Value>(i))
                        .collect::<Result<Vec<_>, _>>()
                })
                .unwrap()
                .map(|r| format!("{:?}", r.unwrap()))
                .collect::<Vec<_>>();
            rows.sort();
            (name, rows)
        })
        .collect()
}
