//! What a page outside the POS needs to show an order's status as the POS
//! draws it, and the API reference's status chips.

use std::collections::BTreeSet;
use ui::ir::{Attr, Node};
use ui::{assets, Component, HttpOutcome, HttpStatusTag, OrderState, StatusIcon};

/// The symbols a template points at: every `href="#..."` it can render,
/// read from its description, whichever branch renders it.
fn symbols_used(nodes: &[Node], used: &mut BTreeSet<&'static str>) {
    for node in nodes {
        match node {
            Node::Element(element) => {
                for attr in &element.attrs {
                    if let Attr::Static("href", target) = attr {
                        used.insert(target.trim_start_matches('#'));
                    }
                }
                symbols_used(element.children.as_deref().unwrap_or_default(), used);
            }
            Node::If {
                then, otherwise, ..
            }
            | Node::IfLet {
                then, otherwise, ..
            } => {
                symbols_used(then, used);
                symbols_used(otherwise, used);
            }
            Node::For { body, .. } => symbols_used(body, used),
            Node::Enhanced { body, fallback, .. } => {
                symbols_used(body, used);
                symbols_used(fallback, used);
            }
            Node::Text(_) | Node::Value(_) => {}
        }
    }
}

/// Every symbol the icon can point at is one `STATUS_SYMBOLS` defines, so
/// a page that includes them once draws every coin and mark.
#[test]
fn every_symbol_an_icon_uses_is_in_the_symbols_a_page_includes() {
    let mut used = BTreeSet::new();
    symbols_used(&StatusIcon::info().template, &mut used);
    let defined: BTreeSet<&str> = assets::STATUS_SYMBOLS
        .split("id=\"")
        .skip(1)
        .map(|rest| &rest[..rest.find('"').expect("the id's end")])
        .collect();
    // Partial, paid, overpaid, expired, double spend, offline, cancelled.
    assert_eq!(used.len(), 7, "{used:?}");
    assert_eq!(
        used.difference(&defined).collect::<Vec<_>>(),
        Vec::<&&str>::new(),
        "symbols the icon uses that STATUS_SYMBOLS doesn't define"
    );
}

/// A confirming order's wedge is its percent; without one, the disc is
/// empty, as an unconfirmed order's.
#[test]
fn a_confirming_icon_fills_its_wedge_by_its_progress() {
    assert_eq!(
        StatusIcon::new(OrderState::Confirming)
            .progress(60u32)
            .render()
            .into_string(),
        r#"<span class="pos-icon pos-icon-confirming state-confirming" aria-hidden="true"><span class="pos-disc" style="--progress: 60%;"></span></span>"#
    );
    assert_eq!(
        StatusIcon::new(OrderState::Confirming)
            .render()
            .into_string(),
        r#"<span class="pos-icon pos-icon-confirming state-confirming" aria-hidden="true"><span class="pos-disc"></span></span>"#
    );
}

/// A status code's tint follows its first digit: 2xx and 3xx worked, 4xx
/// is the client's, 5xx the server's, anything else neutral.
#[test]
fn a_status_codes_kind_is_its_first_digit() {
    for (code, outcome) in [
        ("200", HttpOutcome::Ok),
        ("204", HttpOutcome::Ok),
        ("303", HttpOutcome::Ok),
        ("404", HttpOutcome::ClientError),
        ("4xx", HttpOutcome::ClientError),
        ("503", HttpOutcome::ServerError),
        ("default", HttpOutcome::Other),
        ("", HttpOutcome::Other),
    ] {
        assert_eq!(HttpOutcome::of(code), outcome, "{code:?}");
    }
    assert_eq!(
        HttpStatusTag::of("429").render().into_string(),
        r#"<span class="mk-http-status" data-outcome="client-error">429</span>"#
    );
}
