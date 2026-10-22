//! The networks visitors reach this instance on: the web address and the
//! onion address, served together by the same router.
//!
//! Everything that depends on how a visitor got here asks the [`Network`]
//! their request arrived on, never the request's own `Host` or a single
//! configured address: the base of the absolute links they're given, whether
//! their connection counts as secure, the headers their responses carry and
//! whether a page may load anything from another site. A request arrived
//! over Tor when it carries the `ConnectInfo<OnionPeer>` that both the
//! onion listener (an external tor) and Tor inside monokulo give it;
//! everything else is the web.
//!
//! Who a visitor is (one rate-limit budget each) depends on the network
//! too; `http::abuse::anonymous_identity` already reads it from the same
//! connection info.

use axum::extract::ConnectInfo;
use axum::http::{header, Extensions, HeaderMap, HeaderValue};

use crate::abuse::proxy_protocol::OnionPeer;
use crate::abuse::TrustedProxies;
use crate::settings::{MonokuloSettings, TorMode};
use crate::tor::Tor;

/// Which network a request arrived on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkKind {
    Web,
    Onion,
}

impl NetworkKind {
    /// The network of a request with these extensions.
    pub fn of(extensions: &Extensions) -> Self {
        if extensions.get::<ConnectInfo<OnionPeer>>().is_some() {
            NetworkKind::Onion
        } else {
            NetworkKind::Web
        }
    }
}

/// One way visitors reach this instance.
pub trait Network {
    fn kind(&self) -> NetworkKind;

    /// The base every absolute link for a visitor on this network is built
    /// on (`https://pay.example.com`, `http://….onion`), with no trailing
    /// `/`; `None` while it isn't known.
    fn public_uri(&self) -> Option<String>;

    /// Whether the visitor's connection is encrypted end to end over
    /// `https`, which decides the session cookie's `Secure` flag.
    fn is_secure(&self, extensions: &Extensions, headers: &HeaderMap) -> bool;

    /// Adds the headers every response on this network carries;
    /// `request_target` is the request's path and query.
    fn response_headers(&self, request_target: &str, headers: &mut HeaderMap);

    /// Whether a page may load a resource from another site (a merchant's
    /// favicon): a visitor on Tor must never be sent off Tor.
    fn may_load_external(&self) -> bool;

    /// `path` as an absolute link for a visitor on this network: on its
    /// [`Network::public_uri`], or, while that isn't known (an instance
    /// used locally, Tor still publishing), on the address the visitor
    /// asked for.
    fn link_for(&self, headers: &HeaderMap, extensions: &Extensions, path: &str) -> String {
        if let Some(base) = self.public_uri() {
            return format!("{base}{path}");
        }
        let host = headers
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let scheme = if self.is_secure(extensions, headers) {
            "https"
        } else {
            "http"
        };
        format!("{scheme}://{host}{path}")
    }
}

/// The web address (`public_uri`).
pub struct Web<'a> {
    pub settings: &'a MonokuloSettings,
    pub tor: &'a Tor,
    pub trusted_proxies: TrustedProxies,
}

/// The onion address: Tor inside monokulo's, or an external tor's
/// (`onion_uri`).
pub struct Onion<'a> {
    pub settings: &'a MonokuloSettings,
    pub tor: &'a Tor,
}

/// The onion address visitors on Tor reach this instance at
/// (`http://….onion`), whichever tor serves it, or `None` when Tor is off
/// or hasn't made one yet.
pub fn onion_uri(settings: &MonokuloSettings, tor: &Tor) -> Option<String> {
    let config = settings.tor.load();
    match config.mode {
        TorMode::Inside => tor
            .onion_address()
            .map(|address| format!("http://{address}")),
        TorMode::External => config.onion_uri.clone(),
        TorMode::Off => None,
    }
}

