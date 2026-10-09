//! Shared fixtures for generated boundary tests. All crypto and SQL stay real.
use crate::key_custody::*;
use std::sync::atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;

pub(crate) fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
pub(crate) fn config() -> proptest::test_runner::Config {
    let mut c = proptest::test_runner::Config::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        c.cases = 64;
    }
    c
}
/// Pin replay files to their historical paths when test sources move.
/// Respect Proptest's option to disable persistence and retain seed/shrink settings.
///
/// `path` names a module's file, `dir/module.txt`; each property gets its
/// own `dir/module/<test>.txt`. Proptest replays every seed in its file
/// before generating cases, so one file per module made every property
/// replay every other property's seeds as extra random cases.
pub(crate) fn persist(
    mut config: proptest::test_runner::Config,
    path: &'static str,
) -> proptest::test_runner::Config {
    if config.failure_persistence.is_some() {
        config.failure_persistence = Some(Box::new(
            proptest::test_runner::FileFailurePersistence::Direct(test_file(path)),
        ));
    }
    config
}

/// The running property's file under `path`'s module directory. The test
/// is named from its thread, which libtest names after the test's path:
/// the `proptest!` macro sets `Config::test_name` only after the config
/// expression has been evaluated, so `persist` can't read it there.
fn test_file(path: &str) -> &'static str {
    let module = path
        .strip_suffix(".txt")
        .unwrap_or_else(|| panic!("regression file {path} must end in .txt"));
    let thread = std::thread::current();
    let test = thread
        .name()
        .filter(|name| *name != "main")
        .and_then(|name| name.rsplit("::").next())
        .unwrap_or_else(|| panic!("persist must run on a libtest test thread, not {thread:?}"));
    Box::leak(format!("{module}/{test}.txt").into_boxed_str())
}

#[test]
fn each_property_replays_only_its_own_regression_file() {
    assert_eq!(
        test_file("/seeds/work/properties.txt"),
        "/seeds/work/properties/each_property_replays_only_its_own_regression_file.txt"
    );
}
pub(crate) use crate::verification_temp_db::TempDb as TempFile;
/// Modes: normal, unavailable, rendezvous. The rendezvous lets tests cancel
/// precisely during address derivation instead of relying on timing guesses.
#[derive(Default)]
pub(crate) struct GateCustody {
    pub(crate) inner: PlainKeyCustody,
    pub(crate) mode: AtomicU8,
    pub(crate) attempted: AtomicUsize,
    pub(crate) epoch: AtomicU64,
    pub(crate) registration_mode: AtomicU8,
    pub(crate) registrations: AtomicUsize,
    pub(crate) scan_mode: AtomicU8,
    pub(crate) scans: AtomicUsize,
    pub(crate) scan_entered: tokio::sync::Notify,
    pub(crate) scan_release: tokio::sync::Notify,
    pub(crate) registration_entered: tokio::sync::Notify,
    pub(crate) registration_release: tokio::sync::Notify,
    pub(crate) entered: tokio::sync::Notify,
    pub(crate) release: tokio::sync::Notify,
}
impl GateCustody {
    async fn scan_gate(&self) -> Result<(), KeyCustodyError> {
        self.scans.fetch_add(1, Ordering::SeqCst);
        if self.scan_mode.load(Ordering::SeqCst) == 2 {
            self.scan_entered.notify_one();
            self.scan_release.notified().await;
        }
        if self.scan_mode.load(Ordering::SeqCst) == 1 {
            return Err(KeyCustodyError::BackendUnavailable(
                "injected scan failure".into(),
            ));
        }
        Ok(())
    }
    async fn registration_gate(&self) {
        self.registrations.fetch_add(1, Ordering::Relaxed);
        if self.registration_mode.load(Ordering::Relaxed) == 2 {
            self.registration_entered.notify_one();
            self.registration_release.notified().await;
        }
    }
}
#[async_trait::async_trait]
impl KeyCustody for GateCustody {
    async fn register_wallet(
        &self,
        material: WalletMaterial,
    ) -> Result<WalletHandle, KeyCustodyError> {
        let handle = self.inner.register_wallet(material).await?;
        self.registration_gate().await;
        Ok(handle)
    }
    async fn remove_wallet(&self, handle: WalletHandle) -> Result<(), KeyCustodyError> {
        self.inner.remove_wallet(handle).await
    }
    async fn seal(&self, material: &WalletMaterial) -> Result<Vec<u8>, KeyCustodyError> {
        self.inner.seal(material).await
    }
    async fn unseal_and_register(&self, sealed: &[u8]) -> Result<WalletHandle, KeyCustodyError> {
        let handle = self.inner.unseal_and_register(sealed).await?;
        self.registration_gate().await;
        Ok(handle)
    }
    async fn unseal_and_register_idempotent(
        &self,
        sealed: &[u8],
        registration_id: &str,
    ) -> Result<WalletHandle, KeyCustodyError> {
        let handle = self
            .inner
            .unseal_and_register_idempotent(sealed, registration_id)
            .await?;
        self.registration_gate().await;
        Ok(handle)
    }
    async fn check_state(&self) -> Result<u64, KeyCustodyError> {
        Ok(self.epoch.load(Ordering::Relaxed))
    }
    async fn derive_subaddress(
        &self,
        handle: WalletHandle,
        index: SubaddressIndex,
        network: Network,
    ) -> Result<monero::Address, KeyCustodyError> {
        self.attempted.fetch_add(1, Ordering::Relaxed);
        match self.mode.load(Ordering::Relaxed) {
            1 => {
                return Err(KeyCustodyError::BackendUnavailable(
                    "injected derivation failure".to_owned(),
                ))
            }
            2 => {
                self.entered.notify_one();
                self.release.notified().await;
            }
            _ => {}
        }
        self.inner.derive_subaddress(handle, index, network).await
    }
    async fn scan_tx_outputs(
        &self,
        handle: WalletHandle,
        tx: &ScanInput,
        major_range: std::ops::Range<u32>,
        minor_range: std::ops::Range<u32>,
    ) -> Result<Vec<MatchedOutput>, KeyCustodyError> {
        self.scan_gate().await?;
        self.inner
            .scan_tx_outputs(handle, tx, major_range, minor_range)
            .await
    }
    async fn scan_txs_for_indices(
        &self,
        handle: WalletHandle,
        txs: &[ScanInput],
        indices: &ScanIndices,
    ) -> Result<Vec<TxMatches>, KeyCustodyError> {
        self.scan_gate().await?;
        self.inner.scan_txs_for_indices(handle, txs, indices).await
    }
}
pub(crate) fn custody_arc(custody: &Arc<GateCustody>) -> Arc<dyn KeyCustody> {
    Arc::<GateCustody>::clone(custody)
}

