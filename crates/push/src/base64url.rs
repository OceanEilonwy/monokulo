//! Base64url (RFC 4648 §5) the way Web Push writes it, and the P-256
//! public keys it carries.

use base64::engine::general_purpose::{URL_SAFE_NO_PAD, URL_SAFE_NO_PAD_INDIFFERENT};
use base64::Engine as _;
use p256::elliptic_curve::sec1::ToSec1Point as _;

/// Without padding, as every key and token here is written.
pub(crate) fn encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// A key as a browser or a library hands it over: written without padding
/// (RFC 8291 §3.2), though some libraries pad it, which changes nothing.
pub(crate) fn decode_key(text: &str) -> Result<Vec<u8>, base64::DecodeError> {
    URL_SAFE_NO_PAD_INDIFFERENT.decode(text)
}

/// A JWT segment: never padded (RFC 7515 §2), so a padded one is not a
/// JWT this crate wrote or should accept.
pub(crate) fn decode_segment(text: &str) -> Result<Vec<u8>, base64::DecodeError> {
    URL_SAFE_NO_PAD.decode(text)
}

/// An uncompressed P-256 point: `0x04 || x || y`.
pub(crate) const POINT_LEN: usize = 65;

/// Why a base64url P-256 public key was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    #[error("not base64url")]
    Base64,
    /// Web Push keys are uncompressed points (RFC 8291 §3.1, RFC 8292 §3.2).
    #[error("{0} bytes, not the 65 of an uncompressed P-256 point")]
    Length(usize),
    #[error("not an uncompressed point (its first byte isn't 0x04)")]
    Compressed,
    #[error("not a point on the P-256 curve")]
    NotOnCurve,
}

/// Reads a public key written as [`encode_point`] writes it.
pub(crate) fn decode_point(text: &str) -> Result<p256::PublicKey, KeyError> {
    let bytes = decode_key(text).map_err(|_| KeyError::Base64)?;
    if bytes.len() != POINT_LEN {
        return Err(KeyError::Length(bytes.len()));
    }
    if bytes[0] != 0x04 {
        return Err(KeyError::Compressed);
    }
    p256::PublicKey::from_sec1_bytes(&bytes).map_err(|_| KeyError::NotOnCurve)
}

/// The 65 bytes of `key` as an uncompressed point. A `PublicKey` is never
/// the identity (the one point SEC 1 writes shorter), so its uncompressed
/// encoding is always this long.
pub(crate) fn point_bytes(key: &p256::PublicKey) -> [u8; POINT_LEN] {
    let point = key.to_sec1_point(false);
    let mut bytes = [0u8; POINT_LEN];
    bytes.copy_from_slice(point.as_bytes());
    bytes
}

pub(crate) fn encode_point(key: &p256::PublicKey) -> String {
    encode(&point_bytes(key))
}
