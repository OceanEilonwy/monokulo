//! AMD SEV-SNP attestation, verified "direct-to-AMD" (the AMD Key
//! Distribution Service, never a cloud provider's own attestation wrapper -
//! see `pinned_ark`'s doc comment for why that distinction matters), plus a
//! guest's own requests to the security processor.
//!
//! Module map:
//! - [`report`] - parses the raw 1184-byte `ATTESTATION_REPORT` blob.
//! - [`kds`] - AMD KDS client (VCEK + ASK/ARK cert-chain fetch); feature `kds`.
//! - [`pinned_ark`] - the embedded, pinned AMD root certificates.
//! - [`verify`] - the actual chain-of-trust and signature verification.
//! - [`guest`] - `/dev/sev-guest`: reports and derived keys.

pub mod guest;
#[cfg(feature = "kds")]
pub mod kds;
pub mod pinned_ark;
pub mod report;
pub mod verify;
