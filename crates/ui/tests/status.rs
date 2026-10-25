//! What a page outside the POS needs to show an order's status as the POS
//! draws it, and the API reference's status chips.

use ui::{assets, HttpOutcome, HttpStatusTag, OrderState, StatusIcon};

/// Every state's icon points only at symbols `STATUS_SYMBOLS` defines, so
/// a page that includes them once draws every coin and mark; and the
/// stylesheet a page opts into is the one that draws them.
#[test]
fn every_symbol_an_icon_uses_is_in_the_symbols_a_page_includes() {
    for state in [
        OrderState::Pending,
        OrderState::Unconfirmed,
        OrderState::Confirming,
        OrderState::Partial,
        OrderState::Paid,
        OrderState::Overpaid,
        OrderState::Expired,
        OrderState::Cancelled,
        OrderState::DoubleSpend,
        OrderState::Offline,
        OrderState::Unknown,
    ] {
        let html = StatusIcon::new(state).render().into_string();
        for rest in html.split("href=\"#").skip(1) {
            let id = &rest[..rest.find('"').expect("the id's end")];
            assert!(
                assets::STATUS_SYMBOLS.contains(&format!("id=\"{id}\"")),
                "{state}: #{id} isn't in STATUS_SYMBOLS"
            );
        }
    }
    assert!(assets::STATUS_CSS.contains(".pos-icon"));
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
        r#"<span class="pos-icon pos-icon-confirming" aria-hidden="true"><span class="pos-disc" style="--progress: 60%;"></span></span>"#
    );
    assert_eq!(
        StatusIcon::new(OrderState::Confirming)
            .render()
            .into_string(),
        r#"<span class="pos-icon pos-icon-confirming" aria-hidden="true"><span class="pos-disc"></span></span>"#
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
