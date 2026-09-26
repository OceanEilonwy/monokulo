//! The challenge a client past its soft limit must solve (step 9d).
//!
//! A challenge is a signed, short-lived string - no per-challenge server
//! state until it is redeemed:
//!
//! ```text
//! <hex(payload)>.<hex(HMAC-SHA256(key, payload))>
//! payload = "pow|<difficulty>|<expires_unix>|<client>|<random>"
//! ```
//!
//! The answer is a nonce (decimal) such that `SHA-256(challenge ‖ nonce)`
//! has at least `difficulty` leading zero bits. The proof sent back is
//! `<challenge>.<nonce>` (the `Monokulo-Proof` header or the `monokulo_proof`
//! query parameter).
//!
//! Without JavaScript nobody can compute that, so a page instead gets a
//! *wait token* with the same shape and `payload = "wait|<not_before>|
//! <expires_unix>|<client>|<random>"`: it only redeems from `not_before`
//! (10 seconds after issue). Waiting is the proof.
//!
//! Every token names the client it was issued to and only redeems for that
//! client, and each redeems once: redeemed tokens are remembered until they
//! expire (memory-capped; when full of unexpired tokens further redemptions
//! are refused rather than forgetting one, which would allow a replay).
//!
//! The key is random per process, so a restart simply invalidates
//! outstanding challenges (clients get a new one).

use std::collections::HashMap;
use std::sync::Mutex;

use hmac::{Hmac, KeyInit, Mac};
use rand::Rng;
use sha2::{Digest, Sha256};

use super::ClientIdentity;

type HmacSha256 = Hmac<Sha256>;

/// How long a proof-of-work challenge may be solved and redeemed in.
pub const CHALLENGE_TTL_SECS: i64 = 5 * 60;
/// How long a no-JavaScript visitor waits.
pub const WAIT_SECS: i64 = 10;
/// How long after `not_before` a wait token still redeems.
pub const WAIT_TTL_SECS: i64 = 5 * 60;
/// Redeemed tokens remembered at once (see the module doc comment).
pub const MAX_REDEEMED: usize = 200_000;

#[derive(Debug, PartialEq, Eq)]
pub enum RedeemError {
    Malformed,
    BadSignature,
    Expired,
    TooEarly,
    WrongClient,
    NotSolved,
    Replayed,
    Full,
}

impl RedeemError {
    pub fn message(&self) -> &'static str {
        match self {
            RedeemError::Malformed | RedeemError::BadSignature => "The challenge answer is not valid.",
            RedeemError::Expired => "The challenge expired. Please try again.",
            RedeemError::TooEarly => "Please wait a few more seconds.",
            RedeemError::WrongClient => "The challenge was issued to a different connection.",
            RedeemError::NotSolved => "The challenge answer is not correct.",
            RedeemError::Replayed => "The challenge was already used.",
            RedeemError::Full => "Too many challenges right now. Please try again shortly.",
        }
    }
}

pub struct Challenges {
    key: [u8; 32],
    redeemed: Mutex<HashMap<String, i64>>,
    max_redeemed: usize,
}

impl Default for Challenges {
    fn default() -> Self {
        let mut key = [0u8; 32];
        rand::rng().fill_bytes(&mut key);
        Challenges::with_key(key, MAX_REDEEMED)
    }
}

/// A challenge as the JSON API and the interstitial page present it.
#[derive(Clone, Debug, serde::Serialize)]
pub struct IssuedChallenge {
    pub challenge: String,
    pub difficulty: u32,
    pub expires_in: i64,
}

impl Challenges {
    pub fn with_key(key: [u8; 32], max_redeemed: usize) -> Self {
        Challenges { key, redeemed: Mutex::new(HashMap::new()), max_redeemed }
    }

    fn sign(&self, payload: &str) -> String {
        let mut mac = HmacSha256::new_from_slice(&self.key).expect("HMAC accepts any key length");
        mac.update(payload.as_bytes());
        format!("{}.{}", hex::encode(payload), hex::encode(mac.finalize().into_bytes()))
    }

    fn random() -> String {
        let mut bytes = [0u8; 12];
        rand::rng().fill_bytes(&mut bytes);
        hex::encode(bytes)
    }

    /// A proof-of-work challenge for `client`.
    pub fn issue(&self, client: &ClientIdentity, difficulty: u32, now: i64) -> IssuedChallenge {
        let expires = now + CHALLENGE_TTL_SECS;
        let challenge = self.sign(&format!("pow|{difficulty}|{expires}|{client}|{}", Self::random()));
        IssuedChallenge { challenge, difficulty, expires_in: CHALLENGE_TTL_SECS }
    }

