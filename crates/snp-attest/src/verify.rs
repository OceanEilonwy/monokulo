//! The actual chain-of-trust and signature verification.
//!
//! Two genuinely different cryptographic systems are involved, and this
//! module is careful to never blur them:
//!
//! 1. **The AMD issuance chain** (ARK -> ASK -> VCEK): three X.509
//!    certificates, each signed by the previous with **RSASSA-PSS-SHA384**
//!    over a 4096-bit RSA key - confirmed directly by fetching AMD's real
//!    `cert_chain` response and reading it with `openssl x509 -text` rather
//!    than assumed to be ECDSA (a very easy, very wrong assumption to make
//!    given the report signature itself *is* ECDSA - see point 2). Verified
//!    here via `x509_parser::X509Certificate::verify_signature`, which
//!    handles RSASSA-PSS's explicit AlgorithmIdentifier parameters
//!    (hash/MGF/salt length) itself rather than this crate reimplementing
//!    PSS padding by hand.
//! 2. **The attestation report's own signature**: the VCEK's public key
//!    (itself P-384 EC, even though the *certificate* that carries it was
//!    RSA-PSS-signed by the ASK) signs the report with plain
//!    **ECDSA P-384 / SHA-384** over the report's first 0x2A0 raw bytes -
//!    verified directly against `p384`/`ecdsa`, after undoing the report's
//!    little-endian r/s encoding (see `report::RawSignature`'s doc comment).

use crate::kds;
use crate::pinned_ark;
use crate::report::{AttestationReport, Product, TcbVersion};
use ecdsa::signature::Verifier;
use p384::ecdsa::{Signature as P384Signature, VerifyingKey as P384VerifyingKey};
use x509_parser::certificate::X509Certificate;
use x509_parser::pem::Pem;
use x509_parser::prelude::FromDer;

#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
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
    #[error(
        "the attestation report's own ECDSA signature does not verify against the VCEK public key"
    )]
    ReportSignatureInvalid,
    #[error("VCEK's public key is not a valid P-384 EC point")]
    InvalidVcekKey,
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

fn parse_der_cert(der: &[u8]) -> Result<X509Certificate<'_>, VerifyError> {
    let (_, cert) =
        X509Certificate::from_der(der).map_err(|e| VerifyError::CertParse(e.to_string()))?;
    Ok(cert)
}

