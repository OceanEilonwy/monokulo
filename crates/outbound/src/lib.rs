//! Requests to addresses other people give: a store's webhook endpoint, a
//! connected app's push endpoint, a till's ding endpoint. Any of them
//! could name an address inside the network the sender runs in, so they
//! all go out the same way, in monokulo and in the Monokular Bell.
//!
//! Nothing private: a URL whose host is, or resolves to, a loopback,
//! private, link-local or otherwise internal address
//! ([`is_disallowed_address`]), or is `localhost` by name, is refused at
//! every request (DNS can change between requests), redirects aren't
//! followed, and no proxy is used, unless the caller's setting allows any
//! address ([`Reach::AnyAddress`]). An onion address is refused whatever
//! the setting: nothing here is sent over Tor, and asking DNS for one
//! would tell the resolver the address.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

mod address;

pub use address::{embedded_ipv4, is_disallowed_address};

/// Which addresses a request may go to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// Public addresses only.
    Public,
    /// Private, loopback and link-local ones too: only for testing against
    /// one's own network.
    AnyAddress,
}

impl Reach {
    /// The reach a setting allowing private addresses gives
    /// (`webhooks.allow_private_urls`, `bell.allow_private_endpoints`).
    pub fn allowing_private(allow_private: bool) -> Self {
        if allow_private {
            Reach::AnyAddress
        } else {
            Reach::Public
        }
    }
}

/// A client builder for requests allowed `reach`: redirects off (one to a
/// private address must not be followed), the checking resolver, and no
/// proxy for [`Reach::Public`] (a proxy connects on the sender's behalf,
/// past the resolver). The resolver is what makes the check hold: the
/// addresses a name resolves to are checked by the very lookup the
/// connection is made from, so a name can't answer the check with one
/// address and the connection with another, and a name with any private
/// address among its answers isn't connected to at all. A request on the
/// client still needs its URL's host checked first ([`check_host`]): an
/// IP literal never reaches a resolver.
pub fn client_builder(reach: Reach) -> reqwest::ClientBuilder {
    let builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .dns_resolver(Arc::new(CheckingResolver { reach }));
    match reach {
        Reach::Public => builder.no_proxy(),
        Reach::AnyAddress => builder,
    }
}

/// The system resolver, with every answer checked before it is connected
/// to. An IP literal in a URL never reaches a resolver (the connector
/// parses it first): [`check_host`] checks those.
struct CheckingResolver {
    reach: Reach,
}

impl reqwest::dns::Resolve for CheckingResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let reach = self.reach;
        Box::pin(async move {
            // An onion name is never looked up (RFC 7686): asking DNS for it
            // would tell the resolver the address, and find nothing.
            if is_onion_host(name.as_str()) {
                return Err(BoxError::from(OutboundError::OnionHost));
            }
            // Port 0: the connector sets the URL's port on each address.
            let addrs: Vec<SocketAddr> =
                tokio::net::lookup_host((name.as_str(), 0)).await?.collect();
            if addrs.is_empty() {
                return Err(BoxError::from(OutboundError::UnresolvableHost(
                    name.as_str().to_owned(),
                )));
            }
            // Every answer has to pass: a name resolving to both a public
            // and a private address is the rebinding this defends against.
            if reach == Reach::Public && addrs.iter().any(|addr| is_disallowed_address(addr.ip())) {
                return Err(BoxError::from(OutboundError::PrivateAddress));
            }
            Ok::<reqwest::dns::Addrs, BoxError>(Box::new(addrs.into_iter()))
        })
    }
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Why a request wasn't made to a URL.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OutboundError {
    #[error("the URL's host could not be resolved: {0}")]
    UnresolvableHost(String),
    #[error("the URL is, or resolves to, a private or loopback address")]
    PrivateAddress,
    #[error("the URL is on an onion address, and nothing is sent over Tor from here")]
    OnionHost,
}

/// Refuses a URL on an onion address, so not even a proxy is asked for
/// it, and, unless `reach` allows any address, one whose host is an IP
/// literal of a private, loopback or link-local address, or `localhost`
/// by name: the half of the check a resolver never sees, and the name
/// that is loopback without asking one.
pub fn check_host(url: &url::Url, reach: Reach) -> Result<(), OutboundError> {
    if url.host_str().is_some_and(is_onion_host) {
        return Err(OutboundError::OnionHost);
    }
    if reach == Reach::AnyAddress {
        return Ok(());
    }
    let ip = match url.host() {
        None => return Err(OutboundError::UnresolvableHost("no host".into())),
        Some(url::Host::Ipv4(ip)) => IpAddr::V4(ip),
        Some(url::Host::Ipv6(ip)) => IpAddr::V6(ip),
        Some(url::Host::Domain(name)) if is_localhost(name) => {
            return Err(OutboundError::PrivateAddress);
        }
        Some(url::Host::Domain(_)) => return Ok(()),
    };
    if is_disallowed_address(ip) {
        return Err(OutboundError::PrivateAddress);
    }
    Ok(())
}

