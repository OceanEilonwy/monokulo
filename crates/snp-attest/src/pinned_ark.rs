//! Pinned AMD Root Key (ARK) certificates - the trust anchor for
//! "direct-to-AMD" verification.
//!
//! Fetching the ASK+ARK chain live from `kdsintf.amd.com` on every run (via
//! `crate::kds::fetch_cert_chain_pem`) and trusting whatever comes back is
//! *not* meaningfully stronger than trusting the cloud provider: both are
//! "trust whatever this HTTPS endpoint said today." Pinning the ARK - AMD's
//! actual self-signed root, which never rotates in practice (these are valid
//! until 2045) - means a run of this tool only ever needs to trust "this
//! exact key, embedded in this repo," not "whatever kdsintf.amd.com answers
//! right now." The live KDS fetch is still used for the ASK (the
//! intermediate) and the VCEK (the leaf, unique per chip + TCB), since those
//! legitimately can't be pinned ahead of time - only the root can.
//!
//! These three PEMs were fetched directly from AMD's own KDS
//! (`https://kdsintf.amd.com/vcek/v1/{Milan,Genoa,Turin}/cert_chain`) on
//! 2026-09-15 and are the ARK half of each response (the ASK half is
//! discarded - it's re-fetched live and chain-verified against this pinned
//! ARK every run, never trusted on its own). Before relying on this in
//! production, independently cross-check the fingerprints below against
//! AMD's own published SEV-SNP documentation or a second independent fetch -
//! this file being present in the repo is not itself proof AMD issued it,
//! only a record of what was actually observed.
//!
//! ```text
//! ARK-Milan  sha256: 67:D3:03:BD:39:05:FD:38:DB:8B:20:E0:79:36:99:87:0E:7F:A6:12:EA:AD:5D:EC:35:82:93:FD:8C:0B:AC:1B
//! ARK-Genoa  sha256: 54:64:73:8C:15:46:AE:D5:F2:CE:CF:1D:C9:8C:5C:96:0A:92:E8:91:32:38:A6:17:11:BC:90:EC:6E:82:85:21
//! ARK-Turin  sha256: 5B:77:EF:5F:E7:A7:A0:04:FD:90:32:66:8F:BA:9D:0F:DA:22:F8:8C:44:42:06:9A:47:96:36:A6:AE:3B:31:85
//! ```
//!
//! (Both certs in each fetched `cert_chain` response were inspected with
//! `openssl x509 -noout -subject -issuer -serial`; the one whose issuer
//! equals its own subject - `CN=ARK-{Product}` - is the root kept here. The
//! other, `CN=SEV-{Product}` issued by the ARK, is the ASK - intentionally
//! not pinned, per this module's own reasoning above.)

use crate::report::Product;

pub const ARK_MILAN_PEM: &str = include_str!("pinned_certs/ark_milan.pem");
pub const ARK_GENOA_PEM: &str = include_str!("pinned_certs/ark_genoa.pem");
pub const ARK_TURIN_PEM: &str = include_str!("pinned_certs/ark_turin.pem");

pub fn pinned_ark_pem(product: Product) -> &'static str {
    match product {
        Product::Milan => ARK_MILAN_PEM,
        Product::Genoa => ARK_GENOA_PEM,
        Product::Turin => ARK_TURIN_PEM,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pinned_ark_is_well_formed_pem() {
        for product in [Product::Milan, Product::Genoa, Product::Turin] {
            let pem = pinned_ark_pem(product);
            assert!(pem.contains("-----BEGIN CERTIFICATE-----"));
            assert!(pem.contains("-----END CERTIFICATE-----"));
        }
    }
}
