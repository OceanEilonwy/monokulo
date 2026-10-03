//! Getting a merchant's keys to the `snp` backend so that only it can read
//! them, and the master key from one `snp` engine to the next.
//!
//! The backend makes an X25519 key pair when it starts, inside the
//! confidential VM, and asks the security processor for an attestation report
//! whose REPORT_DATA is a hash of the public key ([`report_data_for`]). A
//! [`Bundle`] carries that report, the AMD certificates it is checked
//! against, the public key, and a single-use challenge the backend issued for
//! one action (and, for a move, one store).
//!
//! Whoever holds the keys (the browser, through this module built as
//! WebAssembly, or `key-custody-cli`) checks the bundle with
//! [`verify_bundle`]: the report was signed by a genuine AMD chip, through
//! AMD's chain up to the pinned root ([`Anchor::Amd`]); the guest can't be
//! debugged; it was launched with an ID block signed by the trusted ID key and
//! is at least the minimum security version ([`TrustPolicy`]); and the public
//! key is the one in the report. Then [`seal`] encrypts the keys to it with
//! HPKE (RFC 9180: X25519, HKDF-SHA256, AES-256-GCM), binding the challenge,
//! action and store as associated data, into an [`Envelope`].
//!
//! Monokulo and the engine's HTTP layer only ever relay the envelope. Only the
//! backend's [`ReceiverKey`] opens it, and the backend accepts its challenge
//! once.
//!
//! What this protects depends on who runs the check. `key-custody-cli` is
//! installed by the merchant and pins the trust anchors itself, so it holds
//! even against a compromised monokulo. The browser runs code monokulo
//! served, so there it protects against everything except monokulo itself
//! being compromised when the page loads.

use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use snp_attest::report::{self, AttestationReport, Product};
use snp_attest::verify::{self, Evidence};
use zeroize::Zeroizing;

use hpke::{Deserializable as _, Kem as _, Serializable as _};

type Kem = hpke::kem::X25519HkdfSha256;
type Kdf = hpke::kdf::HkdfSha256;
type Aead = hpke::aead::AesGcm256;

/// The version of the bundle and envelope formats. A bundle or envelope of
/// another version is refused, naming both, so a merchant with the wrong
/// `key-custody-cli` is told which one they need.
pub const PROTOCOL_VERSION: u32 = 1;

/// The length of a merchant's keys as sealed: the private view key, then the
/// public spend key.
pub const KEYS_LEN: usize = 64;

const REPORT_DATA_CONTEXT: &[u8] = b"monokulo key custody bundle v1\0";
const HPKE_INFO: &[u8] = b"monokulo key custody v1";

/// What a bundle's keys are for. A challenge is issued for one action and
/// an envelope is opened only for the action it was issued for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    /// A new store's keys.
    Create,
    /// An existing store's keys, moving to this backend (or entered again).
    Move,
    /// The master key, from the engine being upgraded from to its successor.
    Handoff,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Create => "create",
            Action::Move => "move",
            Action::Handoff => "handoff",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "create" => Some(Action::Create),
            "move" => Some(Action::Move),
            "handoff" => Some(Action::Handoff),
            _ => None,
        }
    }
}

/// Everything a client needs to check the backend and encrypt to it. Byte
/// fields are hex.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bundle {
    pub v: u32,
    /// The AMD product line (`Milan`, `Genoa`, `Turin`): which pinned root
    /// the chain must lead to.
    pub product: String,
    /// The attestation report, 1184 bytes.
    pub report: String,
    /// AMD's intermediate certificate (ASK), DER.
    pub ask: String,
    /// The chip's certificate (VCEK) for the report's TCB, DER.
    pub vcek: String,
    /// AMD's revocation list, DER.
    pub crl: String,
    /// The backend's X25519 public key, 32 bytes.
    pub public_key: String,
    /// The single-use challenge, 32 bytes.
    pub challenge: String,
    pub action: Action,
    /// The store a `move` challenge is for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store: Option<String>,
    /// When the challenge stops being accepted, in seconds since the epoch.
    pub expires_at: i64,
}