/// Whether `host` is an onion name: `onion` itself or any name under it,
/// in any case, with or without the root's dot (RFC 7686). Valid or not:
/// a mistyped onion address is no more a name to look up.
pub fn is_onion_host(host: &str) -> bool {
    let host = host.strip_suffix('.').unwrap_or(host);
    host.eq_ignore_ascii_case("onion")
        || host
            .len()
            .checked_sub(".onion".len())
            .and_then(|start| host.get(start..))
            .is_some_and(|suffix| suffix.eq_ignore_ascii_case(".onion"))
}

/// `localhost` and every name under it, in any case, with or without the
/// root's dot: loopback whatever a resolver says (RFC 6761 §6.3), so
/// refused without asking one.
pub fn is_localhost(name: &str) -> bool {
    let name = name.strip_suffix('.').unwrap_or(name);
    name.eq_ignore_ascii_case("localhost")
        || name
            .len()
            .checked_sub(".localhost".len())
            .and_then(|start| name.get(start..))
            .is_some_and(|suffix| suffix.eq_ignore_ascii_case(".localhost"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The check is the resolver's: the addresses a name resolves to are
    /// what the connection is made to, and a name with a private address
    /// among its answers is refused before anything connects. Checking
    /// with one lookup and connecting with another would be no check
    /// against whoever controls the name's DNS (a zero-TTL record, or a
    /// round-robin between a public and a private address).
    #[tokio::test]
    async fn the_resolver_refuses_a_name_with_a_private_address_and_answers_otherwise() {
        use reqwest::dns::Resolve as _;
        let name = |host: &str| host.parse::<reqwest::dns::Name>().unwrap();
        let public = CheckingResolver {
            reach: Reach::Public,
        };
        let refused = match public.resolve(name("localhost")).await {
            Err(e) => e.to_string(),
            Ok(_) => panic!("a loopback answer must be refused"),
        };
        assert!(refused.contains("private or loopback"), "{refused}");

        let any = CheckingResolver {
            reach: Reach::AnyAddress,
        };
        let addrs: Vec<_> = any.resolve(name("localhost")).await.unwrap().collect();
        assert!(
            addrs.iter().all(|addr| addr.ip().is_loopback()),
            "{addrs:?}"
        );
        assert!(
            addrs.iter().all(|addr| addr.port() == 0),
            "the connector sets the URL's port: {addrs:?}"
        );

        // An onion name is refused as one, whatever the reach, before any
        // lookup: a lookup would have failed with the system resolver's own
        // error, after asking its DNS for the name.
        for onion in [
            "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion",
            "SHOP.2GZYXA5IHM7NSGGFXNU52RCK2VV4RVMDLKIU3ZZUI5DU4XYCLEN53WID.ONION.",
        ] {
            for resolver in [&public, &any] {
                let refused = match resolver.resolve(name(onion)).await {
                    Err(e) => e.to_string(),
                    Ok(_) => panic!("{onion} must not be resolved"),
                };
                assert!(refused.contains("onion address"), "{onion}: {refused}");
            }
        }
    }

    /// An IP literal never reaches the resolver, so it is classified on
    /// its own, as is `localhost` by name; any other name is left to the
    /// resolver, and an onion host is refused whatever the reach.
    #[test]
    fn a_urls_host_is_checked_before_the_request() {
        let url = |u: &str| url::Url::parse(u).unwrap();
        for private in [
            "http://[::1]:9999/hook",
            "http://127.0.0.1/hook",
            "http://169.254.169.254/latest",
            "http://10.0.0.1/hook",
            "http://[::ffff:127.0.0.1]/hook",
            "https://[::ffff:10.0.0.1]/push",
            "https://localhost/push",
            "http://LocalHost:8080/push",
            "https://localhost./push",
            "https://push.localhost/push",
        ] {
            assert_eq!(
                check_host(&url(private), Reach::Public),
                Err(OutboundError::PrivateAddress),
                "{private}"
            );
            assert_eq!(check_host(&url(private), Reach::AnyAddress), Ok(()));
        }
        for public in [
            "https://[2606:4700:4700::1111]/hook",
            "https://1.1.1.1/hook",
            "https://shop.example/hook",
            "https://localhost.example.org/push",
        ] {
            assert_eq!(check_host(&url(public), Reach::Public), Ok(()), "{public}");
        }
        for onion in [
            "http://shop.example.onion/hook",
            "https://EXAMPLE.ONION./push",
        ] {
            for reach in [Reach::Public, Reach::AnyAddress] {
                assert_eq!(
                    check_host(&url(onion), reach),
                    Err(OutboundError::OnionHost),
                    "{onion}"
                );
            }
        }
    }

    #[test]
    fn onion_and_localhost_names_in_any_spelling() {
        for onion in ["onion", "x.onion", "x.onion.", "A.B.ONION", "onion."] {
            assert!(is_onion_host(onion), "{onion}");
        }
        for not in ["onion.example", "xonion", "", "é.onionx"] {
            assert!(!is_onion_host(not), "{not}");
        }
        for local in ["localhost", "LOCALHOST.", "a.localhost"] {
            assert!(is_localhost(local), "{local}");
        }
        for not in ["localhost.example", "mylocalhost", ""] {
            assert!(!is_localhost(not), "{not}");
        }
    }
}
