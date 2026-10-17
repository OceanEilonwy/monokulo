//! A small DNS client for one job: reading a name's TXT records the moment
//! they're published (monokulo's verified domains).
//!
//! It resolves iteratively from the root servers, following referrals down
//! to the zone's own name servers and asking them, so no cache sits between
//! it and the answer. A caching resolver can go on saying "no such name"
//! for as long as the zone's SOA allows (often half an hour) after a look
//! made before the record was there, so a merchant checking straight after
//! publishing would see nothing. Where the network won't let it reach the
//! root servers, it asks the machine's own resolvers (`/etc/resolv.conf`)
//! instead.
//!
//! The wire format is RFC 1035's, with an EDNS(0) OPT record (RFC 6891) so
//! a UDP answer can be up to 1232 bytes, and TCP for an answer that is
//! truncated anyway. Answers come from other people's servers, so [`parse`]
//! takes any bytes without panicking (`fuzz/fuzz_targets/dns_message.rs`).

use std::collections::HashMap;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};

/// The root servers' IPv4 addresses, a to m (IANA's root hints).
pub const ROOT_SERVERS: [Ipv4Addr; 13] = [
    Ipv4Addr::new(198, 41, 0, 4),
    Ipv4Addr::new(170, 247, 170, 2),
    Ipv4Addr::new(192, 33, 4, 12),
    Ipv4Addr::new(199, 7, 91, 13),
    Ipv4Addr::new(192, 203, 230, 10),
    Ipv4Addr::new(192, 5, 5, 241),
    Ipv4Addr::new(192, 112, 36, 4),
    Ipv4Addr::new(198, 97, 190, 53),
    Ipv4Addr::new(192, 36, 148, 17),
    Ipv4Addr::new(192, 58, 128, 30),
    Ipv4Addr::new(193, 0, 14, 129),
    Ipv4Addr::new(199, 7, 83, 42),
    Ipv4Addr::new(202, 12, 27, 33),
];

pub const TYPE_A: u16 = 1;
pub const TYPE_NS: u16 = 2;
pub const TYPE_CNAME: u16 = 5;
pub const TYPE_TXT: u16 = 16;
pub const TYPE_AAAA: u16 = 28;
const TYPE_OPT: u16 = 41;
const CLASS_IN: u16 = 1;

pub const RCODE_NOERROR: u8 = 0;
pub const RCODE_NXDOMAIN: u8 = 3;

/// The UDP answer size offered (the DNS Flag Day 2020 figure).
const UDP_PAYLOAD: u16 = 1232;
/// Referrals followed for one name before giving up.
const MAX_REFERRALS: usize = 16;
/// CNAMEs followed for one name before giving up.
const MAX_CNAMES: usize = 8;
/// Lookups of name servers' own addresses nested inside a lookup.
const MAX_DEPTH: u8 = 3;
/// Servers tried for one question before giving up on it.
const SERVERS_PER_QUESTION: usize = 3;
/// Compression pointers followed in one name.
const MAX_POINTERS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DnsError {
    #[error("{0:?} is not a domain name")]
    BadName(String),
    #[error("timed out")]
    Timeout,
    #[error("{0}")]
    Io(String),
    #[error("a malformed answer ({0})")]
    Malformed(&'static str),
    #[error("the name servers answered with error code {0}")]
    ServerError(u8),
    #[error("no name server for it could be found")]
    NoNameServers,
    #[error("too many referrals or CNAMEs")]
    TooLong,
}

impl From<std::io::Error> for DnsError {
    fn from(e: std::io::Error) -> Self {
        DnsError::Io(e.to_string())
    }
}

/// One record of an answer: its owner name (lower case, no trailing dot;
/// the root is "") and what it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub name: String,
    pub data: RecordData,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordData {
    A(Ipv4Addr),
    Aaaa(Ipv6Addr),
    Ns(String),
    Cname(String),
    /// A TXT record's strings, joined into one value.
    Txt(String),
    /// Any other type, by number.
    Other(u16),
}

impl RecordData {
    pub fn record_type(&self) -> u16 {
        match self {
            RecordData::A(_) => TYPE_A,
            RecordData::Aaaa(_) => TYPE_AAAA,
            RecordData::Ns(_) => TYPE_NS,
            RecordData::Cname(_) => TYPE_CNAME,
            RecordData::Txt(_) => TYPE_TXT,
            RecordData::Other(record_type) => *record_type,
        }
    }
}

/// A DNS message, as much of it as this client reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub id: u16,
    pub response: bool,
    pub authoritative: bool,
    pub truncated: bool,
    pub rcode: u8,
    /// The name and type asked about.
    pub question: Option<(String, u16)>,
    pub answers: Vec<Record>,
    pub authorities: Vec<Record>,
    pub additionals: Vec<Record>,
}

