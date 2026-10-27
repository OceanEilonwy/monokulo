//! Requests monokulo makes to addresses other people give it: stores'
//! webhook endpoints (`crate::webhooks::delivery`) and connected apps'
//! push endpoints (`crate::push::delivery`). Either could name an address
//! inside the network monokulo runs in, so both go out the same way.
//!
//! Nothing private: a URL whose host is, or resolves to, a loopback,
//! private, link-local or otherwise internal address is refused at every
//! request (DNS can change between requests), redirects aren't followed,
//! and no proxy is used, unless `webhooks.allow_private_urls` is on
//! ([`Reach::AnyAddress`]). An onion address is refused whatever the
//! setting: nothing here is sent over Tor, and asking DNS for one would
//! tell the resolver the address.

use std::sync::Arc;

use credentials::webhook_sign::is_disallowed_address;

/// Which addresses a request may go to, as `webhooks.allow_private_urls`
/// says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// Public addresses only.
    Public,
    /// Private, loopback and link-local ones too: only for testing against
    /// one's own network.
    AnyAddress,
}

impl Reach {
    /// The reach `webhooks.allow_private_urls` gives.
    pub fn allowing_private(allow_private_urls: bool) -> Self {
        if allow_private_urls {
            Reach::AnyAddress
        } else {
            Reach::Public
        }
    }
}

/// The HTTP clients these requests are sent with: one for each [`Reach`],
/// reused across requests (building one reads the system's certificates).
/// Each resolver is what makes the private-address check hold: the
/// addresses a name resolves to are checked by the very lookup the
/// connection is made from, so a name can't answer the check with one
/// address and the connection with another; a name with any private
/// address among its answers isn't connected to at all. Separate pools, so
/// turning `webhooks.allow_private_urls` off can't reuse a connection
/// opened while it was on.
#[derive(Clone)]
pub struct Outbound {
    public: reqwest::Client,
    any_address: reqwest::Client,
}

impl Outbound {
    /// Redirects off (one to a private address must not be followed), the
    /// checking resolver, and `user_agent`. Fails only if TLS can't be set
    /// up.
    pub fn build(user_agent: &'static str) -> reqwest::Result<Self> {
        let build = |reach| {
            let builder = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .user_agent(user_agent)
                .dns_resolver(Arc::new(CheckingResolver { reach }));
            // A proxy connects on our behalf, past the checking resolver.
            match reach {
                Reach::Public => builder.no_proxy(),
                Reach::AnyAddress => builder,
            }
            .build()
        };
        Ok(Outbound {
            public: build(Reach::Public)?,
            any_address: build(Reach::AnyAddress)?,
        })
    }

    /// The client whose resolver allows `reach`. A request on it still
    /// needs its URL's host checked first ([`check_host`]): an IP literal
    /// never reaches a resolver.
    pub fn client(&self, reach: Reach) -> &reqwest::Client {
        match reach {
            Reach::Public => &self.public,
            Reach::AnyAddress => &self.any_address,
        }
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
            if tor_service::onion::is_onion_host(name.as_str()) {
                return Err(BoxError::from(OutboundError::OnionHost));
            }
            // Port 0: the connector sets the URL's port on each address.
            let addrs: Vec<std::net::SocketAddr> =
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
#[derive(Debug, thiserror::Error)]
pub enum OutboundError {
    #[error("the URL's host could not be resolved: {0}")]
    UnresolvableHost(String),
    #[error("the URL is, or resolves to, a private or loopback address")]
    PrivateAddress,
    #[error("the URL is on an onion address, and nothing is sent over Tor from here")]
    OnionHost,
}

/// Refuses a URL on an onion address, so not even a proxy is asked for
/// it, and one whose host is an IP literal of a private, loopback or
/// link-local address (unless `reach` allows any): the half of the check a
/// resolver never sees.
pub fn check_host(url: &url::Url, reach: Reach) -> Result<(), OutboundError> {
    if url
        .host_str()
        .is_some_and(tor_service::onion::is_onion_host)
    {
        return Err(OutboundError::OnionHost);
    }
    if reach == Reach::AnyAddress {
        return Ok(());
    }
    let ip = match url.host() {
        None => return Err(OutboundError::UnresolvableHost("no host".into())),
        Some(url::Host::Ipv4(ip)) => std::net::IpAddr::V4(ip),
        Some(url::Host::Ipv6(ip)) => std::net::IpAddr::V6(ip),
        Some(url::Host::Domain(_)) => return Ok(()),
    };
    if is_disallowed_address(ip) {
        return Err(OutboundError::PrivateAddress);
    }
    Ok(())
}

/// What a failed request is recorded as: the error and its causes, without
/// the URL (whose query string may hold a merchant's token, and whose path
/// may be a push subscription's capability).
pub fn request_error_text(e: reqwest::Error) -> String {
    let e = e.without_url();
    let mut text = if e.is_timeout() {
        "timed out".to_string()
    } else if e.is_connect() {
        "could not connect".to_string()
    } else {
        e.to_string()
    };
    let mut source = std::error::Error::source(&e);
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
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
    /// its own: a loopback literal of either family is refused, a public
    /// one passes, and a name is left to the resolver.
    #[test]
    fn an_ip_literal_url_is_classified_before_the_request() {
        let url = |u: &str| url::Url::parse(u).unwrap();
        for private in [
            "http://[::1]:9999/hook",
            "http://127.0.0.1/hook",
            "http://169.254.169.254/latest",
            "http://10.0.0.1/hook",
            "http://[::ffff:127.0.0.1]/hook",
        ] {
            assert!(
                matches!(
                    check_host(&url(private), Reach::Public),
                    Err(OutboundError::PrivateAddress)
                ),
                "{private}"
            );
        }
        check_host(&url("https://[2606:4700:4700::1111]/hook"), Reach::Public).unwrap();
        check_host(&url("https://1.1.1.1/hook"), Reach::Public).unwrap();
        check_host(&url("https://shop.example/hook"), Reach::Public).unwrap();
        check_host(&url("http://[::1]/hook"), Reach::AnyAddress).unwrap();
        assert!(matches!(
            check_host(&url("http://shop.example.onion/hook"), Reach::AnyAddress),
            Err(OutboundError::OnionHost)
        ));
    }
}
