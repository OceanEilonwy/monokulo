//! Per-store key custody (`admin_settings_v2.md` part 5, decision D3).
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

use monero::Address;
use parking_lot::RwLock;

use crate::{
    KeyCustody, KeyCustodyError, MatchedOutput, Network, ScanIndices, ScanInput, SubaddressIndex,
    TxMatches, WalletHandle, WalletMaterial,
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
        let router = Self::default();
        *router.backends.write() = backends;
        default.clone_into(&mut router.default_backend.write());
        router
    }

    /// One in-process backend called `plain`: what tests and a
    /// single-backend engine use.
    pub fn plain() -> Self {
        let mut backends: HashMap<String, Arc<dyn KeyCustody>> = HashMap::new();
        backends.insert(
            "plain".to_owned(),
            Arc::new(crate::PlainKeyCustody::default()),
        );
        Self::new(backends, "plain")
    }

    /// The backend called `name`, if enabled.
    pub fn backend(&self, name: &str) -> Option<Arc<dyn KeyCustody>> {
        self.backends.read().get(name).cloned()
    }

    pub fn default_backend(&self) -> String {
        self.default_backend.read().clone()
    }

    /// Installs a new set of backends (task 5.2). Handles issued by a
    /// backend that is gone, or replaced by a new instance under the same
    /// name (a socket backend pointed at another server), are forgotten: a
    /// replaced backend's stores are registered again in the new instance
    /// by the scan loop, and a removed backend's stores wait until it is
    /// enabled again or they move. Backends kept from the previous set must
    /// be passed in as the same `Arc`, so their wallets stay registered.
    ///
    /// Returns the forgotten handles with the instance that issued them, so
    /// the caller can free them there (best effort - see `free_handles`).
    #[expect(
        clippy::needless_pass_by_value,
        reason = "the router takes the new set as its own; callers build it to hand over"
    )]
    pub fn replace(
        &self,
        backends: HashMap<String, Arc<dyn KeyCustody>>,
        default: &str,
    ) -> Vec<(Arc<dyn KeyCustody>, WalletHandle)> {
        let old = std::mem::replace(&mut *self.backends.write(), backends.clone());
        let kept = |name: &str| match (old.get(name), backends.get(name)) {
            (Some(before), Some(after)) => same_instance(before, after),
            _ => false,
        };
        let mut dropped = Vec::new();
        self.handles.write().retain(|handle, name| {
            if kept(name) {
                return true;
            }
            if let Some(before) = old.get(name.as_str()) {
                dropped.push((Arc::clone(before), *handle));
            }
            false
        });
        self.epochs.write().retain(|name, _| kept(name));
        default.clone_into(&mut self.default_backend.write());
        dropped
    }

    /// Forgets `handle` if its backend says it doesn't know it, so every
    /// liveness check (`handle_is_live`) sees it gone and the store is
    /// registered again - whatever made the backend lose it.
    fn forget_if_unknown<T>(
        &self,
        handle: WalletHandle,
        result: Result<T, KeyCustodyError>,
    ) -> Result<T, KeyCustodyError> {
        if matches!(result, Err(KeyCustodyError::UnknownWallet)) {
            self.handles.write().remove(&handle);
        }
        result
    }

    /// The backends as they are now, for building the next set from.
    pub fn backends(&self) -> HashMap<String, Arc<dyn KeyCustody>> {
        self.backends.read().clone()
    }

    fn for_handle(&self, handle: WalletHandle) -> Result<Arc<dyn KeyCustody>, KeyCustodyError> {
        let name = self
            .handles
            .read()
            .get(&handle)
            .cloned()
            .ok_or(KeyCustodyError::UnknownWallet)?;
        self.backend(&name).ok_or(KeyCustodyError::UnknownWallet)
    }

    fn named(&self, backend: &str) -> Result<Arc<dyn KeyCustody>, KeyCustodyError> {
        self.backend(backend).ok_or_else(|| {
            KeyCustodyError::BackendUnavailable(format!(
                "the {backend:?} key custody backend is not enabled"
            ))
        })
    }

    /// Remembers `handle`, registered in `custody` under `backend` - unless
    /// the backend was replaced or removed while it registered, in which
    /// case the registration is freed there and the caller told to try
    /// again (it will reach the current instance).
    fn remember_if_current(
        &self,
        backend: &str,
        custody: Arc<dyn KeyCustody>,
        handle: WalletHandle,
    ) -> Result<WalletHandle, KeyCustodyError> {
        let backends = self.backends.read();
        if backends
            .get(backend)
            .is_some_and(|current| same_instance(current, &custody))
        {
            self.handles.write().insert(handle, backend.to_owned());
            return Ok(handle);
        }
        drop(backends);
        free_handles(vec![(custody, handle)]);
        Err(KeyCustodyError::BackendUnavailable(format!(
            "the {backend:?} key custody backend was changed while registering; try again"
        )))
    }

    /// Which backend holds `handle`, if it's live.
    pub fn backend_of(&self, handle: WalletHandle) -> Option<String> {
        self.handles.read().get(&handle).cloned()
    }
}

