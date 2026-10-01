//! Webhook delivery signing and SSRF validation. See `docs/DESIGN.md` §11.
//!
//! The HTTP delivery worker itself (retry/backoff against a real `reqwest` client)
//! is not implemented in this pass - these are the two pieces of logic worth getting
//! right and testing in isolation first: how a delivery is authenticated, and how a
//! merchant-supplied URL is checked before this server ever makes a request to it.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

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

/// Whether `ip` is loopback, private, link-local or otherwise no place to
/// send a webhook. Checked by the delivery client's resolver for every
/// address a webhook's host resolves to, at connect time (DNS can change
/// between registration and delivery), and for an IP literal before the
/// request - see `docs/DESIGN.md` §11.
pub fn is_disallowed_address(ip: IpAddr) -> bool {
    // An IPv4-mapped IPv6 address (`::ffff:127.0.0.1`) is a completely ordinary way
    // to reach 127.0.0.1, but none of the `Ipv6Addr` predicates below recognize it -
    // classified as v6 it looks like an unremarkable global address. Unmap first so
    // exactly one set of rules applies to any given destination, regardless of which
    // family a resolver happened to express it in.
    //
    // `to_ipv4`, not `to_ipv4_mapped`: the deprecated IPv4-*compatible* form
    // (`::127.0.0.1`, i.e. `::/96` without the `ffff` marker) embeds a v4 address just
    // as literally, and unmapping only the `::ffff:` form left it classified as an
    // ordinary global v6 address - one character away from the same bypass, in the
    // fix meant to close it. The two v6 addresses this additionally folds into v4,
    // `::` and `::1`, land on 0.0.0.0 and 0.0.0.1, both of which the v4 arm rejects.
    // Likewise a v6 address that is a v4 one in a translation prefix: NAT64
    // (64:ff9b::/96, RFC 6052, and the local-use 64:ff9b:1::/48 of RFC 8215,
    // made for translating to private v4), 6to4 (2002::/16, the v4 address
    // in bits 16-47) and Teredo (2001::/32, the server's v4 address in the
    // last 32 bits, inverted). On a host with such a translator, a
    // connection to one of these lands on the embedded v4 address.
    let ip = match ip {
        IpAddr::V6(v6) => match v6.to_ipv4().or_else(|| embedded_ipv4(v6)) {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        v4 => v4,
    };
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                // "This network" (0.0.0.0/8), not merely the single unspecified
                // address `is_unspecified` covers. A destination of 0.0.0.0 is
                // routed to the local host by every mainstream stack - a
                // well-travelled way to say "localhost" without writing 127 - and
                // the rest of the /8 is unrouteable junk with no legitimate use as
                // a webhook target either way.
                || o[0] == 0
                // Carrier-grade NAT (100.64.0.0/10). Not "private" by
                // `Ipv4Addr::is_private`'s RFC 1918 definition, but just as much a
                // neighbouring-host address space on any deployment behind a
                // CGNAT'd connection - which a self-hosted router install very
                // plausibly is.
                || (o[0] == 100 && (o[1] & 0xc0) == 0x40)
                // Cloud metadata endpoints (AWS/GCP/Azure all use 169.254.169.254,
                // already covered by is_link_local, but called out explicitly since
                // it's the single most consequential SSRF target for a hosted
                // deployment).
                || o == [169, 254, 169, 254]
                // Benchmarking (198.18.0.0/15, RFC 2544). Routed into real lab
                // networks often enough that treating it as public is a needless
                // gamble.
                || (o[0] == 198 && (o[1] & 0xfe) == 18)
                // Multicast (224.0.0.0/4) and the reserved class E space
                // (240.0.0.0/4, which `is_broadcast` only covers the last address
                // of). Neither is a meaningful destination for an outbound TCP
                // connection, so anything asking for one is probing, not
                // integrating.
                || o[0] >= 224
        }
        IpAddr::V6(v6) => {
            let [first, second, ..] = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                || (first & 0xfe00) == 0xfc00 // unique local (fc00::/7)
                || (first & 0xffc0) == 0xfe80 // link-local (fe80::/10) - the v6 counterpart of 169.254.0.0/16
                || (first & 0xff00) == 0xff00 // multicast (ff00::/8) - the v6 counterpart of 224.0.0.0/4
                || (first == 0x0100 && second == 0) // discard-only (100::/64, RFC 6666)
        }
    }
}

