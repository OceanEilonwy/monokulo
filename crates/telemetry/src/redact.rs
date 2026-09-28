//! What never reaches a log line (structured_logging.md task 1.4).
//!
//! Every output (stderr now, the log store and OTLP later) formats field
//! values through [`field`], so these rules hold in every mode, including
//! development mode:
//!
//! - A field whose name says it holds a secret (`secret_token`,
//!   `view_key`, `password`, anything ending in `_key` or `token` apart
//!   from a few known-public names) is replaced by [`REDACTED`].
//! - A field whose name says it holds a client's IP address keeps only its
//!   network: the last octet of an IPv4 address and all but the first 48
//!   bits of an IPv6 address are zeroed, and the port is dropped.
//!   Loopback and private addresses are kept whole; they identify nobody.
//! - In every string, including the message itself, a Monero address is
//!   shortened to its first 6 and last 4 characters (enough to tell two
//!   apart, not enough to pay or look up), and a store secret key
//!   (`sk_` followed by hex) loses everything after `sk_`.
//!
//! Secret values should also never be passed to a log macro in the first
//! place; these rules catch the mistakes.

use std::borrow::Cow;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// What a redacted value is replaced with.
pub const REDACTED: &str = "[redacted]";

/// Field names that end like a secret's but hold nothing secret.
const PUBLIC_NAMES: &[&str] = &["public_key", "idempotency_key", "throttle_key", "cache_key", "settings_key"];

/// Field names that hold a secret whatever their ending.
const SECRET_NAMES: &[&str] =
    &["password", "authorization", "cookie", "seed", "mnemonic", "key_material", "sealed_key_material", "secret"];

/// Field names that hold a client's address.
const CLIENT_ADDRESS_NAMES: &[&str] =
    &["client.address", "client.ip", "client_ip", "client_addr", "peer_addr", "remote_addr", "network.peer.address", "ip"];

/// Whether a field with this name holds a secret.
pub fn is_secret_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let last = name.rsplit('.').next().unwrap_or(&name);
    if PUBLIC_NAMES.contains(&last) {
        return false;
    }
    SECRET_NAMES.contains(&last)
        || last.ends_with("_key")
        || last.ends_with("token")
        || last.ends_with("secret")
        || last.ends_with("password")
}

/// Whether a field with this name holds a client's IP address.
pub fn is_client_address_name(name: &str) -> bool {
    CLIENT_ADDRESS_NAMES.contains(&name.to_ascii_lowercase().as_str())
}

/// The value of field `name` as it may be logged.
pub fn field<'a>(name: &str, value: &'a str) -> Cow<'a, str> {
    if is_secret_name(name) {
        return Cow::Borrowed(REDACTED);
    }
    if is_client_address_name(name) {
        return Cow::Owned(client_address(value));
    }
    text(value)
}

/// A client address with only its network left, or [`REDACTED`] when it
/// doesn't parse as an IP address (with or without a port).
pub fn client_address(value: &str) -> String {
    let ip = value
        .parse::<SocketAddr>()
        .map(|a| a.ip())
        .or_else(|_| value.parse::<IpAddr>())
        .or_else(|_| value.trim_matches(['[', ']']).parse::<IpAddr>());
    match ip {
        Ok(ip) => anonymize_ip(ip).to_string(),
        Err(_) => REDACTED.to_string(),
    }
}

fn anonymize_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(v4) if v4.is_loopback() || v4.is_private() || v4.is_unspecified() => ip,
        IpAddr::V4(v4) => {
            let [a, b, c, _] = v4.octets();
            IpAddr::V4(Ipv4Addr::new(a, b, c, 0))
        }
        IpAddr::V6(v6) if v6.is_loopback() || v6.is_unspecified() => ip,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => anonymize_ip(IpAddr::V4(v4)),
            None => {
                let s = v6.segments();
                IpAddr::V6(Ipv6Addr::new(s[0], s[1], s[2], 0, 0, 0, 0, 0))
            }
        },
    }
}

/// `value` with Monero addresses shortened and store secret keys removed.
/// Borrows when there is nothing to change, which is almost always.
pub fn text(value: &str) -> Cow<'_, str> {
    if !value.contains("sk_") && !has_long_base58_run(value) {
        return Cow::Borrowed(value);
    }
    let mut out = String::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if is_base58(bytes[i]) && (i == 0 || !is_word(bytes[i - 1])) {
            let end = run_end(bytes, i, is_word);
            let word = &value[i..end];
            out.push_str(&word_redacted(word));
            i = end;
        } else {
            // Not the start of a word: copy up to the next char boundary.
            let ch_len = value[i..].chars().next().map_or(1, char::len_utf8);
            out.push_str(&value[i..i + ch_len]);
            i += ch_len;
        }
    }
    Cow::Owned(out)
}

fn word_redacted(word: &str) -> Cow<'_, str> {
    if let Some(rest) = word.strip_prefix("sk_") {
        if rest.len() >= 16 && rest.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Cow::Owned(format!("sk_{REDACTED}"));
        }
    }
    if is_monero_address(word) {
        return Cow::Owned(format!("{}…{}", &word[..6], &word[word.len() - 4..]));
    }
    Cow::Borrowed(word)
}

