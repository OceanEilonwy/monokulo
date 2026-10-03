//! The `KeyCustody` boundary, from the `key-custody` crate.
//!
//! Each store's keys live in one backend of its own choosing; [`CustodyRouter`]
//! holds the enabled ones and routes each call to the backend that issued its
//! handle (`docs/DESIGN.md` §6.4). Re-exported here so the engine names them
//! as its own. [`SnpSlot`] is how the engine starts the `snp` backend.

use std::ops::Range;
use std::sync::Arc;

use ::key_custody::snp::{SnpConfig, SnpKeyCustody, StoredWrap, WrapStore};
use ::key_custody::transport::{Anchor, TrustPolicy};
pub use ::key_custody::{
    remove_wallet_logged, router, size_scan_slots, snp, transport, wallet_matches_address,
    CustodyRouter, KeyCustody, KeyCustodyError, MatchedOutput, Network, PlainKeyCustody,
    ScanIndices, ScanInput, SubaddressIndex, TxMatches, WalletHandle, WalletMaterial,
};
use snp_attest::guest::GuestDevice;

/// The SEV-SNP master key's wraps, in the engine's database.
pub struct StoreWraps(pub crate::store::SharedStore);

impl WrapStore for StoreWraps {
    fn load(&self) -> Result<Vec<StoredWrap>, String> {
        let rows = self.0.lock().snp_master_keys().map_err(|e| e.to_string())?;
        rows.into_iter()
            .map(|(measurement, guest_svn, wrapped)| {
                Ok(StoredWrap {
                    measurement: measurement.try_into().map_err(|bytes: Vec<u8>| {
                        format!("a stored measurement is {} bytes, not 48", bytes.len())
                    })?,
                    guest_svn,
                    wrapped,
                })
            })
            .collect()
    }

    fn save(&self, wrap: &StoredWrap) -> Result<(), String> {
        self.0
            .lock()
            .save_snp_master_key(&wrap.measurement, wrap.guest_svn, &wrap.wrapped)
            .map_err(|e| e.to_string())
    }
}

/// The `snp` backend, started once (its settings apply at a restart) the
/// first time it is enabled, and kept for the life of the process so its
/// stores stay registered whatever else is saved.
pub struct SnpSlot {
    /// `Err` when the settings don't let it start (no product set, no
    /// trusted ID key), with why.
    config: Result<SnpConfig, String>,
    guest: Arc<dyn GuestDevice>,
    wraps: Arc<dyn WrapStore>,
    /// What another engine's report is checked against in a handoff:
    /// AMD's chain.
    anchor: Anchor,
    started: parking_lot::Mutex<Option<Arc<SnpKeyCustody>>>,
}

impl SnpSlot {
    pub fn new(
        config: Result<SnpConfig, String>,
        guest: Arc<dyn GuestDevice>,
        wraps: Arc<dyn WrapStore>,
    ) -> Self {
        Self {
            config,
            guest,
            wraps,
            anchor: Anchor::Amd,
            started: parking_lot::Mutex::new(None),
        }
    }

    /// Checks other engines' reports against `anchor` instead of AMD's
    /// chain: for tests, whose stand-in security processors AMD didn't sign.
    #[must_use = "the slot with the anchor set is returned, not changed in place"]
    pub fn with_anchor(mut self, anchor: Anchor) -> Self {
        self.anchor = anchor;
        self
    }

    pub fn anchor(&self) -> &Anchor {
        &self.anchor
    }

    /// Which images this engine trusts with keys, when its settings let the
    /// backend start: reported on `/status`, for monokulo to compare with
    /// the policy its own key entry forms check.
    pub fn trust(&self) -> Option<TrustPolicy> {
        self.config.as_ref().ok().map(|config| config.trust)
    }

    /// The backend, if it has started.
    pub fn backend(&self) -> Option<Arc<SnpKeyCustody>> {
        self.started.lock().clone()
    }

    /// The backend, starting it if this is the first time. A start that
    /// fails is tried again next time.
    pub fn start(&self) -> Result<Arc<SnpKeyCustody>, String> {
        let mut started = self.started.lock();
        if let Some(backend) = started.as_ref() {
            return Ok(Arc::clone(backend));
        }
        let config = self.config.clone()?;
        let backend = Arc::new(SnpKeyCustody::start(
            Arc::clone(&self.guest),
            config,
            Arc::clone(&self.wraps),
        )?);
        *started = Some(Arc::clone(&backend));
        Ok(backend)
    }
}

/// Where an upgraded engine asks for the snp master key: the engine it
/// replaces, and the engine token to call it with.
pub struct Handoff {
    pub url: String,
    pub token: String,
}

/// How often AMD's certificates are fetched again once in hand: the
/// revocation list can change.
const EVIDENCE_REFRESH: std::time::Duration = std::time::Duration::from_hours(12);
/// How soon a failed fetch or handoff is tried again.
const RETRY: std::time::Duration = std::time::Duration::from_secs(30);