    /// A wait token for `client`, redeemable from `now + WAIT_SECS`.
    pub fn issue_wait(&self, client: &ClientIdentity, now: i64) -> String {
        let not_before = now + WAIT_SECS;
        self.sign(&format!("wait|{not_before}|{}|{client}|{}", not_before + WAIT_TTL_SECS, Self::random()))
    }

    /// Checks a signed token and returns its payload fields.
    fn open(&self, token: &str) -> Result<Vec<String>, RedeemError> {
        let (payload_hex, mac_hex) = token.split_once('.').ok_or(RedeemError::Malformed)?;
        let payload = hex::decode(payload_hex).map_err(|_| RedeemError::Malformed)?;
        let mac_bytes = hex::decode(mac_hex).map_err(|_| RedeemError::Malformed)?;
        let mut mac = HmacSha256::new_from_slice(&self.key).expect("HMAC accepts any key length");
        mac.update(&payload);
        mac.verify_slice(&mac_bytes).map_err(|_| RedeemError::BadSignature)?;
        let payload = String::from_utf8(payload).map_err(|_| RedeemError::Malformed)?;
        let fields: Vec<String> = payload.split('|').map(str::to_string).collect();
        if fields.len() != 5 {
            return Err(RedeemError::Malformed);
        }
        Ok(fields)
    }

    fn remember(&self, token: &str, expires: i64, now: i64) -> Result<(), RedeemError> {
        let mut redeemed = self.redeemed.lock().unwrap();
        if redeemed.contains_key(token) {
            return Err(RedeemError::Replayed);
        }
        if redeemed.len() >= self.max_redeemed {
            redeemed.retain(|_, until| *until > now);
            if redeemed.len() >= self.max_redeemed {
                return Err(RedeemError::Full);
            }
        }
        redeemed.insert(token.to_string(), expires);
        Ok(())
    }

    /// Redeems a proof `<challenge>.<nonce>` for `client`.
    pub fn redeem_proof(&self, proof: &str, client: &ClientIdentity, now: i64) -> Result<(), RedeemError> {
        let (challenge, nonce) = proof.trim().rsplit_once('.').ok_or(RedeemError::Malformed)?;
        if nonce.is_empty() || nonce.len() > 20 || !nonce.bytes().all(|b| b.is_ascii_digit()) {
            return Err(RedeemError::Malformed);
        }
        let fields = self.open(challenge)?;
        if fields[0] != "pow" {
            return Err(RedeemError::Malformed);
        }
        let difficulty: u32 = fields[1].parse().map_err(|_| RedeemError::Malformed)?;
        let expires: i64 = fields[2].parse().map_err(|_| RedeemError::Malformed)?;
        if now > expires {
            return Err(RedeemError::Expired);
        }
        if fields[3] != client.to_string() {
            return Err(RedeemError::WrongClient);
        }
        if leading_zero_bits(&Sha256::digest(format!("{challenge}{nonce}").as_bytes())) < difficulty {
            return Err(RedeemError::NotSolved);
        }
        self.remember(challenge, expires, now)
    }

    /// Redeems a wait token for `client`.
    pub fn redeem_wait(&self, token: &str, client: &ClientIdentity, now: i64) -> Result<(), RedeemError> {
        let fields = self.open(token.trim())?;
        if fields[0] != "wait" {
            return Err(RedeemError::Malformed);
        }
        let not_before: i64 = fields[1].parse().map_err(|_| RedeemError::Malformed)?;
        let expires: i64 = fields[2].parse().map_err(|_| RedeemError::Malformed)?;
        if now < not_before {
            return Err(RedeemError::TooEarly);
        }
        if now > expires {
            return Err(RedeemError::Expired);
        }
        if fields[3] != client.to_string() {
            return Err(RedeemError::WrongClient);
        }
        self.remember(token.trim(), expires, now)
    }
}

pub fn leading_zero_bits(hash: &[u8]) -> u32 {
    let mut bits = 0;
    for byte in hash {
        if *byte == 0 {
            bits += 8;
        } else {
            return bits + byte.leading_zeros();
        }
    }
    bits
}

