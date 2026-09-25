//! `GET/POST /dashboard/stores/{id}/orders/new` -
//! `http::orders::create_order_page`/`create_order`.
//!
//! A widget page, same shape as [`super::pos`]'s POS terminal: rather than a
//! form embedded inline on the busy store overview page, this is its own
//! full page a merchant navigates to (and back from) - reachable from a
//! "Create an order" tile on the store page's own widget row.

use maud::{html, Markup};

use super::{layout, PageChrome};

pub struct CreateOrderData {
    pub connection_id: String,
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
}

pub fn page(chrome: &PageChrome, data: &CreateOrderData) -> Markup {
    let body = html! {
        div class="wrap" {
            (super::store_breadcrumb(&data.connection_id, &data.display_name, true))
            h1 { "Create order" }
            p class="hint" { "Creates a real order on the engine and takes you straight to its payment page." }
            @if let Some(error) = &data.order_creation_error {
                div class="error" { (error) }
            }
            form method="post" action=(format!("/dashboard/stores/{}/orders/new", data.connection_id)) {
                label for="amount" { "Amount" }
                input type="text" id="amount" name="amount" value="10.00" required;
                label for="currency" { "Currency" }
                @if data.order_currency_is_locked_to_xmr {
                    input type="text" id="currency" name="currency" value="XMR" readonly;
                    span class="field-help" {
                        "\"XMR\" - the only currency this instance can price an order in right now. Enable an "
                        "exchange rate provider (in this store's settings) to offer others."
                    }
                } @else {
                    select id="currency" name="currency" {
                        @for currency in &data.order_currency_options {
                            option value=(currency) { (currency) }
                        }
                    }
                    span class="field-help" {
                        "\"XMR\" always works, with no provider needed. Any other currency here comes from this "
                        "store's own exchange rate provider (in this store's settings)."
                    }
                }
                label for="merchant_order_id" { "Merchant Reference " span class="field-help" style="display:inline" { "(optional)" } }
                input type="text" id="merchant_order_id" name="merchant_order_id";
                span class="field-help" {
                    "Your own order/cart id, if you have one - shown on this order's detail page so you can "
                    "match it back to your own records."
                }
                button type="submit" { "Create order" }
            }
        }
    };
    layout(chrome, &format!("Create order - {} - Monokulo", data.display_name), body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chrome() -> PageChrome {
        PageChrome::from_user(None, "/dashboard/stores/conn_1/orders/new")
    }

    fn data() -> CreateOrderData {
        CreateOrderData {
            connection_id: "conn_1".to_string(),
            display_name: "shop.example.com".to_string(),
            order_creation_error: None,
            order_currency_options: vec!["XMR".to_string(), "USD".to_string()],
            order_currency_is_locked_to_xmr: false,
        }
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
        let store = CreateOrderData { order_creation_error: Some("unsupported currency: XYZ".to_string()), ..data() };
        let html = page(&chrome(), &store).into_string();
        assert!(html.contains("unsupported currency: XYZ"));
        assert!(html.contains("<form"));
    }

    #[test]
    fn locks_currency_to_xmr_when_no_provider_is_available() {
        let store = CreateOrderData { order_currency_options: vec!["XMR".to_string()], order_currency_is_locked_to_xmr: true, ..data() };
        let html = page(&chrome(), &store).into_string();
        assert!(html.contains(r#"value="XMR" readonly"#));
    }
}
