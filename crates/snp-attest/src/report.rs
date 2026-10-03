//! Raw parsing of the AMD SEV-SNP `ATTESTATION_REPORT` structure - the fixed
//! 1184-byte binary blob a guest gets back from `/dev/sev-guest`'s
//! `SNP_GET_REPORT` ioctl (or an equivalent provider-supplied mechanism).
//!
//! No crypto lives in this module - just byte layout. Field offsets and the
//! two `TcbVersion` byte encodings (legacy Milan/Genoa/Bergamo/Siena vs.
//! Turin/Venice+) were confirmed directly against `virtee/sev` (the
//! actively-maintained reference Rust implementation this workspace also
//! draws its AMD KDS URL format from - see `crate::kds`), not reconstructed
//! from memory of AMD's PDF spec alone:
//! <https://github.com/virtee/sev/blob/main/src/firmware/guest/types/snp.rs>
//! <https://github.com/virtee/sev/blob/main/src/firmware/host/types/snp.rs>

use thiserror::Error;

pub const REPORT_LEN: usize = 1184;
/// The report's signature (at 0x2A0) covers exactly the preceding bytes -
/// confirmed directly against the reference implementation's own doc
/// comment ("Signature of bytes 0 to 0x29F inclusive").
pub const SIGNED_LEN: usize = 0x2A0;

#[derive(Debug, Error)]
pub enum ReportParseError {
    #[error("report is {got} bytes, expected exactly {REPORT_LEN}")]
    WrongLength { got: usize },
    #[error("unsupported sig_algo {0} - only 1 (ECDSA P-384 SHA-384) is implemented")]
    UnsupportedSigAlgo(u32),
}

/// AMD's `TCB_VERSION` - a platform's committed firmware/microcode versions,
/// encoded as 8 raw bytes whose *meaning per byte position* differs between
/// chip generations. This is exactly why every function that decodes one
/// takes an explicit [`Product`] rather than guessing from context - guessing
/// wrong silently reads the wrong byte as "the microcode SVN," which is
/// precisely the kind of bug that would make this tool falsely pass an
/// unpatched host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TcbVersion {
    pub raw: [u8; 8],
    pub bootloader: u8,
    pub tee: u8,
    pub snp: u8,
    pub microcode: u8,
    /// Turin+ only; `None` on Milan/Genoa/Bergamo/Siena, which have no FMC.
    pub fmc: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Product {
    Milan,
    Genoa,
    Turin,
}

impl Product {
    /// The path segment AMD's KDS uses for this product - also what
    /// `--product` on the CLI expects verbatim.
    pub fn kds_name(self) -> &'static str {
        match self {
            Product::Milan => "Milan",
            Product::Genoa => "Genoa",
            Product::Turin => "Turin",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "Milan" => Some(Product::Milan),
            "Genoa" => Some(Product::Genoa),
            "Turin" => Some(Product::Turin),
            _ => None,
        }
    }

    fn is_turin_generation(self) -> bool {
        matches!(self, Product::Turin)
    }
}

impl TcbVersion {
    /// The raw 8 bytes as the firmware's `TCB_VERSION` integer, as a derived
    /// key request carries it.
    pub fn as_u64(&self) -> u64 {
        u64::from_le_bytes(self.raw)
    }

    /// Whether every security patch level is at least `other`'s: firmware
    /// at `self` is no older than `other` in any part.
    pub fn at_least(&self, other: &TcbVersion) -> bool {
        self.fmc.unwrap_or(0) >= other.fmc.unwrap_or(0)
            && self.bootloader >= other.bootloader
            && self.tee >= other.tee
            && self.snp >= other.snp
            && self.microcode >= other.microcode
    }

    /// The patch levels as `bootloader,tee,snp,microcode` (with `fmc,`
    /// first on Turin and later).
    pub fn to_text(&self) -> String {
        let levels = format!(
            "{},{},{},{}",
            self.bootloader, self.tee, self.snp, self.microcode
        );
        match self.fmc {
            Some(fmc) => format!("fmc {fmc}, {levels}"),
            None => levels,
        }
    }

