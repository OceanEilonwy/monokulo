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
    /// Decodes an 8-byte `TCB_VERSION` per the product's own generation
    /// layout. Legacy (Milan/Genoa/Bergamo/Siena):
    /// `[bootloader, tee, _, _, _, _, snp, microcode]`. Turin+:
    /// `[fmc, bootloader, tee, snp, _, _, _, microcode]`.
    fn decode(raw: [u8; 8], product: Product) -> Self {
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
    /// 64 bytes on Milan/Genoa/Bergamo/Siena; only the first 8 are
    /// meaningful on Turin+ (the rest zero) - see `crate::kds`'s hwID
    /// handling, which trims accordingly per AMD's own KDS behavior.
    pub chip_id: [u8; 64],
    pub signature: RawSignature,
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
    let mut chip_id = [0u8; 64];
    chip_id.copy_from_slice(&raw[0x1A0..0x1E0]);

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
        chip_id,
        signature: RawSignature { r_le, s_le },
    })
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
