//! Message encryption for Web Push (RFC 8291): the `aes128gcm` content
//! coding (RFC 8188) under a key agreed between a fresh sender key pair
//! and the device's [`P256dh`], mixed with its [`AuthSecret`].
//!
//! The body is one record:
//!
//! ```text
//! salt (16) | record size (4, = 4096) | key id length (1, = 65) | sender public key (65) | AES-128-GCM(plaintext | 0x02)
//! ```
//!
//! Each call makes a new salt and sender key, as RFC 8291 §3.4 requires, so
//! the same plaintext never encrypts the same way twice and a key is never
//! reused across messages.
//!
//! [`P256dh`]: crate::P256dh
//! [`AuthSecret`]: crate::AuthSecret

use aes_gcm::aead::{Aead as _, Key, KeyInit as _, Nonce};
use aes_gcm::Aes128Gcm;
use hkdf::Hkdf;
use p256::elliptic_curve::Generate as _;
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::base64url::{self, POINT_LEN};
use crate::subscription::SubscriptionKeys;
use crate::MAX_BODY;

const SALT_LEN: usize = 16;
/// Salt, record size, key id length and the sender's public key as key id.
const HEADER_LEN: usize = SALT_LEN + 4 + 1 + POINT_LEN;
const TAG_LEN: usize = 16;
/// The record size written: RFC 8291 §4 has a sender use 4096, so the one
/// record holds any push a push service has to accept.
const RECORD_SIZE: u32 = 4096;
/// The smallest record size RFC 8188 §2 allows: a delimiter and a tag.
const MIN_RECORD_SIZE: u32 = 18;
/// Ends the last (here, the only) record's plaintext (RFC 8188 §2).
const LAST_RECORD: u8 = 0x02;

/// The most plaintext one push can carry: [`MAX_BODY`] less the header
/// (86 bytes), the delimiter (1) and the tag (16), the 3993 octets of RFC
/// 8291 §4.
pub const MAX_PLAINTEXT: usize = MAX_BODY - HEADER_LEN - 1 - TAG_LEN;

/// An `aes128gcm` body: what is sent with `Content-Encoding: aes128gcm`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encrypted(Vec<u8>);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EncryptError {
    #[error("{0} bytes of plaintext, more than a push may carry ({MAX_PLAINTEXT})")]
    TooLarge(usize),
}

/// Why a body isn't one `aes128gcm` record as RFC 8291 has a sender write
/// it. A relay checks this much before forwarding a body it can't decrypt.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EncryptedError {
    #[error("{0} bytes, more than a push may carry ({MAX_BODY})")]
    TooLarge(usize),
    #[error("too short for the aes128gcm header and one record")]
    Truncated,
    #[error("key id is {0} bytes, not the sender's 65-byte public key")]
    KeyId(u8),
    #[error("record size {0} is below the minimum of 18")]
    RecordSize(u32),
    #[error("more than one record")]
    MultipleRecords,
}

impl Encrypted {
    /// A body as a relay receives it, checked for the shape of one record.
    pub fn parse(body: &[u8]) -> Result<Self, EncryptedError> {
        split(body)?;
        Ok(Encrypted(body.to_vec()))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// A body split into its salt, the sender's public key and its one
/// record, once checked for that shape.
type Parts<'a> = (&'a [u8; SALT_LEN], &'a [u8; POINT_LEN], &'a [u8]);

fn split(body: &[u8]) -> Result<Parts<'_>, EncryptedError> {
    if body.len() > MAX_BODY {
        return Err(EncryptedError::TooLarge(body.len()));
    }
    let (salt, rest) = body
        .split_first_chunk::<SALT_LEN>()
        .ok_or(EncryptedError::Truncated)?;
    let (record_size, rest) = rest
        .split_first_chunk::<4>()
        .ok_or(EncryptedError::Truncated)?;
    let (&key_id_len, rest) = rest.split_first().ok_or(EncryptedError::Truncated)?;
    if usize::from(key_id_len) != POINT_LEN {
        return Err(EncryptedError::KeyId(key_id_len));
    }
    let (sender_key, record) = rest
        .split_first_chunk::<POINT_LEN>()
        .ok_or(EncryptedError::Truncated)?;
    let record_size = u32::from_be_bytes(*record_size);
    if record_size < MIN_RECORD_SIZE {
        return Err(EncryptedError::RecordSize(record_size));
    }
    if record.len() < 1 + TAG_LEN {
        return Err(EncryptedError::Truncated);
    }
    // A record is at most `record_size` bytes with its tag; anything
    // longer would be a second record, which RFC 8291 §4 rules out.
    if record.len() > record_size as usize {
        return Err(EncryptedError::MultipleRecords);
    }
    Ok((salt, sender_key, record))
}