/// Keys encrypted to a backend. Byte fields are hex.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub v: u32,
    /// The challenge from the bundle it was sealed against.
    pub challenge: String,
    /// The HPKE encapsulated key.
    pub enc: String,
    /// The HPKE ciphertext, tag included.
    pub ciphertext: String,
}

impl Envelope {
    /// The envelope as the text a merchant pastes (JSON).
    pub fn to_text(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// An envelope from the text a merchant pasted, surrounding whitespace
    /// allowed.
    pub fn from_text(text: &str) -> Result<Self, TransportError> {
        let envelope: Envelope = serde_json::from_str(text.trim()).map_err(|_| {
            TransportError::Malformed(
                "this is not encrypted keys from key-custody-cli or the key entry form".into(),
            )
        })?;
        check_version(envelope.v)?;
        Ok(envelope)
    }
}

/// What a report is checked against besides its own signature.
#[derive(Debug, Clone)]
pub enum Anchor {
    /// AMD's chain, up to the pinned root, with the bundle's certificates.
    Amd,
    /// A report signing key the caller already trusts, in place of AMD's
    /// chain: `snp_attest::guest::TestGuest::vcek` in tests.
    Vcek(p384::ecdsa::VerifyingKey),
}

/// Which guests are trusted with keys: launched with an ID block signed by
/// the key whose SHA-384 is `id_key_digest`, at security version
/// `min_guest_svn` or later, on firmware at `min_tcb` or later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrustPolicy {
    pub id_key_digest: [u8; 48],
    pub min_guest_svn: u32,
    pub min_tcb: TcbFloor,
}

/// The lowest firmware a report may come from: the security patch levels of
/// its reported TCB, each at least this. AMD keeps certifying old firmware,
/// so a report from firmware with a known SEV-SNP break still checks out
/// unless a floor refuses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TcbFloor {
    pub bootloader: u8,
    pub tee: u8,
    pub snp: u8,
    pub microcode: u8,
}

impl TcbFloor {
    /// A floor from `bootloader,tee,snp,microcode` (each 0-255); empty is no
    /// floor.
    pub fn parse(text: &str) -> Result<Self, TransportError> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(TcbFloor::default());
        }
        let parts: Vec<u8> = text
            .split(',')
            .map(|part| part.trim().parse::<u8>())
            .collect::<Result<_, _>>()
            .map_err(|_| TransportError::Malformed(TCB_FLOOR_FORMAT.into()))?;
        match parts.as_slice() {
            [bootloader, tee, snp, microcode] => Ok(TcbFloor {
                bootloader: *bootloader,
                tee: *tee,
                snp: *snp,
                microcode: *microcode,
            }),
            _ => Err(TransportError::Malformed(TCB_FLOOR_FORMAT.into())),
        }
    }

    /// The floor as [`Self::parse`] reads it; empty when there is none.
    pub fn to_text(self) -> String {
        if self == TcbFloor::default() {
            return String::new();
        }
        format!(
            "{},{},{},{}",
            self.bootloader, self.tee, self.snp, self.microcode
        )
    }

    fn admits(self, tcb: &report::TcbVersion) -> bool {
        tcb.bootloader >= self.bootloader
            && tcb.tee >= self.tee
            && tcb.snp >= self.snp
            && tcb.microcode >= self.microcode
    }
}

const TCB_FLOOR_FORMAT: &str =
    "a minimum TCB is four numbers, bootloader,tee,snp,microcode (each 0-255)";

/// The digest of the official ID key, the one release builds of the engine
/// image are signed with, if this build has one (see
/// `src/official_id_key_digest.txt`).
pub fn official_id_key_digest() -> Option<[u8; 48]> {
    parse_id_key_digest(include_str!("official_id_key_digest.txt").trim()).ok()
}

/// An ID key digest from its 96 hex characters.
pub fn parse_id_key_digest(text: &str) -> Result<[u8; 48], TransportError> {
    let bytes = hex::decode(text.trim())
        .map_err(|_| TransportError::Malformed("an ID key digest is 96 hex characters".into()))?;
    bytes
        .try_into()
        .map_err(|_| TransportError::Malformed("an ID key digest is 96 hex characters".into()))
}

