//! AMD Key Distribution Service (KDS) client - fetches the two pieces of
//! certificate material this tool needs directly from AMD, deliberately
//! never through a cloud provider's own attestation service. That's the
//! whole point of "direct-to-AMD" (per this WBS item's own decision): the
//! provider is exactly the party a confidential-computing attestation is
//! meant to not have to trust, so routing verification through *their*
//! attestation wrapper would defeat it.
//!
//! URL formats confirmed directly against `virtee/snpguest` (the reference
//! CLI AMD's own ecosystem uses for this), not reconstructed from memory:
//! <https://github.com/virtee/snpguest/blob/main/src/fetch.rs>

use crate::report::Product;

pub const KDS_BASE: &str = "https://kdsintf.amd.com";

#[derive(Debug, thiserror::Error)]
pub enum KdsError {
    #[error("HTTP request to AMD KDS failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("AMD KDS returned HTTP {0} for {1}")]
    BadStatus(reqwest::StatusCode, String),
}

/// The VCEK certificate chain's request URL. `hw_id` is the report's
/// `chip_id` field, hex-encoded - 128 hex chars (64 bytes) on
/// Milan/Genoa/Bergamo/Siena, trimmed to 16 hex chars (8 bytes) on Turin+
/// per AMD's own hwID length change for that generation (confirmed via
/// `google/go-sev-guest`'s KDS package doc comments, independently of the
/// `virtee` sources this module otherwise leans on - a second source for
/// this specific fact since getting hwID length wrong would make every
/// Turin+ VCEK lookup fail closed, not silently succeed wrong).
pub fn vcek_url(product: Product, hw_id: &[u8], tcb: &crate::report::TcbVersion) -> String {
    let hw_id_hex = hex::encode(hw_id);
    match product {
        Product::Turin => {
            format!(
                "{KDS_BASE}/vcek/v1/{}/{hw_id_hex}?fmcSPL={:02}&blSPL={:02}&teeSPL={:02}&snpSPL={:02}&ucodeSPL={:02}",
                product.kds_name(),
                tcb.fmc.unwrap_or(0),
                tcb.bootloader,
                tcb.tee,
                tcb.snp,
                tcb.microcode,
            )
        }
        _ => {
            format!(
                "{KDS_BASE}/vcek/v1/{}/{hw_id_hex}?blSPL={:02}&teeSPL={:02}&snpSPL={:02}&ucodeSPL={:02}",
                product.kds_name(),
                tcb.bootloader,
                tcb.tee,
                tcb.snp,
                tcb.microcode,
            )
        }
    }
}

pub fn cert_chain_url(product: Product) -> String {
    format!("{KDS_BASE}/vcek/v1/{}/cert_chain", product.kds_name())
}

/// The hwID KDS expects in the VCEK URL path - full 64-byte `chip_id` on
/// legacy products, first 8 bytes only on Turin+.
pub fn hw_id_for_product(product: Product, chip_id: &[u8; 64]) -> Vec<u8> {
    match product {
        Product::Turin => chip_id[..8].to_vec(),
        _ => chip_id.to_vec(),
    }
}

/// Fetches the DER-encoded VCEK certificate for the exact reported TCB in
/// `report` - AMD's KDS issues a VCEK bound to one specific
/// (hwID, blSPL, teeSPL, snpSPL, ucodeSPL) tuple, so a successful fetch here
/// is itself already a meaningful check: it proves AMD issued a certificate
/// for exactly the SPL combination this report claims, not merely that
/// *some* VCEK exists for this chip.
pub async fn fetch_vcek_der(
    client: &reqwest::Client,
    product: Product,
    report: &crate::report::AttestationReport,
) -> Result<Vec<u8>, KdsError> {
    let hw_id = hw_id_for_product(product, &report.chip_id);
    let url = vcek_url(product, &hw_id, &report.reported_tcb);
    let resp = client.get(&url).send().await?;
    if !resp.status().is_success() {
        return Err(KdsError::BadStatus(resp.status(), url));
    }
    Ok(resp.bytes().await?.to_vec())
}

/// Fetches AMD's ASK+ARK certificate chain (concatenated PEM, ASK first then
/// ARK, per AMD's own response format). Prefer `crate::pinned_ark`'s embedded
/// copies over calling this live in production - see that module's doc
/// comment for why.
pub async fn fetch_cert_chain_pem(
    client: &reqwest::Client,
    product: Product,
) -> Result<String, KdsError> {
    let url = cert_chain_url(product);
    let resp = client.get(&url).send().await?;
    if !resp.status().is_success() {
        return Err(KdsError::BadStatus(resp.status(), url));
    }
    Ok(resp.text().await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::TcbVersion;

    fn legacy_tcb() -> TcbVersion {
        TcbVersion {
            raw: [3, 2, 0, 0, 0, 0, 1, 9],
            fmc: None,
            bootloader: 3,
            tee: 2,
            snp: 1,
            microcode: 9,
        }
    }

    #[test]
    fn vcek_url_for_legacy_product_has_no_fmc_param() {
        let url = vcek_url(Product::Milan, &[0xAB; 64], &legacy_tcb());
        assert!(url.starts_with("https://kdsintf.amd.com/vcek/v1/Milan/"));
        assert!(url.contains(&hex::encode([0xABu8; 64])));
        assert!(url.contains("blSPL=03"));
        assert!(url.contains("teeSPL=02"));
        assert!(url.contains("snpSPL=01"));
        assert!(url.contains("ucodeSPL=09"));
        assert!(!url.contains("fmcSPL"));
    }

    #[test]
    fn vcek_url_for_turin_includes_fmc_param() {
        let tcb = TcbVersion {
            raw: [5, 3, 2, 1, 0, 0, 0, 9],
            fmc: Some(5),
            bootloader: 3,
            tee: 2,
            snp: 1,
            microcode: 9,
        };
        let url = vcek_url(Product::Turin, &[0xCD; 8], &tcb);
        assert!(url.starts_with("https://kdsintf.amd.com/vcek/v1/Turin/"));
        assert!(url.contains("fmcSPL=05"));
    }

    #[test]
    fn cert_chain_url_matches_amd_kds_format() {
        assert_eq!(
            cert_chain_url(Product::Milan),
            "https://kdsintf.amd.com/vcek/v1/Milan/cert_chain"
        );
        assert_eq!(
            cert_chain_url(Product::Genoa),
            "https://kdsintf.amd.com/vcek/v1/Genoa/cert_chain"
        );
    }

    #[test]
    fn hw_id_trimmed_to_8_bytes_on_turin_full_64_elsewhere() {
        let chip_id = [7u8; 64];
        assert_eq!(hw_id_for_product(Product::Milan, &chip_id).len(), 64);
        assert_eq!(hw_id_for_product(Product::Genoa, &chip_id).len(), 64);
        assert_eq!(hw_id_for_product(Product::Turin, &chip_id).len(), 8);
    }
}
