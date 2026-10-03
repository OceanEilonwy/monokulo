//! `snp`: key custody for an engine running inside an AMD SEV-SNP
//! confidential VM.
//!
//! **In use**, keys sit in the engine's own memory, in the same registry
//! [`PlainKeyCustody`] keeps; the hardware encrypts that memory against the
//! host and the hypervisor. **In transit**, merchants send keys encrypted to
//! this backend ([`crate::transport`]): it refuses keys in the clear.
//! **At rest**, `seal` encrypts a store's keys (AES-256-GCM) under a master key
//! only engines of the trusted image can recover.
//!
//! ## The master key
//!
//! One random 32-byte key encrypts every store's keys. It is stored wrapped,
//! once per engine image, under a key the security processor derives from the
//! chip's secret, the guest policy and the image's **launch measurement**,
//! mixed with the digest of the ID key the image was launched with, as the
//! report attests it ([`WrapStore`], a table in the engine's database). Only a
//! guest launched from that exact image, under that ID key, on that chip, can
//! unwrap it: the same image relaunched with an ID block someone else signed
//! gets another key.
//!
//! The measurement, and not the ID block's family and image IDs, because the
//! firmware does not mix the ID key into derived keys: a host could launch an
//! image of its own with an ID block it signed itself, carrying the same IDs,
//! and derive the same key. The measurement it can't fake.
//!
//! A new image (an upgrade) therefore can't unwrap the old image's wrap. It
//! gets the master key from the engine it replaces instead, by **handoff**:
//! the new engine sends a bundle for [`Action::Handoff`]; the old one checks,
//! against AMD's chain, that it comes from an image signed by **its own** ID
//! key at the same security version or later ([`SnpKeyCustody::answer_handoff`]),
//! and encrypts the master key to it, attesting the answer with a report of
//! its own. The new engine takes it only from such an attested answer, from
//! an engine signed by its own ID key ([`SnpKeyCustody::accept_handoff`]), and
//! wraps it under its own measurement. So upgrades signed with the same ID key
//! keep every store's keys, and nothing else can take them or plant one.
//!
//! Which case applies at start:
//! - a wrap for this image: unwrap it;
//! - no wraps at all: a new installation; make a master key;
//! - only other images' wraps: wait for a handoff
//!   ([`SnpKeyCustody::handoff_bundle`], [`SnpKeyCustody::accept_handoff`]),
//!   and refuse everything that needs the master key until then.
//!
//! A store whose sealed keys can't be opened (another chip, a lost master key)
//! is reported unavailable, and its owner enters its keys again.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use aes_gcm::aead::Aead as _;
use aes_gcm::KeyInit as _;
use hmac::Mac as _;
use monero::Address;
use parking_lot::{Mutex, RwLock};
use snp_attest::guest::{DerivedKeyRequest, GuestDevice, FIELD_MEASUREMENT, FIELD_POLICY};
use snp_attest::report::{self, AttestationReport, Product};
use snp_attest::verify::Evidence;
use zeroize::Zeroizing;

use crate::transport::{
    self, Action, Anchor, Attestation, Bundle, Envelope, HandoffAnswer, ReceiverKey,
    TransportError, TrustPolicy, KEYS_LEN,
};
use crate::{
    KeyCustody, KeyCustodyError, MatchedOutput, Network, PlainKeyCustody, ScanIndices, ScanInput,
    SubaddressIndex, TxMatches, WalletHandle, WalletMaterial,
};

/// What a store's sealed keys start with: format and version.
const SEALED_TAG: &[u8; 4] = b"SNP1";
const SEAL_AAD: &[u8] = b"monokulo snp sealed store keys v1";
const WRAP_LABEL: &[u8] = b"monokulo snp master key wrap v1";
const NONCE_LEN: usize = 12;

/// How long a challenge is accepted after it is issued: a form left open
/// that long is loaded again.
pub const CHALLENGE_TTL_SECS: i64 = 60 * 60;
/// The most challenges outstanding at once; the oldest go first.
const MAX_CHALLENGES: usize = 10_000;

/// The master key wrapped for one engine image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredWrap {
    pub measurement: [u8; 48],
    pub guest_svn: u32,
    pub wrapped: Vec<u8>,
}

/// Where the wrapped master keys are kept: the engine's database.
pub trait WrapStore: Send + Sync {
    fn load(&self) -> Result<Vec<StoredWrap>, String>;
    fn save(&self, wrap: &StoredWrap) -> Result<(), String>;
}

/// Where the AMD certificates for this engine's own report come from: AMD's
/// KDS in production.
#[async_trait::async_trait]
pub trait EvidenceSource: Send + Sync {
    async fn evidence(
        &self,
        product: Product,
        report: &AttestationReport,
    ) -> Result<Evidence, String>;
}

/// AMD's Key Distribution Service.
pub struct KdsEvidence(pub reqwest::Client);

#[async_trait::async_trait]
impl EvidenceSource for KdsEvidence {
    async fn evidence(
        &self,
        product: Product,
        report: &AttestationReport,
    ) -> Result<Evidence, String> {
        snp_attest::verify::fetch_evidence(&self.0, product, report)
            .await
            .map_err(|e| e.to_string())
    }
}

