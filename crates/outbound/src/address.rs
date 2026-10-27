//! Which addresses are no place to send a request: private, loopback,
//! link-local and every other internal range, in either family, an IPv4
//! address inside an IPv6 one judged as the IPv4 address it carries.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Whether `ip` is loopback, private, link-local or otherwise no place to
/// send a request someone else addressed. Checked by the checking
/// resolver for every address a host resolves to, at connect time (DNS
/// can change between registration and delivery), and for an IP literal
/// before the request ([`crate::check_host`]).
pub fn is_disallowed_address(ip: IpAddr) -> bool {
    // An IPv4-mapped IPv6 address (`::ffff:127.0.0.1`) is a completely ordinary way
    // to reach 127.0.0.1, but none of the `Ipv6Addr` predicates below recognize it -
    // classified as v6 it looks like an unremarkable global address. Unmap first so
    // exactly one set of rules applies to any given destination, regardless of which
    // family a resolver happened to express it in.
    //
    // The deprecated IPv4-*compatible* form (`::127.0.0.1`, i.e. `::/96` without the
    // `ffff` marker) embeds a v4 address just as literally, and unmapping only the
    // `::ffff:` form left it classified as an ordinary global v6 address - one
    // character away from the same bypass, in the fix meant to close it. The two v6
    // addresses this additionally folds into v4, `::` and `::1`, land on 0.0.0.0 and
    // 0.0.0.1, both of which the v4 arm rejects. Likewise a v6 address that is a v4
    // one in a translation prefix (NAT64, including the local-use prefix made for
    // translating to private v4, 6to4 and Teredo): on a host with such a translator,
    // a connection to one of these lands on the embedded v4 address. The log
    // redactor uses the same `embedded_ipv4`, so the two agree on which v6
    // addresses are really v4 ones.
    let ip = match ip {
        IpAddr::V6(v6) => match embedded_ipv4(v6) {
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

/// The IPv4 address an IPv6 address carries, if it carries one: the
/// IPv4-mapped (`::ffff:0:0/96`) and the deprecated IPv4-compatible
/// (`::/96`) forms, NAT64 (`64:ff9b::/96`, RFC 6052, and the local-use
/// `64:ff9b:1::/48` of RFC 8215), 6to4 (`2002::/16`, the IPv4 address in
/// bits 16-47) and Teredo (`2001::/32`, the client's IPv4 address in the
/// last 32 bits, inverted). `::` and `::1` come out as 0.0.0.0 and
/// 0.0.0.1.
///
/// The log redactor (`telemetry::redact`) anonymizes the IPv4 address in
/// place of the IPv6 one, and [`is_disallowed_address`] judges a
/// destination by it, so neither lets an IPv4 address through dressed as
/// an IPv6 one.
pub fn embedded_ipv4(v6: Ipv6Addr) -> Option<Ipv4Addr> {
    if let Some(v4) = v6.to_ipv4() {
        return Some(v4);
    }
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

    /// What the checking resolver does with a resolved address.
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

    /// Every address either copy of this rule (monokulo's and the Bell's)
    /// was tested with, in one table.
    #[test]
    fn internal_addresses_are_disallowed_in_either_family_and_public_ones_not() {
        for private in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "0.0.0.0",
            "100.64.0.1",
            "198.18.0.1",
            "224.0.0.1",
            "255.255.255.255",
            "::1",
            "::",
            "fc00::1",
            "fe80::1",
            "ff02::1",
            "100::1",
            "::ffff:127.0.0.1",
            "::127.0.0.1",
            "64:ff9b::a00:1",
            "64:ff9b:1::a00:1",
            "2002:c0a8:101::1",
            // Teredo, carrying 127.0.0.1 inverted.
            "2001:0:4136:e378:8000:63bf:80ff:fffe",
        ] {
            assert!(is_disallowed_address(private.parse().unwrap()), "{private}");
        }
        for public in [
            "93.184.216.34",
            "1.1.1.1",
            "2606:4700:4700::1111",
            "::ffff:8.8.8.8",
            "64:ff9b::808:808",
        ] {
            assert!(!is_disallowed_address(public.parse().unwrap()), "{public}");
        }
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
}