/// Finds a nonce for `challenge` at `difficulty` (tests, and the Tor e2e
/// test's stand-in for a browser).
pub fn solve(challenge: &str, difficulty: u32) -> String {
    (0u64..)
        .map(|n| n.to_string())
        .find(|nonce| leading_zero_bits(&Sha256::digest(format!("{challenge}{nonce}").as_bytes())) >= difficulty)
        .expect("a nonce exists")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(n: u32) -> ClientIdentity {
        ClientIdentity::Circuit(n)
    }

    #[test]
    fn a_solved_challenge_redeems_once_for_its_own_client_only() {
        let challenges = Challenges::with_key([1; 32], 100);
        let issued = challenges.issue(&client(1), 8, 1000);
        let proof = format!("{}.{}", issued.challenge, solve(&issued.challenge, 8));
        assert_eq!(challenges.redeem_proof(&proof, &client(2), 1001), Err(RedeemError::WrongClient));
        assert_eq!(challenges.redeem_proof(&proof, &client(1), 1001), Ok(()));
        assert_eq!(challenges.redeem_proof(&proof, &client(1), 1002), Err(RedeemError::Replayed));
    }

    #[test]
    fn unsolved_expired_forged_and_malformed_proofs_are_refused() {
        let challenges = Challenges::with_key([1; 32], 100);
        let issued = challenges.issue(&client(1), 12, 1000);
        let nonce = solve(&issued.challenge, 12);
        // A nonce that doesn't meet the difficulty.
        let wrong = (0u64..)
            .map(|n| n.to_string())
            .find(|n| leading_zero_bits(&Sha256::digest(format!("{}{n}", issued.challenge).as_bytes())) < 12)
            .unwrap();
        assert_eq!(challenges.redeem_proof(&format!("{}.{wrong}", issued.challenge), &client(1), 1001), Err(RedeemError::NotSolved));
        assert_eq!(
            challenges.redeem_proof(&format!("{}.{nonce}", issued.challenge), &client(1), 1000 + CHALLENGE_TTL_SECS + 1),
            Err(RedeemError::Expired)
        );
        // Signed with another process's key.
        let other = Challenges::with_key([2; 32], 100).issue(&client(1), 1, 1000);
        let other_nonce = solve(&other.challenge, 1);
        assert_eq!(challenges.redeem_proof(&format!("{}.{other_nonce}", other.challenge), &client(1), 1001), Err(RedeemError::BadSignature));
        // A lowered difficulty can't be forged into the payload.
        let (payload, mac) = issued.challenge.split_once('.').unwrap();
        let tampered = String::from_utf8(hex::decode(payload).unwrap()).unwrap().replacen("|12|", "|1|", 1);
        let forged = format!("{}.{mac}", hex::encode(tampered));
        assert_eq!(challenges.redeem_proof(&format!("{forged}.{}", solve(&forged, 1)), &client(1), 1001), Err(RedeemError::BadSignature));
        for bad in ["", "nonsense", "zz.zz.1", &format!("{}.abc", issued.challenge)] {
            assert_eq!(challenges.redeem_proof(bad, &client(1), 1001), Err(RedeemError::Malformed), "{bad:?}");
        }
    }

    #[test]
    fn a_wait_token_only_redeems_after_its_wait_and_once() {
        let challenges = Challenges::with_key([1; 32], 100);
        let token = challenges.issue_wait(&client(1), 1000);
        assert_eq!(challenges.redeem_wait(&token, &client(1), 1000 + WAIT_SECS - 1), Err(RedeemError::TooEarly));
        assert_eq!(challenges.redeem_wait(&token, &client(2), 1000 + WAIT_SECS), Err(RedeemError::WrongClient));
        assert_eq!(challenges.redeem_wait(&token, &client(1), 1000 + WAIT_SECS), Ok(()));
        assert_eq!(challenges.redeem_wait(&token, &client(1), 1000 + WAIT_SECS + 1), Err(RedeemError::Replayed));
        let late = challenges.issue_wait(&client(1), 1000);
        assert_eq!(challenges.redeem_wait(&late, &client(1), 1000 + WAIT_SECS + WAIT_TTL_SECS + 1), Err(RedeemError::Expired));
        // A proof-of-work challenge is not a wait token, and vice versa.
        let pow = challenges.issue(&client(1), 1, 1000);
        assert_eq!(challenges.redeem_wait(&pow.challenge, &client(1), 1100), Err(RedeemError::Malformed));
        assert_eq!(challenges.redeem_proof(&format!("{late}.1"), &client(1), 1011), Err(RedeemError::Malformed));
    }

    #[test]
    fn the_replay_memory_is_capped_and_fails_closed() {
        let challenges = Challenges::with_key([1; 32], 2);
        for n in 0..2 {
            let token = challenges.issue_wait(&client(n), 0);
            challenges.redeem_wait(&token, &client(n), WAIT_SECS).unwrap();
        }
        let third = challenges.issue_wait(&client(3), 0);
        assert_eq!(challenges.redeem_wait(&third, &client(3), WAIT_SECS), Err(RedeemError::Full));
        // Once the remembered ones expire there is room again.
        let fourth = challenges.issue_wait(&client(4), 1000);
        assert_eq!(challenges.redeem_wait(&fourth, &client(4), 1000 + WAIT_SECS), Ok(()));
    }

    #[test]
    fn leading_zero_bits_counts_across_bytes() {
        assert_eq!(leading_zero_bits(&[0, 0, 0x0f]), 20);
        assert_eq!(leading_zero_bits(&[0x80]), 0);
        assert_eq!(leading_zero_bits(&[0, 1]), 15);
    }
}
