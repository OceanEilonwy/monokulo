//! Webhook delivery signing: how a delivery is authenticated. Which
//! addresses a delivery may go to is the `outbound` crate's.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// How far a delivery's signed timestamp may be from the receiver's clock,
/// either way, for it to be accepted: five minutes. A captured delivery
/// can be replayed only within this window (and a receiver deduplicating
/// on `event_id` refuses it even then).
pub const SIGNATURE_TOLERANCE_SECS: i64 = 5 * 60;

/// The `X-Monokulo-Signature` header value for a delivery of `payload`
/// sent at `timestamp` (unix seconds): `t=<timestamp>,v1=<hex>`, where
/// `<hex>` is HMAC-SHA256, keyed by the webhook's `signing_secret`, of
/// `"<timestamp>.<payload>"`. The timestamp is signed with the body, so a
/// captured delivery can't be passed off as a new one later. Documented
/// with fixed test vectors so a merchant implementing verification can
/// cross-check their own computation.
pub fn sign_payload(secret: &str, timestamp: i64, payload: &[u8]) -> String {
    format!(
        "t={timestamp},v1={}",
        hex::encode(mac(secret, timestamp, payload).finalize().into_bytes())
    )
}

fn mac(secret: &str, timestamp: i64, payload: &[u8]) -> HmacSha256 {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(payload);
    mac
}

/// Verifies a presented `X-Monokulo-Signature` header for `payload`, as
/// received at `now` (unix seconds): its timestamp must be within
/// [`SIGNATURE_TOLERANCE_SECS`] of `now`, and its `v1` tag must be the HMAC
/// of `"<timestamp>.<payload>"`.
///
/// The tag comparison is constant-time, which is the entire reason this goes
/// through `Mac::verify_slice` (backed by `subtle`'s `ct_eq`) rather than
/// comparing strings. String equality short-circuits at the first differing
/// byte, so the time it takes to reject a forgery is a direct readout of how
/// many leading bytes were right - the standard byte-at-a-time
/// signature-forgery oracle, and a real one here because this is the helper
/// a merchant integrating against this service calls on *their* HTTP
/// endpoint, with an attacker choosing both the payload and the candidate
/// signature and able to re-measure as many times as they like.
///
/// The tag is decoded from hex and compared as raw bytes. A header that
/// isn't exactly `t=<digits>,v1=<64 hex>` is rejected before any
/// comparison: its shape is attacker-supplied public data and reveals
/// nothing.
pub fn verify_signature(secret: &str, payload: &[u8], header: &str, now: i64) -> bool {
    let Some((timestamp, tag)) = parse_header(header) else {
        return false;
    };
    if now.abs_diff(timestamp) > SIGNATURE_TOLERANCE_SECS.unsigned_abs() {
        return false;
    }
    let Ok(presented) = hex::decode(tag) else {
        return false;
    };
    mac(secret, timestamp, payload)
        .verify_slice(&presented)
        .is_ok()
}

