//! `GET/POST /dashboard/stores/{id}/orders/new` -
//! `http::orders::create_order_page`/`create_order`.
//!
//! A widget page, same shape as [`super::pos`]'s POS terminal: rather than a
//! form embedded inline on the busy store overview page, this is its own
//! full page a merchant navigates to (and back from) - reachable from a
//! "Create an order" tile on the store page's own widget row.

use maud::{html, Markup};

use super::{layout_with_head, script, Load, PageChrome};

pub struct CreateOrderData {
    pub connection_id: crate::db::ConnectionId,
    pub display_name: String,
    /// Set only when a submission from this exact page was just rejected -
    /// the engine's own validation error, surfaced verbatim. `None` on a
    /// plain page load.
    pub order_creation_error: Option<String>,
    /// Every currency the form's dropdown can offer right now - always
    /// includes `"XMR"` first.
    pub order_currency_options: Vec<String>,
    /// `true` exactly when `order_currency_options` is `["XMR"]` alone (no
    /// fiat provider enabled/configured for this store) - shows a plain
    /// `readonly` "XMR" field instead of a one-option dropdown.
    pub order_currency_is_locked_to_xmr: bool,
    /// What the form shows: the defaults, or what a rejected submission
    /// typed.
    pub amount: String,
    pub currency: String,
    pub merchant_order_id: String,
    /// The hidden per-form idempotency key (see `http::orders::create_order`).
    pub request_key: String,
}

pub fn page(chrome: &PageChrome, data: &CreateOrderData) -> Markup {
    let body = html! {
        div class="wrap" {
            (super::store_breadcrumb(data.connection_id.as_str(), &data.display_name, true))
            h1 { "Create order" }
            p class="hint" { "Creates a real order on the engine and takes you straight to its payment page." }
            @if let Some(error) = &data.order_creation_error {
                div class="error" { (error) }
            }
            form method="post" action=(format!("/dashboard/stores/{}/orders/new", data.connection_id)) {
                label for="amount" { "Amount" }
                input type="hidden" name="request_key" value=(data.request_key);
                input type="text" id="amount" name="amount" value=(data.amount) required;
                label for="currency" { "Currency" }
                @if data.order_currency_is_locked_to_xmr {
                    input type="text" id="currency" name="currency" value="XMR" readonly;
                    span class="field-help" {
                        "\"XMR\" - the only currency this instance can price an order in right now. Enable an "
                        "exchange rate provider (in this store's settings) to offer others."
                    }
                } @else {
                    mk-select {
                        select id="currency" name="currency" {
                            @for currency in &data.order_currency_options {
                                (super::controls::Choice::new(currency, currency).selected(*currency == data.currency))
                            }
                        }
                    }
                    span class="field-help" {
                        "\"XMR\" always works, with no provider needed. Any other currency here comes from this "
                        "store's own exchange rate provider (in this store's settings)."
                    }
                }
                label for="merchant_order_id" { "Merchant Reference " span class="field-help" style="display:inline" { "(optional)" } }
                input type="text" id="merchant_order_id" name="merchant_order_id" value=(data.merchant_order_id);
                span class="field-help" {
                    "Your own order/cart id, if you have one - shown on this order's detail page so you can "
                    "match it back to your own records."
                }
                button type="submit" class="btn-primary" { "Create order" }
            }
        }
    };
    let head = script("create-order.js", Load::Now);
    layout_with_head(
        chrome,
        &format!("Create order - {} - Monokulo", data.display_name),
        head,
        body,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chrome() -> PageChrome {
        PageChrome::from_user(None, "/dashboard/stores/conn_1/orders/new")
    }

    fn data() -> CreateOrderData {
        CreateOrderData {
            connection_id: shared::ids::ConnectionId::new("conn_1".to_string()),
            display_name: "shop.example.com".to_string(),
            order_creation_error: None,
            order_currency_options: vec!["XMR".to_string(), "USD".to_string()],
            order_currency_is_locked_to_xmr: false,
            amount: "10.00".to_string(),
            currency: String::new(),
            merchant_order_id: String::new(),
            request_key: "key-1".to_string(),
        }
    }

    /// A rejected submission comes back with what was typed and the same
    /// request key, so resubmitting can't make a second order.
    #[test]
    fn a_rejected_submission_keeps_its_values_and_its_request_key() {
        let html = page(
            &chrome(),
            &CreateOrderData {
                amount: "25.50".to_string(),
                currency: "USD".to_string(),
                merchant_order_id: "table 4".to_string(),
                request_key: "kept-key".to_string(),
                order_creation_error: Some("try again".to_string()),
                ..data()
            },
        )
        .into_string();
        assert!(html.contains(r#"value="25.50""#), "{html}");
        assert!(html.contains(r#"<option value="USD" selected>"#), "{html}");
        assert!(html.contains(r#"value="table 4""#), "{html}");
        assert!(
            html.contains(r#"name="request_key" value="kept-key""#),
            "{html}"
        );
    }

    #[test]
    fn renders_the_form_with_a_merchant_reference_field() {
        let html = page(&chrome(), &data()).into_string();
        assert!(html.contains("Merchant Reference"));
        assert!(html.contains(r#"name="merchant_order_id""#));
        assert!(html.contains(r#"action="/dashboard/stores/conn_1/orders/new""#));
    }

    #[test]
    fn shows_the_create_order_error_when_present() {
        let store = CreateOrderData {
            order_creation_error: Some("unsupported currency: XYZ".to_string()),
            ..data()
        };
        let html = page(&chrome(), &store).into_string();
        assert!(html.contains("unsupported currency: XYZ"));
        assert!(html.contains("<form"));
    }

    #[test]
    fn locks_currency_to_xmr_when_no_provider_is_available() {
        let store = CreateOrderData {
            order_currency_options: vec!["XMR".to_string()],
            order_currency_is_locked_to_xmr: true,
            ..data()
        };
        let html = page(&chrome(), &store).into_string();
        assert!(html.contains(r#"value="XMR" readonly"#));
    }
}
