//! Requests monokulo makes to addresses other people give it: stores'
//! webhook endpoints (`crate::webhooks::delivery`) and connected apps'
//! push endpoints (`crate::push::delivery`). Either could name an address
//! inside the network monokulo runs in, so both go out the same way.
//!
//! Nothing private, as the `outbound` crate checks it (the Monokular Bell
//! sends its dings the same way): a URL whose host is, or resolves to, a
//! loopback, private, link-local or otherwise internal address is refused
//! at every request, unless `webhooks.allow_private_urls` is on
//! ([`Reach::AnyAddress`](outbound::Reach)), and an onion address
//! whatever the setting.

use outbound::{client_builder, Reach};

/// The HTTP clients these requests are sent with: one for each [`Reach`],
/// reused across requests (building one reads the system's certificates),
/// each with `outbound`'s checking resolver. Separate pools, so turning
/// `webhooks.allow_private_urls` off can't reuse a connection opened
/// while it was on.
#[derive(Clone)]
pub struct Outbound {
    public: reqwest::Client,
    any_address: reqwest::Client,
}

impl Outbound {
    /// [`outbound::client_builder`]'s clients with `user_agent`. Fails
    /// only if TLS can't be set up.
    pub fn build(user_agent: &'static str) -> reqwest::Result<Self> {
        let build = |reach| client_builder(reach).user_agent(user_agent).build();
        Ok(Outbound {
            public: build(Reach::Public)?,
            any_address: build(Reach::AnyAddress)?,
        })
    }

    /// The client whose resolver allows `reach`. A request on it still
    /// needs its URL's host checked first ([`outbound::check_host`]): an
    /// IP literal never reaches a resolver.
    pub fn client(&self, reach: Reach) -> &reqwest::Client {
        match reach {
            Reach::Public => &self.public,
            Reach::AnyAddress => &self.any_address,
        }
    }
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
