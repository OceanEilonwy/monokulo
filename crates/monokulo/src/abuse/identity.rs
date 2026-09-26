//! Who a request is from, for rate limiting, the open-stream cap and
//! challenges (`docs/workpacks/engine-boundary/README.md` step 9a).
//!
//! Keying everything on the connecting address breaks in two common setups:
//! behind a reverse proxy every visitor arrives from the proxy, and on an
//! onion service every visitor arrives from the local tor process
//! (`127.0.0.1`). One busy address then throttles everyone. So:
//!
//! - **Onion visitors** come in on a separate loopback listener that tor
//!   feeds with a PROXY protocol header carrying the Tor circuit the
//!   connection arrived on (`HiddenServiceExportCircuitID haproxy`, see
//!   `super::proxy_protocol`). Tor Browser uses one circuit per site per
//!   session, so a circuit behaves like one visitor.
//! - **Clearnet visitors** are identified by the peer address, or - when the
//!   peer is a configured trusted proxy - by the last address in
//!   `X-Forwarded-For` that isn't itself a trusted proxy. IPv6 addresses are
//!   grouped by `/64`, the block one customer's connection is normally given.
//! - **Signed-in merchants** are identified by their user, and **a shop's
//!   server** by the store whose secret key it presented. Those have their
//!   own higher limit and are never challenged.

use std::fmt;
use std::net::{IpAddr, Ipv6Addr};
use std::str::FromStr;

/// One client, as the rate limiter, the stream cap and the challenge see it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ClientIdentity {
    /// A clearnet address (IPv6 already reduced to its `/64`).
    Address(IpAddr),
    /// A Tor circuit, from the onion listener.
    Circuit(u32),
    /// A signed-in merchant (their user id).
    User(String),
    /// A shop's server, authenticated with the store's secret key (the
    /// store's public key).
    Store(String),
}

impl ClientIdentity {
    /// A clearnet address, grouped: an IPv4 address as-is (an IPv4-mapped
    /// IPv6 address counts as the IPv4 it carries), an IPv6 address reduced
    /// to its `/64` network.
    pub fn from_address(ip: IpAddr) -> Self {
        ClientIdentity::Address(group_address(ip))
    }

    /// Signed in, or holding a store's key: own limit, never challenged.
    pub fn is_authenticated(&self) -> bool {
        matches!(self, ClientIdentity::User(_) | ClientIdentity::Store(_))
    }
}

/// Stable text form, used inside signed challenges so a challenge is bound
/// to the client it was issued to.
impl fmt::Display for ClientIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClientIdentity::Address(ip) => write!(f, "ip:{ip}"),
            ClientIdentity::Circuit(id) => write!(f, "circuit:{id}"),
            ClientIdentity::User(id) => write!(f, "user:{id}"),
            ClientIdentity::Store(pk) => write!(f, "store:{pk}"),
        }
    }
}

/// See [`ClientIdentity::from_address`].
pub fn group_address(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V4(v4) => IpAddr::V4(v4),
        IpAddr::V6(v6) => {
            let s = v6.segments();
            IpAddr::V6(Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
        }
    }
}

/// One entry of the trusted-proxies setting: a single address or a CIDR
/// range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IpNet {
    network: IpAddr,
    prefix: u8,
}

impl IpNet {
    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = ip.to_canonical();
        match (self.network, ip) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                let mask = if self.prefix == 0 { 0 } else { u32::MAX << (32 - self.prefix) };
                u32::from(net) & mask == u32::from(ip) & mask
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                let mask = if self.prefix == 0 { 0 } else { u128::MAX << (128 - self.prefix) };
                u128::from(net) & mask == u128::from(ip) & mask
            }
            _ => false,
        }
    }
}

impl FromStr for IpNet {
    type Err = String;

    fn from_str(input: &str) -> Result<Self, String> {
        let input = input.trim();
        let (address, prefix) = match input.split_once('/') {
            Some((address, prefix)) => (address, Some(prefix)),
            None => (input, None),
        };
        let network: IpAddr = address.parse().map_err(|_| format!("{input:?} is not an IP address or CIDR range"))?;
        let network = network.to_canonical();
        let max = if network.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            Some(prefix) => prefix
                .parse::<u8>()
                .ok()
                .filter(|p| *p <= max)
                .ok_or_else(|| format!("{input:?} has an invalid prefix length (0 to {max})"))?,
            None => max,
        };
        Ok(IpNet { network, prefix })
    }
}

/// The trusted-proxies setting: comma- or whitespace-separated addresses
/// and CIDR ranges. Empty means "trust no proxy".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrustedProxies(Vec<IpNet>);

