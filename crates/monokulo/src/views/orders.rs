//! `GET /dashboard/connections/{id}/orders` and
//! `/dashboard/connections/{id}/orders/{payment_id}` -
//! `http::orders::orders_list`/`order_detail`/`lookup_payment`.

use maud::{html, Markup, PreEscaped};

use super::{layout, layout_with_head, PageChrome};

/// One row of the orders list page - just the fields the table shows, not
/// the full engine `OrderView`.
pub struct OrderRowViewModel {
    pub payment_id: String,
    pub status: String,
    pub amount: String,
    pub currency: String,
    pub created_at: i64,
}

pub struct OrdersViewModel {
    pub connection_id: String,
    pub orders: Vec<OrderRowViewModel>,
}

/// The "Look up a payment" card - a customer's transaction id, looked up
/// directly against the engine with no need to know which order it belongs
/// to. Lives on the store overview page's own "Recent orders" section
/// (`views::store_detail`) - `action` and `order_href` are still parameters
/// rather than baked in, purely so this fragment doesn't need to know that
/// page's own routing to render itself.
pub fn lookup_payment_card(
    action: &str,
    txid_value: &str,
    message: &Option<String>,
    found_payment_id: &Option<String>,
    order_href: impl Fn(&str) -> String,
) -> Markup {
    html! {
        div class="card" {
            h2 { "Look up a payment" }
            p { "Have a customer's transaction ID? Look it up directly - no need to know which order it belongs to." }
            form method="post" action=(action) {
                input
                    type="text"
                    name="txid"
                    value=(txid_value)
                    placeholder="Transaction ID (64 hex characters)"
                    pattern="[0-9a-fA-F]{64}"
                    maxlength="64"
                    required;
                button type="submit" { "Look up" }
            }
            @if let Some(message) = message {
                p {
                    (message)
                    @if let Some(payment_id) = found_payment_id {
                        " "
                        a href=(order_href(payment_id)) { "View order →" }
                    }
                }
            }
        }
    }
}

pub fn list_page(chrome: &PageChrome, data: &OrdersViewModel) -> Markup {
    let body = html! {
        div class="wrap" {
            p { a href=(format!("/dashboard/connections/{}", data.connection_id)) { "← back to store" } }
            h1 { "Orders" }
            table {
                thead {
                    tr { th { "Payment ID" } th { "Status" } th { "Amount" } th { "Created" } }
                }
                tbody {
                    @for order in &data.orders {
                        tr {
                            td {
                                a href=(format!("/dashboard/connections/{}/orders/{}", data.connection_id, order.payment_id)) {
                                    (order.payment_id)
                                }
                            }
                            td { (order.status) }
                            td { (order.amount) " " (order.currency) }
                            td { (order.created_at) }
                        }
                    }
                }
            }
        }
    };
    layout(chrome, "Orders - Monokulo", body)
}

/// One payment row inside the order detail page's `payments` table - mirrors
/// the engine's own `PaymentView`, but with every timestamp/optional field
/// already rendered to a display string (`Option<i64>` -> human-readable UTC
/// or a muted dash) rather than left for the view to interpret. The display
/// strings are trusted HTML (a dash fallback carries a real `<span>`), so
/// they're rendered via `PreEscaped` below, same as every other
/// already-rendered display string on this page.
pub struct PaymentRowViewModel {
    pub txid: String,
    pub output_index: i64,
    pub amount_piconero: u64,
    pub first_seen_at_display: String,
    pub block_height_display: String,
    pub voided_at_display: String,
}

pub struct OrderDetailData {
    pub payment_id: String,
    /// Raw, *not* pre-rendered to a trusted-HTML display string like the
    /// timestamp fields below - a merchant order id is caller-supplied free
    /// text, so it must stay ordinary escaped output, never `PreEscaped`.
    pub merchant_order_id: Option<String>,
    pub address: String,
    pub currency: String,
    pub amount: String,
    pub rate_display: String,
    pub rate_provider: String,
    pub xmr_amount_piconero: u64,
    pub amount_received_piconero: u64,
    pub status: String,
    pub confirmations: u64,
    pub confirmations_required_display: String,
    pub base_currency_display: String,
    pub base_currency_rate_display: String,
    pub double_spend_detected_at: Option<i64>,
    pub double_spend_detected_at_display: String,
    /// Same caller-supplied-text caveat as `merchant_order_id` above.
    pub refund_address: Option<String>,
    pub created_at_display: String,
    pub expires_at_display: String,
    pub updated_at_display: String,
    pub payments: Vec<PaymentRowViewModel>,
    pub payment_link: String,
    pub scan_range_display: String,
}

pub struct OrderDetailViewModel {
    pub connection_id: String,
    pub order: Option<OrderDetailData>,
    pub meta_refresh_secs: u32,
}

