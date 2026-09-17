//! WBS 2.2.1: a real attestation-verification library/tool for AMD SEV-SNP,
//! verifying "direct-to-AMD" (the AMD Key Distribution Service, never a
//! cloud provider's own attestation wrapper - see `pinned_ark`'s doc comment
//! for why that distinction matters).
//!
//! Module map:
//! - [`report`] - parses the raw 1184-byte `ATTESTATION_REPORT` blob.
//! - [`kds`] - AMD KDS client (VCEK + ASK/ARK cert-chain fetch).
//! - [`pinned_ark`] - the embedded, pinned AMD root certificates.
//! - [`verify`] - the actual chain-of-trust and signature verification.

pub mod kds;
pub mod pinned_ark;
pub mod report;
pub mod verify;