/// A bundle that checked out: who the keys go to and under which challenge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    pub public_key: [u8; 32],
    pub challenge: [u8; 32],
    pub action: Action,
    pub store: Option<String>,
    /// The engine image's launch measurement and security version, for
    /// display.
    pub measurement: [u8; 48],
    pub guest_svn: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    #[error("this is format version {got}, and this side speaks version {PROTOCOL_VERSION}: use the key-custody-cli release that matches the monokulo you are using")]
    Version { got: u32 },
    #[error("this bundle expired; load the form again (or fetch the bundle again) for a new one")]
    Expired,
    #[error("{0}")]
    Malformed(String),
    #[error("the attestation report doesn't check out: {0}")]
    Attestation(String),
    #[error("the engine was not launched as the trusted image: its ID key is {got}, not the trusted {want}")]
    UntrustedImage { got: String, want: String },
    #[error("the engine was launched without an ID block, so nothing says which image it runs")]
    NoIdBlock,
    #[error("the engine runs security version {got}, below the minimum {min}")]
    SvnTooLow { got: u32, min: u32 },
    #[error("the engine's firmware (TCB {got}) is below the minimum {min}: it may have known SEV-SNP vulnerabilities")]
    TcbTooLow { got: String, min: String },
    #[error(
        "the master key's handoff didn't come from a trusted engine, or wasn't made for this one"
    )]
    HandoffNotAttested,
    #[error("the report was requested at VMPL {0}, not by the guest kernel (VMPL 0)")]
    WrongVmpl(u32),
    #[error("the public key in the bundle is not the one the report vouches for")]
    KeyNotInReport,
    #[error("encrypting failed: {0}")]
    Seal(String),
    #[error("these keys weren't encrypted for this engine, or for this request; load the form again and encrypt them once more")]
    Open,
}

/// Refuses a bundle or envelope of another format version.
pub fn check_version(got: u32) -> Result<(), TransportError> {
    if got == PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(TransportError::Version { got })
    }
}

fn hex_field<const N: usize>(what: &str, text: &str) -> Result<[u8; N], TransportError> {
    hex::decode(text)
        .ok()
        .and_then(|bytes| <[u8; N]>::try_from(bytes).ok())
        .ok_or_else(|| {
            TransportError::Malformed(format!("the bundle's {what} is not {N} bytes of hex"))
        })
}

fn hex_bytes(what: &str, text: &str) -> Result<Vec<u8>, TransportError> {
    hex::decode(text)
        .map_err(|_| TransportError::Malformed(format!("the bundle's {what} is not hex")))
}

/// The REPORT_DATA a backend's report carries for `public_key`.
pub fn report_data_for(public_key: &[u8; 32]) -> [u8; 64] {
    let mut hash = sha2::Sha512::new();
    hash.update(REPORT_DATA_CONTEXT);
    hash.update(public_key);
    hash.finalize().into()
}

/// The parts of a guest's identity [`TrustPolicy`] decides on, checked on a
/// report already known to be genuine.
pub fn check_identity(
    report: &AttestationReport,
    policy: &TrustPolicy,
) -> Result<(), TransportError> {
    if report.debug_allowed() {
        return Err(TransportError::Attestation(
            verify::VerifyError::DebugAllowed.to_string(),
        ));
    }
    if report.vmpl != 0 {
        return Err(TransportError::WrongVmpl(report.vmpl));
    }
    if !report.has_id_key() {
        return Err(TransportError::NoIdBlock);
    }
    if report.id_key_digest != policy.id_key_digest {
        return Err(TransportError::UntrustedImage {
            got: hex::encode(report.id_key_digest),
            want: hex::encode(policy.id_key_digest),
        });
    }
    if report.guest_svn < policy.min_guest_svn {
        return Err(TransportError::SvnTooLow {
            got: report.guest_svn,
            min: policy.min_guest_svn,
        });
    }
    if !policy.min_tcb.admits(&report.reported_tcb) {
        let tcb = &report.reported_tcb;
        return Err(TransportError::TcbTooLow {
            got: format!(
                "{},{},{},{}",
                tcb.bootloader, tcb.tee, tcb.snp, tcb.microcode
            ),
            min: policy.min_tcb.to_text(),
        });
    }
    Ok(())
}

