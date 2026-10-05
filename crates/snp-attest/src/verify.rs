//! The actual chain-of-trust and signature verification.
//!
//! Two genuinely different cryptographic systems are involved, and this
//! module is careful to never blur them:
//!
//! 1. **The AMD issuance chain** (ARK -> ASK -> VCEK): three X.509
//!    certificates, each signed by the previous with **RSASSA-PSS-SHA384**
//!    (48-byte salt) over a 4096-bit RSA key - confirmed directly by fetching
//!    AMD's real `cert_chain` response and reading it with
//!    `openssl x509 -text` rather than assumed to be ECDSA (a very easy, very
//!    wrong assumption to make given the report signature itself *is* ECDSA -
//!    see point 2). Verified here with the pure-Rust `rsa` crate over each
//!    certificate's signed bytes, so the verifier builds for WebAssembly too.
//! 2. **The attestation report's own signature**: the VCEK's public key
//!    (itself P-384 EC, even though the *certificate* that carries it was
//!    RSA-PSS-signed by the ASK) signs the report with plain
//!    **ECDSA P-384 / SHA-384** over the report's first 0x2A0 raw bytes -
//!    verified directly against `p384`/`ecdsa`, after undoing the report's
//!    little-endian r/s encoding (see `report::RawSignature`'s doc comment).
//!
//! [`verify_evidence`] does all of it offline, from certificates the caller
//! already has and a time the caller supplies: the browser and
//! `key-custody-cli` get them in a key custody bundle. [`verify`] (the `kds`
//! feature) fetches them from AMD first.

#[cfg(feature = "kds")]
use crate::kds;
use crate::pinned_ark;
use crate::report::{AttestationReport, Product, TcbVersion, SIGNING_KEY_VCEK};
use ecdsa::signature::Verifier;
use p384::ecdsa::{Signature as P384Signature, VerifyingKey as P384VerifyingKey};
use x509_parser::certificate::X509Certificate;
use x509_parser::pem::Pem;
use x509_parser::prelude::FromDer;
use x509_parser::revocation_list::CertificateRevocationList;
use x509_parser::time::ASN1Time;

#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[cfg(feature = "kds")]
    #[error("KDS request failed: {0}")]
    Kds(#[from] kds::KdsError),
    #[error("failed to parse a certificate: {0}")]
    CertParse(String),
    #[error("the live ASK+ARK chain's root does not match the pinned AMD root key for this product - refusing to trust it")]
    ArkMismatch,
    #[error("the ASK certificate's signature does not verify against the pinned ARK - chain is broken or forged")]
    AskNotSignedByArk,
    #[error("the VCEK certificate's signature does not verify against the live ASK - chain is broken or forged")]
    VcekNotSignedByAsk,
    #[error("the VCEK's TCB certificate extensions ({field}) don't match the report's reported_tcb - report may be tampered or mismatched with the wrong VCEK")]
    VcekTcbMismatch { field: &'static str },
    #[error("the VCEK's hwID certificate extension doesn't match the report's chip_id - the VCEK belongs to another chip")]
    VcekHwIdMismatch,
    #[error(
        "the attestation report's own ECDSA signature does not verify against the VCEK public key"
    )]
    ReportSignatureInvalid,
    #[error("VCEK's public key is not a valid P-384 EC point")]
    InvalidVcekKey,
    #[error("the {which} certificate is outside its validity period")]
    CertExpired { which: &'static str },
    #[error("the guest's policy allows the hypervisor to debug it (policy bit 19): its memory is not protected, whatever the report says")]
    DebugAllowed,
    #[error("the report was not signed by the chip's own key (VCEK)")]
    NotSignedByVcek,
    #[error("AMD's revocation list does not verify against the pinned ARK - refusing to trust it")]
    CrlNotSignedByArk,
    #[error("AMD's revocation list is out of date (or not yet in force): an old list could hide a revocation")]
    CrlStale,
    #[error("AMD has revoked the {which} certificate: its key is not to be trusted")]
    Revoked { which: &'static str },
}

/// Outcome of a full verification run - every check this tool performed, so
/// the CLI (or a caller embedding this crate) can report exactly what was
/// and wasn't established, not just a single pass/fail bit.
#[derive(Debug)]
pub struct VerifiedReport {
    pub reported_tcb: TcbVersion,
    pub current_tcb: TcbVersion,
    /// Always true on `Ok` - kept as a field (rather than the function simply
    /// returning `Ok(())`) so future additional non-fatal observations can be
    /// attached here without changing the error type.
    pub chain_verified: bool,
}

/// The certificates a report is checked against, apart from the pinned
/// root: AMD's intermediate (ASK), the chip's own key (VCEK, issued for the
/// report's exact TCB) and AMD's revocation list, all DER. Untrusted until
/// checked: each must verify up to the pinned ARK.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    pub ask_der: Vec<u8>,
    pub vcek_der: Vec<u8>,
    pub crl_der: Vec<u8>,
}