/// `name` in lower case without a trailing dot, if it is a domain name: one
/// or more labels of 1 to 63 bytes, 253 characters at most.
pub fn normalize(name: &str) -> Result<String, DnsError> {
    let bad = || DnsError::BadName(name.to_string());
    let trimmed = name.strip_suffix('.').unwrap_or(name).to_ascii_lowercase();
    if trimmed.is_empty() || trimmed.len() > 253 {
        return Err(bad());
    }
    for label in trimmed.split('.') {
        if label.is_empty() || label.len() > 63 || !label.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(bad());
        }
    }
    Ok(trimmed)
}

/// A query for `name`'s records of `record_type` (`name` as [`normalize`]
/// leaves it). `recursion` asks the server to resolve it for us, which only
/// a resolver does; name servers are asked without.
pub fn encode_query(id: u16, name: &str, record_type: u16, recursion: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(name.len() + 29);
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&(if recursion { 0x0100u16 } else { 0 }).to_be_bytes());
    // One question, one additional record (the OPT).
    for count in [1u16, 0, 0, 1] {
        out.extend_from_slice(&count.to_be_bytes());
    }
    for label in name.split('.').filter(|label| !label.is_empty()) {
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out.extend_from_slice(&record_type.to_be_bytes());
    out.extend_from_slice(&CLASS_IN.to_be_bytes());
    // The OPT record: the root name, its type, the UDP size in place of a
    // class, no extended flags, no options.
    out.push(0);
    out.extend_from_slice(&TYPE_OPT.to_be_bytes());
    out.extend_from_slice(&UDP_PAYLOAD.to_be_bytes());
    out.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
    out
}

/// Reads a DNS message. Any bytes at all give a message or an error, never
/// a panic, and a hostile message can't make it loop or allocate much.
pub fn parse(buf: &[u8]) -> Result<Message, DnsError> {
    let header = buf.get(..12).ok_or(DnsError::Malformed("short header"))?;
    let word = |at: usize| u16::from_be_bytes([header[at], header[at + 1]]);
    let flags = word(2);
    let (questions, answers, authorities, additionals) = (word(4), word(6), word(8), word(10));
    if questions > 1 {
        return Err(DnsError::Malformed("more than one question"));
    }
    let mut pos = 12;
    let question = if questions == 1 {
        let (name, next) = read_name(buf, pos)?;
        let record_type = read_u16(buf, next)?;
        read_u16(buf, next + 2)?;
        pos = next + 4;
        Some((name, record_type))
    } else {
        None
    };
    let mut section = |count: u16| -> Result<Vec<Record>, DnsError> {
        let mut records = Vec::new();
        for _ in 0..count {
            let (record, next) = read_record(buf, pos)?;
            records.extend(record);
            pos = next;
        }
        Ok(records)
    };
    let answers = section(answers)?;
    let authorities = section(authorities)?;
    let additionals = section(additionals)?;
    Ok(Message {
        id: word(0),
        response: flags & 0x8000 != 0,
        authoritative: flags & 0x0400 != 0,
        truncated: flags & 0x0200 != 0,
        rcode: (flags & 0x000f) as u8,
        question,
        answers,
        authorities,
        additionals,
    })
}

fn read_u16(buf: &[u8], at: usize) -> Result<u16, DnsError> {
    match buf.get(at..at + 2) {
        Some(bytes) => Ok(u16::from_be_bytes([bytes[0], bytes[1]])),
        None => Err(DnsError::Malformed("ends early")),
    }
}

/// The record at `pos` (`None` for one not in class IN), and where the
/// next one starts.
fn read_record(buf: &[u8], pos: usize) -> Result<(Option<Record>, usize), DnsError> {
    let (name, next) = read_name(buf, pos)?;
    let record_type = read_u16(buf, next)?;
    let class = read_u16(buf, next + 2)?;
    // Then a 4-byte TTL this client has no use for.
    let length = read_u16(buf, next + 8)? as usize;
    let start = next + 10;
    let data = buf
        .get(start..start + length)
        .ok_or(DnsError::Malformed("record data ends early"))?;
    let end = start + length;
    if class != CLASS_IN {
        return Ok((None, end));
    }
    let data = match record_type {
        TYPE_A => RecordData::A(Ipv4Addr::from(
            <[u8; 4]>::try_from(data).map_err(|_| DnsError::Malformed("A record size"))?,
        )),
        TYPE_AAAA => RecordData::Aaaa(Ipv6Addr::from(
            <[u8; 16]>::try_from(data).map_err(|_| DnsError::Malformed("AAAA record size"))?,
        )),
        // A name in record data may point back into the whole message.
        TYPE_NS => RecordData::Ns(read_name(buf, start)?.0),
        TYPE_CNAME => RecordData::Cname(read_name(buf, start)?.0),
        TYPE_TXT => {
            // Length-prefixed strings, joined.
            let mut text = Vec::with_capacity(data.len());
            let mut rest = data;
            while let Some((&len, tail)) = rest.split_first() {
                let part = tail
                    .get(..len as usize)
                    .ok_or(DnsError::Malformed("TXT string ends early"))?;
                text.extend_from_slice(part);
                rest = &tail[len as usize..];
            }
            RecordData::Txt(String::from_utf8_lossy(&text).into_owned())
        }
        other => RecordData::Other(other),
    };
    Ok((Some(Record { name, data }), end))
}