/// Checks `report` is genuine: under `anchor`, with `evidence` for AMD's chain.
pub fn check_report(
    product: Product,
    report: &AttestationReport,
    evidence: &Evidence,
    anchor: &Anchor,
    now_unix: i64,
) -> Result<(), TransportError> {
    let checked = match anchor {
        Anchor::Amd => verify::verify_evidence(product, report, evidence, now_unix).map(|_| ()),
        Anchor::Vcek(key) => verify::verify_report_signed_by(report, key),
    };
    checked.map_err(|e| TransportError::Attestation(e.to_string()))
}

/// Parses a product name the way bundles carry it.
pub fn parse_product(name: &str) -> Result<Product, TransportError> {
    Product::parse(name).ok_or_else(|| {
        TransportError::Malformed(format!(
            "unknown AMD product {name:?}: expected Milan, Genoa or Turin"
        ))
    })
}

/// An engine's attestation report and AMD's certificates for it (hex), as a
/// handoff answer carries them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attestation {
    pub product: String,
    pub report: String,
    pub ask: String,
    pub vcek: String,
    pub crl: String,
}

/// The master key, from the engine being upgraded from to its successor:
/// encrypted to the successor, and attested by the engine handing it over
/// (its report's REPORT_DATA binds the envelope and the successor's key and
/// challenge, [`handoff_answer_report_data`]), so nothing but a trusted
/// engine can plant a master key in a successor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffAnswer {
    pub v: u32,
    pub envelope: Envelope,
    pub from: Attestation,
}

const HANDOFF_ANSWER_CONTEXT: &[u8] = b"monokulo key custody handoff answer v1\0";

/// The REPORT_DATA of the report attesting a handoff answer: the envelope,
/// for the successor whose key is `successor_key`.
pub fn handoff_answer_report_data(successor_key: &[u8; 32], envelope: &Envelope) -> [u8; 64] {
    let mut hash = sha2::Sha512::new();
    hash.update(HANDOFF_ANSWER_CONTEXT);
    hash.update(successor_key);
    for part in [&envelope.challenge, &envelope.enc, &envelope.ciphertext] {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part.as_bytes());
    }
    hash.finalize().into()
}

/// A genuine report from a trusted guest: AMD-signed (under `anchor`) and
/// admitted by `policy`.
fn verify_attestation(
    from: &Attestation,
    policy: &TrustPolicy,
    anchor: &Anchor,
    now_unix: i64,
) -> Result<AttestationReport, TransportError> {
    let product = parse_product(&from.product)?;
    let raw = hex_bytes("report", &from.report)?;
    let report =
        report::parse(&raw, product).map_err(|e| TransportError::Attestation(e.to_string()))?;
    let evidence = Evidence {
        ask_der: hex_bytes("ASK", &from.ask)?,
        vcek_der: hex_bytes("VCEK", &from.vcek)?,
        crl_der: hex_bytes("revocation list", &from.crl)?,
    };
    check_report(product, &report, &evidence, anchor, now_unix)?;
    check_identity(&report, policy)?;
    Ok(report)
}

/// Checks a handoff answer was made by a trusted engine (under `policy` and
/// `anchor`) for the successor whose key is `successor_key`.
pub fn verify_handoff_answer(
    answer: &HandoffAnswer,
    policy: &TrustPolicy,
    anchor: &Anchor,
    now_unix: i64,
    successor_key: &[u8; 32],
) -> Result<(), TransportError> {
    check_version(answer.v)?;
    let report = verify_attestation(&answer.from, policy, anchor, now_unix)?;
    if report.report_data == handoff_answer_report_data(successor_key, &answer.envelope) {
        Ok(())
    } else {
        Err(TransportError::HandoffNotAttested)
    }
}