fn parse_der_cert(der: &[u8]) -> Result<X509Certificate<'_>, VerifyError> {
    let (_, cert) =
        X509Certificate::from_der(der).map_err(|e| VerifyError::CertParse(e.to_string()))?;
    Ok(cert)
}

/// Splits a "ASK then ARK" concatenated PEM (AMD KDS's `cert_chain`
/// response format) into its two `Pem` entries.
pub(crate) fn parse_pem_chain(pem_str: &str) -> Result<Vec<Pem>, VerifyError> {
    let mut certs = Vec::new();
    let mut input = pem_str.as_bytes();
    while !input.trim_ascii().is_empty() {
        let (rest, pem) = x509_parser::pem::parse_x509_pem(input)
            .map_err(|e| VerifyError::CertParse(e.to_string()))?;
        certs.push(pem);
        input = rest;
    }
    Ok(certs)
}

/// The pinned ARK for `product`, as DER.
fn pinned_ark_der(product: Product) -> Result<Vec<u8>, VerifyError> {
    parse_pem_chain(pinned_ark::pinned_ark_pem(product))?
        .into_iter()
        .next()
        .map(|pem| pem.contents)
        .ok_or_else(|| VerifyError::CertParse("pinned ARK PEM is empty".into()))
}

/// Whether `signature` over `signed` verifies with `issuer`'s RSA key under
/// RSASSA-PSS-SHA384 with a 48-byte salt, the only scheme AMD's chain uses.
fn rsa_pss_verifies(signed: &[u8], signature: &[u8], issuer: &X509Certificate<'_>) -> bool {
    use rsa::pkcs1::DecodeRsaPublicKey as _;
    let Ok(key) = rsa::RsaPublicKey::from_pkcs1_der(&issuer.public_key().subject_public_key.data)
    else {
        return false;
    };
    let Ok(signature) = rsa::pss::Signature::try_from(signature) else {
        return false;
    };
    rsa::signature::Verifier::verify(
        &rsa::pss::VerifyingKey::<sha2_rsa::Sha384>::new(key),
        signed,
        &signature,
    )
    .is_ok()
}

/// Whether `cert` was signed by `issuer`.
fn signed_by(cert: &X509Certificate<'_>, issuer: &X509Certificate<'_>) -> bool {
    rsa_pss_verifies(
        cert.tbs_certificate.as_ref(),
        &cert.signature_value.data,
        issuer,
    )
}

/// `now_unix` (seconds since the epoch) as a certificate time.
fn asn1_time(now_unix: i64) -> Result<ASN1Time, VerifyError> {
    ASN1Time::from_timestamp(now_unix).map_err(|e| VerifyError::CertParse(e.to_string()))
}