/// Owns a crash subprocess, including cleanup when an assertion fails.
pub(crate) struct CrashChild(pub(crate) Option<std::process::Child>);
impl CrashChild {
    pub(crate) async fn rendezvous(&mut self, path: &str, point: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if std::fs::read_to_string(format!("{path}.ready")).is_ok_and(|ready| ready == point) {
                return;
            }
            let finished = self.0.as_mut().unwrap().try_wait().unwrap().is_some();
            if finished || std::time::Instant::now() >= deadline {
                let output = self.finish();
                panic!(
                    "child never reached {point}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            // The deadline is wall time: virtual-clock tests may intentionally
            // prevent automatic time jumps while an OS worker is outstanding.
            tokio::task::spawn_blocking(|| std::thread::sleep(std::time::Duration::from_millis(2)))
                .await
                .unwrap();
        }
    }
    pub(crate) fn finish(&mut self) -> std::process::Output {
        let mut child = self.0.take().unwrap();
        let _ = child.kill();
        child.wait_with_output().unwrap()
    }
}
impl Drop for CrashChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub(crate) struct WorkerRelease(Option<std::sync::mpsc::Sender<()>>);
impl WorkerRelease {
    pub(crate) fn release(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}
impl Drop for WorkerRelease {
    fn drop(&mut self) {
        self.release();
    }
}

/// Holds a real worker between transactions, with a positive admission ack.
pub(crate) async fn hold_worker(db: &crate::store::Db) -> WorkerRelease {
    let (release, hold) = std::sync::mpsc::channel();
    let (entered, ready) = tokio::sync::oneshot::channel();
    let db = db.clone();
    let task = tokio::spawn(async move {
        db.run(
            crate::store::db::Class::Admin,
            move |_| -> Result<(), crate::store::StoreError> {
                let _ = entered.send(());
                let _ = hold.recv();
                Ok(())
            },
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), ready)
        .await
        .unwrap()
        .unwrap();
    // Dropping a JoinHandle detaches this bounded job. Release always unblocks it.
    drop(task);
    WorkerRelease(Some(release))
}

pub(crate) async fn wait_queued(
    db: &crate::store::Db,
    class: crate::store::db::Class,
    count: usize,
) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while db.queued(class) != count {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

/// Keep a paused clock from jumping past real OS-worker replies. Tests still
/// advance it explicitly; the guard is aborted when the scenario ends.
pub(crate) struct VirtualClockHold(tokio::task::JoinHandle<()>);
impl Drop for VirtualClockHold {
    fn drop(&mut self) {
        self.0.abort();
    }
}
pub(crate) fn hold_virtual_clock() -> VirtualClockHold {
    VirtualClockHold(tokio::spawn(async {
        loop {
            tokio::task::yield_now().await;
        }
    }))
}