const PAGE_STYLE: &str = r#"
.kv-table th { width: 14em; }
.kv-table td, .payments-table td { word-break: break-all; overflow-wrap: anywhere; }
.payments-table th { min-width: 8em; }
.order-title { display: flex; align-items: center; justify-content: space-between; gap: 0.8em; }
.share-btn {
  flex-shrink: 0;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 1.5em;
  height: 1.5em;
  border: 2px solid var(--line);
  border-radius: var(--radius-sm);
  color: var(--ink);
  text-decoration: none;
}
.share-btn:hover { color: var(--accent-ink); background: var(--accent); border-color: var(--accent); }
.share-btn svg { width: 60%; height: 60%; }
.progress-bar { border: 1px solid var(--line); border-radius: var(--radius-sm); overflow: hidden; height: 0.5em; background: var(--paper); margin: 0.6em 0 1em; }
.progress-fill { height: 100%; background: var(--accent); }
"#;

/// Progressive enhancement only - the share button is a real, working
/// `<a href>` to the payment link either way. Where the native Web Share API
/// is available, this hands the link to the device's own share sheet
/// instead of just navigating.
const SHARE_SCRIPT: &str = r#"(function () {
  var link = document.getElementById("share-payment-link");
  if (!link || typeof navigator.share !== "function") return;
  link.addEventListener("click", function (event) {
    event.preventDefault();
    navigator.share({ title: "Pay this Monero order", url: link.href }).catch(function () {});
  });
})();"#;