/// Full verification, offline: `report` was signed by a genuine AMD chip,
/// whose key (`evidence.vcek_der`) AMD issued through `evidence.ask_der`
/// under the pinned root for `product`, at `now_unix`, with nothing on
/// AMD's revocation list, from a guest the hypervisor can't debug.
///
/// Does **not** check any minimum patch level or the guest's identity
/// (measurement, ID key, SVN): those are the caller's policy, applied to the
/// report once this has established it is genuine.
pub fn verify_evidence(
    product: Product,
    report: &AttestationReport,
    evidence: &Evidence,
    now_unix: i64,
) -> Result<VerifiedReport, VerifyError> {
    let now = asn1_time(now_unix)?;
    let ark_der = pinned_ark_der(product)?;
    let ark = parse_der_cert(&ark_der)?;
    let ask = parse_der_cert(&evidence.ask_der)?;
    let vcek = parse_der_cert(&evidence.vcek_der)?;

    // Each certificate must be in its validity period: a signature that
    // checks out says nothing about a certificate that has expired.
    for (which, cert) in [("ARK", &ark), ("ASK", &ask), ("VCEK", &vcek)] {
        if !cert.validity().is_valid_at(now) {
            return Err(VerifyError::CertExpired { which });
        }
    }

    // AMD signs the ASK and every VCEK with RSASSA-PSS: anything else is
    // not AMD's chain, whatever else checks out.
    require_pss("ASK", &ask)?;
    require_pss("VCEK", &vcek)?;

    // The ASK must be signed by the pinned ARK, the VCEK by the ASK.
    if !signed_by(&ask, &ark) {
        return Err(VerifyError::AskNotSignedByArk);
    }
    if !signed_by(&vcek, &ask) {
        return Err(VerifyError::VcekNotSignedByAsk);
    }

    // And neither may be on AMD's revocation list: a chip whose key was
    // revoked still signs reports that check out against the chain.
    check_not_revoked(
        &evidence.crl_der,
        &ark,
        &[("ASK", &ask), ("VCEK", &vcek)],
        now,
    )?;

    // A VCEK is issued bound to one TCB tuple and one chip: this catches a
    // report paired with the wrong VCEK (a stale one, or another chip's).
    verify_vcek_tcb_extensions(&vcek, &report.reported_tcb)?;
    verify_vcek_hw_id(&vcek, product, &report.chip_id)?;

    // The report's own ECDSA P-384 signature, by the VCEK.
    if report.signing_key() != SIGNING_KEY_VCEK {
        return Err(VerifyError::NotSignedByVcek);
    }
    verify_report_signature(report, &vcek)?;

    // A guest the hypervisor may debug has no confidentiality to attest.
    if report.debug_allowed() {
        return Err(VerifyError::DebugAllowed);
    }

    Ok(VerifiedReport {
        reported_tcb: report.reported_tcb,
        current_tcb: report.current_tcb,
        chain_verified: true,
    })
}

/// Fetches the evidence for `report` from AMD's KDS: the ASK (after checking
/// the chain's root is the pinned ARK), the VCEK for the report's chip and
/// reported TCB, and the revocation list.
#[cfg(feature = "kds")]
pub async fn fetch_evidence(
    client: &reqwest::Client,
    product: Product,
    report: &AttestationReport,
) -> Result<Evidence, VerifyError> {
    let chain_pem = kds::fetch_cert_chain_pem(client, product).await?;
    let mut chain = parse_pem_chain(&chain_pem)?;
    if chain.len() != 2 {
        return Err(VerifyError::CertParse(format!(
            "expected 2 certs in AMD's cert_chain response, got {}",
            chain.len()
        )));
    }
    let live_ark = chain.pop().map(|pem| pem.contents).unwrap_or_default();
    if live_ark != pinned_ark_der(product)? {
        return Err(VerifyError::ArkMismatch);
    }
    let ask_der = chain.pop().map(|pem| pem.contents).unwrap_or_default();
    let vcek_der = kds::fetch_vcek_der(client, product, report).await?;
    let crl_der = kds::fetch_crl_der(client, product).await?;
    Ok(Evidence {
        ask_der,
        vcek_der,
        crl_der,
    })
}

/// Seconds since the Unix epoch, now.
#[cfg(feature = "kds")]
pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// [`fetch_evidence`], then [`verify_evidence`] at the current time. Does
/// **not** check any minimum patch-level threshold - that's a separate,
/// deliberately explicit step in the CLI (see `main.rs`), since "what SPL
/// corresponds to AMD-SB-3019" is operator-supplied configuration, not
/// something this crate hardcodes as if it were a settled fact - see that
/// binary's own `--min-*-spl` flag documentation for why.
#[cfg(feature = "kds")]
pub async fn verify(
    client: &reqwest::Client,
    product: Product,
    report: &AttestationReport,
) -> Result<VerifiedReport, VerifyError> {
    let evidence = fetch_evidence(client, product, report).await?;
    verify_evidence(product, report, &evidence, unix_now())
}