/// Encrypts `plaintext` to the device `keys` belong to, under a fresh salt
/// and sender key.
pub fn encrypt(keys: &SubscriptionKeys, plaintext: &[u8]) -> Result<Encrypted, EncryptError> {
    let salt: [u8; SALT_LEN] = rand::random();
    let sender = p256::SecretKey::generate_from_rng(&mut rand::rng());
    encrypt_with(keys, plaintext, &salt, &sender)
}

/// [`encrypt`] with the salt and sender key given, so RFC 8291's example
/// can be checked byte for byte.
fn encrypt_with(
    keys: &SubscriptionKeys,
    plaintext: &[u8],
    salt: &[u8; SALT_LEN],
    sender: &p256::SecretKey,
) -> Result<Encrypted, EncryptError> {
    if plaintext.len() > MAX_PLAINTEXT {
        return Err(EncryptError::TooLarge(plaintext.len()));
    }
    let sender_public = base64url::point_bytes(&sender.public_key());
    let receiver_public = base64url::point_bytes(keys.p256dh.key());
    let shared = sender.diffie_hellman(keys.p256dh.key());
    let (key, nonce) = content_key(
        shared.raw_secret_bytes().as_ref(),
        keys.auth.as_bytes(),
        &receiver_public,
        &sender_public,
        salt,
    );

    let mut record = Zeroizing::new(Vec::with_capacity(plaintext.len() + 1));
    record.extend_from_slice(plaintext);
    record.push(LAST_RECORD);
    let cipher = Aes128Gcm::new(&Key::<Aes128Gcm>::from(*key));
    // AES-GCM refuses only a plaintext of more than 64 GiB; this one is
    // at most `MAX_PLAINTEXT` bytes.
    let Ok(ciphertext) = cipher.encrypt(&Nonce::<Aes128Gcm>::from(*nonce), record.as_slice())
    else {
        unreachable!("AES-128-GCM refused {} bytes", record.len())
    };

    let mut body = Vec::with_capacity(HEADER_LEN + ciphertext.len());
    body.extend_from_slice(salt);
    body.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    body.push(POINT_LEN as u8);
    body.extend_from_slice(&sender_public);
    body.extend_from_slice(&ciphertext);
    Ok(Encrypted(body))
}

