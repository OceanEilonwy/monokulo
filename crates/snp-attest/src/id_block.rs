//! ID blocks: what a confidential VM's launcher hands the security processor
//! to say which image it is starting, signed by the image's ID key.
//!
//! The firmware checks the ID block's signature with the ID key that comes
//! with it, checks the block's launch digest is the image's actual
//! measurement, and then reports the image's family and image IDs, its
//! security version (SVN), and the SHA-384 of the ID key
//! ([`id_key_digest`]) in every attestation report. Key custody trusts an
//! engine image by that digest (`key-custody`'s `TrustPolicy`).
//!
//! Layouts are those of AMD's SEV-SNP firmware ABI specification: the ID
//! block (`ID_BLOCK`, 0x60 bytes) and the authentication information
//! (`ID_AUTH_INFO`, 0x1000 bytes) carrying its signature and the ID key. A
//! signature and a public key's coordinates are little-endian and padded to
//! 72 bytes, as in attestation reports. The launcher (QEMU's
//! `sev-snp-guest` object: `id-block=` and `id-auth=`, base64) passes both
//! to `SNP_LAUNCH_FINISH`.
//!
//! The ID key here is ECDSA P-384 with SHA-384, the only algorithm the
//! firmware takes. No author key: the ID key is the trust anchor.

use p384::ecdsa::{Signature, SigningKey, VerifyingKey};
use sha2::Digest as _;

pub const ID_BLOCK_LEN: usize = 0x60;
pub const ID_AUTH_LEN: usize = 0x1000;
/// The length of a public key as the firmware encodes it (`ID_KEY`).
pub const PUBLIC_KEY_LEN: usize = 0x404;

/// `ID_KEY_ALGO`/`AUTH_KEY_ALGO`: ECDSA P-384 with SHA-384.
const ALGO_ECDSA_P384_SHA384: u32 = 1;
/// A public key's `CURVE`: P-384.
const CURVE_P384: u32 = 2;
/// Where in `ID_AUTH_INFO` the block's signature and the ID key go.
const ID_BLOCK_SIG_AT: usize = 0x40;
const ID_KEY_AT: usize = 0x240;
/// The ID block format version.
const ID_BLOCK_VERSION: u32 = 1;

/// What an ID block says about an image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdBlock {
    /// The image's launch measurement (`LD`).
    pub measurement: [u8; 48],
    pub family_id: [u8; 16],
    pub image_id: [u8; 16],
    pub guest_svn: u32,
    /// The guest policy the image must be launched with.
    pub policy: u64,
}

impl IdBlock {
    pub fn to_bytes(&self) -> [u8; ID_BLOCK_LEN] {
        let mut bytes = [0u8; ID_BLOCK_LEN];
        bytes[0x00..0x30].copy_from_slice(&self.measurement);
        bytes[0x30..0x40].copy_from_slice(&self.family_id);
        bytes[0x40..0x50].copy_from_slice(&self.image_id);
        bytes[0x50..0x54].copy_from_slice(&ID_BLOCK_VERSION.to_le_bytes());
        bytes[0x54..0x58].copy_from_slice(&self.guest_svn.to_le_bytes());
        bytes[0x58..0x60].copy_from_slice(&self.policy.to_le_bytes());
        bytes
    }
}

/// A P-384 coordinate or scalar, big-endian, as the firmware lays it out:
/// little-endian, padded to 72 bytes.
fn little_endian_72(big_endian: &[u8]) -> [u8; 72] {
    let mut out = [0u8; 72];
    for (to, from) in out.iter_mut().zip(big_endian.iter().rev()) {
        *to = *from;
    }
    out
}

/// `key` as the firmware encodes a public key (`ID_KEY`).
pub fn public_key_bytes(key: &VerifyingKey) -> [u8; PUBLIC_KEY_LEN] {
    let point = key.to_sec1_point(false);
    let (x, y) = (
        point.x().map(|x| x.to_vec()).unwrap_or_default(),
        point.y().map(|y| y.to_vec()).unwrap_or_default(),
    );
    let mut bytes = [0u8; PUBLIC_KEY_LEN];
    bytes[0x00..0x04].copy_from_slice(&CURVE_P384.to_le_bytes());
    bytes[0x04..0x4C].copy_from_slice(&little_endian_72(&x));
    bytes[0x4C..0x94].copy_from_slice(&little_endian_72(&y));
    bytes
}