/// `t=<timestamp>,v1=<tag>` into its two parts.
fn parse_header(header: &str) -> Option<(i64, &str)> {
    let (t, v1) = header.split_once(',')?;
    let timestamp = t.strip_prefix("t=")?;
    if timestamp.is_empty() || !timestamp.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((timestamp.parse().ok()?, v1.strip_prefix("v1=")?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: i64 = 1_700_000_000;

    #[test]
    fn signature_is_sensitive_to_the_secret_the_payload_and_the_time() {
        let payload = b"{\"event\":\"order.paid\"}";
        let sig_a = sign_payload("secret_a", T, payload);
        assert_ne!(sig_a, sign_payload("secret_b", T, payload));
        assert_ne!(
            sig_a,
            sign_payload("secret_a", T, b"{\"event\":\"order.partial\"}")
        );
        assert_ne!(sig_a, sign_payload("secret_a", T + 1, payload));
    }

    #[test]
    fn verify_rejects_a_tampered_payload_or_timestamp() {
        let secret = "whsec_abc";
        let payload = b"original";
        let signature = sign_payload(secret, T, payload);
        assert!(!verify_signature(secret, b"tampered", &signature, T));
        // The tag of time T presented as another time.
        let moved = signature.replace("t=1700000000", "t=1700000001");
        assert!(!verify_signature(secret, payload, &moved, T));
    }

    /// A delivery is accepted within the tolerance of the receiver's clock,
    /// either way, and refused outside it: a captured delivery replayed
    /// later fails.
    #[test]
    fn verify_refuses_a_delivery_signed_too_long_ago_or_too_far_ahead() {
        let (secret, payload) = ("whsec_abc", b"{}");
        let signature = sign_payload(secret, T, payload);
        assert!(verify_signature(
            secret,
            payload,
            &signature,
            T + SIGNATURE_TOLERANCE_SECS
        ));
        assert!(verify_signature(
            secret,
            payload,
            &signature,
            T - SIGNATURE_TOLERANCE_SECS
        ));
        assert!(!verify_signature(
            secret,
            payload,
            &signature,
            T + SIGNATURE_TOLERANCE_SECS + 1
        ));
        assert!(!verify_signature(
            secret,
            payload,
            &signature,
            T - SIGNATURE_TOLERANCE_SECS - 1
        ));
    }

    #[test]
    fn verify_compares_the_decoded_tag_in_constant_time_and_refuses_every_near_miss() {
        // The vulnerability this pins closed: comparing signatures as strings
        // stops at the first differing byte, turning rejection latency into a
        // readout of how many leading bytes an attacker has guessed.
        //
        // Timing itself isn't assertable in a unit test without flakiness, so what's
        // pinned here is the observable half: every one-byte-off neighbour of a
        // valid tag, every truncation, every malformed header must be rejected,
        // and the genuine signature accepted.
        let secret = "whsec_abc";
        let payload = b"{\"event\":\"order.paid\"}";
        let good = sign_payload(secret, T, payload);
        assert!(verify_signature(secret, payload, &good, T));
        let tag = good.split_once(",v1=").unwrap().1.to_string();
        let with_tag = |tag: &str| format!("t={T},v1={tag}");

        // A tag sharing every byte but the last is exactly the input an
        // early-exit comparison would take longest to reject.
        let mut almost = tag.clone();
        let last = almost.pop().unwrap();
        almost.push(if last == '0' { '1' } else { '0' });
        assert!(!verify_signature(secret, payload, &with_tag(&almost), T));

        // ...and one differing only in the *first* byte, the input it would reject
        // fastest. Both must be equally, unconditionally refused.
        let mut first_off = tag.clone();
        let head = first_off.remove(0);
        first_off.insert(0, if head == '0' { '1' } else { '0' });
        assert!(!verify_signature(secret, payload, &with_tag(&first_off), T));

        for bad in [
            String::new(),
            tag.clone(),                     // no t=
            format!("v1={tag},t={T}"),       // wrong order
            format!("t=,v1={tag}"),          // no time
            format!("t=-5,v1={tag}"),        // not digits
            with_tag(&tag[..2]),             // a correct prefix, far too short
            with_tag(&tag[..tag.len() - 2]), // one byte short
            with_tag(&format!("{tag}00")),   // trailing junk
            with_tag("not hex at all"),
        ] {
            assert!(!verify_signature(secret, payload, &bad, T), "{bad:?}");
        }
        // Valid hex in upper case decodes to the same tag.
        assert!(verify_signature(
            secret,
            payload,
            &with_tag(&tag.to_uppercase()),
            T
        ));
    }

    // --- Cross-language known-vector -------------------------------------------------
    //
    // This secret/payload/signature triple is what the WooCommerce plugin's own PHP
    // test (`WebhookSignatureTest.php`, in OceanEilonwy/monokulo-woocommerce) mirrors,
    // to check its HMAC-SHA256 signing against this Rust implementation - a
    // regression/drift guard for both sides, not an external contract: if `sign_payload` genuinely changes, update this constant
    // (and the PHP copy) deliberately, the same as any other hardcoded expected value.
    //
    // PHP equivalent to check against:
    // `hash_hmac('sha256', KNOWN_VECTOR_TIMESTAMP . '.' . KNOWN_VECTOR_PAYLOAD, KNOWN_VECTOR_SECRET)`.
    const KNOWN_VECTOR_SECRET: &str = "known_vector_secret_for_php_crosscheck";
    const KNOWN_VECTOR_TIMESTAMP: i64 = 1_700_000_000;
    const KNOWN_VECTOR_PAYLOAD: &[u8] =
        br#"{"event":"order.paid","order_id":"12345","amount_piconero":"1000000000000"}"#;
    const KNOWN_VECTOR_SIGNATURE_HEADER: &str =
        "t=1700000000,v1=36a7d36d510620adf9ae9e3e42891dcc796ae88c6ac19818bab778998d2a35e6";

    /// Fixed vectors: `sign_payload`'s output, checked against Python's
    /// and PHP's own HMAC-SHA256 of the same input (the PHP test carries
    /// the first), so a change to it is caught at once.
    #[test]
    fn known_vector_for_cross_language_php_verification() {
        let signature = sign_payload(
            KNOWN_VECTOR_SECRET,
            KNOWN_VECTOR_TIMESTAMP,
            KNOWN_VECTOR_PAYLOAD,
        );
        assert_eq!(signature, KNOWN_VECTOR_SIGNATURE_HEADER);
        assert!(verify_signature(
            KNOWN_VECTOR_SECRET,
            KNOWN_VECTOR_PAYLOAD,
            KNOWN_VECTOR_SIGNATURE_HEADER,
            KNOWN_VECTOR_TIMESTAMP
        ));
        // Python: hmac.new(secret, b"1700000000." + payload, hashlib.sha256).hexdigest()
        let secret = "whsec_test_vector_secret";
        let payload = br#"{"event":"order.paid","order_id":"order_test123"}"#;
        let signature = sign_payload(secret, T, payload);
        assert_eq!(
            signature,
            "t=1700000000,v1=ee45d544d20342265419fe84e4ad7553640a8b069efac546af2c3d9dabc271fc"
        );
        assert!(verify_signature(secret, payload, &signature, T));
    }
}