/// Refuses `certs` if the revocation list `crl_der`, which must be signed
/// by `ark` and in force at `now`, lists any of them.
fn check_not_revoked(
    crl_der: &[u8],
    ark: &X509Certificate<'_>,
    certs: &[(&'static str, &X509Certificate<'_>)],
    now: ASN1Time,
) -> Result<(), VerifyError> {
    let (_, crl) = CertificateRevocationList::from_der(crl_der)
        .map_err(|e| VerifyError::CertParse(e.to_string()))?;
    if crl.signature_algorithm.algorithm != x509_parser::oid_registry::OID_PKCS1_RSASSAPSS
        || !rsa_pss_verifies(crl.tbs_cert_list.as_ref(), &crl.signature_value.data, ark)
    {
        return Err(VerifyError::CrlNotSignedByArk);
    }
    // A list past its next update could be an old one replayed from before
    // a revocation.
    if crl.last_update() > now || crl.next_update().is_some_and(|next| next < now) {
        return Err(VerifyError::CrlStale);
    }
    for (which, cert) in certs {
        if crl
            .iter_revoked_certificates()
            .any(|revoked| revoked.serial() == &cert.tbs_certificate.serial)
        {
            return Err(VerifyError::Revoked { which });
        }
    }
    Ok(())
}

/// Refuses a certificate not signed with RSASSA-PSS.
fn require_pss(which: &str, cert: &X509Certificate<'_>) -> Result<(), VerifyError> {
    if cert.signature_algorithm.algorithm == x509_parser::oid_registry::OID_PKCS1_RSASSAPSS {
        Ok(())
    } else {
        Err(VerifyError::CertParse(format!(
            "the {which} certificate is not signed with RSASSA-PSS"
        )))
    }
}

/// OIDs confirmed against `google/go-sev-guest`'s KDS extension parsing
/// (independent of the `virtee/sev` and `virtee/snpguest` sources this
/// crate's other modules lean on - a second, unrelated implementation
/// agreeing on these OIDs is worth more than one source alone here, since a
/// wrong OID would make this check silently pass on any cert, not fail
/// loudly).
mod oid {
    use x509_parser::oid_registry::Oid;

    pub fn bl_spl() -> Oid<'static> {
        Oid::from(&[1, 3, 6, 1, 4, 1, 3704, 1, 3, 1]).unwrap()
    }
    pub fn tee_spl() -> Oid<'static> {
        Oid::from(&[1, 3, 6, 1, 4, 1, 3704, 1, 3, 2]).unwrap()
    }
    pub fn snp_spl() -> Oid<'static> {
        Oid::from(&[1, 3, 6, 1, 4, 1, 3704, 1, 3, 3]).unwrap()
    }
    pub fn ucode_spl() -> Oid<'static> {
        Oid::from(&[1, 3, 6, 1, 4, 1, 3704, 1, 3, 8]).unwrap()
    }
    pub fn hw_id() -> Oid<'static> {
        Oid::from(&[1, 3, 6, 1, 4, 1, 3704, 1, 4]).unwrap()
    }
}

/// Extracts a DER `INTEGER`'s value as a `u8` from an extension's raw value
/// bytes. AMD encodes each SPL as a plain ASN.1 INTEGER (tag 0x02); this
/// deliberately only handles the short single/double-byte form these small
/// (0-255) values always take, and errors otherwise rather than guessing.
fn read_der_small_integer(der: &[u8]) -> Option<u8> {
    // Tag(1) + Length(1, short form only) + Value(len bytes).
    if der.len() < 3 || der[0] != 0x02 {
        return None;
    }
    let len = der[1] as usize;
    let value = der.get(2..2 + len)?;
    // A leading 0x00 pad byte appears when the high bit of the next byte
    // would otherwise be misread as a sign bit - still represents the same
    // small unsigned value.
    match value {
        [v] => Some(*v),
        [0x00, v] => Some(*v),
        _ => None,
    }
}

fn verify_vcek_tcb_extensions(
    vcek_cert: &X509Certificate<'_>,
    reported_tcb: &TcbVersion,
) -> Result<(), VerifyError> {
    let checks: [(x509_parser::oid_registry::Oid<'static>, &'static str, u8); 4] = [
        (oid::bl_spl(), "blSPL", reported_tcb.bootloader),
        (oid::tee_spl(), "teeSPL", reported_tcb.tee),
        (oid::snp_spl(), "snpSPL", reported_tcb.snp),
        (oid::ucode_spl(), "ucodeSPL", reported_tcb.microcode),
    ];

    for (oid, name, expected) in checks {
        let ext = vcek_cert
            .get_extension_unique(&oid)
            .map_err(|e| VerifyError::CertParse(e.to_string()))?
            .ok_or(VerifyError::VcekTcbMismatch { field: name })?;
        let got = read_der_small_integer(ext.value)
            .ok_or(VerifyError::VcekTcbMismatch { field: name })?;
        if got != expected {
            return Err(VerifyError::VcekTcbMismatch { field: name });
        }
    }
    Ok(())
}