/// The IPv4 address a translation-prefix IPv6 address stands for, if it is
/// one: NAT64 (`64:ff9b::/96` and `64:ff9b:1::/48`), 6to4 (`2002::/16`) or
/// Teredo (`2001::/32`).
fn embedded_ipv4(v6: Ipv6Addr) -> Option<Ipv4Addr> {
    let segments = v6.segments();
    let octets = v6.octets();
    let last_four = |invert: bool| {
        let mut bytes = [octets[12], octets[13], octets[14], octets[15]];
        if invert {
            bytes = bytes.map(|b| !b);
        }
        Ipv4Addr::from(bytes)
    };
    match segments {
        [0x0064, 0xff9b, 0, 0, 0, 0, _, _] => Some(last_four(false)),
        [0x0064, 0xff9b, 0x0001, ..] => Some(last_four(false)),
        [0x2002, ..] => Some(Ipv4Addr::from([octets[2], octets[3], octets[4], octets[5]])),
        [0x2001, 0x0000, ..] => Some(last_four(true)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the delivery path does with a resolved address.
    #[derive(Debug, PartialEq)]
    enum Refused {
        PrivateAddress,
    }

    fn check(ip: IpAddr) -> Result<(), Refused> {
        if is_disallowed_address(ip) {
            Err(Refused::PrivateAddress)
        } else {
            Ok(())
        }
    }

    const T: i64 = 1_700_000_000;

    #[test]
    fn signature_matches_a_fixed_test_vector() {
        // A regression/drift guard: if `sign_payload` ever changes its output for
        // this exact input, this test catches it immediately. Cross-checked with
        // Python's stdlib: hmac.new(secret, b"1700000000." + payload,
        // hashlib.sha256).hexdigest().
        let secret = "whsec_test_vector_secret";
        let payload = br#"{"event":"order.paid","order_id":"order_test123"}"#;
        let signature = sign_payload(secret, T, payload);
        assert_eq!(
            signature,
            "t=1700000000,v1=ee45d544d20342265419fe84e4ad7553640a8b069efac546af2c3d9dabc271fc"
        );
        assert!(verify_signature(secret, payload, &signature, T));
    }

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

    #[test]
    fn public_https_url_with_a_public_ip_is_allowed() {
        assert!(check(IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34))).is_ok());
    }

    #[test]
    fn loopback_resolved_address_is_rejected() {
        let err = check(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))).unwrap_err();
        assert_eq!(err, Refused::PrivateAddress);
    }

    #[test]
    fn private_lan_resolved_address_is_rejected() {
        for ip in [
            Ipv4Addr::new(10, 0, 0, 1),
            Ipv4Addr::new(192, 168, 1, 1),
            Ipv4Addr::new(172, 16, 0, 1),
        ] {
            let err = check(IpAddr::V4(ip)).unwrap_err();
            assert_eq!(err, Refused::PrivateAddress);
        }
    }

    #[test]
    fn cloud_metadata_address_is_rejected() {
        let err = check(IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254))).unwrap_err();
        assert_eq!(err, Refused::PrivateAddress);
    }

    #[test]
    fn ipv4_mapped_ipv6_loopback_is_rejected() {
        // `::ffff:127.0.0.1` reaches 127.0.0.1 exactly like the plain v4 literal
        // does, but none of the `Ipv6Addr` predicates recognize it - classified as a
        // v6 address it looks entirely global, so this was a one-character bypass of
        // every check in this function. Unmapping before classifying is what makes
        // the rules independent of which family the resolver happened to answer in.
        for mapped in [
            "::ffff:127.0.0.1",       // loopback
            "::ffff:10.0.0.1",        // RFC 1918
            "::ffff:169.254.169.254", // cloud metadata
        ] {
            let err = check(mapped.parse().unwrap()).unwrap_err();
            assert_eq!(err, Refused::PrivateAddress, "{mapped} must be rejected");
        }
    }

    #[test]
    fn ipv6_link_local_is_rejected() {
        // fe80::/10 - the v6 counterpart of 169.254.0.0/16, and the same
        // neighbouring-host reachability concern.
        for ip in ["fe80::1", "febf:ffff::1"] {
            let err = check(ip.parse().unwrap()).unwrap_err();
            assert_eq!(err, Refused::PrivateAddress, "{ip} must be rejected");
        }
        // The neighbouring prefix is genuinely global and must still be allowed -
        // this is a /10, not a /16.
        assert!(check("fec0::1".parse().unwrap()).is_ok());
    }

    #[test]
    fn cgnat_shared_address_space_is_rejected() {
        // 100.64.0.0/10. Not "private" by RFC 1918's definition (so
        // `Ipv4Addr::is_private` says nothing about it) but just as much a
        // neighbouring-host range on any deployment behind a carrier-grade NAT,
        // which a self-hosted router install plausibly is.
        for ip in [
            Ipv4Addr::new(100, 64, 0, 1),
            Ipv4Addr::new(100, 127, 255, 254),
        ] {
            let err = check(IpAddr::V4(ip)).unwrap_err();
            assert_eq!(err, Refused::PrivateAddress, "{ip} must be rejected");
        }
        // Immediately either side of the /10 is ordinary public space.
        for ip in [
            Ipv4Addr::new(100, 63, 255, 255),
            Ipv4Addr::new(100, 128, 0, 1),
        ] {
            assert!(
                check(IpAddr::V4(ip)).is_ok(),
                "{ip} is outside 100.64.0.0/10 and must still be allowed"
            );
        }
    }

    #[test]
    fn the_whole_zero_network_is_rejected_not_just_the_unspecified_address() {
        // `0.0.0.0` is a well-travelled way of writing "localhost" without writing
        // 127 - every mainstream stack routes a connection to it at the local host -
        // and `Ipv4Addr::is_unspecified` covers only that single address, leaving the
        // rest of 0.0.0.0/8 classified as ordinary public space.
        for ip in [
            Ipv4Addr::new(0, 0, 0, 0),
            Ipv4Addr::new(0, 0, 0, 1),
            Ipv4Addr::new(0, 255, 255, 254),
        ] {
            let err = check(IpAddr::V4(ip)).unwrap_err();
            assert_eq!(err, Refused::PrivateAddress, "{ip} must be rejected");
        }
        // 1.0.0.0 is the first address outside the /8 and is genuinely routable.
        assert!(check(IpAddr::V4(Ipv4Addr::new(1, 0, 0, 1))).is_ok());
    }

    #[test]
    fn ipv4_compatible_ipv6_is_unmapped_exactly_like_the_ipv4_mapped_form() {
        // `::127.0.0.1` (the deprecated IPv4-*compatible* form, `::/96` with no
        // `ffff` marker) embeds a v4 address just as literally as `::ffff:127.0.0.1`
        // does, but `Ipv6Addr::to_ipv4_mapped` returns `None` for it - so unmapping
        // only the `ffff` form left this one classified as an unremarkable global v6
        // address, one character away from the same bypass the unmapping exists to
        // close.
        for compat in [
            "::127.0.0.1",
            "::10.0.0.1",
            "::169.254.169.254",
            "::0.0.0.1",
        ] {
            let err = check(compat.parse().unwrap()).unwrap_err();
            assert_eq!(err, Refused::PrivateAddress, "{compat} must be rejected");
        }
    }

    /// A v6 address that a translator turns into a private v4 one is that
    /// v4 address: NAT64 (well-known and local-use prefixes), 6to4 and
    /// Teredo embed it, and each is classified by what it embeds.
    #[test]
    fn translation_prefixes_are_classified_by_the_ipv4_address_they_embed() {
        for (private, public) in [
            ("64:ff9b::7f00:1", "64:ff9b::0808:0808"), // NAT64 well-known
            ("64:ff9b:1::a00:1", "64:ff9b:1::0808:0808"), // NAT64 local-use
            ("2002:7f00:1::", "2002:808:808::"),       // 6to4
            ("2001::80ff:fffe", "2001::f7f7:f7f7"),    // Teredo (server inverted)
        ] {
            assert!(
                is_disallowed_address(private.parse().unwrap()),
                "{private} embeds a private address"
            );
            assert!(
                !is_disallowed_address(public.parse().unwrap()),
                "{public} embeds a public address"
            );
        }
        // The discard prefix is no destination either.
        assert!(is_disallowed_address("100::1".parse().unwrap()));
    }

    #[test]
    fn multicast_and_reserved_ranges_are_rejected_in_both_families() {
        // Nothing legitimate ever registers a webhook here; anything that does is
        // probing the classifier rather than integrating with it.
        for ip in [
            "224.0.0.1",       // v4 multicast, 224.0.0.0/4
            "239.255.255.250", // SSDP, the same /4
            "240.0.0.1",       // reserved class E, 240.0.0.0/4
            "255.255.255.255", // broadcast
            "198.18.0.1",      // benchmarking, 198.18.0.0/15
            "198.19.255.255",  // ...and its far end
            "ff02::1",         // v6 multicast, ff00::/8
        ] {
            let err = check(ip.parse().unwrap()).unwrap_err();
            assert_eq!(err, Refused::PrivateAddress, "{ip} must be rejected");
        }
        // Immediately either side of 198.18.0.0/15 is ordinary public space.
        for ip in ["198.17.255.255", "198.20.0.1", "223.255.255.255"] {
            assert!(
                check(ip.parse().unwrap()).is_ok(),
                "{ip} is outside every blocked range and must still be allowed"
            );
        }
    }

    // --- Cross-language known-vector (WBS 0.3 / 1.5.4) -----------------------------
    //
    // This secret/payload/signature triple is what the WooCommerce plugin's own PHP
    // test (`WebhookSignatureTest.php`) mirrors, to check its HMAC-SHA256 signing
    // against this Rust implementation - a regression/drift guard for both sides, not
    // an external contract: if `sign_payload` genuinely changes, update this constant
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

    #[test]
    fn known_vector_for_cross_language_php_verification() {
        // Produced by `sign_payload` and checked against Python's and PHP's own
        // HMAC-SHA256 of the same input; the PHP test carries the same vector.
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
    }
}