/// What the backend is started with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnpConfig {
    /// The AMD product line the engine runs on.
    pub product: Product,
    /// Which images are trusted: this engine's own (checked at start), the
    /// one a handoff goes to, and the one merchants' clients check.
    pub trust: TrustPolicy,
}

enum Master {
    Ready(Zeroizing<[u8; 32]>),
    /// Not yet available, and why.
    Waiting(String),
}

struct Challenge {
    action: Action,
    store: Option<String>,
    expires_at: i64,
}

/// The backend's state, for `/status` and the admin page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnpStatus {
    /// `None` when the master key is ready; otherwise why not.
    pub waiting: Option<String>,
    pub measurement: String,
    pub guest_svn: u32,
    /// Whether AMD's certificates for this engine's report are in hand, so
    /// key entry forms can be offered.
    pub evidence: bool,
}

pub struct SnpKeyCustody {
    registry: PlainKeyCustody,
    guest: Arc<dyn GuestDevice>,
    config: SnpConfig,
    receiver: ReceiverKey,
    /// This engine's report (raw), vouching for `receiver`'s public key.
    report_raw: Vec<u8>,
    report: AttestationReport,
    evidence: RwLock<Option<Evidence>>,
    master: RwLock<Master>,
    wraps: Arc<dyn WrapStore>,
    challenges: Mutex<HashMap<[u8; 32], Challenge>>,
    /// The challenge this engine's own handoff request carries.
    handoff_challenge: [u8; 32],
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).expect("the operating system's random number generator");
    bytes
}

fn unavailable(message: impl Into<String>) -> KeyCustodyError {
    KeyCustodyError::BackendUnavailable(message.into())
}

/// Encrypts `plaintext` under `key`: a random nonce, then the ciphertext.
fn encrypt(key: &[u8; 32], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let cipher = aes_gcm::Aes256Gcm::new(key.into());
    let nonce: [u8; NONCE_LEN] = random_bytes();
    let ciphertext = cipher
        .encrypt(
            &nonce.into(),
            aes_gcm::aead::Payload {
                msg: plaintext,
                aad,
            },
        )
        .expect("AES-GCM encrypts a short message");
    let mut out = nonce.to_vec();
    out.extend_from_slice(&ciphertext);
    out
}

/// The inverse of [`encrypt`]; `None` if `sealed` wasn't encrypted under
/// `key` with `aad`.
fn decrypt(key: &[u8; 32], aad: &[u8], sealed: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    if sealed.len() < NONCE_LEN {
        return None;
    }
    let (nonce, ciphertext) = sealed.split_at(NONCE_LEN);
    let nonce: [u8; NONCE_LEN] = nonce.try_into().ok()?;
    aes_gcm::Aes256Gcm::new(key.into())
        .decrypt(
            &nonce.into(),
            aes_gcm::aead::Payload {
                msg: ciphertext,
                aad,
            },
        )
        .ok()
        .map(Zeroizing::new)
}

impl SnpKeyCustody {
    /// Starts the backend: makes its receiving key, gets a report vouching
    /// for it and checks this engine is a trusted image (refusing to start
    /// otherwise), then recovers or makes the master key as the module docs
    /// describe. A master key that has to come from a handoff leaves the
    /// backend started but waiting.
    pub fn start(
        guest: Arc<dyn GuestDevice>,
        config: SnpConfig,
        wraps: Arc<dyn WrapStore>,
    ) -> Result<Self, String> {
        let receiver = ReceiverKey::generate();
        let report_raw = guest
            .report(&transport::report_data_for(&receiver.public_key()))
            .map_err(|e| e.to_string())?;
        let report = report::parse(&report_raw, config.product).map_err(|e| e.to_string())?;
        transport::check_identity(&report, &config.trust)
            .map_err(|e| format!("this engine is not a trusted SEV-SNP image: {e}"))?;
        let backend = SnpKeyCustody {
            registry: PlainKeyCustody::default(),
            guest,
            config,
            receiver,
            report_raw,
            report,
            evidence: RwLock::new(None),
            master: RwLock::new(Master::Waiting(String::new())),
            wraps,
            challenges: Mutex::new(HashMap::new()),
            handoff_challenge: random_bytes(),
        };
        *backend.master.write() = backend.recover_master()?;
        Ok(backend)
    }

    /// The key this image's wrap of the master key is encrypted under.
    fn wrapping_key(&self) -> Result<Zeroizing<[u8; 32]>, String> {
        let derived = self
            .guest
            .derived_key(&DerivedKeyRequest {
                guest_field_select: FIELD_POLICY | FIELD_MEASUREMENT,
                vmpl: 0,
                guest_svn: 0,
                tcb_version: 0,
            })
            .map_err(|e| e.to_string())?;
        let mut mac = <hmac::Hmac<sha2::Sha256>>::new_from_slice(derived.as_slice())
            .map_err(|e| e.to_string())?;
        mac.update(WRAP_LABEL);
        // The ID key isn't among the fields the firmware mixes in; this
        // launch's, as its report attests it, is mixed in here.
        mac.update(&self.report.id_key_digest);
        Ok(Zeroizing::new(mac.finalize().into_bytes().into()))
    }

    fn wrap_aad(&self) -> Vec<u8> {
        let mut aad = WRAP_LABEL.to_vec();
        aad.extend_from_slice(&self.report.measurement);
        aad
    }