/// Refuses a VCEK whose hwID extension names a chip other than `chip_id`.
fn verify_vcek_hw_id(
    vcek_cert: &X509Certificate<'_>,
    product: Product,
    chip_id: &[u8; 64],
) -> Result<(), VerifyError> {
    let ext = vcek_cert
        .get_extension_unique(&oid::hw_id())
        .map_err(|e| VerifyError::CertParse(e.to_string()))?
        .ok_or(VerifyError::VcekHwIdMismatch)?;
    if hw_id_matches(ext.value, product, chip_id) {
        Ok(())
    } else {
        Err(VerifyError::VcekHwIdMismatch)
    }
}

/// Whether a hwID extension value (a DER `OCTET STRING`) names the chip
/// `chip_id`. The extension holds the chip ID in the form KDS indexes the
/// product's VCEKs by (`report::hw_id_for_product`: 8 bytes on Turin, all 64
/// elsewhere); the full 64 bytes are accepted too, since they name the
/// same chip.
fn hw_id_matches(der: &[u8], product: Product, chip_id: &[u8; 64]) -> bool {
    // Tag(1) + Length(1, short form: hwIDs are at most 64 bytes) + Value.
    let value = match der {
        [0x04, len, value @ ..] if usize::from(*len) == value.len() && *len < 0x80 => value,
        _ => return false,
    };
    value == crate::report::hw_id_for_product(product, chip_id).as_slice()
        || value == chip_id.as_slice()
}

fn verify_report_signature(
    report: &AttestationReport,
    vcek_cert: &X509Certificate<'_>,
) -> Result<(), VerifyError> {
    let spki_bytes = vcek_cert.public_key().subject_public_key.as_ref();
    let verifying_key =
        P384VerifyingKey::from_sec1_bytes(spki_bytes).map_err(|_| VerifyError::InvalidVcekKey)?;
    verify_report_signed_by(report, &verifying_key)
}