/// The name at `pos`, lower case and without a trailing dot, and where what
/// follows it starts. A byte that isn't a printable character, or a dot
/// inside a label, comes out as `\ddd`, so no label can pass for two.
fn read_name(buf: &[u8], mut pos: usize) -> Result<(String, usize), DnsError> {
    let mut name = String::new();
    let mut after = None;
    let mut pointers = 0;
    loop {
        let len = *buf.get(pos).ok_or(DnsError::Malformed("name ends early"))? as usize;
        match len & 0xc0 {
            0x00 if len == 0 => {
                return Ok((name, after.unwrap_or(pos + 1)));
            }
            0x00 => {
                let label = buf
                    .get(pos + 1..pos + 1 + len)
                    .ok_or(DnsError::Malformed("label ends early"))?;
                if !name.is_empty() {
                    name.push('.');
                }
                for &byte in label {
                    if byte.is_ascii_graphic() && byte != b'.' && byte != b'\\' {
                        name.push(byte.to_ascii_lowercase() as char);
                    } else {
                        name.push_str(&format!("\\{byte:03}"));
                    }
                }
                if name.len() > 1024 {
                    return Err(DnsError::Malformed("name too long"));
                }
                pos += 1 + len;
            }
            0xc0 => {
                let low = *buf
                    .get(pos + 1)
                    .ok_or(DnsError::Malformed("pointer ends early"))?;
                let target = ((len & 0x3f) << 8) | low as usize;
                after.get_or_insert(pos + 2);
                pointers += 1;
                if target >= pos || pointers > MAX_POINTERS {
                    return Err(DnsError::Malformed("bad name pointer"));
                }
                pos = target;
            }
            _ => return Err(DnsError::Malformed("unknown label type")),
        }
    }
}

/// Whether `name` is `zone` or below it ("" is the root, above every name).
fn within(name: &str, zone: &str) -> bool {
    zone.is_empty()
        || name == zone
        || name
            .strip_suffix(zone)
            .is_some_and(|head| head.ends_with('.'))
}

/// Where to look names up: the roots to start from, the resolvers to ask
/// when they can't be reached, how long to wait for each answer, and where
/// a server's address is answered (port 53, unless `addresses` says).
#[derive(Debug, Clone)]
pub struct Resolver {
    roots: Vec<IpAddr>,
    fallback: Vec<IpAddr>,
    timeout: Duration,
    addresses: HashMap<IpAddr, SocketAddr>,
}

impl Resolver {
    /// The real one: the root servers, then the machine's resolvers.
    pub fn system() -> Self {
        Resolver::new(
            ROOT_SERVERS.into_iter().map(IpAddr::V4).collect(),
            system_resolvers(),
            Duration::from_secs(2),
        )
    }

    /// `roots` to start from, `fallback` resolvers, `timeout` for each
    /// answer; every server on port 53.
    pub fn new(roots: Vec<IpAddr>, fallback: Vec<IpAddr>, timeout: Duration) -> Self {
        Resolver {
            roots,
            fallback,
            timeout,
            addresses: HashMap::new(),
        }
    }

    /// Asks the servers at these IPs at the given socket addresses instead:
    /// name servers played on one loopback address, each on its own port,
    /// stand in for the internet's.
    pub fn with_addresses(mut self, addresses: HashMap<IpAddr, SocketAddr>) -> Self {
        self.addresses = addresses;
        self
    }

