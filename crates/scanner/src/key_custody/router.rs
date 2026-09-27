//! Per-store key custody (admin_settings_v2.md part 5, decision D3).
//!
//! A `CustodyRouter` holds one `KeyCustody` per enabled backend, by name
//! (`plain`, `socket`), and is itself a `KeyCustody`, so everything that
//! already takes `&dyn KeyCustody` (the scan loop, order creation) keeps
//! working. New wallets are registered in a named backend
//! (`register_wallet_in`, `unseal_and_register_in`); the router remembers
//! which backend issued each handle and routes every later call on that
//! handle there. A `WalletHandle` means nothing outside the backend that
//! issued it, so the pairing always travels with it here.
//!
//! Nothing ever moves key material between backends: moving a store means
//! entering its keys again (`http::admin::switch_key_custody`).

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use monero::{Address, Transaction};
use parking_lot::RwLock;

use super::{
    KeyCustody, KeyCustodyError, MatchedOutput, Network, ScanIndices, SubaddressIndex, WalletHandle, WalletMaterial,
};

#[derive(Default)]
pub struct CustodyRouter {
    backends: RwLock<HashMap<String, Arc<dyn KeyCustody>>>,
    /// Which backend issued each live handle.
    handles: RwLock<HashMap<WalletHandle, String>>,
    /// Each backend's state epoch as last seen (see `KeyCustody::check_state`).
    epochs: RwLock<HashMap<String, u64>>,
    /// Which backend new wallets go to when a caller doesn't say.
    default_backend: RwLock<String>,
}

impl CustodyRouter {
    /// A router over the given backends, `default` taking wallets registered
    /// without a backend name.
    pub fn new(backends: HashMap<String, Arc<dyn KeyCustody>>, default: &str) -> Self {
        let router = CustodyRouter::default();
        *router.backends.write() = backends;
        *router.default_backend.write() = default.to_string();
        router
    }

    /// One in-process backend called `plain`: what tests and a
    /// single-backend engine use.
    pub fn plain() -> Self {
        let mut backends: HashMap<String, Arc<dyn KeyCustody>> = HashMap::new();
        backends.insert("plain".to_string(), Arc::new(super::PlainKeyCustody::default()));
        CustodyRouter::new(backends, "plain")
    }

    /// The backend called `name`, if enabled.
    pub fn backend(&self, name: &str) -> Option<Arc<dyn KeyCustody>> {
        self.backends.read().get(name).cloned()
    }

    pub fn default_backend(&self) -> String {
        self.default_backend.read().clone()
    }

    /// Installs a new set of backends (task 5.2). Handles issued by a
    /// backend that is gone are forgotten: those stores stop being scanned
    /// until the backend is enabled again or they move to another one.
    /// Backends kept from the previous set must be passed in as the same
    /// `Arc`, so their wallets stay registered.
    pub fn replace(&self, backends: HashMap<String, Arc<dyn KeyCustody>>, default: &str) {
        {
            let mut handles = self.handles.write();
            handles.retain(|_, backend| backends.contains_key(backend));
        }
        {
            let mut epochs = self.epochs.write();
            epochs.retain(|backend, _| backends.contains_key(backend));
        }
        *self.backends.write() = backends;
        *self.default_backend.write() = default.to_string();
    }

    /// The backends as they are now, for building the next set from.
    pub fn backends(&self) -> HashMap<String, Arc<dyn KeyCustody>> {
        self.backends.read().clone()
    }

    fn for_handle(&self, handle: WalletHandle) -> Result<Arc<dyn KeyCustody>, KeyCustodyError> {
        let name = self.handles.read().get(&handle).cloned().ok_or(KeyCustodyError::UnknownWallet)?;
        self.backend(&name).ok_or(KeyCustodyError::UnknownWallet)
    }

    fn named(&self, backend: &str) -> Result<Arc<dyn KeyCustody>, KeyCustodyError> {
        self.backend(backend)
            .ok_or_else(|| KeyCustodyError::BackendUnavailable(format!("the {backend:?} key custody backend is not enabled")))
    }

    fn remember(&self, handle: WalletHandle, backend: &str) {
        self.handles.write().insert(handle, backend.to_string());
    }

    /// Which backend holds `handle`, if it's live.
    pub fn backend_of(&self, handle: WalletHandle) -> Option<String> {
        self.handles.read().get(&handle).cloned()
    }
}

#[async_trait::async_trait]
impl KeyCustody for CustodyRouter {
    async fn register_wallet(&self, material: WalletMaterial) -> Result<WalletHandle, KeyCustodyError> {
        let backend = self.default_backend();
        self.register_wallet_in(&backend, material).await
    }

    async fn remove_wallet(&self, handle: WalletHandle) -> Result<(), KeyCustodyError> {
        let result = match self.for_handle(handle) {
            Ok(custody) => custody.remove_wallet(handle).await,
            Err(e) => Err(e),
        };
        self.handles.write().remove(&handle);
        result
    }

    async fn seal(&self, material: &WalletMaterial) -> Result<Vec<u8>, KeyCustodyError> {
        let backend = self.default_backend();
        self.seal_in(&backend, material).await
    }

    async fn unseal_and_register(&self, sealed: &[u8]) -> Result<WalletHandle, KeyCustodyError> {
        let backend = self.default_backend();
        self.unseal_and_register_in(&backend, sealed).await
    }

    async fn derive_subaddress(
        &self,
        handle: WalletHandle,
        index: SubaddressIndex,
        network: Network,
    ) -> Result<Address, KeyCustodyError> {
        self.for_handle(handle)?.derive_subaddress(handle, index, network).await
    }