    /// Wraps `master` for this image and stores it.
    fn store_master(&self, master: &[u8; 32]) -> Result<(), String> {
        let wrapped = encrypt(&*self.wrapping_key()?, &self.wrap_aad(), master);
        self.wraps.save(&StoredWrap {
            measurement: self.report.measurement,
            guest_svn: self.report.guest_svn,
            wrapped,
        })
    }

    fn recover_master(&self) -> Result<Master, String> {
        let wraps = self.wraps.load()?;
        if let Some(own) = wraps
            .iter()
            .find(|wrap| wrap.measurement == self.report.measurement)
        {
            let opened = decrypt(&*self.wrapping_key()?, &self.wrap_aad(), &own.wrapped);
            return Ok(
                match opened
                    .as_deref()
                    .map(|key| <[u8; 32]>::try_from(key.as_slice()))
                {
                    Some(Ok(master)) => Master::Ready(Zeroizing::new(master)),
                    _ => Master::Waiting(
                        "the master key stored for this engine image doesn't open on this chip: \
                     the database was moved from another machine. Hand the key over from an \
                     engine that has it, or start afresh (see the incident runbook)"
                            .into(),
                    ),
                },
            );
        }
        if wraps.is_empty() {
            let master = Zeroizing::new(random_bytes::<32>());
            self.store_master(&master)?;
            return Ok(Master::Ready(master));
        }
        Ok(Master::Waiting(
            "this engine image is new here: it waits for the master key from the engine it \
             replaces (key_custody.snp_handoff_url)"
                .into(),
        ))
    }

    fn master(&self) -> Result<Zeroizing<[u8; 32]>, KeyCustodyError> {
        match &*self.master.read() {
            Master::Ready(key) => Ok(key.clone()),
            Master::Waiting(why) => Err(unavailable(why.clone())),
        }
    }

    /// The backend's state.
    pub fn status(&self) -> SnpStatus {
        SnpStatus {
            waiting: match &*self.master.read() {
                Master::Ready(_) => None,
                Master::Waiting(why) => Some(why.clone()),
            },
            measurement: hex::encode(self.report.measurement),
            guest_svn: self.report.guest_svn,
            evidence: self.evidence.read().is_some(),
        }
    }

    pub fn config(&self) -> &SnpConfig {
        &self.config
    }

    /// Fetches AMD's certificates for this engine's report from `source`
    /// (again: the revocation list changes).
    pub async fn refresh_evidence(&self, source: &dyn EvidenceSource) -> Result<(), String> {
        let evidence = source.evidence(self.config.product, &self.report).await?;
        *self.evidence.write() = Some(evidence);
        Ok(())
    }

    fn bundle_with(
        &self,
        challenge: [u8; 32],
        action: Action,
        store: Option<&str>,
        expires_at: i64,
    ) -> Result<Bundle, KeyCustodyError> {
        let evidence = self.evidence.read().clone().ok_or_else(|| {
            unavailable("AMD's certificates for this engine haven't been fetched yet; try again in a minute")
        })?;
        Ok(Bundle {
            v: transport::PROTOCOL_VERSION,
            product: self.config.product.kds_name().to_owned(),
            report: hex::encode(&self.report_raw),
            ask: hex::encode(&evidence.ask_der),
            vcek: hex::encode(&evidence.vcek_der),
            crl: hex::encode(&evidence.crl_der),
            public_key: hex::encode(self.receiver.public_key()),
            challenge: hex::encode(challenge),
            action,
            store: store.map(str::to_owned),
            expires_at,
        })
    }

    /// Issues a single-use challenge for `action` (and `store`) and returns
    /// the bundle a client encrypts keys against. `now` is seconds since the
    /// epoch.
    pub fn issue_bundle(
        &self,
        action: Action,
        store: Option<&str>,
        now: i64,
    ) -> Result<Bundle, KeyCustodyError> {
        if action == Action::Handoff {
            return Err(KeyCustodyError::InvalidKeyMaterial(
                "a handoff bundle is the engine's own".into(),
            ));
        }
        let challenge: [u8; 32] = random_bytes();
        let expires_at = now + CHALLENGE_TTL_SECS;
        let bundle = self.bundle_with(challenge, action, store, expires_at)?;
        let mut challenges = self.challenges.lock();
        challenges.retain(|_, c| c.expires_at > now);
        if challenges.len() >= MAX_CHALLENGES {
            if let Some(oldest) = challenges
                .iter()
                .min_by_key(|(_, c)| c.expires_at)
                .map(|(k, _)| *k)
            {
                challenges.remove(&oldest);
            }
        }
        challenges.insert(
            challenge,
            Challenge {
                action,
                store: store.map(str::to_owned),
                expires_at,
            },
        );
        Ok(bundle)
    }