    /// The TXT values published at `name`; none for a name with none, or
    /// no such name.
    pub async fn txt(&self, name: &str) -> Result<Vec<String>, DnsError> {
        let name = normalize(name)?;
        let records = match self.iterate(name.clone(), TYPE_TXT, 0).await {
            Ok(records) => records,
            Err(e) if self.fallback.is_empty() => return Err(e),
            Err(e) => {
                tracing::debug!(error = %e, "resolving from the root servers failed; asking this machine's resolvers");
                self.recursive(&name, TYPE_TXT).await?
            }
        };
        Ok(records
            .into_iter()
            .filter_map(|data| match data {
                RecordData::Txt(text) => Some(text),
                _ => None,
            })
            .collect())
    }

    /// `name`'s records of `record_type`, asked of the name servers
    /// themselves, from the roots down.
    fn iterate(
        &self,
        name: String,
        record_type: u16,
        depth: u8,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<RecordData>, DnsError>> + Send + '_>> {
        Box::pin(async move {
            let mut name = name;
            let mut cnames = 0;
            let mut servers = self.roots.clone();
            let mut zone = String::new();
            for _ in 0..MAX_REFERRALS {
                let message = self.ask(&servers, &name, record_type, false).await?;
                if message.rcode == RCODE_NXDOMAIN {
                    return Ok(Vec::new());
                }
                let (target, found) = follow(&message.answers, &name, record_type, &mut cnames)?;
                if !found.is_empty() {
                    return Ok(found);
                }
                if target != name {
                    // A CNAME to a name this answer doesn't cover: from the
                    // roots again.
                    name = target;
                    servers = self.roots.clone();
                    zone.clear();
                    continue;
                }
                if message.authoritative {
                    return Ok(Vec::new());
                }
                let (child, name_servers) =
                    referral(&message, &name, &zone).ok_or(DnsError::NoNameServers)?;
                servers = self.addresses(&message, &name_servers, depth).await?;
                zone = child;
            }
            Err(DnsError::TooLong)
        })
    }

    /// The addresses of `name_servers`: the glue the referral came with,
    /// else looked up.
    async fn addresses(
        &self,
        message: &Message,
        name_servers: &[String],
        depth: u8,
    ) -> Result<Vec<IpAddr>, DnsError> {
        let mut addresses: Vec<IpAddr> = message
            .additionals
            .iter()
            .filter(|record| name_servers.contains(&record.name))
            .filter_map(|record| match record.data {
                RecordData::A(ip) => Some(IpAddr::V4(ip)),
                RecordData::Aaaa(ip) => Some(IpAddr::V6(ip)),
                _ => None,
            })
            .collect();
        if addresses.is_empty() && depth < MAX_DEPTH {
            for server in name_servers.iter().take(SERVERS_PER_QUESTION) {
                if let Ok(found) = self.iterate(server.clone(), TYPE_A, depth + 1).await {
                    addresses.extend(found.into_iter().filter_map(|data| match data {
                        RecordData::A(ip) => Some(IpAddr::V4(ip)),
                        _ => None,
                    }));
                }
                if !addresses.is_empty() {
                    break;
                }
            }
        }
        // IPv4 first: plenty of machines have no IPv6 route.
        addresses.sort_by_key(|ip| ip.is_ipv6());
        if addresses.is_empty() {
            Err(DnsError::NoNameServers)
        } else {
            Ok(addresses)
        }
    }

    /// `name`'s records of `record_type`, asked of the machine's resolvers.
    async fn recursive(&self, name: &str, record_type: u16) -> Result<Vec<RecordData>, DnsError> {
        let message = self.ask(&self.fallback, name, record_type, true).await?;
        if message.rcode == RCODE_NXDOMAIN {
            return Ok(Vec::new());
        }
        Ok(follow(&message.answers, name, record_type, &mut 0)?.1)
    }

