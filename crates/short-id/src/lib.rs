//! How every shortened identifier reads: order ids, keys, txids, event,
//! session and trace ids, Monero addresses and attestation measurements.
//! Each is cut to a fixed length here, as text, so no CSS ever truncates
//! one by width and nothing else has a rule of its own: monokulo's pages
//! (`views::short_id`), its POS app (sent these by the server) and the
//! browser's key checker (`key-custody`'s WebAssembly) all call these.
//!
//! | Value | Shown as |
//! |---|---|
//! | Order id (`order_` + hex) | `a8723b…b0d44e` (`order_` dropped) |
//! | Prefixed id (`pk_`, `sk_`, `evt_`, ...) | `pk_b4c4e8…3fa21c` |
//! | Txid (64 hex) | `3f9a1c…e21b07` |
//! | Anything 16 characters or fewer after its prefix | whole |
//! | Monero address, measurement | `44AFFq…Xft3Xj…QBEP3A` (start, middle, end) |

/// How many characters a shortened identifier keeps at each end (and, for
/// an address, in its middle).
const EDGE: usize = 6;
/// Shortening that saves fewer characters than this isn't worth it: the
/// value is shown whole instead.
const MIN_SAVING: usize = 4;
/// The ellipsis that stands for what's cut: always the one character
/// U+2026, never three dots.
const ELLIPSIS: char = '…';

/// An identifier as it's shown wherever it's shortened.
///
/// - A known prefix stays: a short lowercase word ending in `_` (`pk_`,
///   `sk_`, `evt_`, `sess_`, ...). `order_` is the exception: every order id
///   starts with it, so it's dropped.
/// - After the prefix come the first 6 characters, "…" (one U+2026, never
///   three dots) and the last 6: `order_a8723b…b0d44e` shows as
///   `a8723b…b0d44e`, a store's public key as `pk_b4c4e8…3fa21c`, a txid as
///   `3f9a1c…e21b07`.
/// - When that wouldn't save at least 4 characters (16 or fewer after the
///   prefix), the value is shown whole.
///
/// Counts characters, not bytes, so it never cuts one in two. A Monero
/// address has its own rule, [`short_address`].
pub fn short_id(value: &str) -> String {
    let value = value.strip_prefix("order_").unwrap_or(value);
    let (prefix, body) = split_prefix(value);
    let chars: Vec<char> = body.chars().collect();
    if chars.len() < 2 * EDGE + 1 + MIN_SAVING {
        return value.to_owned();
    }
    let head: String = chars[..EDGE].iter().collect();
    let tail: String = chars[chars.len() - EDGE..].iter().collect();
    format!("{prefix}{head}{ELLIPSIS}{tail}")
}

/// A Monero address (or another long value best told apart by its middle
/// too, like an attestation measurement) shortened: its first 6
/// characters, "…", the 6 centred on its midpoint (`len/2 - 3 .. len/2 +
/// 3`), "…" and its last 6, so two addresses that share a start and an end
/// still look different: `44AFFq…Xft3Xj…QBEP3A`. Whole when that wouldn't
/// save at least 4 characters. Counts characters, not bytes.
pub fn short_address(address: &str) -> String {
    let chars: Vec<char> = address.chars().collect();
    let len = chars.len();
    if len < 3 * EDGE + 2 + MIN_SAVING {
        return address.to_owned();
    }
    let middle = len / 2 - EDGE / 2;
    let part = |from: usize| chars[from..from + EDGE].iter().collect::<String>();
    format!(
        "{}{ELLIPSIS}{}{ELLIPSIS}{}",
        part(0),
        part(middle),
        part(len - EDGE)
    )
}