    /// Opens a merchant's envelope: its challenge must be one this backend
    /// issued for `action` and `store`, unexpired and unused. It is used up
    /// once the envelope opens, so an envelope that doesn't (sent by anyone
    /// who saw the bundle) can't spend it.
    fn open_merchant_envelope(
        &self,
        envelope: &Envelope,
        action: Action,
        store: Option<&str>,
        now: i64,
    ) -> Result<WalletMaterial, KeyCustodyError> {
        let refused = |e: TransportError| KeyCustodyError::InvalidKeyMaterial(e.to_string());
        let challenge = ReceiverKey::challenge_of(envelope).map_err(refused)?;
        let issued = self
            .challenges
            .lock()
            .get(&challenge)
            .map(|c| (c.action, c.store.clone(), c.expires_at));
        match issued {
            Some((issued_for, issued_store, expires_at))
                if issued_for == action && issued_store.as_deref() == store && expires_at > now => {
            }
            Some((_, _, expires_at)) if expires_at <= now => {
                self.challenges.lock().remove(&challenge);
                return Err(refused(TransportError::Expired));
            }
            _ => return Err(refused(TransportError::Open)),
        }
        let keys = self
            .receiver
            .open(envelope, action, store)
            .map_err(refused)?;
        // Used once: of two copies of the same envelope, only one gets here.
        if self.challenges.lock().remove(&challenge).is_none() {
            return Err(refused(TransportError::Open));
        }
        if keys.len() != KEYS_LEN {
            return Err(KeyCustodyError::InvalidKeyMaterial(
                "the encrypted keys are not a view key and a spend key".into(),
            ));
        }
        WalletMaterial::from_raw_bytes(&keys)
    }

    /// Registers keys from a merchant's envelope (see
    /// [`KeyCustody::register_envelope`]) at time `now`.
    pub async fn register_envelope_at(
        &self,
        envelope: &Envelope,
        action: Action,
        store: Option<&str>,
        now: i64,
    ) -> Result<(WalletHandle, Vec<u8>), KeyCustodyError> {
        let master = self.master()?;
        let material = self.open_merchant_envelope(envelope, action, store, now)?;
        let sealed = seal_with(&master, &material);
        let handle = self.registry.register_wallet(material).await?;
        Ok((handle, sealed))
    }

    // -- Handoff -----------------------------------------------------------

    /// Whether the backend is waiting for its master key from a handoff.
    pub fn awaiting_handoff(&self) -> bool {
        matches!(&*self.master.read(), Master::Waiting(_))
    }

    /// The bundle this engine sends the engine it replaces, asking for the
    /// master key.
    pub fn handoff_bundle(&self, now: i64) -> Result<Bundle, KeyCustodyError> {
        self.bundle_with(
            self.handoff_challenge,
            Action::Handoff,
            None,
            now + CHALLENGE_TTL_SECS,
        )
    }

    /// Who this engine exchanges the master key with: images signed by its
    /// own ID key (as its report attests it, not as configured), at
    /// `min_guest_svn` or later, on firmware the configured floor admits.
    fn handoff_policy(&self, min_guest_svn: u32) -> TrustPolicy {
        TrustPolicy {
            id_key_digest: self.report.id_key_digest,
            min_guest_svn,
            min_tcb: self.config.trust.min_tcb,
        }
    }

    /// Answers a successor's handoff request: checks its bundle (under
    /// `anchor`, AMD's chain in production) names an image signed by this
    /// engine's own ID key, at this engine's security version or later, and
    /// encrypts the master key to it, with a report of this engine's own
    /// attesting the answer.
    pub fn answer_handoff(
        &self,
        bundle: &Bundle,
        anchor: &Anchor,
        now: i64,
    ) -> Result<HandoffAnswer, KeyCustodyError> {
        let master = self.master()?;
        if bundle.action != Action::Handoff {
            return Err(KeyCustodyError::InvalidKeyMaterial(
                "not a handoff request".into(),
            ));
        }
        let policy =
            self.handoff_policy(self.config.trust.min_guest_svn.max(self.report.guest_svn));
        let verified = transport::verify_bundle(bundle, &policy, anchor, now)
            .map_err(|e| KeyCustodyError::InvalidKeyMaterial(e.to_string()))?;
        let envelope = transport::seal(&verified, master.as_slice())
            .map_err(|e| KeyCustodyError::InvalidKeyMaterial(e.to_string()))?;
        let evidence = self.evidence.read().clone().ok_or_else(|| {
            unavailable("AMD's certificates for this engine haven't been fetched yet; try again in a minute")
        })?;
        let report = self
            .guest
            .report(&transport::handoff_answer_report_data(
                &verified.public_key,
                &envelope,
            ))
            .map_err(|e| unavailable(e.to_string()))?;
        Ok(HandoffAnswer {
            v: transport::PROTOCOL_VERSION,
            envelope,
            from: Attestation {
                product: self.config.product.kds_name().to_owned(),
                report: hex::encode(report),
                ask: hex::encode(&evidence.ask_der),
                vcek: hex::encode(&evidence.vcek_der),
                crl: hex::encode(&evidence.crl_der),
            },
        })
    }

    /// Takes the master key from the predecessor's answer, wraps it for this
    /// image and starts serving. The answer must be attested (under
    /// `anchor`) by an engine signed by this engine's own ID key, made for
    /// this engine's handoff request.
    pub fn accept_handoff(
        &self,
        answer: &HandoffAnswer,
        anchor: &Anchor,
        now: i64,
    ) -> Result<(), String> {
        transport::verify_handoff_answer(
            answer,
            &self.handoff_policy(self.config.trust.min_guest_svn),
            anchor,
            now,
            &self.receiver.public_key(),
        )
        .map_err(|e| e.to_string())?;
        let envelope = &answer.envelope;
        if ReceiverKey::challenge_of(envelope).map_err(|e| e.to_string())? != self.handoff_challenge
        {
            return Err(TransportError::Open.to_string());
        }
        let opened = self
            .receiver
            .open(envelope, Action::Handoff, None)
            .map_err(|e| e.to_string())?;
        let master: [u8; 32] = opened
            .as_slice()
            .try_into()
            .map_err(|_| "the handoff didn't carry a master key".to_owned())?;
        let master = Zeroizing::new(master);
        self.store_master(&master)?;
        *self.master.write() = Master::Ready(master);
        Ok(())
    }
}