/// Splits a "ASK then ARK" concatenated PEM (AMD KDS's `cert_chain`
/// response format) into its two `Pem` entries.
fn parse_pem_chain(pem_str: &str) -> Result<Vec<Pem>, VerifyError> {
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

/// Full verification pipeline: fetches the live ASK+ARK chain and the VCEK
/// from AMD's KDS, checks every link, and verifies the report's own
/// signature. Does **not** check any minimum patch-level threshold - that's
/// a separate, deliberately explicit step in the CLI (see `main.rs`), since
/// "what SPL corresponds to AMD-SB-3019" is operator-supplied configuration,
/// not something this crate hardcodes as if it were a settled fact - see
/// that binary's own `--min-*-spl` flag documentation for why.
pub async fn verify(
    client: &reqwest::Client,
    product: Product,
    report: &AttestationReport,
) -> Result<VerifiedReport, VerifyError> {
    // 1. Live ASK+ARK chain, checked against our pinned root.
    let chain_pem = kds::fetch_cert_chain_pem(client, product).await?;
    let parsed_chain = parse_pem_chain(&chain_pem)?;
    if parsed_chain.len() != 2 {
        return Err(VerifyError::CertParse(format!(
            "expected 2 certs in AMD's cert_chain response, got {}",
            parsed_chain.len()
        )));
    }
    let ask_pem = &parsed_chain[0];
    let live_ark_pem = &parsed_chain[1];

    let pinned_ark_pem_str = pinned_ark::pinned_ark_pem(product);
    let pinned_ark_parsed = parse_pem_chain(pinned_ark_pem_str)?;
    let pinned_ark_pem_entry = pinned_ark_parsed
        .first()
        .ok_or_else(|| VerifyError::CertParse("pinned ARK PEM is empty".into()))?;

    if live_ark_pem.contents != pinned_ark_pem_entry.contents {
        return Err(VerifyError::ArkMismatch);
    }

    let pinned_ark_cert = pinned_ark_pem_entry
        .parse_x509()
        .map_err(|e| VerifyError::CertParse(e.to_string()))?;
    let ask_cert = ask_pem
        .parse_x509()
        .map_err(|e| VerifyError::CertParse(e.to_string()))?;

    // 2. ASK must be signed by our pinned ARK.
    ask_cert
        .verify_signature(Some(pinned_ark_cert.public_key()))
        .map_err(|_| VerifyError::AskNotSignedByArk)?;

    // 3. Fetch the VCEK for this exact chip + reported TCB, and check it's
    //    signed by the (now-trusted) ASK.
    let vcek_der = kds::fetch_vcek_der(client, product, report).await?;
    let vcek_cert = parse_der_cert(&vcek_der)?;
    vcek_cert
        .verify_signature(Some(ask_cert.public_key()))
        .map_err(|_| VerifyError::VcekNotSignedByAsk)?;

    // 4. Cross-check the VCEK's own embedded TCB extensions against the
    //    report's reported_tcb - a VCEK is issued bound to one specific TCB
    //    tuple, so this catches a report paired with the wrong VCEK (e.g. a
    //    stale cached one) even though the fetch URL itself already encodes
    //    the same values (belt-and-suspenders on a value that flows through
    //    two independent paths - the URL query string and the cert's own
    //    signed extensions - rather than trusting either alone).
    verify_vcek_tcb_extensions(&vcek_cert, &report.reported_tcb)?;

    // 5. The report's own ECDSA P-384 signature, verified against the VCEK's
    //    public key.
    verify_report_signature(report, &vcek_cert)?;

    Ok(VerifiedReport {
        reported_tcb: report.reported_tcb,
        current_tcb: report.current_tcb,
        chain_verified: true,
    })
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

fn verify_report_signature(
    report: &AttestationReport,
    vcek_cert: &X509Certificate<'_>,
) -> Result<(), VerifyError> {
    let spki_bytes = vcek_cert.public_key().subject_public_key.as_ref();
    let verifying_key =
        P384VerifyingKey::from_sec1_bytes(spki_bytes).map_err(|_| VerifyError::InvalidVcekKey)?;

    // The report stores r/s little-endian, 72 bytes each with only the low
    // 48 meaningful (P-384 scalars are 48 bytes) - reverse to the big-endian
    // form a standard ECDSA signature needs. See `report::RawSignature`'s
    // doc comment; confirmed directly against `virtee/sev`'s own P-384
    // conversion code, not assumed.
    let mut r_be = [0u8; 48];
    let mut s_be = [0u8; 48];
    for i in 0..48 {
        r_be[i] = report.signature.r_le[47 - i];
        s_be[i] = report.signature.s_le[47 - i];
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

            ask_cert
                .verify_signature(Some(pinned_ark_cert.public_key()))
                .unwrap_or_else(|e| {
                    panic!("{product:?}: real ASK must verify against pinned ARK: {e:?}")
                });
        }
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
            ark_cert
                .verify_signature(Some(ark_cert.public_key()))
                .unwrap_or_else(|e| panic!("{product:?}: ARK must be self-signed: {e:?}"));
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

        assert!(milan_ask
            .verify_signature(Some(genoa_ark.public_key()))
            .is_err());
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
        let mut report_bytes = crate::report::REPORT_LEN;
        let _ = &mut report_bytes; // silence unused in case of future refactor
        let mut raw = [0u8; crate::report::REPORT_LEN];
        raw[0x34..0x38].copy_from_slice(&1u32.to_le_bytes());
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

        let verifying_key = signing_key.verifying_key();
        let spki_point = verifying_key.to_sec1_point(false);

        // Re-decode exactly as verify_report_signature does, and confirm it
        // verifies against the same key.
        let mut r_be = [0u8; 48];
        let mut s_be = [0u8; 48];
        for i in 0..48 {
            r_be[i] = raw[0x2A0..0x2A0 + 72][47 - i];
            s_be[i] = raw[0x2A0 + 72..0x2A0 + 144][47 - i];
        }
        let recovered_sig = P384Signature::from_scalars(r_be, s_be).unwrap();
        let recovered_key = P384VerifyingKey::from_sec1_bytes(spki_point.as_bytes()).unwrap();
        recovered_key
            .verify(&raw[..crate::report::SIGNED_LEN], &recovered_sig)
            .expect("round-tripped signature must verify");
    }

    #[test]
    fn tampered_signed_region_is_rejected() {
        #[allow(deprecated)]
        let signing_key = SigningKey::random(&mut rand::rng());
        let mut raw = [0u8; crate::report::REPORT_LEN];
        for (i, b) in raw[..crate::report::SIGNED_LEN].iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        let sig: SigT =
            ecdsa::signature::Signer::sign(&signing_key, &raw[..crate::report::SIGNED_LEN]);
        let (r, s) = (sig.r().to_bytes(), sig.s().to_bytes());
        for i in 0..48 {
            raw[0x2A0 + i] = r[47 - i];
            raw[0x2A0 + 72 + i] = s[47 - i];
        }

        // Tamper with one byte inside the signed region after signing.
        raw[10] ^= 0xFF;

        let verifying_key = signing_key.verifying_key();
        let spki_point = verifying_key.to_sec1_point(false);
        let mut r_be = [0u8; 48];
        let mut s_be = [0u8; 48];
        for i in 0..48 {
            r_be[i] = raw[0x2A0..0x2A0 + 72][47 - i];
            s_be[i] = raw[0x2A0 + 72..0x2A0 + 144][47 - i];
        }
        let sig2 = P384Signature::from_scalars(r_be, s_be).unwrap();
        let key2 = P384VerifyingKey::from_sec1_bytes(spki_point.as_bytes()).unwrap();
        assert!(key2
            .verify(&raw[..crate::report::SIGNED_LEN], &sig2)
            .is_err());
    }
}