/// `pk_b4c4…` as `("pk_", "b4c4…")`: a prefix is 1 to 8 lowercase ASCII
/// letters and a `_`. Without one, `("", value)`.
fn split_prefix(value: &str) -> (&str, &str) {
    match value.find('_') {
        Some(i) if (1..=8).contains(&i) && value[..i].bytes().all(|b| b.is_ascii_lowercase()) => {
            value.split_at(i + 1)
        }
        _ => ("", value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADDRESS: &str = "44AFFq5kSiGBoZ4NMDwYtN18obc8AemS33DBLWs3H7otXft3XjrpDtQGv7SqSsaBYBb98uNbr2VBBEt7f2wfn3RVGQBEP3A";

    /// What each kind of value is shortened to: six characters at each
    /// end (after a known prefix, which stays; `order_` goes), an address
    /// or measurement's middle too, centred; a value shortening would save
    /// fewer than four characters of stays whole; one ellipsis, never
    /// three dots.
    #[test]
    fn each_kind_of_value_keeps_its_ends_and_an_address_its_middle() {
        assert_eq!(ADDRESS.len(), 95);
        let txid = format!("3f9a1c{}e21b07", "9".repeat(52));
        assert_eq!(txid.len(), 64);
        for (value, short) in [
            (txid, "3f9a1c…e21b07"),
            (
                "order_a8723b2e45b0d44ea8723b2e45b0d44e".to_owned(),
                "a8723b…b0d44e",
            ),
            // Short enough to show whole: still without `order_`.
            ("order_a8723b2e45b0d44e".to_owned(), "a8723b2e45b0d44e"),
            ("abc".to_owned(), "abc"),
            (
                format!("pk_b4c4e8{}3fa21c", "0".repeat(36)),
                "pk_b4c4e8…3fa21c",
            ),
            (format!("sk_{}", "ab".repeat(32)), "sk_ababab…ababab"),
            (
                "evt_0123456789abcdef0123456789abcdef".to_owned(),
                "evt_012345…abcdef",
            ),
            (format!("sess_{}", "f".repeat(64)), "sess_ffffff…ffffff"),
            // Not a prefix: too long a word, capitals, or nothing before the `_`.
            ("verylongword_0123456789".to_owned(), "verylo…456789"),
            ("PK_0123456789abcdef".to_owned(), "PK_012…abcdef"),
            ("_0123456789abcdef".to_owned(), "_01234…abcdef"),
            // 13 characters are shown, so 17 is the shortest value cut,
            // counted after the prefix.
            ("0123456789abcdef".to_owned(), "0123456789abcdef"),
            ("0123456789abcdefg".to_owned(), "012345…bcdefg"),
            ("evt_0123456789abcdef".to_owned(), "evt_0123456789abcdef"),
            ("evt_0123456789abcdefg".to_owned(), "evt_012345…bcdefg"),
            (String::new(), ""),
        ] {
            assert_eq!(short_id(&value), short, "{value}");
        }
        for (value, short) in [
            (ADDRESS.to_owned(), "44AFFq…Xft3Xj…QBEP3A"),
            // 48 bytes in hex: 96 characters, the middle 6 at 45 to 50.
            (
                format!("{}{}{}", "a".repeat(45), "012345", "b".repeat(45)),
                "aaaaaa…012345…bbbbbb",
            ),
            // Odd, 25: len/2 is 12, so characters 9 to 14.
            (
                "abcdefghijklmnopqrstuvwxy".to_owned(),
                "abcdef…jklmno…tuvwxy",
            ),
            // Even, 26: len/2 is 13, so characters 10 to 15.
            (
                "abcdefghijklmnopqrstuvwxyz".to_owned(),
                "abcdef…klmnop…uvwxyz",
            ),
            // 20 are shown, so 24 is the shortest cut.
            (
                "0123456789abcdefghijklm".to_owned(),
                "0123456789abcdefghijklm",
            ),
            (
                "0123456789abcdefghijklmn".to_owned(),
                "012345…9abcde…ijklmn",
            ),
        ] {
            assert_eq!(short_address(&value), short, "{value}");
        }
        for value in [
            "order_a8723b2e45b0d44ea8723b2e45b0d44e".to_owned(),
            format!("pk_{}", "a".repeat(48)),
            "f".repeat(64),
            ADDRESS.to_owned(),
        ] {
            for short in [short_id(&value), short_address(&value)] {
                assert!(!short.contains("..."), "{short}");
                assert!(short.contains('…'), "{short}");
            }
        }
    }

    #[test]
    fn shortening_cuts_on_character_boundaries() {
        assert_eq!(short_id("evt_ααααααββββββββγγγγγγ"), "evt_αααααα…γγγγγγ");
        assert_eq!(short_id(&format!("é{}ü", "€".repeat(30))), "é€€€€€…€€€€€ü");
        let address = format!("ä{}ö", "ß".repeat(40));
        assert_eq!(short_address(&address), "äßßßßß…ßßßßßß…ßßßßßö");
    }
}
