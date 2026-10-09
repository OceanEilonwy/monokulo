//! Standalone setup for the shared scanner history harness under fuzzing.
#![expect(
    clippy::unwrap_used,
    reason = "checked fixtures and isolated exploration setup"
)]
#![cfg_attr(
    not(test),
    expect(
        clippy::future_not_send,
        reason = "single-thread setup borrows SQLite while registering the fixture"
    )
)]
use crate::key_custody::{
    KeyCustody, KeyCustodyError, MatchedOutput, PlainKeyCustody, ScanInput, SubaddressIndex,
    WalletHandle, WalletMaterial,
};
use crate::store::{NewOrder, NewTenant, Store};
use monero::consensus::encode::deserialize;
use monero::{Address, Network, PrivateKey, PublicKey, Transaction};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
pub(crate) fn fixture_tx() -> Transaction {
    let raw_tx = hex::decode(include_str!("../../../fixtures/subaddress_tx.hex")).unwrap();
    deserialize(&raw_tx).unwrap()
}

pub(crate) fn fixture_view_key() -> [u8; 32] {
    PrivateKey::from_slice(
        &hex::decode("bcfdda53205318e1c14fa0ddca1a45df363bb427972981d0249d0f4652a7df07").unwrap(),
    )
    .unwrap()
    .to_bytes()
}

pub(crate) fn fixture_spend_pubkey() -> [u8; 32] {
    let secret_spend = PrivateKey::from_slice(
        &hex::decode("e5f4301d32f3bdaef814a835a18aaaa24b13cc76cf01a832a7852faf9322e907").unwrap(),
    )
    .unwrap();
    PublicKey::from_private_key(&secret_spend).to_bytes()
}

#[derive(Default)]
pub(crate) struct FlakyKeyCustody {
    inner: PlainKeyCustody,
    failing: parking_lot::Mutex<HashSet<WalletHandle>>,
    /// Scan calls made for each handle, failed or not.
    pub(crate) attempts: parking_lot::Mutex<HashMap<WalletHandle, u32>>,
    /// Run once, at the next scan call: for changing the world mid-scan.
    on_next_scan: parking_lot::Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl FlakyKeyCustody {
    #[cfg(test)]
    pub(crate) fn on_next_scan(&self, hook: impl FnOnce() + Send + 'static) {
        *self.on_next_scan.lock() = Some(Box::new(hook));
    }
    pub(crate) fn fail(&self, handle: WalletHandle) {
        self.failing.lock().insert(handle);
    }
    pub(crate) fn recover(&self, handle: WalletHandle) {
        self.failing.lock().remove(&handle);
    }
}

#[async_trait::async_trait]
impl KeyCustody for FlakyKeyCustody {
    async fn register_wallet(
        &self,
        material: WalletMaterial,
    ) -> Result<WalletHandle, KeyCustodyError> {
        self.inner.register_wallet(material).await
    }
    async fn remove_wallet(&self, handle: WalletHandle) -> Result<(), KeyCustodyError> {
        self.inner.remove_wallet(handle).await
    }
    async fn seal(&self, material: &WalletMaterial) -> Result<Vec<u8>, KeyCustodyError> {
        self.inner.seal(material).await
    }
    async fn unseal_and_register(&self, sealed: &[u8]) -> Result<WalletHandle, KeyCustodyError> {
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
        *self.attempts.lock().entry(handle).or_default() += 1;
        let hook = self.on_next_scan.lock().take();
        if let Some(hook) = hook {
            hook();
        }
        if self.failing.lock().contains(&handle) {
            return Err(KeyCustodyError::BackendUnavailable(
                "simulated backend outage".into(),
            ));
        }
        self.inner
            .scan_tx_outputs(handle, tx, major_range, minor_range)
            .await
    }
}

pub(crate) async fn fixture_tenant(
    store: &Store,
    key_custody: &dyn KeyCustody,
    expires_at: i64,
) -> (crate::store::TenantId, WalletHandle, crate::store::OrderId) {
    let handle = register_fixture_wallet(key_custody).await;
    let (tenant, order) = fixture_tenant_rows(store, expires_at);
    (tenant, handle, order)
}

pub(crate) async fn register_fixture_wallet(key_custody: &dyn KeyCustody) -> WalletHandle {
    key_custody
        .register_wallet(WalletMaterial::new(
            fixture_view_key(),
            fixture_spend_pubkey(),
        ))
        .await
        .unwrap()
}

fn fixture_tenant_rows(
    store: &Store,
    expires_at: i64,
) -> (crate::store::TenantId, crate::store::OrderId) {
    let tenant = store
        .create_tenant(
            &NewTenant {
                key_custody_backend: "plain".into(),
                sealed_key_material: vec![],
                primary_address: "4fixture".into(),
                network: "mainnet".into(),
                confirmations_required: Some(10),
                order_expiry_seconds: None,
            },
            1000,
        )
        .unwrap();
    let index = store.allocate_minor_index(&tenant.tenant.id).unwrap();
    assert_eq!(index, 1, "fixture order must use the paying subaddress");
    let order = store
        .create_order(&NewOrder {
            idempotency_key: None,
            confirmations_required_override: None,
            tenant_id: tenant.tenant.id.clone(),
            merchant_order_id: None,
            minor_index: index,
            address: "fixture".into(),
            xmr_amount_piconero: 1,
            description: None,
            created_at: 1000,
            expires_at,
        })
        .unwrap();
    (
        shared::ids::TenantId::new(tenant.tenant.id.into_string()),
        shared::ids::OrderId::new(order.id.into_string()),
    )
}

pub(crate) use crate::verification_temp_db::TempDb;
pub(crate) fn file_store() -> (Store, TempDb) {
    let path = TempDb::new();
    (Store::create_file(&path).unwrap(), path)
}

#[cfg(test)]
pub(crate) fn unrelated_tx(seed: u8) -> Transaction {
    let mut tx = fixture_tx();
    let mut key_bytes = [seed.wrapping_add(7); 32];
    key_bytes[31] &= 0x0f;
    let other = PublicKey::from_private_key(&PrivateKey::from_slice(&key_bytes).unwrap());
    let extra = tx.prefix.extra.try_parse();
    let replaced = monero::blockdata::transaction::ExtraField(
        extra
            .0
            .into_iter()
            .map(|field| match field {
                monero::blockdata::transaction::SubField::TxPublicKey(_) => {
                    monero::blockdata::transaction::SubField::TxPublicKey(other)
                }
                monero::blockdata::transaction::SubField::AdditionalPublickKey(keys) => {
                    monero::blockdata::transaction::SubField::AdditionalPublickKey(
                        keys.iter().map(|_| other).collect(),
                    )
                }
                field @ (monero::blockdata::transaction::SubField::Nonce(_)
                | monero::blockdata::transaction::SubField::Padding(_)
                | monero::blockdata::transaction::SubField::MergeMining(..)
                | monero::blockdata::transaction::SubField::MysteriousMinerGate(_)) => field,
            })
            .collect(),
    );
    tx.prefix.extra = replaced.into();
    tx
}

#[cfg(test)]
pub(crate) async fn fixture_tenant_shared(
    store: &crate::store::SharedStore,
    key_custody: &dyn KeyCustody,
    expires_at: i64,
) -> (crate::store::TenantId, WalletHandle, crate::store::OrderId) {
    let handle = register_fixture_wallet(key_custody).await;
    let (tenant, order) = fixture_tenant_rows(&store.lock(), expires_at);
    (tenant, handle, order)
}