impl TrustedProxies {
    pub fn parse(input: &str) -> Result<Self, String> {
        input
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|part| !part.is_empty())
            .map(IpNet::from_str)
            .collect::<Result<Vec<_>, _>>()
            .map(TrustedProxies)
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        self.0.iter().any(|net| net.contains(ip))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// The client address of a clearnet request: the peer, unless the peer is
/// a trusted proxy, in which case the last address in `X-Forwarded-For`
/// that isn't a trusted proxy itself (proxies append, so walking from the
/// right skips our own chain of proxies and stops at the first address
/// nobody we trust vouched for). If every listed address is trusted, the
/// left-most one is used. A malformed entry ends the walk: everything to its
/// left was written by someone we don't trust, so the last good address
/// seen is used, or the peer if there is none.
pub fn client_address(peer: IpAddr, forwarded_for: Option<&str>, trusted: &TrustedProxies) -> IpAddr {
    if !trusted.contains(peer) {
        return peer;
    }
    let Some(header) = forwarded_for else { return peer };
    let mut chosen = peer;
    for entry in header.rsplit(',') {
        let Some(address) = parse_forwarded_entry(entry) else { break };
        chosen = address;
        if !trusted.contains(address) {
            break;
        }
    }
    chosen
}

/// One `X-Forwarded-For` entry: an address, possibly with a port
/// (`1.2.3.4:5678`, `[2001:db8::1]:443`).
fn parse_forwarded_entry(entry: &str) -> Option<IpAddr> {
    let entry = entry.trim();
    if let Ok(ip) = entry.parse::<IpAddr>() {
        return Some(ip);
    }
    if let Ok(socket) = entry.parse::<std::net::SocketAddr>() {
        return Some(socket.ip());
    }
    entry.strip_prefix('[').and_then(|rest| rest.split_once(']')).and_then(|(ip, _)| ip.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn ipv6_clients_are_grouped_by_their_64_and_ipv4_is_kept_whole() {
        assert_eq!(ClientIdentity::from_address(ip("2001:db8:1:2:aaaa::1")), ClientIdentity::from_address(ip("2001:db8:1:2:bbbb::9")));
        assert_ne!(ClientIdentity::from_address(ip("2001:db8:1:2::1")), ClientIdentity::from_address(ip("2001:db8:1:3::1")));
        assert_eq!(ClientIdentity::from_address(ip("192.0.2.7")), ClientIdentity::Address(ip("192.0.2.7")));
        assert_ne!(ClientIdentity::from_address(ip("192.0.2.7")), ClientIdentity::from_address(ip("192.0.2.8")));
        assert_eq!(ClientIdentity::from_address(ip("::ffff:192.0.2.7")), ClientIdentity::Address(ip("192.0.2.7")));
    }

    #[test]
    fn trusted_proxies_parse_addresses_and_ranges_and_refuse_nonsense() {
        let trusted = TrustedProxies::parse("127.0.0.1, 10.0.0.0/8 fd00::/8").unwrap();
        assert!(trusted.contains(ip("127.0.0.1")));
        assert!(!trusted.contains(ip("127.0.0.2")));
        assert!(trusted.contains(ip("10.200.3.4")));
        assert!(trusted.contains(ip("fd12::1")));
        assert!(!trusted.contains(ip("fe80::1")));
        assert!(trusted.contains(ip("::ffff:10.1.1.1")), "an IPv4-mapped peer matches its IPv4 range");
        assert!(TrustedProxies::parse("").unwrap().is_empty());
        for bad in ["localhost", "10.0.0.0/33", "fd00::/129", "1.2.3"] {
            assert!(TrustedProxies::parse(bad).is_err(), "{bad} should be refused");
        }
    }

    #[test]
    fn forwarded_for_is_only_believed_from_a_trusted_proxy() {
        let trusted = TrustedProxies::parse("127.0.0.1").unwrap();
        // An untrusted peer can't claim to be someone else.
        assert_eq!(client_address(ip("203.0.113.5"), Some("198.51.100.1"), &trusted), ip("203.0.113.5"));
        // A trusted proxy's forwarded address is used.
        assert_eq!(client_address(ip("127.0.0.1"), Some("198.51.100.1"), &trusted), ip("198.51.100.1"));
        // The last untrusted address wins, so a spoofed left-most entry is ignored.
        assert_eq!(client_address(ip("127.0.0.1"), Some("6.6.6.6, 198.51.100.1"), &trusted), ip("198.51.100.1"));
        // A chain of trusted proxies is walked through.
        let chain = TrustedProxies::parse("127.0.0.1, 10.0.0.0/8").unwrap();
        assert_eq!(client_address(ip("127.0.0.1"), Some("6.6.6.6, 198.51.100.1, 10.0.0.2"), &chain), ip("198.51.100.1"));
        // Ports and brackets are tolerated.
        assert_eq!(client_address(ip("127.0.0.1"), Some("[2001:db8::1]:443"), &trusted), ip("2001:db8::1"));
        assert_eq!(client_address(ip("127.0.0.1"), Some("198.51.100.1:5000"), &trusted), ip("198.51.100.1"));
        // No header, or garbage: the peer.
        assert_eq!(client_address(ip("127.0.0.1"), None, &trusted), ip("127.0.0.1"));
        assert_eq!(client_address(ip("127.0.0.1"), Some("not-an-ip"), &trusted), ip("127.0.0.1"));
        // Garbage to the left of a good entry stops the walk at the good entry.
        assert_eq!(client_address(ip("127.0.0.1"), Some("junk, 198.51.100.1"), &trusted), ip("198.51.100.1"));
    }
}