/// Checks `bundle` (see the module docs) and returns what to encrypt to.
pub fn verify_bundle(
    bundle: &Bundle,
    policy: &TrustPolicy,
    anchor: &Anchor,
    now_unix: i64,
) -> Result<Verified, TransportError> {
    check_version(bundle.v)?;
    if bundle.expires_at <= now_unix {
        return Err(TransportError::Expired);
    }
    let from = Attestation {
        product: bundle.product.clone(),
        report: bundle.report.clone(),
        ask: bundle.ask.clone(),
        vcek: bundle.vcek.clone(),
        crl: bundle.crl.clone(),
    };
    let report = verify_attestation(&from, policy, anchor, now_unix)?;
    let public_key: [u8; 32] = hex_field("public key", &bundle.public_key)?;
    if report.report_data != report_data_for(&public_key) {
        return Err(TransportError::KeyNotInReport);
    }
    Ok(Verified {
        public_key,
        challenge: hex_field("challenge", &bundle.challenge)?,
        action: bundle.action,
        store: bundle.store.clone(),
        measurement: report.measurement,
        guest_svn: report.guest_svn,
    })
}

/// The associated data an envelope is bound to.
fn associated_data(action: Action, store: Option<&str>, challenge: &[u8; 32]) -> Vec<u8> {
    let mut aad = format!(
        "monokulo key custody v{PROTOCOL_VERSION}\0{}\0{}\0",
        action.as_str(),
        store.unwrap_or("")
    )
    .into_bytes();
    aad.extend_from_slice(challenge);
    aad
}

/// Encrypts `plaintext` to the backend `verified` names.
pub fn seal(verified: &Verified, plaintext: &[u8]) -> Result<Envelope, TransportError> {
    let public_key = <Kem as hpke::Kem>::PublicKey::from_bytes(&verified.public_key)
        .map_err(|e| TransportError::Seal(e.to_string()))?;
    let aad = associated_data(
        verified.action,
        verified.store.as_deref(),
        &verified.challenge,
    );
    let (enc, ciphertext) = hpke::single_shot_seal::<Aead, Kdf, Kem>(
        &hpke::OpModeS::Base,
        &public_key,
        HPKE_INFO,
        plaintext,
        &aad,
    )
    .map_err(|e| TransportError::Seal(e.to_string()))?;
    Ok(Envelope {
        v: PROTOCOL_VERSION,
        challenge: hex::encode(verified.challenge),
        enc: hex::encode(enc.to_bytes()),
        ciphertext: hex::encode(ciphertext),
    })
}

/// A backend's key pair for receiving envelopes. Made at start and never
/// stored: an engine that restarts makes a new one, and envelopes sealed to
/// the old one no longer open.
pub struct ReceiverKey {
    private: <Kem as hpke::Kem>::PrivateKey,
    public: [u8; 32],
}