impl Network for Web<'_> {
    fn kind(&self) -> NetworkKind {
        NetworkKind::Web
    }

    fn public_uri(&self) -> Option<String> {
        self.settings.public_uri()
    }

    /// `https` when a trusted proxy says so (`X-Forwarded-Proto` from
    /// anyone else could be the client's own), or, without one, when the
    /// browser's own `Origin`/`Referer` names an `https` page.
    fn is_secure(&self, extensions: &Extensions, headers: &HeaderMap) -> bool {
        let value = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .map(str::to_ascii_lowercase)
        };
        let from_trusted_proxy = extensions
            .get::<ConnectInfo<std::net::SocketAddr>>()
            .is_some_and(|ConnectInfo(peer)| self.trusted_proxies.contains(peer.ip()));
        if from_trusted_proxy {
            if let Some(proto) = value("x-forwarded-proto") {
                return proto.split(',').next().is_some_and(|p| p.trim() == "https");
            }
        }
        value("origin")
            .or_else(|| value("referer"))
            .is_some_and(|url| url.starts_with("https://"))
    }

    /// No full URLs (which can carry order ids) in the Referer to other
    /// sites, and, for a page, where the same page is on Tor
    /// (`Onion-Location`), so Tor Browser offers to switch.
    fn response_headers(&self, request_target: &str, headers: &mut HeaderMap) {
        headers
            .entry(header::REFERRER_POLICY)
            .or_insert(HeaderValue::from_static("strict-origin-when-cross-origin"));
        let is_page = headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|t| t.starts_with("text/html"));
        if !is_page || !self.settings.tor.load().onion_location {
            return;
        }
        if let Some(onion) = onion_uri(self.settings, self.tor) {
            if let Ok(value) = HeaderValue::from_str(&format!("{onion}{request_target}")) {
                headers.insert(header::HeaderName::from_static("onion-location"), value);
            }
        }
    }

    fn may_load_external(&self) -> bool {
        true
    }
}

impl Network for Onion<'_> {
    fn kind(&self) -> NetworkKind {
        NetworkKind::Onion
    }

    fn public_uri(&self) -> Option<String> {
        onion_uri(self.settings, self.tor)
    }

    /// Never: an onion service is plain `http` (Tor encrypts it end to end
    /// itself), and a `Secure` cookie would be dropped by any browser but
    /// Tor Browser. Forwarding headers are ignored: nothing in front of
    /// the onion listener but tor.
    fn is_secure(&self, _extensions: &Extensions, _headers: &HeaderMap) -> bool {
        false
    }

    /// No Referer at all: a link to another site must not tell it this
    /// onion address, or that the visitor came from it.
    fn response_headers(&self, _request_target: &str, headers: &mut HeaderMap) {
        headers.insert(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        );
    }

    fn may_load_external(&self) -> bool {
        false
    }
}

/// The network a request arrived on, borrowed from the state it is
/// answered with (`AppState::network`).
pub enum Reached<'a> {
    Web(Web<'a>),
    Onion(Onion<'a>),
}

impl<'a> std::ops::Deref for Reached<'a> {
    type Target = dyn Network + 'a;
    fn deref(&self) -> &Self::Target {
        match self {
            Reached::Web(web) => web,
            Reached::Onion(onion) => onion,
        }
    }
}

/// What a handler needs to know about the network its request arrived on,
/// as an extractor: links to give this visitor, and what their connection
/// and pages may do.
#[derive(Debug, Clone)]
pub struct OnNetwork {
    pub kind: NetworkKind,
    /// The connection is `https` end to end ([`Network::is_secure`]).
    pub secure: bool,
    /// Pages may load resources from other sites ([`Network::may_load_external`]).
    pub may_load_external: bool,
    base: String,
}

impl OnNetwork {
    /// What `network` says for a request with these headers and extensions.
    pub fn new(network: &dyn Network, headers: &HeaderMap, extensions: &Extensions) -> Self {
        OnNetwork {
            kind: network.kind(),
            secure: network.is_secure(extensions, headers),
            may_load_external: network.may_load_external(),
            base: network.link_for(headers, extensions, ""),
        }
    }

    /// `path` (starting with `/`) as an absolute link for this visitor
    /// ([`Network::link_for`]).
    pub fn link(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }
}

impl axum::extract::FromRequestParts<crate::http::AppState> for OnNetwork {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &crate::http::AppState,
    ) -> Result<Self, Self::Rejection> {
        let network = state.network(&parts.extensions);
        Ok(OnNetwork::new(&*network, &parts.headers, &parts.extensions))
    }
}

#[cfg(test)]
mod tests;