/// Keeps the snp backend supplied once it has started, for the life of the
/// process.
///
/// It needs AMD's certificates for its report (without them no key entry
/// form can be offered) and, while it waits for its master key, a handoff
/// from the engine it replaces.
#[expect(
    clippy::infinite_loop,
    reason = "a supervised loop: `shared::supervise` restarts one that returns"
)]
pub async fn run_snp_upkeep(slot: Arc<SnpSlot>, handoff: Option<Handoff>) {
    let source = match snp_attest::kds::client() {
        Ok(client) => ::key_custody::snp::KdsEvidence(client),
        Err(e) => {
            tracing::error!(error = %e, "snp key custody: can't build a client for AMD's key distribution service");
            return;
        }
    };
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();
    let mut next_fetch = tokio::time::Instant::now();
    loop {
        if let Some(snp) = slot.backend() {
            if tokio::time::Instant::now() >= next_fetch {
                match snp.refresh_evidence(&source).await {
                    Ok(()) => {
                        tracing::info!(
                            "snp key custody: fetched AMD's certificates for this engine's report"
                        );
                        next_fetch = tokio::time::Instant::now() + EVIDENCE_REFRESH;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "snp key custody: fetching AMD's certificates for this engine's report failed; key entry forms can't be offered until it works");
                        next_fetch = tokio::time::Instant::now() + RETRY;
                    }
                }
            }
            if let (true, Some(handoff)) = (snp.awaiting_handoff(), handoff.as_ref()) {
                match request_handoff(&http, &snp, handoff, slot.anchor()).await {
                    Ok(()) => {
                        tracing::info!(from = %handoff.url, "snp key custody: took the master key over from the engine this one replaces");
                    }
                    Err(e) => {
                        tracing::warn!(from = %handoff.url, error = %e, "snp key custody: the handoff of the master key failed; trying again");
                    }
                }
            }
        }
        tokio::time::sleep(RETRY).await;
    }
}

/// Asks the engine at `handoff.url` for the master key, and takes it.
///
/// The request is this engine's handoff bundle; the answer must be attested
/// (under `anchor`, AMD's chain in production) by an engine signed by this
/// one's ID key.
pub async fn request_handoff(
    http: &reqwest::Client,
    snp: &SnpKeyCustody,
    handoff: &Handoff,
    anchor: &Anchor,
) -> Result<(), String> {
    let bundle = snp
        .handoff_bundle(::key_custody::snp::unix_now())
        .map_err(|e| e.to_string())?;
    let url = format!(
        "{}/api/v1/admin/key-custody/handoff",
        handoff.url.trim_end_matches('/')
    );
    let response = http
        .post(&url)
        .header(shared::auth::ENGINE_TOKEN_HEADER, &handoff.token)
        .json(&bundle)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(format!("{status}: {body}"));
    }
    let answer: ::key_custody::transport::HandoffAnswer =
        response.json().await.map_err(|e| e.to_string())?;
    snp.accept_handoff(&answer, anchor, ::key_custody::snp::unix_now())
}

/// Stands in for a backend that couldn't start: every call says why, so its
/// stores are reported unavailable with the reason, and the other backends
/// carry on.
pub struct Unstarted(pub String);

impl Unstarted {
    fn error(&self) -> KeyCustodyError {
        KeyCustodyError::BackendUnavailable(self.0.clone())
    }
}

#[async_trait::async_trait]
impl KeyCustody for Unstarted {
    async fn register_wallet(
        &self,
        _material: WalletMaterial,
    ) -> Result<WalletHandle, KeyCustodyError> {
        Err(self.error())
    }
    async fn remove_wallet(&self, _handle: WalletHandle) -> Result<(), KeyCustodyError> {
        Err(KeyCustodyError::UnknownWallet)
    }
    async fn seal(&self, _material: &WalletMaterial) -> Result<Vec<u8>, KeyCustodyError> {
        Err(self.error())
    }
    async fn unseal_and_register(&self, _sealed: &[u8]) -> Result<WalletHandle, KeyCustodyError> {
        Err(self.error())
    }
    async fn derive_subaddress(
        &self,
        _handle: WalletHandle,
        _index: SubaddressIndex,
        _network: Network,
    ) -> Result<monero::Address, KeyCustodyError> {
        Err(self.error())
    }
    async fn scan_tx_outputs(
        &self,
        _handle: WalletHandle,
        _tx: &ScanInput,
        _major_range: Range<u32>,
        _minor_range: Range<u32>,
    ) -> Result<Vec<MatchedOutput>, KeyCustodyError> {
        Err(self.error())
    }
    fn unavailable(&self) -> Option<String> {
        Some(self.0.clone())
    }
    fn takes_raw_keys(&self) -> bool {
        false
    }
    async fn key_bundle(
        &self,
        _action: transport::Action,
        _store: Option<&str>,
    ) -> Result<transport::Bundle, KeyCustodyError> {
        Err(self.error())
    }
}