/// A store's keys sealed under the master key.
fn seal_with(master: &[u8; 32], material: &WalletMaterial) -> Vec<u8> {
    let raw = Zeroizing::new(material.to_raw_bytes());
    let mut sealed = SEALED_TAG.to_vec();
    sealed.extend_from_slice(&encrypt(master, SEAL_AAD, raw.as_slice()));
    sealed
}

/// The current time in seconds since the epoch.
pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

#[async_trait::async_trait]
impl KeyCustody for SnpKeyCustody {
    /// Refused: this backend takes keys only encrypted to it.
    async fn register_wallet(
        &self,
        material: WalletMaterial,
    ) -> Result<WalletHandle, KeyCustodyError> {
        drop(material);
        Err(KeyCustodyError::InvalidKeyMaterial(
            "SEV-SNP key custody takes keys only encrypted to it (key-custody-cli or the key entry form), never in the clear".into(),
        ))
    }

    async fn remove_wallet(&self, handle: WalletHandle) -> Result<(), KeyCustodyError> {
        self.registry.remove_wallet(handle).await
    }

    async fn seal(&self, material: &WalletMaterial) -> Result<Vec<u8>, KeyCustodyError> {
        Ok(seal_with(&*self.master()?, material))
    }

    async fn unseal_and_register(&self, sealed: &[u8]) -> Result<WalletHandle, KeyCustodyError> {
        let master = self.master()?;
        let body = sealed.strip_prefix(SEALED_TAG.as_slice()).ok_or_else(|| {
            KeyCustodyError::InvalidKeyMaterial(
                "these sealed keys weren't sealed by SEV-SNP key custody".into(),
            )
        })?;
        let raw = decrypt(&master, SEAL_AAD, body).ok_or_else(|| {
            KeyCustodyError::InvalidKeyMaterial(
                "these sealed keys don't open with this engine's master key: the store's keys must be entered again".into(),
            )
        })?;
        let material = WalletMaterial::from_raw_bytes(&raw)?;
        self.registry.register_wallet(material).await
    }

    async fn derive_subaddress(
        &self,
        handle: WalletHandle,
        index: SubaddressIndex,
        network: Network,
    ) -> Result<Address, KeyCustodyError> {
        self.registry
            .derive_subaddress(handle, index, network)
            .await
    }

    async fn scan_tx_outputs(
        &self,
        handle: WalletHandle,
        tx: &ScanInput,
        major_range: Range<u32>,
        minor_range: Range<u32>,
    ) -> Result<Vec<MatchedOutput>, KeyCustodyError> {
        self.registry
            .scan_tx_outputs(handle, tx, major_range, minor_range)
            .await
    }

    async fn scan_txs_for_indices(
        &self,
        handle: WalletHandle,
        txs: &[ScanInput],
        indices: &ScanIndices,
    ) -> Result<Vec<TxMatches>, KeyCustodyError> {
        self.registry
            .scan_txs_for_indices(handle, txs, indices)
            .await
    }

    /// Why the backend can't serve: it is waiting for its master key.
    fn unavailable(&self) -> Option<String> {
        self.status().waiting
    }

    async fn key_bundle(
        &self,
        action: Action,
        store: Option<&str>,
    ) -> Result<Bundle, KeyCustodyError> {
        self.master()?;
        self.issue_bundle(action, store, unix_now())
    }

    async fn register_envelope(
        &self,
        envelope: &Envelope,
        action: Action,
        store: Option<&str>,
    ) -> Result<(WalletHandle, Vec<u8>), KeyCustodyError> {
        self.register_envelope_at(envelope, action, store, unix_now())
            .await
    }