/// Standard (95 characters) and integrated (106) addresses on every
/// network start with one of these.
fn is_monero_address(word: &str) -> bool {
    matches!(word.len(), 95 | 106)
        && word.bytes().all(is_base58)
        && matches!(word.as_bytes()[0], b'4' | b'8' | b'5' | b'7' | b'9' | b'A' | b'B')
}

fn has_long_base58_run(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if is_base58(bytes[i]) {
            let end = run_end(bytes, i, is_base58);
            if end - i >= 95 {
                return true;
            }
            i = end;
        } else {
            i += 1;
        }
    }
    false
}

fn run_end(bytes: &[u8], start: usize, keep: fn(u8) -> bool) -> usize {
    let mut end = start;
    while end < bytes.len() && keep(bytes[end]) {
        end += 1;
    }
    end
}

fn is_base58(b: u8) -> bool {
    b.is_ascii_alphanumeric() && !matches!(b, b'0' | b'O' | b'I' | b'l')
}

/// Characters that make up one token for [`text`]: `sk_` keys contain an
/// underscore, addresses don't.
fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A made-up address of the standard length (95) and one of the
    /// integrated length (106); only the shape matters here.
    fn address() -> String {
        format!("44AFFq{}wEP3A", "5kSiGBoZ4NMDwYtN18obc8".repeat(4).chars().take(84).collect::<String>())
    }

    fn integrated() -> String {
        format!("4LL9oS{}khPK", "LmtpccfufTMvppY6JwXN".repeat(5).chars().take(96).collect::<String>())
    }

    #[test]
    fn secret_names_are_redacted_whatever_their_prefix() {
        for name in [
            "secret_token",
            "admin_token",
            "engine.admin_token",
            "view_key",
            "spend_key",
            "encryption_key",
            "password",
            "Authorization",
            "cookie",
            "seed",
            "sealed_key_material",
            "webhook_secret",
        ] {
            assert_eq!(field(name, "abc"), REDACTED, "{name}");
        }
    }

    #[test]
    fn public_names_and_ordinary_fields_are_kept() {
        for name in ["public_key", "idempotency_key", "throttle_key", "key", "store.id", "order.id", "network", "message"] {
            assert_eq!(field(name, "abc"), "abc", "{name}");
        }
    }

    #[test]
    fn client_addresses_keep_only_their_network() {
        assert_eq!(field("client.address", "203.0.113.77:51234"), "203.0.113.0");
        assert_eq!(field("client.address", "203.0.113.77"), "203.0.113.0");
        assert_eq!(field("client.address", "[2001:db8:abcd:12:1:2:3:4]:443"), "2001:db8:abcd::");
        assert_eq!(field("client.address", "::ffff:203.0.113.77"), "203.0.113.0");
        assert_eq!(field("client.address", "127.0.0.1:9000"), "127.0.0.1");
        assert_eq!(field("client.address", "192.168.1.20"), "192.168.1.20");
        assert_eq!(field("client.address", "not an ip"), REDACTED);
    }

    #[test]
    fn monero_addresses_are_shortened_anywhere_in_a_string() {
        let (address, integrated) = (address(), integrated());
        assert_eq!((address.len(), integrated.len()), (95, 106));
        assert_eq!(text(&address), "44AFFq…EP3A");
        let message = format!("payment to {address}, and to {integrated}.");
        assert_eq!(text(&message), "payment to 44AFFq…EP3A, and to 4LL9oS…khPK.");
        // A field with an ordinary name still gets it.
        assert_eq!(field("primary_address", &address), "44AFFq…EP3A");
    }

    #[test]
    fn store_secret_keys_are_removed_anywhere_in_a_string() {
        let secret = format!("sk_{}", "ab".repeat(32));
        assert_eq!(text(&format!("rotated to {secret} ok")), "rotated to sk_[redacted] ok");
        assert_eq!(field("detail", &format!("({secret})")), "(sk_[redacted])");
    }

    #[test]
    fn strings_without_anything_to_redact_are_borrowed_unchanged() {
        for s in ["scan tick failed for Stagenet: timeout", "", "tx 5f2c9e", "é ünïcode ✓"] {
            assert!(matches!(text(s), Cow::Borrowed(b) if b == s), "{s}");
        }
        // Too short to be a key: copied, but unchanged.
        assert_eq!(text("sk_short, sk_12"), "sk_short, sk_12");
        // Words that merely look long, or an address-length run with a
        // character base58 lacks, are left alone.
        let not_address = format!("0{}", &address()[1..]);
        assert_eq!(text(&not_address), not_address);
        let hex_hash = "a".repeat(64);
        assert_eq!(text(&hex_hash), hex_hash);
    }

    #[test]
    fn unicode_around_a_redaction_survives() {
        assert_eq!(text(&format!("→{}←", address())), "→44AFFq…EP3A←");
    }
}