#[async_trait::async_trait]
impl KeyCustody for CustodyRouter {
    async fn register_wallet(
        &self,
        material: WalletMaterial,
    ) -> Result<WalletHandle, KeyCustodyError> {
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
        let result = self
            .for_handle(handle)?
            .derive_subaddress(handle, index, network)
            .await;
        self.forget_if_unknown(handle, result)
    }

    async fn scan_tx_outputs(
        &self,
        handle: WalletHandle,
        tx: &ScanInput,
        major_range: Range<u32>,
        minor_range: Range<u32>,
    ) -> Result<Vec<MatchedOutput>, KeyCustodyError> {
        let result = self
            .for_handle(handle)?
            .scan_tx_outputs(handle, tx, major_range, minor_range)
            .await;
        self.forget_if_unknown(handle, result)
    }

    async fn scan_txs_for_indices(
        &self,
        handle: WalletHandle,
        txs: &[ScanInput],
        indices: &ScanIndices,
    ) -> Result<Vec<TxMatches>, KeyCustodyError> {
        let result = self
            .for_handle(handle)?
            .scan_txs_for_indices(handle, txs, indices)
            .await;
        self.forget_if_unknown(handle, result)
    }

    /// Asks every backend. A backend whose epoch went up has lost its
    /// wallets: its handles are forgotten here (so `handle_is_live` says so
    /// and the caller registers them again). Returns the sum of the epochs,
    /// which only ever goes up.
    async fn check_state(&self) -> Result<u64, KeyCustodyError> {
        // By name, so which backend's error is reported first doesn't vary.
        let mut backends: Vec<_> = self.backends().into_iter().collect();
        backends.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));
        let mut total = 0;
        let mut first_error = None;
        for (name, custody) in backends {
            match custody.check_state().await {
                Ok(epoch) => {
                    total += epoch;
                    let previous = self.epochs.write().insert(name.clone(), epoch).unwrap_or(0);
                    if epoch > previous {
                        // Some handles may have been registered since the
                        // restart and still be live there: free them all
                        // (freeing one the restart already lost is harmless).
                        let mut lost = Vec::new();
                        self.handles.write().retain(|handle, backend| {
                            let keep = backend != &name;
                            if !keep {
                                lost.push((Arc::clone(&custody), *handle));
                            }
                            keep
                        });
                        free_handles(lost);
                    }
                }
                Err(e) => {
                    tracing::warn!(backend = %name, error = %e, "key custody backend could not be checked");
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

    async fn register_wallet_in(
        &self,
        backend: &str,
        material: WalletMaterial,
    ) -> Result<WalletHandle, KeyCustodyError> {
        let custody = self.named(backend)?;
        let handle = custody.register_wallet(material).await?;
        self.remember_if_current(backend, custody, handle)
    }

    async fn unseal_and_register_in(
        &self,
        backend: &str,
        sealed: &[u8],
    ) -> Result<WalletHandle, KeyCustodyError> {
        let custody = self.named(backend)?;
        let handle = custody.unseal_and_register(sealed).await?;
        self.remember_if_current(backend, custody, handle)
    }

    async fn unseal_and_register_in_idempotent(
        &self,
        backend: &str,
        sealed: &[u8],
        registration_id: &str,
    ) -> Result<WalletHandle, KeyCustodyError> {
        let custody = self.named(backend)?;
        let handle = custody
            .unseal_and_register_idempotent(sealed, registration_id)
            .await?;
        self.remember_if_current(backend, custody, handle)
    }

    async fn seal_in(
        &self,
        backend: &str,
        material: &WalletMaterial,
    ) -> Result<Vec<u8>, KeyCustodyError> {
        self.named(backend)?.seal(material).await
    }

    fn takes_raw_keys_in(&self, backend: &str) -> bool {
        self.backend(backend)
            .is_none_or(|custody| custody.takes_raw_keys())
    }

    async fn key_bundle_in(
        &self,
        backend: &str,
        action: crate::transport::Action,
        store: Option<&str>,
    ) -> Result<crate::transport::Bundle, KeyCustodyError> {
        self.named(backend)?.key_bundle(action, store).await
    }

    async fn register_envelope_in(
        &self,
        backend: &str,
        envelope: &crate::transport::Envelope,
        action: crate::transport::Action,
        store: Option<&str>,
    ) -> Result<(WalletHandle, Vec<u8>), KeyCustodyError> {
        let custody = self.named(backend)?;
        let (handle, sealed) = custody.register_envelope(envelope, action, store).await?;
        Ok((self.remember_if_current(backend, custody, handle)?, sealed))
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
        let mut backends: Vec<(String, Arc<dyn KeyCustody>)> =
            self.backends().into_iter().collect();
        backends.sort_by(|a, b| a.0.cmp(&b.0));
        let mut health = Vec::with_capacity(backends.len());
        for (name, custody) in backends {
            let error = match tokio::time::timeout(
                std::time::Duration::from_secs(2),
                custody.check_state(),
            )
            .await
            {
                Ok(Ok(_)) => custody.unavailable(),
                Ok(Err(e)) => Some(e.to_string()),
                Err(_) => Some("did not answer within 2 seconds".to_owned()),
            };
            health.push((name, error));
        }
        health
    }
}

/// Whether two `Arc`s are the same backend instance.
fn same_instance(a: &Arc<dyn KeyCustody>, b: &Arc<dyn KeyCustody>) -> bool {
    std::ptr::eq(Arc::as_ptr(a).cast::<()>(), Arc::as_ptr(b).cast::<()>())
}

/// Removes forgotten handles from the backend instance that issued them, in
/// the background and best effort: a backend that is down keeps them only
/// until it restarts, which loses them anyway.
///
/// Leaving them would keep a copy of a store's view key in a backend it no
/// longer uses.
pub fn free_handles(handles: Vec<(Arc<dyn KeyCustody>, WalletHandle)>) {
    if handles.is_empty() {
        return;
    }
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    runtime.spawn(async move {
        for (custody, handle) in handles {
            let removed = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                crate::remove_wallet_logged(
                    custody.as_ref(),
                    handle,
                    None,
                    "freeing a handle the router no longer uses",
                ),
            )
            .await;
            if removed.is_err() {
                tracing::warn!(
                    wallet.handle = ?handle,
                    "freeing a handle the router no longer uses: removing a store's keys from key custody timed out, so a copy stays there until the backend restarts"
                );
            }
        }
    });
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::PlainKeyCustody;

    fn material(seed: u8) -> WalletMaterial {
        let mut view = [seed; 32];
        view[31] &= 0x0f;
        let mut spend = [seed.wrapping_add(1); 32];
        spend[31] &= 0x0f;
        let spend =
            monero::PublicKey::from_private_key(&monero::PrivateKey::from_slice(&spend).unwrap())
                .to_bytes();
        WalletMaterial::new(view, spend)
    }

    fn two_backends() -> (CustodyRouter, Arc<dyn KeyCustody>, Arc<dyn KeyCustody>) {
        let a: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
        let b: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
        let mut backends = HashMap::new();
        backends.insert("plain".to_owned(), Arc::clone(&a));
        backends.insert("socket".to_owned(), Arc::clone(&b));
        (CustodyRouter::new(backends, "plain"), a, b)
    }

    #[tokio::test]
    async fn each_wallet_lives_in_its_own_backend_and_calls_are_routed_by_handle() {
        let (router, a, b) = two_backends();
        let in_a = router
            .register_wallet_in("plain", material(1))
            .await
            .unwrap();
        let in_b = router
            .register_wallet_in("socket", material(3))
            .await
            .unwrap();
        a.derive_subaddress(in_a, SubaddressIndex::default(), Network::Mainnet)
            .await
            .unwrap();
        assert!(
            b.derive_subaddress(in_a, SubaddressIndex::default(), Network::Mainnet)
                .await
                .is_err(),
            "not in the other one"
        );
        router
            .derive_subaddress(in_b, SubaddressIndex::default(), Network::Mainnet)
            .await
            .unwrap();
        assert_eq!(router.backend_of(in_b).as_deref(), Some("socket"));
    }

    #[tokio::test]
    async fn a_disabled_backend_can_not_take_wallets_and_its_handles_stop_being_live() {
        let (router, a, _) = two_backends();
        let in_b = router
            .register_wallet_in("socket", material(3))
            .await
            .unwrap();
        let mut only_plain: HashMap<String, Arc<dyn KeyCustody>> = HashMap::new();
        only_plain.insert("plain".to_owned(), a);
        let dropped = router.replace(only_plain, "plain");
        assert_eq!(dropped.len(), 1);
        assert!(!router.handle_is_live(in_b));
        assert!(matches!(
            router.register_wallet_in("socket", material(5)).await,
            Err(KeyCustodyError::BackendUnavailable(_))
        ));
        assert_eq!(router.enabled_backends(), vec!["plain".to_owned()]);
    }

    #[tokio::test]
    async fn a_backend_replaced_by_a_new_instance_under_the_same_name_drops_its_handles_and_frees_them(
    ) {
        let (router, a, b) = two_backends();
        let in_a = router
            .register_wallet_in("plain", material(1))
            .await
            .unwrap();
        let in_b = router
            .register_wallet_in("socket", material(3))
            .await
            .unwrap();
        let new_socket: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
        let dropped = router.replace(
            HashMap::from([
                ("plain".to_owned(), Arc::clone(&a)),
                ("socket".to_owned(), new_socket),
            ]),
            "plain",
        );
        assert!(
            router.handle_is_live(in_a),
            "the kept backend's stores are untouched"
        );
        assert!(
            !router.handle_is_live(in_b),
            "so the scan loop registers the store in the new instance"
        );
        assert_eq!(dropped.len(), 1);
        assert!(
            same_instance(&dropped[0].0, &b) && dropped[0].1 == in_b,
            "freed in the instance that issued it"
        );
        free_handles(dropped);
        for _ in 0..100 {
            if b.derive_subaddress(in_b, SubaddressIndex::default(), Network::Mainnet)
                .await
                .is_err()
            {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("the old instance still holds the store's keys");
    }

    #[tokio::test]
    async fn a_handle_its_backend_no_longer_knows_stops_being_live() {
        let (router, a, _) = two_backends();
        let handle = router
            .register_wallet_in("plain", material(1))
            .await
            .unwrap();
        a.remove_wallet(handle).await.unwrap(); // lost behind the router's back
        assert!(router.handle_is_live(handle), "not known yet");
        assert!(matches!(
            router
                .derive_subaddress(handle, SubaddressIndex::default(), Network::Mainnet)
                .await,
            Err(KeyCustodyError::UnknownWallet)
        ));
        assert!(
            !router.handle_is_live(handle),
            "now it is, so it gets registered again"
        );
    }

    /// A backend that restarts (its epoch goes up) and forgets its wallets,
    /// and that can be down for removals.
    #[derive(Default)]
    struct Restartable {
        inner: PlainKeyCustody,
        epoch: std::sync::atomic::AtomicU64,
        refuses_removals: std::sync::atomic::AtomicBool,
    }

    #[async_trait::async_trait]
    impl KeyCustody for Restartable {
        async fn register_wallet(
            &self,
            material: WalletMaterial,
        ) -> Result<WalletHandle, KeyCustodyError> {
            self.inner.register_wallet(material).await
        }
        async fn remove_wallet(&self, handle: WalletHandle) -> Result<(), KeyCustodyError> {
            if self
                .refuses_removals
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                return Err(KeyCustodyError::BackendUnavailable("down".to_owned()));
            }
            self.inner.remove_wallet(handle).await
        }
        async fn seal(&self, material: &WalletMaterial) -> Result<Vec<u8>, KeyCustodyError> {
            self.inner.seal(material).await
        }
        async fn unseal_and_register(
            &self,
            sealed: &[u8],
        ) -> Result<WalletHandle, KeyCustodyError> {
            self.inner.unseal_and_register(sealed).await
        }
        async fn derive_subaddress(
            &self,
            handle: WalletHandle,
            index: SubaddressIndex,
            network: Network,
        ) -> Result<Address, KeyCustodyError> {
            self.inner.derive_subaddress(handle, index, network).await
        }
        async fn scan_tx_outputs(
            &self,
            handle: WalletHandle,
            tx: &ScanInput,
            major_range: Range<u32>,
            minor_range: Range<u32>,
        ) -> Result<Vec<MatchedOutput>, KeyCustodyError> {
            self.inner
                .scan_tx_outputs(handle, tx, major_range, minor_range)
                .await
        }
        async fn check_state(&self) -> Result<u64, KeyCustodyError> {
            Ok(self.epoch.load(std::sync::atomic::Ordering::SeqCst))
        }
    }

    #[tokio::test]
    async fn a_removal_the_backend_refuses_is_logged_and_one_already_done_is_not() {
        const FAILED: &str = "removing a store's keys from key custody failed";
        let backend = Restartable::default();
        let handle = backend.register_wallet(material(1)).await.unwrap();
        let (_guard, logs) = crate::test_log::capture();

        backend
            .refuses_removals
            .store(true, std::sync::atomic::Ordering::SeqCst);
        crate::remove_wallet_logged(&backend, handle, Some("st_down"), "deleting a store").await;
        assert_eq!(logs.count(FAILED), 1, "{}", logs.text());
        assert!(logs.text().contains("st_down"), "{}", logs.text());
        assert!(logs.text().contains("deleting a store"), "{}", logs.text());

        backend
            .refuses_removals
            .store(false, std::sync::atomic::Ordering::SeqCst);
        crate::remove_wallet_logged(&backend, handle, Some("st_down"), "deleting a store").await;
        crate::remove_wallet_logged(&backend, handle, None, "deleting a store").await;
        assert_eq!(
            logs.count(FAILED),
            1,
            "a removal that worked, and one of a handle already gone, log nothing: {}",
            logs.text()
        );
    }

    #[tokio::test]
    async fn a_restarted_backend_loses_only_its_own_handles() {
        let plain: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
        let restartable = Arc::new(Restartable::default());
        let socket: Arc<dyn KeyCustody> = Arc::<Restartable>::clone(&restartable);
        let router = CustodyRouter::new(
            HashMap::from([("plain".to_owned(), plain), ("socket".to_owned(), socket)]),
            "plain",
        );
        let in_plain = router
            .register_wallet_in("plain", material(1))
            .await
            .unwrap();
        let in_socket = router
            .register_wallet_in("socket", material(3))
            .await
            .unwrap();
        router.check_state().await.unwrap();
        assert!(router.handle_is_live(in_socket), "nothing restarted yet");

        restartable
            .epoch
            .store(1, std::sync::atomic::Ordering::SeqCst);
        router.check_state().await.unwrap();
        assert!(!router.handle_is_live(in_socket));
        assert!(
            router.handle_is_live(in_plain),
            "the other backend's stores are untouched"
        );
        router.check_state().await.unwrap();
        let again = router
            .register_wallet_in("socket", material(3))
            .await
            .unwrap();
        router.check_state().await.unwrap();
        assert!(
            router.handle_is_live(again),
            "the same epoch seen twice drops nothing"
        );
    }

    /// Holds each registration until let go, so a test can change the
    /// router in the middle of one.
    struct Gated {
        inner: PlainKeyCustody,
        entered: tokio::sync::Notify,
        gate: tokio::sync::Notify,
    }

    #[async_trait::async_trait]
    impl KeyCustody for Gated {
        async fn register_wallet(
            &self,
            material: WalletMaterial,
        ) -> Result<WalletHandle, KeyCustodyError> {
            self.entered.notify_one();
            self.gate.notified().await;
            self.inner.register_wallet(material).await
        }
        async fn remove_wallet(&self, handle: WalletHandle) -> Result<(), KeyCustodyError> {
            self.inner.remove_wallet(handle).await
        }
        async fn seal(&self, material: &WalletMaterial) -> Result<Vec<u8>, KeyCustodyError> {
            self.inner.seal(material).await
        }
        async fn unseal_and_register(
            &self,
            sealed: &[u8],
        ) -> Result<WalletHandle, KeyCustodyError> {
            self.inner.unseal_and_register(sealed).await
        }
        async fn derive_subaddress(
            &self,
            handle: WalletHandle,
            index: SubaddressIndex,
            network: Network,
        ) -> Result<Address, KeyCustodyError> {
            self.inner.derive_subaddress(handle, index, network).await
        }
        async fn scan_tx_outputs(
            &self,
            handle: WalletHandle,
            tx: &ScanInput,
            major_range: Range<u32>,
            minor_range: Range<u32>,
        ) -> Result<Vec<MatchedOutput>, KeyCustodyError> {
            self.inner
                .scan_tx_outputs(handle, tx, major_range, minor_range)
                .await
        }
    }

    #[tokio::test]
    async fn a_registration_that_finishes_after_its_backend_was_replaced_is_freed_there_and_retried(
    ) {
        let gated = Arc::new(Gated {
            inner: PlainKeyCustody::default(),
            entered: tokio::sync::Notify::new(),
            gate: tokio::sync::Notify::new(),
        });
        let old: Arc<dyn KeyCustody> = Arc::<Gated>::clone(&gated);
        let router = Arc::new(CustodyRouter::new(
            HashMap::from([("socket".to_owned(), old)]),
            "socket",
        ));
        let registering = {
            let router = Arc::clone(&router);
            tokio::spawn(async move { router.register_wallet_in("socket", material(1)).await })
        };
        gated.entered.notified().await;
        let fresh: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
        router.replace(HashMap::from([("socket".to_owned(), fresh)]), "socket");
        gated.gate.notify_one();
        assert!(
            matches!(
                registering.await.unwrap(),
                Err(KeyCustodyError::BackendUnavailable(_))
            ),
            "told to try again"
        );
        // Freed in the old instance, in the background.
        for _ in 0..100 {
            if gated.inner.wallet_count() == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            gated.inner.wallet_count(),
            0,
            "no copy left behind in the replaced instance"
        );
        assert!(
            router
                .register_wallet_in("socket", material(1))
                .await
                .is_ok(),
            "the retry reaches the current one"
        );
    }
}