    /// Decodes an 8-byte `TCB_VERSION` per the product's own generation
    /// layout. Legacy (Milan/Genoa/Bergamo/Siena):
    /// `[bootloader, tee, _, _, _, _, snp, microcode]`. Turin+:
    /// `[fmc, bootloader, tee, snp, _, _, _, microcode]`.
    pub fn decode(raw: [u8; 8], product: Product) -> Self {
        if product.is_turin_generation() {
            TcbVersion {
                raw,
                fmc: Some(raw[0]),
                bootloader: raw[1],
                tee: raw[2],
                snp: raw[3],
                microcode: raw[7],
            }
        } else {
            TcbVersion {
                raw,
                fmc: None,
                bootloader: raw[0],
                tee: raw[1],
                snp: raw[6],
                microcode: raw[7],
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RawSignature {
    /// Little-endian in the report, 72 bytes, only the low 48 meaningful -
    /// see `crate::verify` for the reversal into a standard big-endian P-384
    /// scalar. Kept raw (not yet a `p384::ecdsa::Signature`) here since this
    /// module does no crypto.
    pub r_le: [u8; 72],
    pub s_le: [u8; 72],
}

/// A parsed `ATTESTATION_REPORT`. Only the fields this tool actually needs
/// are exposed named; everything else is available via `raw` for anything
/// this crate doesn't yet interpret.
#[derive(Debug, Clone)]
pub struct AttestationReport {
    pub raw: [u8; REPORT_LEN],
    pub version: u32,
    pub sig_algo: u32,
    pub current_tcb: TcbVersion,
    pub reported_tcb: TcbVersion,
    /// The TCB the platform has committed to (0x1E0): firmware older than
    /// it can no longer be loaded, and no derived key can be bound to a TCB
    /// above it.
    pub committed_tcb: TcbVersion,
    /// 64 bytes on Milan/Genoa/Bergamo/Siena; only the first 8 are
    /// meaningful on Turin+ (the rest zero) - see `crate::kds`'s hwID
    /// handling, which trims accordingly per AMD's own KDS behavior.
    pub chip_id: [u8; 64],
    /// The guest policy the VM was launched with (0x08).
    pub policy: u64,
    /// What the guest asked to be signed with the report (0x50): the nonce
    /// a verifier gave it, so an old report can't be passed off as fresh.
    pub report_data: [u8; 64],
    /// The launch measurement of the guest (0x90): which image is running.
    pub measurement: [u8; 48],
    /// The guest's security version (0x04), from its ID block: raised by a
    /// release that must not be rolled back from.
    pub guest_svn: u32,
    /// The privilege level the report was requested at (0x30).
    pub vmpl: u32,
    /// Report flags (0x48): bit 0 author key present, bit 1 chip key
    /// masked, bits 2-4 which key signed (0 = VCEK).
    pub flags: u32,
    /// SHA-384 of the public key that signed the guest's ID block (0xE0);
    /// all zero when the guest was launched without one.
    pub id_key_digest: [u8; 48],
    pub signature: RawSignature,
}

/// Which key signed a report (flags bits 2-4).
pub const SIGNING_KEY_VCEK: u32 = 0;

/// Guest policy bit 19: the hypervisor may debug the guest, reading and
/// writing its memory. A report from such a guest proves nothing about
/// what it keeps secret.
pub const POLICY_DEBUG: u64 = 1 << 19;

/// Guest policy bit 18: a migration agent may be associated with the guest,
/// which can export its memory. Key custody refuses such guests.
pub const POLICY_MIGRATE_MA: u64 = 1 << 18;

impl AttestationReport {
    /// Whether the guest's policy lets the hypervisor debug it.
    pub fn debug_allowed(&self) -> bool {
        self.policy & POLICY_DEBUG != 0
    }

    /// Which key signed the report: [`SIGNING_KEY_VCEK`] for the chip's own.
    pub fn signing_key(&self) -> u32 {
        (self.flags >> 2) & 0b111
    }

    /// Whether the guest was launched with an ID block (its digest is set).
    pub fn has_id_key(&self) -> bool {
        self.id_key_digest.iter().any(|b| *b != 0)
    }
}

pub fn parse(bytes: &[u8], product: Product) -> Result<AttestationReport, ReportParseError> {
    if bytes.len() != REPORT_LEN {
        return Err(ReportParseError::WrongLength { got: bytes.len() });
    }
    let mut raw = [0u8; REPORT_LEN];
    raw.copy_from_slice(bytes);

    let version = u32::from_le_bytes(raw[0x00..0x04].try_into().unwrap());
    let sig_algo = u32::from_le_bytes(raw[0x34..0x38].try_into().unwrap());
    if sig_algo != 1 {
        return Err(ReportParseError::UnsupportedSigAlgo(sig_algo));
    }

    let current_tcb_raw: [u8; 8] = raw[0x38..0x40].try_into().unwrap();
    let reported_tcb_raw: [u8; 8] = raw[0x180..0x188].try_into().unwrap();
    let committed_tcb_raw: [u8; 8] = raw[0x1E0..0x1E8].try_into().unwrap();
    let mut chip_id = [0u8; 64];
    chip_id.copy_from_slice(&raw[0x1A0..0x1E0]);
    let policy = u64::from_le_bytes(raw[0x08..0x10].try_into().unwrap());
    let mut report_data = [0u8; 64];
    report_data.copy_from_slice(&raw[0x50..0x90]);
    let mut measurement = [0u8; 48];
    measurement.copy_from_slice(&raw[0x90..0xC0]);
    let u32_at = |at: usize| u32::from_le_bytes(raw[at..at + 4].try_into().unwrap());
    let guest_svn = u32_at(0x04);
    let vmpl = u32_at(0x30);
    let flags = u32_at(0x48);
    let id_key_digest: [u8; 48] = raw[0xE0..0x110].try_into().unwrap();

    let mut r_le = [0u8; 72];
    let mut s_le = [0u8; 72];
    r_le.copy_from_slice(&raw[0x2A0..0x2A0 + 72]);
    s_le.copy_from_slice(&raw[0x2A0 + 72..0x2A0 + 144]);

    Ok(AttestationReport {
        raw,
        version,
        sig_algo,
        current_tcb: TcbVersion::decode(current_tcb_raw, product),
        reported_tcb: TcbVersion::decode(reported_tcb_raw, product),
        committed_tcb: TcbVersion::decode(committed_tcb_raw, product),
        chip_id,
        policy,
        report_data,
        measurement,
        guest_svn,
        vmpl,
        flags,
        id_key_digest,
        signature: RawSignature { r_le, s_le },
    })
}

/// The hwID KDS expects in the VCEK URL path - full 64-byte `chip_id` on
/// legacy products, first 8 bytes only on Turin+.
pub fn hw_id_for_product(product: Product, chip_id: &[u8; 64]) -> Vec<u8> {
    match product {
        Product::Turin => chip_id[..8].to_vec(),
        _ => chip_id.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a synthetic but correctly-shaped 1184-byte report buffer for
    /// pure offset/decode testing - never claims to be a genuine AMD-signed
    /// report (there is no key material here at all). Real signature
    /// verification against genuine AMD-issued material is exercised
    /// separately in `crate::verify`'s tests, using the real ARK/ASK
    /// certificate chains fetched live from `kdsintf.amd.com` and checked
    /// into `tests/fixtures/`.
    fn synthetic_report_bytes() -> [u8; REPORT_LEN] {
        let mut b = [0u8; REPORT_LEN];
        b[0x00..0x04].copy_from_slice(&2u32.to_le_bytes()); // version
        b[0x34..0x38].copy_from_slice(&1u32.to_le_bytes()); // sig_algo = ECDSA P-384
        b[0x38..0x40].copy_from_slice(&[9, 8, 0, 0, 0, 0, 7, 6]); // current_tcb (legacy layout)
        b[0x180..0x188].copy_from_slice(&[3, 2, 0, 0, 0, 0, 1, 200]); // reported_tcb
        for (i, byte) in b[0x1A0..0x1E0].iter_mut().enumerate() {
            *byte = i as u8; // chip_id, distinguishable pattern
        }
        b[0x2A0] = 0xAB; // first byte of r
        b[0x2A0 + 72] = 0xCD; // first byte of s
        b
    }

    /// The policy, report data and measurement come from their offsets,
    /// and the DEBUG bit is read from the policy.
    #[test]
    fn policy_report_data_and_measurement_are_read_from_their_offsets() {
        let mut bytes = synthetic_report_bytes();
        bytes[0x08..0x10].copy_from_slice(&(POLICY_DEBUG | 0x30000).to_le_bytes());
        bytes[0x50] = 0x11;
        bytes[0x8F] = 0x22;
        bytes[0x90] = 0x33;
        bytes[0xBF] = 0x44;
        let report = parse(&bytes, Product::Milan).unwrap();
        assert!(report.debug_allowed());
        assert_eq!(
            (report.report_data[0], report.report_data[63]),
            (0x11, 0x22)
        );
        assert_eq!(
            (report.measurement[0], report.measurement[47]),
            (0x33, 0x44)
        );
        bytes[0x08..0x10].copy_from_slice(&0x30000u64.to_le_bytes());
        assert!(!parse(&bytes, Product::Milan).unwrap().debug_allowed());
    }

    /// The ID-block fields key custody checks come from their offsets.
    #[test]
    fn identity_fields_are_read_from_their_offsets() {
        let mut bytes = synthetic_report_bytes();
        bytes[0x04..0x08].copy_from_slice(&7u32.to_le_bytes());
        bytes[0x30..0x34].copy_from_slice(&1u32.to_le_bytes());
        bytes[0x48..0x4C].copy_from_slice(&(1u32 << 2 | 1).to_le_bytes());
        bytes[0xE0] = 0xD1;
        bytes[0x10F] = 0xD2;
        let report = parse(&bytes, Product::Genoa).unwrap();
        assert_eq!(report.guest_svn, 7);
        assert_eq!(report.vmpl, 1);
        assert_eq!(report.signing_key(), 1, "bits 2-4");
        assert_eq!(
            (report.id_key_digest[0], report.id_key_digest[47]),
            (0xD1, 0xD2)
        );
        assert!(report.has_id_key());
        assert!(!parse(&synthetic_report_bytes(), Product::Genoa)
            .unwrap()
            .has_id_key());
    }

    #[test]
    fn parses_legacy_layout_offsets_correctly() {
        let bytes = synthetic_report_bytes();
        let report = parse(&bytes, Product::Milan).unwrap();
        assert_eq!(report.version, 2);
        assert_eq!(report.sig_algo, 1);
        assert_eq!(report.current_tcb.bootloader, 9);
        assert_eq!(report.current_tcb.tee, 8);
        assert_eq!(report.current_tcb.snp, 7);
        assert_eq!(report.current_tcb.microcode, 6);
        assert_eq!(report.current_tcb.fmc, None);
        assert_eq!(report.reported_tcb.bootloader, 3);
        assert_eq!(report.reported_tcb.tee, 2);
        assert_eq!(report.reported_tcb.snp, 1);
        assert_eq!(report.reported_tcb.microcode, 200);
        assert_eq!(report.chip_id[0], 0);
        assert_eq!(report.chip_id[1], 1);
        assert_eq!(report.chip_id[63], 63);
        assert_eq!(report.signature.r_le[0], 0xAB);
        assert_eq!(report.signature.s_le[0], 0xCD);
    }

    #[test]
    fn parses_turin_layout_offsets_correctly() {
        let mut bytes = synthetic_report_bytes();
        // Turin layout: [fmc, bootloader, tee, snp, _, _, _, microcode].
        bytes[0x180..0x188].copy_from_slice(&[5, 3, 2, 1, 0, 0, 0, 200]);
        let report = parse(&bytes, Product::Turin).unwrap();
        assert_eq!(report.reported_tcb.fmc, Some(5));
        assert_eq!(report.reported_tcb.bootloader, 3);
        assert_eq!(report.reported_tcb.tee, 2);
        assert_eq!(report.reported_tcb.snp, 1);
        assert_eq!(report.reported_tcb.microcode, 200);
    }

    #[test]
    fn rejects_wrong_length() {
        let err = parse(&[0u8; 100], Product::Milan).unwrap_err();
        assert!(matches!(err, ReportParseError::WrongLength { got: 100 }));
    }

    #[test]
    fn rejects_unsupported_sig_algo() {
        let mut bytes = synthetic_report_bytes();
        bytes[0x34..0x38].copy_from_slice(&99u32.to_le_bytes());
        let err = parse(&bytes, Product::Milan).unwrap_err();
        assert!(matches!(err, ReportParseError::UnsupportedSigAlgo(99)));
    }

    #[test]
    fn product_kds_name_and_parse_round_trip() {
        for p in [Product::Milan, Product::Genoa, Product::Turin] {
            assert_eq!(Product::parse(p.kds_name()), Some(p));
        }
        assert_eq!(Product::parse("Rome"), None);
    }
}