    async fn scan_tx_outputs(
        &self,
        handle: WalletHandle,
        tx: &Transaction,
        major_range: Range<u32>,
        minor_range: Range<u32>,
    ) -> Result<Vec<MatchedOutput>, KeyCustodyError> {
        self.for_handle(handle)?.scan_tx_outputs(handle, tx, major_range, minor_range).await
    }

    async fn scan_tx_outputs_for_indices(
        &self,
        handle: WalletHandle,
        tx: &Transaction,
        indices: &ScanIndices,
    ) -> Result<Vec<MatchedOutput>, KeyCustodyError> {
        self.for_handle(handle)?.scan_tx_outputs_for_indices(handle, tx, indices).await
    }

    /// Asks every backend. A backend whose epoch went up has lost its
    /// wallets: its handles are forgotten here (so `handle_is_live` says so
    /// and the caller registers them again). Returns the sum of the epochs,
    /// which only ever goes up.
    async fn check_state(&self) -> Result<u64, KeyCustodyError> {
        let backends = self.backends();
        let mut total = 0;
        let mut first_error = None;
        for (name, custody) in backends {
            match custody.check_state().await {
                Ok(epoch) => {
                    total += epoch;
                    let previous = self.epochs.write().insert(name.clone(), epoch).unwrap_or(0);
                    if epoch > previous {
                        self.handles.write().retain(|_, backend| backend != &name);
                    }
                }
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
        match first_error {
            // A backend that can't be asked (it is down) doesn't stop the
            // others being checked; its own calls fail meanwhile.
            Some(e) if total == 0 && self.backends.read().len() == 1 => Err(e),
            _ => Ok(total),
        }
    }

    async fn register_wallet_in(&self, backend: &str, material: WalletMaterial) -> Result<WalletHandle, KeyCustodyError> {
        let handle = self.named(backend)?.register_wallet(material).await?;
        self.remember(handle, backend);
        Ok(handle)
    }

    async fn unseal_and_register_in(&self, backend: &str, sealed: &[u8]) -> Result<WalletHandle, KeyCustodyError> {
        let handle = self.named(backend)?.unseal_and_register(sealed).await?;
        self.remember(handle, backend);
        Ok(handle)
    }

    async fn seal_in(&self, backend: &str, material: &WalletMaterial) -> Result<Vec<u8>, KeyCustodyError> {
        self.named(backend)?.seal(material).await
    }

    fn handle_is_live(&self, handle: WalletHandle) -> bool {
        self.for_handle(handle).is_ok()
    }

    fn enabled_backends(&self) -> Vec<String> {
        let mut names: Vec<String> = self.backends.read().keys().cloned().collect();
        names.sort();
        names
    }

    /// Asks each backend directly, with a short deadline, without touching
    /// the epochs the scan loop acts on.
    async fn backend_health(&self) -> Vec<(String, Option<String>)> {
        let mut backends: Vec<(String, Arc<dyn KeyCustody>)> = self.backends().into_iter().collect();
        backends.sort_by(|a, b| a.0.cmp(&b.0));
        let mut health = Vec::with_capacity(backends.len());
        for (name, custody) in backends {
            let error = match tokio::time::timeout(std::time::Duration::from_secs(2), custody.check_state()).await {
                Ok(Ok(_)) => None,
                Ok(Err(e)) => Some(e.to_string()),
                Err(_) => Some("did not answer within 2 seconds".to_string()),
            };
            health.push((name, error));
        }
        health
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key_custody::PlainKeyCustody;

    fn material(seed: u8) -> WalletMaterial {
        let mut view = [seed; 32];
        view[31] &= 0x0f;
        let mut spend = [seed.wrapping_add(1); 32];
        spend[31] &= 0x0f;
        let spend = monero::PublicKey::from_private_key(&monero::PrivateKey::from_slice(&spend).unwrap()).to_bytes();
        WalletMaterial::new(view, spend)
    }

    fn two_backends() -> (CustodyRouter, Arc<dyn KeyCustody>, Arc<dyn KeyCustody>) {
        let a: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
        let b: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
        let mut backends = HashMap::new();
        backends.insert("plain".to_string(), a.clone());
        backends.insert("socket".to_string(), b.clone());
        (CustodyRouter::new(backends, "plain"), a, b)
    }

    #[tokio::test]
    async fn each_wallet_lives_in_its_own_backend_and_calls_are_routed_by_handle() {
        let (router, a, b) = two_backends();
        let in_a = router.register_wallet_in("plain", material(1)).await.unwrap();
        let in_b = router.register_wallet_in("socket", material(3)).await.unwrap();
        assert!(a.derive_subaddress(in_a, SubaddressIndex::default(), Network::Mainnet).await.is_ok());
        assert!(b.derive_subaddress(in_a, SubaddressIndex::default(), Network::Mainnet).await.is_err(), "not in the other one");
        assert!(router.derive_subaddress(in_b, SubaddressIndex::default(), Network::Mainnet).await.is_ok());
        assert_eq!(router.backend_of(in_b).as_deref(), Some("socket"));
    }

    #[tokio::test]
    async fn a_disabled_backend_can_not_take_wallets_and_its_handles_stop_being_live() {
        let (router, a, _) = two_backends();
        let in_b = router.register_wallet_in("socket", material(3)).await.unwrap();
        let mut only_plain: HashMap<String, Arc<dyn KeyCustody>> = HashMap::new();
        only_plain.insert("plain".to_string(), a);
        router.replace(only_plain, "plain");
        assert!(!router.handle_is_live(in_b));
        assert!(matches!(
            router.register_wallet_in("socket", material(5)).await,
            Err(KeyCustodyError::BackendUnavailable(_))
        ));
        assert_eq!(router.enabled_backends(), vec!["plain".to_string()]);
    }
}