pub fn detail_page(chrome: &PageChrome, data: &OrderDetailViewModel) -> Markup {
    let extra_head = html! {
        meta http-equiv="refresh" content=(data.meta_refresh_secs);
        style { (PreEscaped(PAGE_STYLE)) }
    };

    let body = html! {
        div class="wrap" {
            p { a href=(format!("/dashboard/connections/{}/orders", data.connection_id)) { "← back to orders" } }
            @if let Some(order) = &data.order {
                h1 class="order-title" {
                    span { "Order " (order.payment_id) }
                    a class="share-btn" id="share-payment-link" href=(order.payment_link) target="_blank" rel="noopener"
                       aria-label="Share payment link" title="Share payment link" {
                        svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"
                            stroke-linejoin="round" aria-hidden="true" focusable="false" {
                            circle cx="18" cy="5" r="3" {}
                            circle cx="6" cy="12" r="3" {}
                            circle cx="18" cy="19" r="3" {}
                            line x1="8.59" y1="13.51" x2="15.42" y2="17.49" {}
                            line x1="15.41" y1="6.51" x2="8.59" y2="10.49" {}
                        }
                    }
                }
                p class="hint" {
                    "This page refreshes automatically every 15s. Share the payment link (icon above) with whoever "
                    "needs to pay this order."
                }
                table class="kv-table" {
                    tr {
                        th { "Merchant Reference" }
                        td {
                            @if let Some(v) = &order.merchant_order_id { (v) } @else { span class="muted" { "-" } }
                        }
                    }
                    tr { th { "Address" } td { code { (order.address) } } }
                    tr { th { "Amount" } td { (order.amount) " " (order.currency) } }
                    tr { th { "Exchange rate" } td { (order.rate_display) } }
                    tr { th { "Rate provider" } td { (order.rate_provider) } }
                    tr { th { "XMR amount (piconero)" } td { (order.xmr_amount_piconero) } }
                    tr { th { "Amount received (piconero)" } td { (order.amount_received_piconero) } }
                    tr { th { "Status" } td { (order.status) } }
                    tr { th { "Confirmations" } td { (order.confirmations) } }
                    tr { th { "Confirmations required" } td { (order.confirmations_required_display) } }
                    tr { th { "Store base currency (at order creation)" } td { (order.base_currency_display) } }
                    tr { th { "Base currency rate used" } td { (order.base_currency_rate_display) } }
                    @if order.double_spend_detected_at.is_some() {
                        tr { th { "Double-spend detected at" } td { (PreEscaped(&order.double_spend_detected_at_display)) } }
                    }
                    tr {
                        th { "Refund address" }
                        td {
                            @if let Some(v) = &order.refund_address { code { (v) } } @else { span class="muted" { "-" } }
                        }
                    }
                    tr { th { "Created at" } td { (PreEscaped(&order.created_at_display)) } }
                    tr { th { "Expires at" } td { (PreEscaped(&order.expires_at_display)) } }
                    tr { th { "Updated at" } td { (PreEscaped(&order.updated_at_display)) } }
                    tr { th { "Scan range" } td { (PreEscaped(&order.scan_range_display)) } }
                }
                h2 { "Payments" }
                table class="payments-table" {
                    thead {
                        tr {
                            th { "Txid" } th { "Output index" } th { "Amount (piconero)" }
                            th { "First seen" } th { "Block height" } th { "Voided at" }
                        }
                    }
                    tbody {
                        @for payment in &order.payments {
                            tr {
                                td { code { (payment.txid) } }
                                td { (payment.output_index) }
                                td { (payment.amount_piconero) }
                                td { (PreEscaped(&payment.first_seen_at_display)) }
                                td { (PreEscaped(&payment.block_height_display)) }
                                td { (PreEscaped(&payment.voided_at_display)) }
                            }
                        }
                    }
                }

            } @else {
                h1 { "Order not found" }
                p { "This order does not exist." }
            }
        }
        @if data.order.is_some() {
            script { (PreEscaped(SHARE_SCRIPT)) }
        }
    };

    layout_with_head(chrome, "Order detail - Monokulo", extra_head, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chrome() -> PageChrome {
        PageChrome::from_user(None, "/dashboard/connections/conn_1/orders")
    }

    #[test]
    fn list_page_links_to_each_order_detail_page() {
        let data = OrdersViewModel {
            connection_id: "conn_1".to_string(),
            orders: vec![OrderRowViewModel {
                payment_id: "pay_xyz".to_string(),
                status: "paid".to_string(),
                amount: "25.00".to_string(),
                currency: "USD".to_string(),
                created_at: 1000,
            }],
        };
        let html = list_page(&chrome(), &data).into_string();
        assert!(html.contains("pay_xyz"));
        assert!(html.contains(r#"href="/dashboard/connections/conn_1/orders/pay_xyz""#));
        assert!(html.contains(r#"href="/dashboard/connections/conn_1""#), "expected a back-to-store link");
    }

    fn test_order_detail_data(double_spend_detected_at: Option<i64>) -> OrderDetailData {
        OrderDetailData {
            payment_id: "pay_abc123".to_string(),
            merchant_order_id: None,
            address: "86hiL7n5RcVJJKBztLP1UFjCSXJZTSa276LaNaXcQuw1ZcauZJShLbB61YabbizKYVB3jHh7K3s1GCLwLVs6AwMX9FGCnfC".to_string(),
            currency: "XMR".to_string(),
            amount: "0.5".to_string(),
            rate_display: "1.000000000000 XMR per 1 XMR".to_string(),
            rate_provider: "xmr".to_string(),
            xmr_amount_piconero: 500_000_000_000,
            amount_received_piconero: 0,
            status: "pending".to_string(),
            confirmations: 0,
            confirmations_required_display: "10".to_string(),
            base_currency_display: "XMR".to_string(),
            base_currency_rate_display: "same as order currency".to_string(),
            double_spend_detected_at,
            double_spend_detected_at_display: crate::templates::display_timestamp_or_dash(double_spend_detected_at),
            refund_address: None,
            created_at_display: "1000".to_string(),
            expires_at_display: "2000".to_string(),
            updated_at_display: "1000".to_string(),
            payments: vec![],
            payment_link: "http://127.0.0.1:8081/pay/pk_abc123/orders/pay_abc123/share".to_string(),
            scan_range_display: "<span class=\"muted\">-</span>".to_string(),
        }
    }

    #[test]
    fn detail_page_hides_the_double_spend_row_entirely_when_none_was_detected() {
        let data = OrderDetailViewModel { connection_id: "conn_1".to_string(), order: Some(test_order_detail_data(None)), meta_refresh_secs: 15 };
        let html = detail_page(&chrome(), &data).into_string();
        // The real point of this follow-up: no dash, no row at all - a
        // permanently-visible "Double-spend detected at" label reads as a
        // warning even when it says nothing happened.
        assert!(!html.contains("Double-spend detected at"), "expected the row fully absent when no double-spend occurred, got: {html}");
    }

    #[test]
    fn detail_page_shows_the_double_spend_row_when_one_was_detected() {
        let data =
            OrderDetailViewModel { connection_id: "conn_1".to_string(), order: Some(test_order_detail_data(Some(1_700_000_000))), meta_refresh_secs: 15 };
        let html = detail_page(&chrome(), &data).into_string();
        assert!(html.contains("Double-spend detected at"), "expected the row present when a double-spend was detected, got: {html}");
        assert!(html.contains("1700000000"), "expected the real detected-at timestamp shown, got: {html}");
    }

    #[test]
    fn detail_page_shows_a_not_found_state_when_order_is_none() {
        let data = OrderDetailViewModel { connection_id: "conn_1".to_string(), order: None, meta_refresh_secs: 15 };
        let html = detail_page(&chrome(), &data).into_string();
        assert!(html.to_lowercase().contains("not found"));
        // The nav's own status-dot poll script always renders regardless -
        // it's the order-specific share-button script that must be absent
        // with no order to attach it to.
        assert!(!html.contains("share-payment-link"), "no order means no share button to enhance");
    }
}