    /// The first answer, NOERROR or NXDOMAIN, from up to a few of `servers`,
    /// starting at a random one so no one server takes every question.
    async fn ask(
        &self,
        servers: &[IpAddr],
        name: &str,
        record_type: u16,
        recursion: bool,
    ) -> Result<Message, DnsError> {
        let mut last = DnsError::NoNameServers;
        let start = if servers.is_empty() {
            0
        } else {
            rand::random_range(0..servers.len())
        };
        let ipv4_first = {
            let mut order: Vec<IpAddr> = servers
                .iter()
                .cycle()
                .skip(start)
                .take(servers.len())
                .copied()
                .collect();
            order.sort_by_key(|ip| ip.is_ipv6());
            order
        };
        for server in ipv4_first.into_iter().take(SERVERS_PER_QUESTION) {
            match self.exchange(server, name, record_type, recursion).await {
                Ok(message) if matches!(message.rcode, RCODE_NOERROR | RCODE_NXDOMAIN) => {
                    return Ok(message)
                }
                Ok(message) => last = DnsError::ServerError(message.rcode),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    /// One question to one server: over UDP, then TCP if the answer was cut.
    async fn exchange(
        &self,
        server: IpAddr,
        name: &str,
        record_type: u16,
        recursion: bool,
    ) -> Result<Message, DnsError> {
        let id: u16 = rand::random();
        let query = encode_query(id, name, record_type, recursion);
        let address = self
            .addresses
            .get(&server)
            .copied()
            .unwrap_or(SocketAddr::new(server, 53));
        let ours = |message: &Message| {
            message.response
                && message.id == id
                && message.question.as_ref() == Some(&(name.to_string(), record_type))
        };
        let local: SocketAddr = match server {
            IpAddr::V4(_) => (Ipv4Addr::UNSPECIFIED, 0).into(),
            IpAddr::V6(_) => (Ipv6Addr::UNSPECIFIED, 0).into(),
        };
        let udp = async {
            let socket = UdpSocket::bind(local).await?;
            // Connected, so only that server's datagrams arrive.
            socket.connect(address).await?;
            socket.send(&query).await?;
            let mut buf = vec![0u8; 4096];
            loop {
                let n = socket.recv(&mut buf).await?;
                // Anything else (a late answer to an earlier question, a
                // forgery) is dropped and the wait goes on.
                if let Ok(message) = parse(&buf[..n]) {
                    if ours(&message) {
                        return Ok::<_, DnsError>(message);
                    }
                }
            }
        };
        let message = tokio::time::timeout(self.timeout, udp)
            .await
            .map_err(|_| DnsError::Timeout)??;
        if !message.truncated {
            return Ok(message);
        }
        let tcp = async {
            let mut stream = TcpStream::connect(address).await?;
            let mut framed = (query.len() as u16).to_be_bytes().to_vec();
            framed.extend_from_slice(&query);
            stream.write_all(&framed).await?;
            let len = stream.read_u16().await? as usize;
            let mut buf = vec![0u8; len];
            stream.read_exact(&mut buf).await?;
            let message = parse(&buf)?;
            if ours(&message) {
                Ok(message)
            } else {
                Err(DnsError::Malformed("an answer to another question"))
            }
        };
        tokio::time::timeout(self.timeout, tcp)
            .await
            .map_err(|_| DnsError::Timeout)?
    }
}

/// Follows `name`'s CNAMEs through `answers`: the name it ends at, and the
/// records of `record_type` found there.
fn follow(
    answers: &[Record],
    name: &str,
    record_type: u16,
    cnames: &mut usize,
) -> Result<(String, Vec<RecordData>), DnsError> {
    let mut name = name.to_string();
    loop {
        let found: Vec<RecordData> = answers
            .iter()
            .filter(|record| record.name == name && record.data.record_type() == record_type)
            .map(|record| record.data.clone())
            .collect();
        if !found.is_empty() {
            return Ok((name, found));
        }
        let target = answers.iter().find_map(|record| match &record.data {
            RecordData::Cname(target) if record.name == name => Some(target.clone()),
            _ => None,
        });
        match target {
            Some(target) => {
                *cnames += 1;
                if *cnames > MAX_CNAMES {
                    return Err(DnsError::TooLong);
                }
                name = target;
            }
            None => return Ok((name, Vec::new())),
        }
    }
}

/// A referral in `message`: the zone below `zone` that holds `name`, and
/// its name servers.
fn referral(message: &Message, name: &str, zone: &str) -> Option<(String, Vec<String>)> {
    let child = message
        .authorities
        .iter()
        .find_map(|record| match record.data {
            RecordData::Ns(_)
                if within(name, &record.name)
                    && record.name != zone
                    && within(&record.name, zone) =>
            {
                Some(record.name.clone())
            }
            _ => None,
        })?;
    let servers = message
        .authorities
        .iter()
        .filter(|record| record.name == child)
        .filter_map(|record| match &record.data {
            RecordData::Ns(server) => Some(server.clone()),
            _ => None,
        })
        .collect();
    Some((child, servers))
}

/// The resolvers `/etc/resolv.conf` names (none where there is no such
/// file).
fn system_resolvers() -> Vec<IpAddr> {
    std::fs::read_to_string("/etc/resolv.conf")
        .map(|conf| parse_resolv_conf(&conf))
        .unwrap_or_default()
}

fn parse_resolv_conf(conf: &str) -> Vec<IpAddr> {
    conf.lines()
        .filter_map(|line| line.trim().strip_prefix("nameserver"))
        .filter_map(|rest| {
            // An IPv6 address may carry a zone ("fe80::1%eth0").
            let address = rest.trim().split('%').next()?;
            address.parse().ok()
        })
        .collect()
}

#[cfg(test)]
mod tests;