/// The SHA-384 of `key` as the firmware encodes it: what attestation
/// reports carry as `ID_KEY_DIGEST`.
pub fn id_key_digest(key: &VerifyingKey) -> [u8; 48] {
    sha2::Sha384::digest(public_key_bytes(key)).into()
}

/// `ID_AUTH_INFO` for `block`, signed with the ID key `key`.
pub fn sign(block: &IdBlock, key: &SigningKey) -> [u8; ID_AUTH_LEN] {
    let signature: Signature = ecdsa::signature::Signer::sign(key, &block.to_bytes());
    let (r, s) = (signature.r().to_bytes(), signature.s().to_bytes());
    let mut auth = [0u8; ID_AUTH_LEN];
    auth[0x00..0x04].copy_from_slice(&ALGO_ECDSA_P384_SHA384.to_le_bytes());
    auth[0x04..0x08].copy_from_slice(&ALGO_ECDSA_P384_SHA384.to_le_bytes());
    auth[ID_BLOCK_SIG_AT..ID_BLOCK_SIG_AT + 72].copy_from_slice(&little_endian_72(&r));
    auth[ID_BLOCK_SIG_AT + 72..ID_BLOCK_SIG_AT + 144].copy_from_slice(&little_endian_72(&s));
    auth[ID_KEY_AT..ID_KEY_AT + PUBLIC_KEY_LEN]
        .copy_from_slice(&public_key_bytes(key.verifying_key()));
    auth
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> SigningKey {
        SigningKey::from_slice(&[0x42; 48]).unwrap()
    }

    fn block() -> IdBlock {
        IdBlock {
            measurement: [0x11; 48],
            family_id: [0x22; 16],
            image_id: [0x33; 16],
            guest_svn: 7,
            policy: 0x3_0000,
        }
    }

    #[test]
    fn an_id_block_lays_out_its_fields_where_the_firmware_reads_them() {
        let bytes = block().to_bytes();
        assert_eq!(&bytes[0x00..0x30], &[0x11; 48]);
        assert_eq!(&bytes[0x30..0x40], &[0x22; 16]);
        assert_eq!(&bytes[0x40..0x50], &[0x33; 16]);
        assert_eq!(&bytes[0x50..0x54], &1u32.to_le_bytes(), "version");
        assert_eq!(&bytes[0x54..0x58], &7u32.to_le_bytes());
        assert_eq!(&bytes[0x58..0x60], &0x3_0000u64.to_le_bytes());
    }

    /// The signature in the auth info checks out against the ID key in it,
    /// read back the way the firmware reads them.
    #[test]
    fn the_auth_info_carries_a_signature_the_id_key_in_it_verifies() {
        let auth = sign(&block(), &key());
        assert_eq!(&auth[0..4], &1u32.to_le_bytes());
        let be = |le: &[u8]| -> [u8; 48] {
            assert!(le[48..72].iter().all(|b| *b == 0), "padding is zero");
            let mut out = [0u8; 48];
            for (to, from) in out.iter_mut().zip(le[..48].iter().rev()) {
                *to = *from;
            }
            out
        };
        let signature = Signature::from_scalars(
            be(&auth[ID_BLOCK_SIG_AT..ID_BLOCK_SIG_AT + 72]),
            be(&auth[ID_BLOCK_SIG_AT + 72..ID_BLOCK_SIG_AT + 144]),
        )
        .unwrap();
        let id_key = &auth[ID_KEY_AT..ID_KEY_AT + PUBLIC_KEY_LEN];
        assert_eq!(&id_key[0..4], &2u32.to_le_bytes(), "P-384");
        let mut sec1 = vec![4u8];
        sec1.extend_from_slice(&be(&id_key[0x04..0x4C]));
        sec1.extend_from_slice(&be(&id_key[0x4C..0x94]));
        let verifying = VerifyingKey::from_sec1_bytes(&sec1).unwrap();
        assert_eq!(verifying, *key().verifying_key());
        ecdsa::signature::Verifier::verify(&verifying, &block().to_bytes(), &signature).unwrap();
        assert_eq!(
            id_key_digest(&verifying),
            <[u8; 48]>::from(sha2::Sha384::digest(id_key))
        );
    }
}
