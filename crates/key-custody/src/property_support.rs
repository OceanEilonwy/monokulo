//! Shared fixtures for generated custody tests. All crypto stays real.
use crate::*;
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
    pub(crate) registration_entered: tokio::sync::Notify,
    pub(crate) registration_release: tokio::sync::Notify,
    pub(crate) entered: tokio::sync::Notify,
    pub(crate) release: tokio::sync::Notify,
}
impl GateCustody {
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
        self.inner.scan_txs_for_indices(handle, txs, indices).await
    }
}
pub(crate) fn custody_arc(custody: &Arc<GateCustody>) -> Arc<dyn KeyCustody> {
    Arc::<GateCustody>::clone(custody)
}