    fn takes_raw_keys(&self) -> bool {
        false
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use snp_attest::guest::{TestGuest, TestIdentity};

    const NOW: i64 = 1_790_899_200;

    #[derive(Default)]
    struct MemoryWraps(Mutex<Vec<StoredWrap>>);

    impl WrapStore for MemoryWraps {
        fn load(&self) -> Result<Vec<StoredWrap>, String> {
            Ok(self.0.lock().clone())
        }
        fn save(&self, wrap: &StoredWrap) -> Result<(), String> {
            let mut wraps = self.0.lock();
            wraps.retain(|w| w.measurement != wrap.measurement);
            wraps.push(wrap.clone());
            Ok(())
        }
    }

    struct FixedEvidence;

    #[async_trait::async_trait]
    impl EvidenceSource for FixedEvidence {
        async fn evidence(&self, _: Product, _: &AttestationReport) -> Result<Evidence, String> {
            Ok(Evidence {
                ask_der: vec![1],
                vcek_der: vec![2],
                crl_der: vec![3],
            })
        }
    }

    fn identity(measurement: u8, svn: u32) -> TestIdentity {
        TestIdentity {
            measurement: [measurement; 48],
            guest_svn: svn,
            ..TestIdentity::default()
        }
    }

    fn trust() -> TrustPolicy {
        TrustPolicy {
            id_key_digest: TestIdentity::default().id_key_digest,
            min_guest_svn: 1,
            min_tcb: transport::TcbFloor::default(),
        }
    }

    struct Engine {
        backend: SnpKeyCustody,
        vcek: p384::ecdsa::VerifyingKey,
    }

    async fn engine(chip: u8, identity: TestIdentity, wraps: &Arc<MemoryWraps>) -> Engine {
        let guest = TestGuest::new([chip; 32], identity);
        let vcek = guest.vcek();
        let backend = SnpKeyCustody::start(
            Arc::new(guest),
            SnpConfig {
                product: Product::Genoa,
                trust: trust(),
            },
            Arc::<MemoryWraps>::clone(wraps) as Arc<dyn WrapStore>,
        )
        .unwrap();
        backend.refresh_evidence(&FixedEvidence).await.unwrap();
        Engine { backend, vcek }
    }

    fn keys(seed: u8) -> [u8; KEYS_LEN] {
        let mut view = [seed; 32];
        view[31] &= 0x0f;
        let mut spend = [seed.wrapping_add(1); 32];
        spend[31] &= 0x0f;
        let spend =
            monero::PublicKey::from_private_key(&monero::PrivateKey::from_slice(&spend).unwrap())
                .to_bytes();
        let mut out = [0u8; KEYS_LEN];
        out[..32].copy_from_slice(&view);
        out[32..].copy_from_slice(&spend);
        out
    }

    /// What a merchant's client does: check the bundle, then seal to it.
    fn client_seal(engine: &Engine, bundle: &Bundle, keys: &[u8]) -> Envelope {
        let verified =
            transport::verify_bundle(bundle, &trust(), &Anchor::Vcek(engine.vcek), NOW).unwrap();
        transport::seal(&verified, keys).unwrap()
    }

    #[tokio::test]
    async fn a_merchants_encrypted_keys_are_registered_sealed_and_survive_a_restart() {
        let wraps = Arc::new(MemoryWraps::default());
        let first = engine(1, identity(1, 1), &wraps).await;
        let bundle = first
            .backend
            .issue_bundle(Action::Create, None, NOW)
            .unwrap();
        let envelope = client_seal(&first, &bundle, &keys(5));

        let (handle, sealed) = first
            .backend
            .register_envelope_at(&envelope, Action::Create, None, NOW)
            .await
            .unwrap();
        let address = first
            .backend
            .derive_subaddress(handle, SubaddressIndex::default(), Network::Mainnet)
            .await
            .unwrap();
        assert!(sealed.starts_with(SEALED_TAG));
        assert!(
            !sealed.windows(32).any(|w| w == &keys(5)[..32]),
            "the view key is not in the sealed bytes"
        );

        // The same image on the same chip, restarted: the master key comes
        // back from its wrap and the store's keys unseal.
        let again = engine(1, identity(1, 1), &wraps).await;
        assert_eq!(again.backend.status().waiting, None);
        let handle = again.backend.unseal_and_register(&sealed).await.unwrap();
        assert_eq!(
            again
                .backend
                .derive_subaddress(handle, SubaddressIndex::default(), Network::Mainnet)
                .await
                .unwrap(),
            address
        );
    }

    #[tokio::test]
    async fn an_envelope_is_used_once_and_only_for_what_its_challenge_was_issued_for() {
        let wraps = Arc::new(MemoryWraps::default());
        let e = engine(1, identity(1, 1), &wraps).await;
        let bundle = e
            .backend
            .issue_bundle(Action::Move, Some("st_1"), NOW)
            .unwrap();
        let envelope = client_seal(&e, &bundle, &keys(5));

        let refused = |r: Result<(WalletHandle, Vec<u8>), KeyCustodyError>| {
            matches!(r, Err(KeyCustodyError::InvalidKeyMaterial(_)))
        };
        // Another store's request: refused, and the challenge is kept for
        // the store it was issued to, which then uses it up.
        assert!(refused(
            e.backend
                .register_envelope_at(&envelope, Action::Move, Some("st_2"), NOW)
                .await
        ));
        e.backend
            .register_envelope_at(&envelope, Action::Move, Some("st_1"), NOW)
            .await
            .unwrap();
        assert!(refused(
            e.backend
                .register_envelope_at(&envelope, Action::Move, Some("st_1"), NOW)
                .await
        ));

        let bundle = e
            .backend
            .issue_bundle(Action::Move, Some("st_1"), NOW)
            .unwrap();
        let envelope = client_seal(&e, &bundle, &keys(5));
        assert!(
            refused(
                e.backend
                    .register_envelope_at(
                        &envelope,
                        Action::Move,
                        Some("st_1"),
                        NOW + CHALLENGE_TTL_SECS
                    )
                    .await
            ),
            "expired"
        );

        let bundle = e
            .backend
            .issue_bundle(Action::Move, Some("st_1"), NOW)
            .unwrap();
        let envelope = client_seal(&e, &bundle, &keys(5));
        e.backend
            .register_envelope_at(&envelope, Action::Move, Some("st_1"), NOW)
            .await
            .unwrap();
        assert!(
            refused(
                e.backend
                    .register_envelope_at(&envelope, Action::Move, Some("st_1"), NOW)
                    .await
            ),
            "replayed"
        );
    }

    #[tokio::test]
    async fn keys_in_the_clear_are_refused() {
        let wraps = Arc::new(MemoryWraps::default());
        let e = engine(1, identity(1, 1), &wraps).await;
        assert!(!e.backend.takes_raw_keys());
        let material = WalletMaterial::from_raw_bytes(&keys(5)).unwrap();
        assert!(matches!(
            e.backend.register_wallet(material).await,
            Err(KeyCustodyError::InvalidKeyMaterial(_))
        ));
    }

    /// An upgrade: the new image waits, gets the master key from the old
    /// one by handoff, and then opens every store's sealed keys.
    #[tokio::test]
    async fn a_signed_upgrade_takes_the_master_key_by_handoff() {
        let wraps = Arc::new(MemoryWraps::default());
        let old = engine(1, identity(1, 1), &wraps).await;
        let material = WalletMaterial::from_raw_bytes(&keys(5)).unwrap();
        let sealed = old.backend.seal(&material).await.unwrap();

        let new = engine(1, identity(2, 2), &wraps).await;
        assert!(new.backend.awaiting_handoff());
        assert!(matches!(
            new.backend.unseal_and_register(&sealed).await,
            Err(KeyCustodyError::BackendUnavailable(_))
        ));
        assert!(new.backend.unavailable().is_some());
        assert!(matches!(
            new.backend.key_bundle(Action::Create, None).await,
            Err(KeyCustodyError::BackendUnavailable(_))
        ));

        let request = new.backend.handoff_bundle(NOW).unwrap();
        let answer = old
            .backend
            .answer_handoff(&request, &Anchor::Vcek(new.vcek), NOW)
            .unwrap();
        new.backend
            .accept_handoff(&answer, &Anchor::Vcek(old.vcek), NOW)
            .unwrap();
        assert!(!new.backend.awaiting_handoff());
        new.backend.unseal_and_register(&sealed).await.unwrap();

        // And it keeps it across its own restarts.
        let restarted = engine(1, identity(2, 2), &wraps).await;
        restarted
            .backend
            .unseal_and_register(&sealed)
            .await
            .unwrap();
    }

    /// The old engine hands over only to a trusted image at its own
    /// security version or later; a handoff answer opens only for the engine
    /// that asked.
    #[tokio::test]
    async fn a_handoff_goes_only_to_a_trusted_image_that_is_not_older() {
        let wraps = Arc::new(MemoryWraps::default());
        let old = engine(1, identity(1, 2), &wraps).await;

        let older = engine(1, identity(3, 1), &wraps).await;
        let request = older.backend.handoff_bundle(NOW).unwrap();
        assert!(matches!(
            old.backend.answer_handoff(&request, &Anchor::Vcek(older.vcek), NOW),
            Err(KeyCustodyError::InvalidKeyMaterial(m)) if m.contains("security version")
        ));

        let untrusted_guest = TestGuest::new(
            [1; 32],
            TestIdentity {
                id_key_digest: [0xEE; 48],
                ..identity(4, 5)
            },
        );
        let impostor_key = ReceiverKey::generate();
        let impostor_report = untrusted_guest
            .report(&transport::report_data_for(&impostor_key.public_key()))
            .unwrap();
        let mut impostor = old.backend.handoff_bundle(NOW).unwrap();
        impostor.report = hex::encode(impostor_report);
        impostor.public_key = hex::encode(impostor_key.public_key());
        assert!(matches!(
            old.backend.answer_handoff(&impostor, &Anchor::Vcek(untrusted_guest.vcek()), NOW),
            Err(KeyCustodyError::InvalidKeyMaterial(m)) if m.contains("ID key")
        ));

        let newer = engine(1, identity(5, 3), &wraps).await;
        let other = engine(1, identity(6, 3), &wraps).await;
        let answer = old
            .backend
            .answer_handoff(
                &newer.backend.handoff_bundle(NOW).unwrap(),
                &Anchor::Vcek(newer.vcek),
                NOW,
            )
            .unwrap();
        assert!(
            other
                .backend
                .accept_handoff(&answer, &Anchor::Vcek(old.vcek), NOW)
                .is_err(),
            "not the engine that asked"
        );
        newer
            .backend
            .accept_handoff(&answer, &Anchor::Vcek(old.vcek), NOW)
            .unwrap();
    }

    /// The database moved to another chip: the wrap doesn't open there, and
    /// the backend says so instead of making a new master key.
    #[tokio::test]
    async fn a_wrap_from_another_chip_leaves_the_backend_waiting() {
        let wraps = Arc::new(MemoryWraps::default());
        engine(1, identity(1, 1), &wraps).await;
        let elsewhere = engine(2, identity(1, 1), &wraps).await;
        let waiting = elsewhere.backend.status().waiting.unwrap();
        assert!(waiting.contains("another machine"), "{waiting}");
    }

    #[test]
    fn an_untrusted_engine_image_does_not_start() {
        let wraps: Arc<dyn WrapStore> = Arc::new(MemoryWraps::default());
        let config = SnpConfig {
            product: Product::Genoa,
            trust: trust(),
        };
        for identity in [
            TestIdentity {
                id_key_digest: [0xEE; 48],
                ..TestIdentity::default()
            },
            TestIdentity {
                guest_svn: 0,
                ..TestIdentity::default()
            },
        ] {
            let started = SnpKeyCustody::start(
                Arc::new(TestGuest::new([1; 32], identity)),
                config,
                Arc::clone(&wraps),
            );
            assert!(started.err().unwrap().contains("not a trusted"));
        }
    }

    #[tokio::test]
    async fn without_amds_certificates_no_bundle_is_offered() {
        let wraps: Arc<dyn WrapStore> = Arc::new(MemoryWraps::default());
        let backend = SnpKeyCustody::start(
            Arc::new(TestGuest::new([1; 32], identity(1, 1))),
            SnpConfig {
                product: Product::Genoa,
                trust: trust(),
            },
            wraps,
        )
        .unwrap();
        assert!(!backend.status().evidence);
        assert!(matches!(
            backend.issue_bundle(Action::Create, None, NOW),
            Err(KeyCustodyError::BackendUnavailable(_))
        ));
    }

    /// A handoff answer is taken only when a trusted engine attested it for
    /// this request: one sealed to the new engine's key by anyone else (the
    /// host, who saw the request), or attested by an engine signed by
    /// another ID key, plants nothing.
    #[tokio::test]
    async fn a_handoff_answer_nobody_trusted_attested_is_refused() {
        let wraps = Arc::new(MemoryWraps::default());
        let old = engine(1, identity(1, 1), &wraps).await;
        let new = engine(1, identity(2, 2), &wraps).await;
        let request = new.backend.handoff_bundle(NOW).unwrap();
        let genuine = old
            .backend
            .answer_handoff(&request, &Anchor::Vcek(new.vcek), NOW)
            .unwrap();

        // The host's own key, sealed to the new engine, with the genuine
        // answer's attestation reused: the report doesn't vouch for it.
        let verified =
            transport::verify_bundle(&request, &trust(), &Anchor::Vcek(new.vcek), NOW).unwrap();
        let planted = HandoffAnswer {
            envelope: transport::seal(&verified, &[0x66; 32]).unwrap(),
            ..genuine.clone()
        };
        assert!(new
            .backend
            .accept_handoff(&planted, &Anchor::Vcek(old.vcek), NOW)
            .is_err());

        // Attested by an engine on the host's own ID key.
        let rogue_guest = TestGuest::new(
            [1; 32],
            TestIdentity {
                id_key_digest: [0xEE; 48],
                ..identity(9, 9)
            },
        );
        let rogue_report = rogue_guest
            .report(&transport::handoff_answer_report_data(
                &verified.public_key,
                &planted.envelope,
            ))
            .unwrap();
        let rogue = HandoffAnswer {
            from: Attestation {
                report: hex::encode(rogue_report),
                ..planted.from.clone()
            },
            ..planted
        };
        assert!(new
            .backend
            .accept_handoff(&rogue, &Anchor::Vcek(rogue_guest.vcek()), NOW)
            .is_err());
        assert!(new.backend.awaiting_handoff());

        new.backend
            .accept_handoff(&genuine, &Anchor::Vcek(old.vcek), NOW)
            .unwrap();
    }

    /// The genuine image relaunched with an ID block someone else signed
    /// (and configured to trust that key) gets another wrapping key: it
    /// can't unwrap the master key, and waits.
    #[tokio::test]
    async fn the_same_image_under_another_id_key_cannot_unwrap_the_master_key() {
        let wraps = Arc::new(MemoryWraps::default());
        engine(1, identity(1, 1), &wraps).await;
        let host_key = [0xEE; 48];
        let relaunched = SnpKeyCustody::start(
            Arc::new(TestGuest::new(
                [1; 32],
                TestIdentity {
                    id_key_digest: host_key,
                    ..identity(1, 1)
                },
            )),
            SnpConfig {
                product: Product::Genoa,
                trust: TrustPolicy {
                    id_key_digest: host_key,
                    ..trust()
                },
            },
            Arc::<MemoryWraps>::clone(&wraps) as Arc<dyn WrapStore>,
        )
        .unwrap();
        assert!(relaunched.awaiting_handoff());
    }

    /// A garbage envelope under a real challenge doesn't spend it.
    #[tokio::test]
    async fn an_envelope_that_does_not_open_leaves_its_challenge_for_the_real_one() {
        let wraps = Arc::new(MemoryWraps::default());
        let e = engine(1, identity(1, 1), &wraps).await;
        let bundle = e.backend.issue_bundle(Action::Create, None, NOW).unwrap();
        let garbage = Envelope {
            v: transport::PROTOCOL_VERSION,
            challenge: bundle.challenge.clone(),
            enc: hex::encode([1u8; 32]),
            ciphertext: hex::encode([2u8; 80]),
        };
        assert!(e
            .backend
            .register_envelope_at(&garbage, Action::Create, None, NOW)
            .await
            .is_err());
        let envelope = client_seal(&e, &bundle, &keys(5));
        e.backend
            .register_envelope_at(&envelope, Action::Create, None, NOW)
            .await
            .unwrap();
    }
}