/// RFC 8291 §3.3 and §3.4: the content encryption key and nonce for one
/// message, from the ECDH secret, the device's auth secret, both public
/// keys and the message's salt.
fn content_key(
    ecdh_secret: &[u8],
    auth: &[u8; 16],
    receiver_public: &[u8; POINT_LEN],
    sender_public: &[u8; POINT_LEN],
    salt: &[u8; SALT_LEN],
) -> (Zeroizing<[u8; 16]>, Zeroizing<[u8; 12]>) {
    // PRK_key = HMAC-SHA-256(auth_secret, ecdh_secret), then
    // IKM = HKDF-Expand(PRK_key, "WebPush: info" || 0x00 || ua_public || as_public, 32).
    let mut ikm = Zeroizing::new([0u8; 32]);
    Hkdf::<Sha256>::new(Some(auth), ecdh_secret)
        .expand_multi_info(
            &[b"WebPush: info\0", receiver_public, sender_public],
            ikm.as_mut(),
        )
        .unwrap_or_else(|_| unreachable!("32 bytes is a valid HKDF-SHA-256 output length"));

    // PRK = HMAC-SHA-256(salt, IKM), then the key and nonce (RFC 8188 §2.2, §2.3).
    let prk = Hkdf::<Sha256>::new(Some(salt), ikm.as_ref());
    let mut key = Zeroizing::new([0u8; 16]);
    let mut nonce = Zeroizing::new([0u8; 12]);
    prk.expand(b"Content-Encoding: aes128gcm\0", key.as_mut())
        .unwrap_or_else(|_| unreachable!("16 bytes is a valid HKDF-SHA-256 output length"));
    prk.expand(b"Content-Encoding: nonce\0", nonce.as_mut())
        .unwrap_or_else(|_| unreachable!("12 bytes is a valid HKDF-SHA-256 output length"));
    (key, nonce)
}

#[cfg(any(test, feature = "test-support"))]
pub use receiver::{decrypt, DecryptError, ReceiverKeys};

/// The device's side, for tests that check what a push service would hand
/// it: a key pair and auth secret to subscribe with, and decryption.
#[cfg(any(test, feature = "test-support"))]
mod receiver {
    use super::*;
    use crate::subscription::{AuthSecret, P256dh};

    /// A device's private key and auth secret.
    pub struct ReceiverKeys {
        pub(super) secret: p256::SecretKey,
        pub(super) auth: AuthSecret,
    }

    impl ReceiverKeys {
        pub fn generate() -> Self {
            ReceiverKeys {
                secret: p256::SecretKey::generate_from_rng(&mut rand::rng()),
                auth: AuthSecret::from_bytes(rand::random()),
            }
        }