impl ReceiverKey {
    pub fn generate() -> Self {
        let (private, public) = Kem::gen_keypair();
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&public.to_bytes());
        ReceiverKey {
            private,
            public: bytes,
        }
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.public
    }

    /// The challenge an envelope names, for the caller to look up before
    /// opening it.
    pub fn challenge_of(envelope: &Envelope) -> Result<[u8; 32], TransportError> {
        check_version(envelope.v)?;
        hex_field("challenge", &envelope.challenge).map_err(|_| TransportError::Open)
    }

    /// Opens `envelope`, sealed for `action` (and `store`) under its
    /// challenge. The caller has checked the challenge is one it issued for
    /// that action and store, and not used.
    pub fn open(
        &self,
        envelope: &Envelope,
        action: Action,
        store: Option<&str>,
    ) -> Result<Zeroizing<Vec<u8>>, TransportError> {
        let challenge = Self::challenge_of(envelope)?;
        let enc = hex::decode(&envelope.enc).map_err(|_| TransportError::Open)?;
        let enc =
            <Kem as hpke::Kem>::EncappedKey::from_bytes(&enc).map_err(|_| TransportError::Open)?;
        let ciphertext = hex::decode(&envelope.ciphertext).map_err(|_| TransportError::Open)?;
        let aad = associated_data(action, store, &challenge);
        hpke::single_shot_open::<Aead, Kdf, Kem>(
            &hpke::OpModeR::Base,
            &self.private,
            &enc,
            HPKE_INFO,
            &ciphertext,
            &aad,
        )
        .map(Zeroizing::new)
        .map_err(|_| TransportError::Open)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snp_attest::guest::{GuestDevice as _, TestGuest, TestIdentity};

    const NOW: i64 = 1_790_899_200;

    fn policy(guest: &TestGuest) -> TrustPolicy {
        TrustPolicy {
            id_key_digest: guest.identity.id_key_digest,
            min_guest_svn: 1,
            min_tcb: TcbFloor::default(),
        }
    }

    fn bundle_from(
        guest: &TestGuest,
        receiver: &ReceiverKey,
        action: Action,
        store: Option<&str>,
    ) -> Bundle {
        let report = guest
            .report(&report_data_for(&receiver.public_key()))
            .unwrap();
        Bundle {
            v: PROTOCOL_VERSION,
            product: "Genoa".into(),
            report: hex::encode(report),
            ask: String::new(),
            vcek: String::new(),
            crl: String::new(),
            public_key: hex::encode(receiver.public_key()),
            challenge: hex::encode([7u8; 32]),
            action,
            store: store.map(str::to_owned),
            expires_at: NOW + 600,
        }
    }

    /// Keys sealed against a bundle open with the backend's key, for the
    /// action and store they were sealed for and no other.
    #[test]
    fn keys_sealed_to_a_verified_bundle_open_only_where_they_were_meant_to() {
        let guest = TestGuest::new([1; 32], TestIdentity::default());
        let receiver = ReceiverKey::generate();
        let bundle = bundle_from(&guest, &receiver, Action::Move, Some("st_1"));
        let verified =
            verify_bundle(&bundle, &policy(&guest), &Anchor::Vcek(guest.vcek()), NOW).unwrap();
        assert_eq!(verified.measurement, guest.identity.measurement);

        let keys = [9u8; KEYS_LEN];
        let envelope = Envelope::from_text(&seal(&verified, &keys).unwrap().to_text()).unwrap();
        assert_eq!(ReceiverKey::challenge_of(&envelope).unwrap(), [7u8; 32]);
        assert_eq!(
            receiver
                .open(&envelope, Action::Move, Some("st_1"))
                .unwrap()
                .as_slice(),
            keys.as_slice()
        );

        for (action, store) in [
            (Action::Move, Some("st_2")),
            (Action::Create, Some("st_1")),
            (Action::Move, None),
        ] {
            assert_eq!(
                receiver.open(&envelope, action, store),
                Err(TransportError::Open),
                "{action:?} {store:?}"
            );
        }
        assert_eq!(
            ReceiverKey::generate().open(&envelope, Action::Move, Some("st_1")),
            Err(TransportError::Open),
            "another backend's key"
        );
        let mut tampered = envelope.clone();
        tampered.challenge = hex::encode([8u8; 32]);
        assert_eq!(
            receiver.open(&tampered, Action::Move, Some("st_1")),
            Err(TransportError::Open)
        );
    }

    /// Each check a bundle can fail names what was wrong.
    #[test]
    fn a_bundle_that_does_not_check_out_is_refused_by_reason() {
        let guest = TestGuest::new([1; 32], TestIdentity::default());
        let receiver = ReceiverKey::generate();
        let good = bundle_from(&guest, &receiver, Action::Create, None);
        let anchor = Anchor::Vcek(guest.vcek());
        let trusted = policy(&guest);
        let refusal = |bundle: &Bundle, policy: &TrustPolicy, anchor: &Anchor| {
            verify_bundle(bundle, policy, anchor, NOW).unwrap_err()
        };

        assert!(matches!(
            refusal(
                &Bundle {
                    v: 2,
                    ..good.clone()
                },
                &trusted,
                &anchor
            ),
            TransportError::Version { got: 2 }
        ));
        assert_eq!(
            refusal(
                &Bundle {
                    expires_at: NOW,
                    ..good.clone()
                },
                &trusted,
                &anchor
            ),
            TransportError::Expired
        );
        assert!(
            matches!(
                refusal(
                    &good,
                    &trusted,
                    &Anchor::Vcek(TestGuest::new([2; 32], TestIdentity::default()).vcek())
                ),
                TransportError::Attestation(_)
            ),
            "signed by another chip"
        );
        assert!(
            matches!(
                refusal(&good, &trusted, &Anchor::Amd),
                TransportError::Attestation(_)
            ),
            "no AMD chain"
        );
        assert!(matches!(
            refusal(
                &good,
                &TrustPolicy {
                    id_key_digest: [0xEE; 48],
                    ..trusted
                },
                &anchor
            ),
            TransportError::UntrustedImage { .. }
        ));
        assert_eq!(
            refusal(
                &good,
                &TrustPolicy {
                    min_guest_svn: 2,
                    ..trusted
                },
                &anchor
            ),
            TransportError::SvnTooLow { got: 1, min: 2 }
        );
        let other_key = Bundle {
            public_key: hex::encode(ReceiverKey::generate().public_key()),
            ..good.clone()
        };
        assert_eq!(
            refusal(&other_key, &trusted, &anchor),
            TransportError::KeyNotInReport
        );

        let debuggable = TestGuest::new(
            [1; 32],
            TestIdentity {
                policy: TestIdentity::default().policy | snp_attest::report::POLICY_DEBUG,
                ..TestIdentity::default()
            },
        );
        let bundle = bundle_from(&debuggable, &receiver, Action::Create, None);
        assert!(matches!(
            refusal(&bundle, &trusted, &Anchor::Vcek(debuggable.vcek())),
            TransportError::Attestation(_)
        ));

        let no_id_block = TestGuest::new(
            [1; 32],
            TestIdentity {
                id_key_digest: [0; 48],
                ..TestIdentity::default()
            },
        );
        let bundle = bundle_from(&no_id_block, &receiver, Action::Create, None);
        assert_eq!(
            refusal(&bundle, &trusted, &Anchor::Vcek(no_id_block.vcek())),
            TransportError::NoIdBlock
        );
    }

    #[test]
    fn a_firmware_floor_refuses_older_firmware_and_reads_its_own_text() {
        let guest = TestGuest::new([1; 32], TestIdentity::default());
        let receiver = ReceiverKey::generate();
        let bundle = bundle_from(&guest, &receiver, Action::Create, None);
        let floor = TcbFloor::parse("0,0,0,1").unwrap();
        assert_eq!(floor.to_text(), "0,0,0,1");
        assert_eq!(TcbFloor::parse("").unwrap(), TcbFloor::default());
        assert!(TcbFloor::parse("1,2,3").is_err());
        assert!(TcbFloor::parse("1,2,3,256").is_err());
        let refused = verify_bundle(
            &bundle,
            &TrustPolicy {
                min_tcb: floor,
                ..policy(&guest)
            },
            &Anchor::Vcek(guest.vcek()),
            NOW,
        );
        assert!(
            matches!(refused, Err(TransportError::TcbTooLow { .. })),
            "{refused:?}"
        );
    }

    #[test]
    fn id_key_digests_are_96_hex_characters() {
        assert_eq!(parse_id_key_digest(&"ab".repeat(48)).unwrap(), [0xAB; 48]);
        assert!(parse_id_key_digest("ab").is_err());
        assert!(parse_id_key_digest(&"zz".repeat(48)).is_err());
    }

    #[test]
    fn pasted_text_that_is_not_an_envelope_is_refused_plainly() {
        assert!(matches!(
            Envelope::from_text("view key here"),
            Err(TransportError::Malformed(_))
        ));
        let text = Envelope {
            v: 9,
            challenge: String::new(),
            enc: String::new(),
            ciphertext: String::new(),
        }
        .to_text();
        assert_eq!(
            Envelope::from_text(&text),
            Err(TransportError::Version { got: 9 })
        );
    }
}
