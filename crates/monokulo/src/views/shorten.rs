//! Every shortened identifier on the site: order ids, keys, txids, event,
//! session and trace ids, and Monero addresses. Each is cut here, on the
//! server, to a fixed length, so no CSS ever truncates one by width.
//!
//! | Value | Shown as |
//! |---|---|
//! | Order id (`order_` + hex) | `a8723b…b0d44e` (`order_` dropped) |
//! | Prefixed id (`pk_`, `sk_`, `evt_`, ...) | `pk_b4c4e8…3fa21c` |
//! | Txid (64 hex) | `3f9a1c…e21b07` |
//! | Anything 16 characters or fewer after its prefix | whole |
//! | Monero address | `44AFFq…Xft3Xj…QBEP3A` (start, middle, end) |

use maud::{html, Markup};

/// How many characters a shortened identifier keeps at each end (and, for
/// an address, in its middle).
const EDGE: usize = 6;
/// Shortening that saves fewer characters than this isn't worth it: the
/// value is shown whole instead.
const MIN_SAVING: usize = 4;
/// The ellipsis that stands for what's cut: always the one character
/// U+2026, never three dots.
const ELLIPSIS: char = '…';

/// An identifier as the site shows it wherever it's shortened, as text.
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
/// address has its own rule, [`short_address_text`].
pub fn short_id_text(value: &str) -> String {
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

/// A Monero address shortened as text: its first 6 characters, "…", the 6
/// centred on its midpoint (`len/2 - 3 .. len/2 + 3`), "…" and its last 6,
/// so two addresses that share a start and an end still look different:
/// `44AFFq…Xft3Xj…QBEP3A`. Whole when that wouldn't save at least 4
/// characters. Counts characters, not bytes.
pub fn short_address_text(address: &str) -> String {
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

/// An identifier shortened by [`short_id_text`], with the whole value still
/// in reach: in `title` (on hover), and as visually hidden text, which is
/// what a screen reader reads (the short form is `aria-hidden`) and what
/// find-in-page matches. A value shown whole is plain text.
pub fn short_id(value: &str) -> Markup {
    shortened(value, &short_id_text(value))
}

/// A Monero address shortened by [`short_address_text`], the whole address
/// in reach as with [`short_id`].
pub fn short_address(address: &str) -> Markup {
    shortened(address, &short_address_text(address))
}

fn shortened(full: &str, short: &str) -> Markup {
    html! {
        @if short == full { (full) } @else {
            span title=(full) {
                span aria-hidden="true" { (short) }
                span class="sr-only" { (full) }
            }
        }
    }
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

    #[test]
    fn an_order_id_drops_order_and_keeps_six_characters_at_each_end() {
        assert_eq!(
            short_id_text("order_a8723b2e45b0d44ea8723b2e45b0d44e"),
            "a8723b…b0d44e"
        );
        // Short enough to show whole: still without `order_`.
        assert_eq!(short_id_text("order_a8723b2e45b0d44e"), "a8723b2e45b0d44e");
        assert_eq!(short_id_text("abc"), "abc");
    }

    #[test]
    fn a_known_prefix_stays_in_front_of_the_shortened_value() {
        let pk = format!("pk_b4c4e8{}3fa21c", "0".repeat(36));
        assert_eq!(short_id_text(&pk), "pk_b4c4e8…3fa21c");
        assert_eq!(
            short_id_text(&format!("sk_{}", "ab".repeat(32))),
            "sk_ababab…ababab"
        );
        assert_eq!(
            short_id_text("evt_0123456789abcdef0123456789abcdef"),
            "evt_012345…abcdef"
        );
        assert_eq!(
            short_id_text(&format!("sess_{}", "f".repeat(64))),
            "sess_ffffff…ffffff"
        );
        // Not a prefix: too long a word, capitals, or nothing before the `_`.
        assert_eq!(short_id_text("verylongword_0123456789"), "verylo…456789");
        assert_eq!(short_id_text("PK_0123456789abcdef"), "PK_012…abcdef");
        assert_eq!(short_id_text("_0123456789abcdef"), "_01234…abcdef");
    }

    #[test]
    fn a_txid_keeps_six_characters_at_each_end() {
        let txid = format!("3f9a1c{}e21b07", "9".repeat(52));
        assert_eq!(txid.len(), 64);
        assert_eq!(short_id_text(&txid), "3f9a1c…e21b07");
    }

    #[test]
    fn a_value_is_whole_when_shortening_would_save_fewer_than_four_characters() {
        // 13 characters are shown, so 17 is the shortest value cut.
        assert_eq!(short_id_text("0123456789abcdef"), "0123456789abcdef");
        assert_eq!(short_id_text("0123456789abcdefg"), "012345…bcdefg");
        // Counted after the prefix: an event id of 16 is whole.
        assert_eq!(
            short_id_text("evt_0123456789abcdef"),
            "evt_0123456789abcdef"
        );
        assert_eq!(short_id_text("evt_0123456789abcdefg"), "evt_012345…bcdefg");
        assert_eq!(short_id_text(""), "");
        // An address: 20 are shown, so 24 is the shortest cut.
        let whole = "0123456789abcdefghijklm";
        assert_eq!(short_address_text(whole), whole);
        assert_eq!(
            short_address_text("0123456789abcdefghijklmn"),
            "012345…9abcde…ijklmn"
        );
    }

    #[test]
    fn an_address_shows_its_start_its_middle_and_its_end() {
        assert_eq!(ADDRESS.len(), 95);
        assert_eq!(short_address_text(ADDRESS), "44AFFq…Xft3Xj…QBEP3A");
    }

    #[test]
    fn an_address_middle_is_centred_on_odd_and_even_lengths() {
        // Odd, 25: len/2 is 12, so characters 9 to 14.
        assert_eq!(
            short_address_text("abcdefghijklmnopqrstuvwxy"),
            "abcdef…jklmno…tuvwxy"
        );
        // Even, 26: len/2 is 13, so characters 10 to 15.
        assert_eq!(
            short_address_text("abcdefghijklmnopqrstuvwxyz"),
            "abcdef…klmnop…uvwxyz"
        );
    }

    #[test]
    fn shortening_cuts_on_character_boundaries() {
        assert_eq!(
            short_id_text("evt_ααααααββββββββγγγγγγ"),
            "evt_αααααα…γγγγγγ"
        );
        assert_eq!(
            short_id_text(&format!("é{}ü", "€".repeat(30))),
            "é€€€€€…€€€€€ü"
        );
        let address = format!("ä{}ö", "ß".repeat(40));
        assert_eq!(short_address_text(&address), "äßßßßß…ßßßßßß…ßßßßßö");
    }

    #[test]
    fn a_shortened_value_keeps_the_whole_in_its_title_and_accessible_text() {
        let id = "order_a8723b2e45b0d44ea8723b2e45b0d44e";
        assert_eq!(
            short_id(id).into_string(),
            format!(
                r#"<span title="{id}"><span aria-hidden="true">a8723b…b0d44e</span><span class="sr-only">{id}</span></span>"#
            )
        );
        assert_eq!(
            short_address(ADDRESS).into_string(),
            format!(
                r#"<span title="{ADDRESS}"><span aria-hidden="true">44AFFq…Xft3Xj…QBEP3A</span><span class="sr-only">{ADDRESS}</span></span>"#
            )
        );
        // Whole: plain text, nothing hidden.
        assert_eq!(short_id("pay_abc123").into_string(), "pay_abc123");
        // Whole but for `order_`: the full id is still in reach.
        assert_eq!(
            short_id("order_abc").into_string(),
            r#"<span title="order_abc"><span aria-hidden="true">abc</span><span class="sr-only">order_abc</span></span>"#
        );
    }

    #[test]
    fn a_shortened_value_shows_one_ellipsis_never_three_dots() {
        let values = [
            "order_a8723b2e45b0d44ea8723b2e45b0d44e".to_owned(),
            format!("pk_{}", "a".repeat(48)),
            "f".repeat(64),
            ADDRESS.to_owned(),
        ];
        for value in &values {
            for html in [
                short_id(value).into_string(),
                short_address(value).into_string(),
            ] {
                assert!(!html.contains("..."), "{html}");
                assert!(html.contains('…'), "{html}");
            }
        }
    }
}
