//! The wire format against hand-made bytes, and whole lookups against name
//! servers played on 127.0.0.1, each on a port of its own: the resolver is
//! told which documentation address (192.0.2.x) each one answers for, so
//! referrals and glue name real-looking addresses on every platform.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use super::*;

#[test]
fn a_name_is_lower_cased_without_its_trailing_dot_or_refused() {
    assert_eq!(
        normalize("_Monokulo.Shop.Example.").unwrap(),
        "_monokulo.shop.example"
    );
    for bad in [
        "",
        ".",
        "a..b",
        "a b.example",
        &"x".repeat(64),
        &format!("{}.example", "a.".repeat(130)),
    ] {
        assert!(normalize(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn a_query_asks_one_question_with_an_edns_size() {
    assert_eq!(
        encode_query(0x1234, "a.bc", TYPE_TXT, false),
        [
            0x12, 0x34, 0, 0, 0, 1, 0, 0, 0, 0, 0,
            1, // header: no recursion, 1 question, 1 additional
            1, b'a', 2, b'b', b'c', 0, 0, 16, 0, 1, // a.bc TXT IN
            0, 0, 41, 0x04, 0xd0, 0, 0, 0, 0, 0, 0, // OPT: 1232 bytes
        ]
    );
    assert_eq!(
        encode_query(1, "a", TYPE_A, true)[2],
        0x01,
        "recursion desired"
    );
}

/// An answer for `_m.x` TXT: a TXT record of two strings whose owner is a
/// pointer back to the question's name.
fn sample_answer() -> Vec<u8> {
    let mut out = vec![0xab, 0xcd, 0x84, 0x00, 0, 1, 0, 1, 0, 0, 0, 0];
    out.extend_from_slice(&[2, b'_', b'M', 1, b'x', 0, 0, 16, 0, 1]);
    out.extend_from_slice(&[0xc0, 12, 0, 16, 0, 1, 0, 0, 2, 88, 0, 8]);
    out.extend_from_slice(&[3, b'a', b'b', b'c', 3, b'd', b'e', b'f']);
    out
}

#[test]
fn an_answer_is_read_through_name_pointers_and_split_txt_strings() {
    let message = parse(&sample_answer()).unwrap();
    assert!(message.response && message.authoritative && !message.truncated);
    assert_eq!(message.id, 0xabcd);
    assert_eq!(message.question, Some(("_m.x".to_string(), TYPE_TXT)));
    assert_eq!(
        message.answers,
        [Record {
            name: "_m.x".into(),
            data: RecordData::Txt("abcdef".into())
        }]
    );
}

#[test]
fn a_cut_short_or_looping_answer_is_refused_without_a_panic() {
    let answer = sample_answer();
    for end in 0..answer.len() {
        assert!(parse(&answer[..end]).is_err(), "cut at {end}");
    }
    // A pointer to itself, and one pointing forward.
    let mut looping = answer.clone();
    looping[22..24].copy_from_slice(&[0xc0, 22]);
    assert!(parse(&looping).is_err());
    let mut forward = answer.clone();
    forward[22..24].copy_from_slice(&[0xc0, 30]);
    assert!(parse(&forward).is_err());
}

#[test]
fn an_odd_byte_in_a_label_cant_pass_for_a_dot() {
    let mut answer = sample_answer();
    answer[13] = b'.';
    assert_eq!(parse(&answer).unwrap().question.unwrap().0, "\\046m.x");
}

#[test]
fn resolv_conf_gives_its_name_servers() {
    let conf = "# a comment\nsearch lan\nnameserver 127.0.0.53\nnameserver fe80::1%eth0\nnameserver nonsense\noptions edns0\n";
    assert_eq!(
        parse_resolv_conf(conf),
        [
            "127.0.0.53".parse::<IpAddr>().unwrap(),
            "fe80::1".parse().unwrap()
        ]
    );
}

/// What a played name server answers.
#[derive(Default)]
struct Reply {
    rcode: u8,
    authoritative: bool,
    truncated: bool,
    answers: Vec<(&'static str, RecordData)>,
    authorities: Vec<(&'static str, RecordData)>,
    additionals: Vec<(&'static str, RecordData)>,
}

fn txt(value: &str) -> RecordData {
    RecordData::Txt(value.to_string())
}
fn ns(name: &str) -> RecordData {
    RecordData::Ns(name.to_string())
}
fn a(ip: &str) -> RecordData {
    RecordData::A(ip.parse().unwrap())
}

fn put_name(out: &mut Vec<u8>, name: &str) {
    for label in name.split('.').filter(|label| !label.is_empty()) {
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
}

fn encode_reply(query: &Message, reply: &Reply) -> Vec<u8> {
    let flags = 0x8000
        | if reply.authoritative { 0x0400 } else { 0 }
        | if reply.truncated { 0x0200 } else { 0 }
        | reply.rcode as u16;
    let mut out = query.id.to_be_bytes().to_vec();
    for word in [
        flags,
        1,
        reply.answers.len() as u16,
        reply.authorities.len() as u16,
        reply.additionals.len() as u16,
    ] {
        out.extend_from_slice(&word.to_be_bytes());
    }
    let (name, record_type) = query.question.clone().unwrap();
    put_name(&mut out, &name);
    out.extend_from_slice(&record_type.to_be_bytes());
    out.extend_from_slice(&CLASS_IN.to_be_bytes());
    for (owner, data) in reply
        .answers
        .iter()
        .chain(&reply.authorities)
        .chain(&reply.additionals)
    {
        put_name(&mut out, owner);
        out.extend_from_slice(&data.record_type().to_be_bytes());
        out.extend_from_slice(&CLASS_IN.to_be_bytes());
        out.extend_from_slice(&300u32.to_be_bytes());
        let mut data_bytes = Vec::new();
        match data {
            RecordData::A(ip) => data_bytes.extend_from_slice(&ip.octets()),
            RecordData::Aaaa(ip) => data_bytes.extend_from_slice(&ip.octets()),
            RecordData::Ns(name) | RecordData::Cname(name) => put_name(&mut data_bytes, name),
            RecordData::Txt(text) => {
                for chunk in text.as_bytes().chunks(255) {
                    data_bytes.push(chunk.len() as u8);
                    data_bytes.extend_from_slice(chunk);
                }
            }
            RecordData::Other(_) => {}
        }
        out.extend_from_slice(&(data_bytes.len() as u16).to_be_bytes());
        out.extend_from_slice(&data_bytes);
    }
    out
}

type Answerer = Arc<dyn Fn(&Message) -> Reply + Send + Sync>;

/// Plays a name server on a port of its own on 127.0.0.1 (UDP, and TCP on
/// the same port too if `tcp`), answering each question with `answer`;
/// where it listens, and how many questions it was asked.
async fn serve(tcp: bool, answer: Answerer) -> (SocketAddr, Arc<AtomicUsize>) {
    let asked = Arc::new(AtomicUsize::new(0));
    let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let address = socket.local_addr().unwrap();
    let (udp_answer, udp_asked) = (answer.clone(), asked.clone());
    tokio::spawn(async move {
        let mut buf = vec![0u8; 4096];
        loop {
            let (n, from) = socket.recv_from(&mut buf).await.unwrap();
            let query = parse(&buf[..n]).unwrap();
            udp_asked.fetch_add(1, Ordering::SeqCst);
            let reply = udp_answer(&query);
            socket
                .send_to(&encode_reply(&query, &reply), from)
                .await
                .unwrap();
        }
    });
    if tcp {
        let listener = tokio::net::TcpListener::bind(address).await.unwrap();
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let len = stream.read_u16().await.unwrap() as usize;
                let mut buf = vec![0u8; len];
                stream.read_exact(&mut buf).await.unwrap();
                let query = parse(&buf).unwrap();
                let reply = Reply {
                    truncated: false,
                    ..answer(&query)
                };
                let bytes = encode_reply(&query, &reply);
                stream
                    .write_all(&(bytes.len() as u16).to_be_bytes())
                    .await
                    .unwrap();
                stream.write_all(&bytes).await.unwrap();
            }
        });
    }
    (address, asked)
}

/// A 127.0.0.1 address nothing listens on.
async fn nothing_listening() -> SocketAddr {
    let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    socket.local_addr().unwrap()
}

/// A resolver starting at `roots`, falling back to `fallback`, that finds
/// each documentation address in `servers` where it is played.
fn resolver(roots: &[&str], fallback: &[&str], servers: &[(&str, SocketAddr)]) -> Resolver {
    let ips = |list: &[&str]| list.iter().map(|ip| ip.parse().unwrap()).collect();
    let addresses: HashMap<IpAddr, SocketAddr> = servers
        .iter()
        .map(|(ip, address)| (ip.parse().unwrap(), *address))
        .collect();
    Resolver::new(ips(roots), ips(fallback), Duration::from_secs(2)).with_addresses(addresses)
}

/// A root that sends every question about shop.example to its name server
/// at 192.0.2.3, glue included.
fn root_for_shop() -> Answerer {
    Arc::new(|_| Reply {
        authorities: vec![("shop.example", ns("ns1.shop.example"))],
        additionals: vec![("ns1.shop.example", a("192.0.2.3"))],
        ..Reply::default()
    })
}

#[tokio::test]
async fn a_record_is_read_from_the_zones_own_name_server_through_the_referral() {
    let (root, root_asked) = serve(false, root_for_shop()).await;
    let (zone, zone_asked) = serve(
        false,
        Arc::new(|query| match query.question.as_ref().unwrap().0.as_str() {
            "_monokulo.shop.example" => Reply {
                authoritative: true,
                answers: vec![
                    ("_monokulo.shop.example", txt("monokulo-verify=abc")),
                    ("_monokulo.shop.example", a("10.0.0.1")),
                ],
                ..Reply::default()
            },
            "_none.shop.example" => Reply {
                authoritative: true,
                rcode: RCODE_NXDOMAIN,
                ..Reply::default()
            },
            _ => Reply {
                authoritative: true,
                ..Reply::default()
            },
        }),
    )
    .await;
    let dns = resolver(
        &["192.0.2.2"],
        &[],
        &[("192.0.2.2", root), ("192.0.2.3", zone)],
    );
    assert_eq!(
        dns.txt("_monokulo.Shop.Example.").await.unwrap(),
        ["monokulo-verify=abc"]
    );
    assert_eq!(root_asked.load(Ordering::SeqCst), 1);
    assert_eq!(zone_asked.load(Ordering::SeqCst), 1);
    // No such name, and a name with no TXT: none, not an error.
    assert_eq!(
        dns.txt("_none.shop.example").await.unwrap(),
        Vec::<String>::new()
    );
    assert_eq!(
        dns.txt("www.shop.example").await.unwrap(),
        Vec::<String>::new()
    );
    // Nothing is kept: asking again asks again.
    dns.txt("_monokulo.shop.example").await.unwrap();
    assert_eq!(zone_asked.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn a_name_server_without_glue_is_looked_up_first() {
    let (root, _) = serve(
        false,
        Arc::new(|query| match query.question.as_ref().unwrap() {
            (name, TYPE_A) if name == "ns.elsewhere" => Reply {
                authoritative: true,
                answers: vec![("ns.elsewhere", a("192.0.2.5"))],
                ..Reply::default()
            },
            _ => Reply {
                authorities: vec![("shop.example", ns("ns.elsewhere"))],
                ..Reply::default()
            },
        }),
    )
    .await;
    let (zone, _) = serve(
        false,
        Arc::new(|_| Reply {
            authoritative: true,
            answers: vec![("_monokulo.shop.example", txt("found"))],
            ..Reply::default()
        }),
    )
    .await;
    let dns = resolver(
        &["192.0.2.4"],
        &[],
        &[("192.0.2.4", root), ("192.0.2.5", zone)],
    );
    assert_eq!(dns.txt("_monokulo.shop.example").await.unwrap(), ["found"]);
}

#[tokio::test]
async fn a_cut_answer_is_asked_again_over_tcp() {
    let (root, _) = serve(
        false,
        Arc::new(|_| Reply {
            authorities: vec![("shop.example", ns("ns1.shop.example"))],
            additionals: vec![("ns1.shop.example", a("192.0.2.7"))],
            ..Reply::default()
        }),
    )
    .await;
    let (zone, _) = serve(
        true,
        Arc::new(|_| Reply {
            authoritative: true,
            truncated: true,
            answers: vec![("_monokulo.shop.example", txt(&"v".repeat(600)))],
            ..Reply::default()
        }),
    )
    .await;
    let dns = resolver(
        &["192.0.2.6"],
        &[],
        &[("192.0.2.6", root), ("192.0.2.7", zone)],
    );
    assert_eq!(
        dns.txt("_monokulo.shop.example").await.unwrap(),
        ["v".repeat(600)]
    );
}

#[tokio::test]
async fn a_cname_is_followed_and_a_cname_loop_is_refused() {
    let (root, _) = serve(
        false,
        Arc::new(|query| match query.question.as_ref().unwrap().0.as_str() {
            "_monokulo.shop.example" => Reply {
                authoritative: true,
                answers: vec![
                    (
                        "_monokulo.shop.example",
                        RecordData::Cname("verify.shop.example".into()),
                    ),
                    ("verify.shop.example", txt("via cname")),
                ],
                ..Reply::default()
            },
            _ => Reply {
                authoritative: true,
                answers: vec![(
                    "loop.shop.example",
                    RecordData::Cname("loop.shop.example".into()),
                )],
                ..Reply::default()
            },
        }),
    )
    .await;
    let dns = resolver(&["192.0.2.8"], &[], &[("192.0.2.8", root)]);
    assert_eq!(
        dns.txt("_monokulo.shop.example").await.unwrap(),
        ["via cname"]
    );
    assert_eq!(dns.txt("loop.shop.example").await, Err(DnsError::TooLong));
}

#[tokio::test]
async fn unreachable_roots_fall_back_to_the_machines_resolver() {
    let unreachable = nothing_listening().await;
    let (machine, asked) = serve(
        false,
        Arc::new(|_| Reply {
            answers: vec![("_monokulo.shop.example", txt("from the resolver"))],
            ..Reply::default()
        }),
    )
    .await;
    let servers = [("192.0.2.9", unreachable), ("192.0.2.10", machine)];
    let dns = resolver(&["192.0.2.9"], &["192.0.2.10"], &servers);
    assert_eq!(
        dns.txt("_monokulo.shop.example").await.unwrap(),
        ["from the resolver"]
    );
    assert_eq!(asked.load(Ordering::SeqCst), 1);
    // With no fallback, the failure is the answer.
    assert!(resolver(&["192.0.2.9"], &[], &servers)
        .txt("_monokulo.shop.example")
        .await
        .is_err());
}

/// Needs the network, so it runs daily (`cargo xtask live`) rather than on
/// every change.
#[tokio::test]
#[ignore = "needs live DNS"]
async fn live_txt_records_are_read_from_the_root_servers_down() {
    let dns = Resolver::new(
        ROOT_SERVERS.into_iter().map(IpAddr::V4).collect(),
        Vec::new(),
        Duration::from_secs(2),
    );
    let values = dns.txt("_dmarc.google.com").await.unwrap();
    assert!(
        values.iter().any(|v| v.starts_with("v=DMARC1")),
        "got: {values:?}"
    );
    assert_eq!(dns.txt("_monokulo.example.invalid").await, Ok(vec![]));
}