        /// What the device would hand a sender to subscribe.
        pub fn subscription_keys(&self) -> SubscriptionKeys {
            SubscriptionKeys {
                p256dh: P256dh::from_key(self.secret.public_key()),
                auth: self.auth.clone(),
            }
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
    pub enum DecryptError {
        #[error(transparent)]
        Malformed(#[from] EncryptedError),
        #[error("the sender's key isn't a point on the curve")]
        SenderKey,
        #[error("doesn't decrypt with this device's keys")]
        Authentication,
        #[error("the record has no end-of-record delimiter")]
        Delimiter,
    }

    /// The plaintext of `body`, as the device that holds `receiver` reads it.
    pub fn decrypt(receiver: &ReceiverKeys, body: &[u8]) -> Result<Vec<u8>, DecryptError> {
        let (salt, sender_key, ciphertext) = split(body)?;
        let sender =
            p256::PublicKey::from_sec1_bytes(sender_key).map_err(|_| DecryptError::SenderKey)?;
        let receiver_public = base64url::point_bytes(&receiver.secret.public_key());
        let shared = receiver.secret.diffie_hellman(&sender);
        let (key, nonce) = content_key(
            shared.raw_secret_bytes().as_ref(),
            receiver.auth.as_bytes(),
            &receiver_public,
            sender_key,
            salt,
        );
        let cipher = Aes128Gcm::new(&Key::<Aes128Gcm>::from(*key));
        let mut record = cipher
            .decrypt(&Nonce::<Aes128Gcm>::from(*nonce), ciphertext)
            .map_err(|_| DecryptError::Authentication)?;
        // Padding is zeros after the delimiter (RFC 8188 §2).
        let end = record
            .iter()
            .rposition(|&byte| byte != 0)
            .ok_or(DecryptError::Delimiter)?;
        if record[end] != LAST_RECORD {
            return Err(DecryptError::Delimiter);
        }
        record.truncate(end);
        Ok(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subscription::{AuthSecret, P256dh};

    /// RFC 8291 Appendix A, every value as the RFC prints it.
    mod rfc8291 {
        pub const PLAINTEXT: &str = "V2hlbiBJIGdyb3cgdXAsIEkgd2FudCB0byBiZSBhIHdhdGVybWVsb24";
        pub const AS_PUBLIC: &str =
            "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8";
        pub const AS_PRIVATE: &str = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";
        pub const UA_PUBLIC: &str =
            "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
        pub const UA_PRIVATE: &str = "q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94";
        pub const SALT: &str = "DGv6ra1nlYgDCS1FRnbzlw";
        pub const AUTH_SECRET: &str = "BTBZMqHH6r4Tts7J_aSIgg";
        pub const CEK: &str = "oIhVW04MRdy2XN9CiKLxTg";
        pub const NONCE: &str = "4h_95klXJ5E_qnoN";
        pub const BODY: &str = "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN";
    }

    fn bytes(text: &str) -> Vec<u8> {
        base64url::decode_key(text).unwrap()
    }

    fn rfc_receiver() -> ReceiverKeys {
        ReceiverKeys {
            secret: p256::SecretKey::from_slice(&bytes(rfc8291::UA_PRIVATE)).unwrap(),
            auth: AuthSecret::from_base64url(rfc8291::AUTH_SECRET).unwrap(),
        }
    }

    fn rfc_sender() -> p256::SecretKey {
        p256::SecretKey::from_slice(&bytes(rfc8291::AS_PRIVATE)).unwrap()
    }

    fn rfc_salt() -> [u8; SALT_LEN] {
        bytes(rfc8291::SALT).try_into().unwrap()
    }

    #[test]
    fn the_rfc_8291_example_encrypts_byte_for_byte() {
        let receiver = rfc_receiver();
        let keys = receiver.subscription_keys();
        assert_eq!(keys.p256dh.to_base64url(), rfc8291::UA_PUBLIC);
        assert_eq!(
            base64url::encode_point(&rfc_sender().public_key()),
            rfc8291::AS_PUBLIC
        );

        let encrypted = encrypt_with(
            &keys,
            &bytes(rfc8291::PLAINTEXT),
            &rfc_salt(),
            &rfc_sender(),
        )
        .unwrap();
        assert_eq!(base64url::encode(encrypted.as_bytes()), rfc8291::BODY);
    }

    /// The key schedule's intermediate values, so a mismatch above says
    /// which step went wrong.
    #[test]
    fn the_rfc_8291_example_derives_its_key_and_nonce() {
        let sender = rfc_sender();
        let receiver = P256dh::from_base64url(rfc8291::UA_PUBLIC).unwrap();
        let shared = sender.diffie_hellman(receiver.key());
        let auth: [u8; 16] = bytes(rfc8291::AUTH_SECRET).try_into().unwrap();
        let (key, nonce) = content_key(
            shared.raw_secret_bytes().as_ref(),
            &auth,
            &base64url::point_bytes(receiver.key()),
            &base64url::point_bytes(&sender.public_key()),
            &rfc_salt(),
        );
        assert_eq!(base64url::encode(key.as_ref()), rfc8291::CEK);
        assert_eq!(base64url::encode(nonce.as_ref()), rfc8291::NONCE);
    }

    #[test]
    fn the_rfc_8291_example_decrypts() {
        let plaintext = decrypt(&rfc_receiver(), &bytes(rfc8291::BODY)).unwrap();
        assert_eq!(plaintext, b"When I grow up, I want to be a watermelon");
    }

    #[test]
    fn a_push_decrypts_with_the_device_keys_and_no_others() {
        let receiver = ReceiverKeys::generate();
        let encrypted = encrypt(&receiver.subscription_keys(), b"{\"sale\":1}").unwrap();
        assert_eq!(
            decrypt(&receiver, encrypted.as_bytes()).unwrap(),
            b"{\"sale\":1}"
        );

        let other = ReceiverKeys::generate();
        assert_eq!(
            decrypt(&other, encrypted.as_bytes()),
            Err(DecryptError::Authentication)
        );

        // Same public key, another auth secret.
        let wrong_auth = ReceiverKeys {
            secret: receiver.secret.clone(),
            auth: AuthSecret::from_bytes([9; 16]),
        };
        assert_eq!(
            decrypt(&wrong_auth, encrypted.as_bytes()),
            Err(DecryptError::Authentication)
        );

        let mut tampered = encrypted.as_bytes().to_vec();
        *tampered.last_mut().unwrap() ^= 1;
        assert_eq!(
            decrypt(&receiver, &tampered),
            Err(DecryptError::Authentication)
        );
    }

    /// RFC 8291 §3.4: a new salt and sender key for every message.
    #[test]
    fn the_same_plaintext_encrypts_differently_each_time() {
        let keys = ReceiverKeys::generate().subscription_keys();
        let first = encrypt(&keys, b"same").unwrap();
        let second = encrypt(&keys, b"same").unwrap();
        assert_ne!(first.as_bytes()[..SALT_LEN], second.as_bytes()[..SALT_LEN]);
        assert_ne!(
            first.as_bytes()[21..HEADER_LEN],
            second.as_bytes()[21..HEADER_LEN]
        );
    }

    #[test]
    fn the_largest_plaintext_fills_a_push_and_one_byte_more_is_refused() {
        assert_eq!(MAX_PLAINTEXT, 3993);
        let receiver = ReceiverKeys::generate();
        let keys = receiver.subscription_keys();
        let largest = vec![b'x'; MAX_PLAINTEXT];
        let encrypted = encrypt(&keys, &largest).unwrap();
        assert_eq!(encrypted.as_bytes().len(), MAX_BODY);
        assert_eq!(decrypt(&receiver, encrypted.as_bytes()).unwrap(), largest);
        assert!(Encrypted::parse(encrypted.as_bytes()).is_ok());

        assert_eq!(
            encrypt(&keys, &vec![b'x'; MAX_PLAINTEXT + 1]),
            Err(EncryptError::TooLarge(MAX_PLAINTEXT + 1))
        );
    }

    #[test]
    fn an_empty_plaintext_is_one_delimiter_and_a_tag() {
        let receiver = ReceiverKeys::generate();
        let encrypted = encrypt(&receiver.subscription_keys(), b"").unwrap();
        assert_eq!(encrypted.as_bytes().len(), HEADER_LEN + 1 + TAG_LEN);
        assert_eq!(decrypt(&receiver, encrypted.as_bytes()).unwrap(), b"");
    }

    #[test]
    fn a_body_that_isnt_one_aes128gcm_record_is_refused() {
        let body = bytes(rfc8291::BODY);
        assert_eq!(Encrypted::parse(&body).unwrap().as_bytes(), body.as_slice());

        assert_eq!(
            Encrypted::parse(&body[..HEADER_LEN + TAG_LEN]),
            Err(EncryptedError::Truncated)
        );
        assert_eq!(
            Encrypted::parse(&body[..10]),
            Err(EncryptedError::Truncated)
        );
        assert_eq!(Encrypted::parse(&[]), Err(EncryptedError::Truncated));

        let mut key_id = body.clone();
        key_id[20] = 32;
        assert_eq!(Encrypted::parse(&key_id), Err(EncryptedError::KeyId(32)));

        let mut small_records = body.clone();
        small_records[16..20].copy_from_slice(&17u32.to_be_bytes());
        assert_eq!(
            Encrypted::parse(&small_records),
            Err(EncryptedError::RecordSize(17))
        );

        // The example's record is 58 bytes: with a record size of 18 it
        // would be several records.
        let mut several = body.clone();
        several[16..20].copy_from_slice(&18u32.to_be_bytes());
        assert_eq!(
            Encrypted::parse(&several),
            Err(EncryptedError::MultipleRecords)
        );

        let over = vec![0u8; MAX_BODY + 1];
        assert_eq!(
            Encrypted::parse(&over),
            Err(EncryptedError::TooLarge(MAX_BODY + 1))
        );
    }
}
