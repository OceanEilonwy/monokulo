//! Every shortened identifier on the site as markup: the text by the
//! `short-id` crate's one rule (`a8723b…b0d44e`, `pk_b4c4e8…3fa21c`,
//! `44AFFq…Xft3Xj…QBEP3A`), the whole value still in reach.

use maud::{html, Markup};

pub use short_id::{short_address as short_address_text, short_id as short_id_text};

/// An identifier shortened by [`short_id_text`], the whole value in reach
/// ([`shortened`]). A value shown whole is plain text.
pub fn short_id(value: &str) -> Markup {
    shortened(value, &short_id_text(value))
}

/// A Monero address shortened by [`short_address_text`], the whole address
/// in reach as with [`short_id`].
pub fn short_address(address: &str) -> Markup {
    shortened(address, &short_address_text(address))
}

/// `short` on screen, `full` everywhere else (`.short-value`, site.css):
///
/// - The short text shows, `aria-hidden` and not selectable.
/// - The full value lies over it, transparent and clipped to its box: a
///   double-click selects it (an id is one word) and a copy takes exactly
///   the full value; a screen reader reads it; find-in-page matches it.
/// - It's in `title` too, on hover.
fn shortened(full: &str, short: &str) -> Markup {
    html! {
        @if short == full { (full) } @else {
            span class="short-value" title=(full) {
                span class="short-value-text" aria-hidden="true" { (short) }
                span class="short-value-full" { (full) }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADDRESS: &str = "44AFFq5kSiGBoZ4NMDwYtN18obc8AemS33DBLWs3H7otXft3XjrpDtQGv7SqSsaBYBb98uNbr2VBBEt7f2wfn3RVGQBEP3A";

    #[test]
    fn a_shortened_value_shows_the_short_text_over_the_whole() {
        let id = "order_a8723b2e45b0d44ea8723b2e45b0d44e";
        assert_eq!(
            short_id(id).into_string(),
            format!(
                r#"<span class="short-value" title="{id}"><span class="short-value-text" aria-hidden="true">a8723b…b0d44e</span><span class="short-value-full">{id}</span></span>"#
            )
        );
        assert_eq!(
            short_address(ADDRESS).into_string(),
            format!(
                r#"<span class="short-value" title="{ADDRESS}"><span class="short-value-text" aria-hidden="true">44AFFq…Xft3Xj…QBEP3A</span><span class="short-value-full">{ADDRESS}</span></span>"#
            )
        );
        // Whole: plain text, nothing hidden.
        assert_eq!(short_id("pay_abc123").into_string(), "pay_abc123");
        // Whole but for `order_`: the full id is still in reach.
        assert_eq!(
            short_id("order_abc").into_string(),
            r#"<span class="short-value" title="order_abc"><span class="short-value-text" aria-hidden="true">abc</span><span class="short-value-full">order_abc</span></span>"#
        );
    }

    #[test]
    fn a_shortened_value_never_shows_three_dots() {
        for value in [
            "order_a8723b2e45b0d44ea8723b2e45b0d44e".to_owned(),
            format!("pk_{}", "a".repeat(48)),
            "f".repeat(64),
            ADDRESS.to_owned(),
        ] {
            for html in [
                short_id(&value).into_string(),
                short_address(&value).into_string(),
            ] {
                assert!(!html.contains("..."), "{html}");
                assert!(html.contains('…'), "{html}");
            }
        }
    }
}