/// The report's own signature against a VCEK's key, for a caller that
/// already trusts that key (and for tests, with a key of their own).
pub fn verify_report_signed_by(
    report: &AttestationReport,
    verifying_key: &P384VerifyingKey,
) -> Result<(), VerifyError> {
    // The report stores r/s little-endian, 72 bytes each with only the low
    // 48 meaningful (P-384 scalars are 48 bytes) - reverse to the big-endian
    // form a standard ECDSA signature needs. See `report::RawSignature`'s
    // doc comment; confirmed directly against `virtee/sev`'s own P-384
    // conversion code, not assumed.
    // Bytes 48..72 are padding and must be zero: a report with anything
    // there is not one the firmware produced.
    if report.signature.r_le[48..].iter().any(|b| *b != 0)
        || report.signature.s_le[48..].iter().any(|b| *b != 0)
    {
        return Err(VerifyError::ReportSignatureInvalid);
    }
    let mut r_be = [0u8; 48];
    let mut s_be = [0u8; 48];
    for (be, le) in r_be
        .iter_mut()
        .zip(report.signature.r_le[..48].iter().rev())
    {
        *be = *le;
    }
    for (be, le) in s_be
        .iter_mut()
        .zip(report.signature.s_le[..48].iter().rev())
    {
        *be = *le;
    }
    let signature =
        P384Signature::from_scalars(r_be, s_be).map_err(|_| VerifyError::ReportSignatureInvalid)?;

    let signed_bytes = &report.raw[..crate::report::SIGNED_LEN];
    verifying_key
        .verify(signed_bytes, &signature)
        .map_err(|_| VerifyError::ReportSignatureInvalid)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-10-02, inside every test certificate's validity period and
    /// every fixture revocation list's.
    const NOW: i64 = 1_790_899_200;

    fn at(unix: i64) -> ASN1Time {
        ASN1Time::from_timestamp(unix).unwrap()
    }

    /// A revocation list past its next update, or not yet issued, is
    /// refused: an old list could hide a revocation.
    #[test]
    fn an_out_of_date_revocation_list_is_refused() {
        let chain =
            parse_pem_chain(include_str!("../tests/fixtures/genoa_ask_ark_chain.pem")).unwrap();
        let ask = chain[0].parse_x509().unwrap();
        let pinned = parse_pem_chain(pinned_ark::pinned_ark_pem(Product::Genoa)).unwrap();
        let ark = pinned[0].parse_x509().unwrap();
        let crl = include_bytes!("../tests/fixtures/genoa_crl.der");
        // 2026-12-01, after its next update (2026-11-09).
        assert!(matches!(
            check_not_revoked(crl, &ark, &[("ASK", &ask)], at(1_796_083_200)),
            Err(VerifyError::CrlStale)
        ));
        // 2026-09-01, before it was issued (2026-09-22).
        assert!(matches!(
            check_not_revoked(crl, &ark, &[("ASK", &ask)], at(1_788_220_800)),
            Err(VerifyError::CrlStale)
        ));
    }
    use p384::ecdsa::{Signature as SigT, SigningKey};

    /// Proves the ASK-by-ARK link of the real AMD chain verifies correctly
    /// against genuine, live-fetched AMD material (`tests/fixtures/*.pem`,
    /// fetched directly from `kdsintf.amd.com` - see that directory's own
    /// provenance note). This is real, not synthetic: if AMD's RSA-PSS
    /// issuance chain didn't verify the way this module assumes, this test
    /// would fail against real bytes, not a mock.
    #[test]
    fn real_ask_verifies_against_pinned_ark_for_every_product() {
        for (product, fixture) in [
            (
                Product::Milan,
                include_str!("../tests/fixtures/milan_ask_ark_chain.pem"),
            ),
            (
                Product::Genoa,
                include_str!("../tests/fixtures/genoa_ask_ark_chain.pem"),
            ),
            (
                Product::Turin,
                include_str!("../tests/fixtures/turin_ask_ark_chain.pem"),
            ),
        ] {
            let parsed = parse_pem_chain(fixture).unwrap();
            assert_eq!(parsed.len(), 2, "{product:?}: expected ASK + ARK");
            let ask_cert = parsed[0].parse_x509().unwrap();

            let pinned = pinned_ark::pinned_ark_pem(product);
            let pinned_parsed = parse_pem_chain(pinned).unwrap();
            let pinned_ark_cert = pinned_parsed[0].parse_x509().unwrap();

            assert!(
                signed_by(&ask_cert, &pinned_ark_cert),
                "{product:?}: real ASK must verify against pinned ARK"
            );
        }
    }

    /// AMD's real revocation lists (fetched from KDS, see
    /// `tests/fixtures/test_crl/README.md`) verify against each product's
    /// pinned ARK and don't list its real ASK; one product's list is refused
    /// under another's root.
    #[test]
    fn real_crls_verify_against_the_pinned_ark_and_list_no_ask() {
        for (product, crl, chain) in [
            (
                Product::Milan,
                &include_bytes!("../tests/fixtures/milan_crl.der")[..],
                include_str!("../tests/fixtures/milan_ask_ark_chain.pem"),
            ),
            (
                Product::Genoa,
                &include_bytes!("../tests/fixtures/genoa_crl.der")[..],
                include_str!("../tests/fixtures/genoa_ask_ark_chain.pem"),
            ),
            (
                Product::Turin,
                &include_bytes!("../tests/fixtures/turin_crl.der")[..],
                include_str!("../tests/fixtures/turin_ask_ark_chain.pem"),
            ),
        ] {
            let chain = parse_pem_chain(chain).unwrap();
            let ask = chain[0].parse_x509().unwrap();
            let pinned = parse_pem_chain(pinned_ark::pinned_ark_pem(product)).unwrap();
            let ark = pinned[0].parse_x509().unwrap();
            check_not_revoked(crl, &ark, &[("ASK", &ask)], at(NOW))
                .unwrap_or_else(|e| panic!("{product:?}: {e}"));
        }

        let genoa = parse_pem_chain(pinned_ark::pinned_ark_pem(Product::Genoa)).unwrap();
        assert!(matches!(
            check_not_revoked(
                include_bytes!("../tests/fixtures/milan_crl.der"),
                &genoa[0].parse_x509().unwrap(),
                &[],
                at(NOW),
            ),
            Err(VerifyError::CrlNotSignedByArk)
        ));
    }

    /// A certificate the list names is refused, by name; one it doesn't is
    /// not. Test material of our own (AMD's lists are empty so far).
    #[test]
    fn a_certificate_on_the_revocation_list_is_refused() {
        let crl = include_bytes!("../tests/fixtures/test_crl/crl.der");
        let ca = parse_der_cert(include_bytes!("../tests/fixtures/test_crl/ca.der")).unwrap();
        let revoked =
            parse_der_cert(include_bytes!("../tests/fixtures/test_crl/revoked.der")).unwrap();
        let good = parse_der_cert(include_bytes!("../tests/fixtures/test_crl/good.der")).unwrap();

        assert!(check_not_revoked(crl, &ca, &[("VCEK", &good)], at(NOW)).is_ok());
        assert!(matches!(
            check_not_revoked(crl, &ca, &[("ASK", &good), ("VCEK", &revoked)], at(NOW)),
            Err(VerifyError::Revoked { which: "VCEK" })
        ));
        // Signed by someone else: refused before its contents count.
        let milan = parse_pem_chain(pinned_ark::pinned_ark_pem(Product::Milan)).unwrap();
        assert!(matches!(
            check_not_revoked(
                crl,
                &milan[0].parse_x509().unwrap(),
                &[("VCEK", &good)],
                at(NOW)
            ),
            Err(VerifyError::CrlNotSignedByArk)
        ));
    }

    #[test]
    fn ark_self_signature_verifies_for_every_product() {
        // The root's own signature, over itself - proves it really is
        // self-signed (issuer == subject alone doesn't prove that; only a
        // verifying signature does).
        for product in [Product::Milan, Product::Genoa, Product::Turin] {
            let pinned = pinned_ark::pinned_ark_pem(product);
            let parsed = parse_pem_chain(pinned).unwrap();
            let ark_cert = parsed[0].parse_x509().unwrap();
            assert!(
                signed_by(&ark_cert, &ark_cert),
                "{product:?}: ARK must be self-signed"
            );
        }
    }

    #[test]
    fn wrong_root_is_rejected() {
        // Milan's ASK must NOT verify against Genoa's ARK - a real negative
        // case, not just the absence of a positive one.
        let milan_chain =
            parse_pem_chain(include_str!("../tests/fixtures/milan_ask_ark_chain.pem")).unwrap();
        let milan_ask = milan_chain[0].parse_x509().unwrap();

        let genoa_ark_pem = pinned_ark::pinned_ark_pem(Product::Genoa);
        let genoa_ark_certs = parse_pem_chain(genoa_ark_pem).unwrap();
        let genoa_ark = genoa_ark_certs[0].parse_x509().unwrap();

        assert!(!signed_by(&milan_ask, &genoa_ark));
    }

    /// The report's own ECDSA P-384 signature path, exercised against a
    /// freshly generated (non-AMD) P-384 key pair - proves the byte-order
    /// reversal and scalar handling are correct in isolation. Real
    /// end-to-end proof that a genuine AMD VCEK's key verifies a genuine
    /// report requires a report actually captured from SEV-SNP hardware,
    /// which this environment doesn't have - documented as a real, open gap
    /// in `docs/DEPLOY_SEV_SNP.md`, not silently assumed covered by this
    /// test.
    #[test]
    fn report_signature_round_trips_through_the_little_endian_encoding() {
        #[allow(deprecated)]
        let signing_key = SigningKey::random(&mut rand::rng());
        let mut raw = [0u8; crate::report::REPORT_LEN];
        // Fill the signed region with distinguishable, non-zero content so a
        // signature-over-wrong-bytes bug would actually be caught.
        for (i, b) in raw[..crate::report::SIGNED_LEN].iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        raw[0x34..0x38].copy_from_slice(&1u32.to_le_bytes());

        let sig: SigT =
            ecdsa::signature::Signer::sign(&signing_key, &raw[..crate::report::SIGNED_LEN]);
        let (r, s) = (sig.r().to_bytes(), sig.s().to_bytes());

        // Encode into the report's own little-endian, 72-byte-padded form.
        for i in 0..48 {
            raw[0x2A0 + i] = r[47 - i];
            raw[0x2A0 + 72 + i] = s[47 - i];
        }

        // Through the real parser and the real decoder.
        let report = crate::report::parse(&raw, Product::Milan).unwrap();
        verify_report_signed_by(&report, signing_key.verifying_key())
            .expect("round-tripped signature must verify");
    }

    /// Evidence built from AMD's real Genoa ASK and revocation list, with
    /// `vcek_der` as the chip key.
    fn genoa_evidence(vcek_der: &[u8]) -> Evidence {
        let chain =
            parse_pem_chain(include_str!("../tests/fixtures/genoa_ask_ark_chain.pem")).unwrap();
        Evidence {
            ask_der: chain[0].contents.clone(),
            vcek_der: vcek_der.to_vec(),
            crl_der: include_bytes!("../tests/fixtures/genoa_crl.der").to_vec(),
        }
    }

    fn any_report() -> AttestationReport {
        let mut raw = [0u8; crate::report::REPORT_LEN];
        raw[0x34..0x38].copy_from_slice(&1u32.to_le_bytes());
        crate::report::parse(&raw, Product::Genoa).unwrap()
    }

    /// The offline pipeline refuses a chip key AMD's ASK didn't issue, a
    /// chain under another product's root, and certificates out of date,
    /// each by name.
    #[test]
    fn evidence_that_does_not_lead_to_the_pinned_root_is_refused() {
        let report = any_report();
        let not_amds = include_bytes!("../tests/fixtures/test_crl/good.der");
        assert!(matches!(
            verify_evidence(Product::Genoa, &report, &genoa_evidence(not_amds), NOW),
            Err(VerifyError::VcekNotSignedByAsk)
        ));
        assert!(matches!(
            verify_evidence(Product::Milan, &report, &genoa_evidence(not_amds), NOW),
            Err(VerifyError::AskNotSignedByArk)
        ));
        // 2100: the roots have expired by then.
        assert!(matches!(
            verify_evidence(
                Product::Genoa,
                &report,
                &genoa_evidence(not_amds),
                4_102_444_800
            ),
            Err(VerifyError::CertExpired { .. })
        ));
        let mut garbage = genoa_evidence(not_amds);
        garbage.ask_der = vec![0x30, 0x00];
        assert!(matches!(
            verify_evidence(Product::Genoa, &report, &garbage, NOW),
            Err(VerifyError::CertParse(_))
        ));
    }

    #[test]
    fn the_vcek_hw_id_must_name_the_reports_chip() {
        let chip_id: [u8; 64] = std::array::from_fn(|i| i as u8 + 1);
        let octets = |bytes: &[u8]| {
            let mut der = vec![0x04, bytes.len() as u8];
            der.extend_from_slice(bytes);
            der
        };

        assert!(hw_id_matches(&octets(&chip_id), Product::Milan, &chip_id));
        assert!(hw_id_matches(&octets(&chip_id), Product::Genoa, &chip_id));
        assert!(hw_id_matches(
            &octets(&chip_id[..8]),
            Product::Turin,
            &chip_id
        ));
        assert!(hw_id_matches(&octets(&chip_id), Product::Turin, &chip_id));

        let mut other_chip = chip_id;
        other_chip[0] ^= 0xff;
        assert!(!hw_id_matches(
            &octets(&other_chip),
            Product::Milan,
            &chip_id
        ));
        assert!(!hw_id_matches(
            &octets(&other_chip[..8]),
            Product::Turin,
            &chip_id
        ));
        assert!(
            !hw_id_matches(&octets(&chip_id[..8]), Product::Milan, &chip_id),
            "outside Turin, KDS names a chip by all 64 bytes"
        );
        assert!(!hw_id_matches(&[], Product::Milan, &chip_id));
        assert!(
            !hw_id_matches(&[0x02, 1, 0], Product::Milan, &chip_id),
            "not an OCTET STRING"
        );
        let mut truncated = octets(&chip_id);
        truncated.pop();
        assert!(!hw_id_matches(&truncated, Product::Milan, &chip_id));
    }

    #[test]
    fn tampered_signed_region_is_rejected() {
        #[allow(deprecated)]
        let signing_key = SigningKey::random(&mut rand::rng());
        let mut raw = [0u8; crate::report::REPORT_LEN];
        for (i, b) in raw[..crate::report::SIGNED_LEN].iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        raw[0x34..0x38].copy_from_slice(&1u32.to_le_bytes()); // sig_algo, for the parser
        let sig: SigT =
            ecdsa::signature::Signer::sign(&signing_key, &raw[..crate::report::SIGNED_LEN]);
        let (r, s) = (sig.r().to_bytes(), sig.s().to_bytes());
        for i in 0..48 {
            raw[0x2A0 + i] = r[47 - i];
            raw[0x2A0 + 72 + i] = s[47 - i];
        }

        // Tamper with one byte inside the signed region after signing.
        raw[0x10] ^= 0xFF;

        let report = crate::report::parse(&raw, Product::Milan).unwrap();
        assert!(matches!(
            verify_report_signed_by(&report, signing_key.verifying_key()),
            Err(VerifyError::ReportSignatureInvalid)
        ));
    }
}
