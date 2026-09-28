//! `GET /dashboard/stores/{id}/orders` and
//! `/dashboard/stores/{id}/orders/{order_id}` -
//! `http::orders::orders_list`/`order_detail`/`lookup_payment`.

use maud::{html, Markup, PreEscaped};

use super::{layout, layout_with_head, PageChrome};

/// One row of the orders list page - just the fields the table shows, not
/// the full engine `OrderView`.
pub struct OrderRowViewModel {
    pub order_id: String,
    /// The shop's own reference (a POS note, a WooCommerce order number) -
    /// caller-supplied text, always escaped.
    pub reference: Option<String>,
    /// Where the order came from: "POS", "WooCommerce", "Website", ...
    pub source: String,
    /// The engine's status, or "cancelled" for an unpaid order the POS
    /// cancelled (the engine does not know about POS cancellations).
    pub status: String,
    pub amount: String,
    pub currency: String,
    pub created_at: i64,
}

pub struct OrdersViewModel {
    pub connection_id: String,
    pub display_name: String,
    pub orders: Vec<OrderRowViewModel>,
    /// The search as typed (empty for none).
    pub search: String,
    /// Zero-based page of `ORDERS_PER_PAGE`.
    pub page: u32,
    pub has_more: bool,
}

pub const ORDERS_PER_PAGE: u32 = 50;

/// The orders table shared by the Orders page and the store page's
/// "Recent orders".
pub fn orders_table(connection_id: &str, orders: &[OrderRowViewModel]) -> Markup {
    html! {
        // On a phone each row is a card (`.table-cards`): order and status,
        // then amount and date, then reference and source.
        div class="table-scroll" { table class="orders-table table-cards" {
            thead { tr { th { "Order" } th { "Reference" } th { "Source" } th { "Status" } th { "Amount" } th { "Created" } } }
            tbody {
                @for order in orders {
                    tr {
                        td class="card-title" { a class="ellipsis order-id" href=(format!("/dashboard/stores/{connection_id}/orders/{}", order.order_id)) { (order.order_id) } }
                        td class="card-meta" { @if let Some(reference) = &order.reference { (reference) } @else { span class="muted" { "—" } } }
                        td class="card-meta" { (order.source) }
                        td class="card-status" { (super::state_badge(&order.status)) }
                        td class="card-amount nowrap" { (super::display_amount(&order.amount, &order.currency)) }
                        td class="card-when" { (order.created_at) }
                    }
                }
            }
        } }
    }
}

/// The "Look up a transaction" card - a customer's transaction id, looked up
/// directly against the engine with no need to know which order it belongs
/// to. Lives on the store overview page's own "Recent orders" section
/// (`views::store_detail`) - `action` and `order_href` are still parameters
/// rather than baked in, purely so this fragment doesn't need to know that
/// page's own routing to render itself.
pub fn lookup_payment_card(
    action: &str,
    txid_value: &str,
    message: &Option<String>,
    found_order_id: &Option<String>,
    order_href: impl Fn(&str) -> String,
) -> Markup {
    html! {
        div class="card" id="payment-lookup" {
            h2 { "Look up a transaction" }
            p { "Have a customer's transaction ID? Look it up directly - no need to know which order it belongs to." }
            p class="hint" {
                "Payments are watched for while an order is open and for a while after it's paid or expires. "
                "A payment sent later than that isn't picked up by itself: look it up here to record it."
            }
            form method="post" action=(action) fx-action=(action) fx-method="POST" fx-target="#payment-lookup" {
                input
                    type="text"
                    name="txid"
                    value=(txid_value)
                    placeholder="Transaction ID (64 hex characters)"
                    pattern="[0-9a-fA-F]{64}"
                    maxlength="64"
                    required;
                button type="submit" class="btn-primary" { "Look up" }
            }
            @if let Some(message) = message {
                p role="status" data-fx-focus tabindex="-1" {
                    (message)
                    @if let Some(order_id) = found_order_id {
                        " "
                        a href=(order_href(order_id)) { "View order →" }
                    }
                }
            }
        }
    }
}

fn orders_base(data: &OrdersViewModel) -> String {
    format!("/dashboard/stores/{}/orders", data.connection_id)
}

/// The list and its paging: what a search or a page change replaces. With
/// fixi, swapped in place with the URL kept in the address bar.
pub fn list_results(data: &OrdersViewModel) -> Markup {
    let base = orders_base(data);
    let page_link = |page: u32| {
        let mut query = Vec::new();
        if !data.search.is_empty() {
            query.push(format!("q={}", urlencoding(&data.search)));
        }
        if page > 0 {
            query.push(format!("page={page}"));
        }
        if query.is_empty() { base.clone() } else { format!("{base}?{}", query.join("&")) }
    };
    html! {
        div id="orders-results" {
            @if data.orders.is_empty() {
                p class="muted" {
                    @if data.search.is_empty() { "No orders yet." } @else { "No orders match “" (data.search) "”." }
                }
            } @else {
                (orders_table(&data.connection_id, &data.orders))
            }
            @if data.page > 0 || data.has_more {
                p class="orders-pages" {
                    @if data.page > 0 {
                        a href=(page_link(data.page - 1)) rel="prev" fx-action=(page_link(data.page - 1)) fx-target="#orders-results" fx-push-url { "← Newer" }
                    }
                    @if data.page > 0 && data.has_more { " · " }
                    @if data.has_more {
                        a href=(page_link(data.page + 1)) rel="next" fx-action=(page_link(data.page + 1)) fx-target="#orders-results" fx-push-url { "Older →" }
                    }
                }
            }
        }
    }
}

pub fn list_page(chrome: &PageChrome, data: &OrdersViewModel) -> Markup {
    let base = orders_base(data);
    let body = html! {
        div class="wrap" {
            (super::store_breadcrumb(&data.connection_id, &data.display_name, false))
            h1 { "Orders" }
            form method="get" action=(base) class="orders-search" role="search"
                fx-action=(base) fx-target="#orders-results" fx-push-url fx-replace {
                label for="orders-search" class="sr-only" { "Search orders" }
                input type="search" id="orders-search" name="q" value=(data.search) placeholder="Search by reference or order ID" maxlength="120";
                button type="submit" class="btn-primary" { "Search" }
                @if !data.search.is_empty() { " " a href=(base) { "Clear" } }
            }
            (list_results(data))
        }
    };
    layout(chrome, &format!("Orders - {} - Monokulo", data.display_name), body)
}

/// Percent-encodes a query value (a search term).
fn urlencoding(value: &str) -> String {
    value.bytes().map(|b| match b {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
        _ => format!("%{b:02X}"),
    }).collect()
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
    /// Taken on the POS: admins get a link to its session's timeline.
    pub from_pos: bool,
    pub order_id: String,
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
    pub display_name: String,
    pub order: Option<OrderDetailData>,
}

const PAGE_STYLE: &str = r#"
.kv-table th { width: 14em; }
/* Codes (address, ids, txids) break anywhere; words and badges never do. */
.kv-table td, .payments-table td { overflow-wrap: anywhere; }
.kv-table td code, .payments-table td code { word-break: break-all; }
.payments-table th { min-width: 8em; }
.order-title { display: flex; align-items: center; gap: 0.5em; min-width: 0; }
.order-title > span { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.order-title-id { font-size: 0.9em; }
.order-title .share-btn { margin-left: auto; }
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

/// The part of the order detail page that changes as the order does:
/// its fields and payments. Streamed as a whole to replace itself
/// (`http::orders::order_detail_events`).
pub fn live_fragment(order: &OrderDetailData) -> Markup {
    html! {
        div id="order-live" {
            table class="kv-table" {
                tr { th { "Order ID" } td { code { (order.order_id) } } }
                tr {
                    th { "Merchant Reference" }
                    td {
                        @if let Some(v) = &order.merchant_order_id { (v) } @else { span class="muted" { "-" } }
                    }
                }
                tr { th { "Address" } td { code { (order.address) } } }
                tr { th { "Amount" } td { (super::display_amount(&order.amount, &order.currency)) } }
                tr { th { "Exchange rate" } td { (order.rate_display) } }
                tr { th { "Rate provider" } td { (order.rate_provider) } }
                tr { th { "XMR amount (piconero)" } td { (order.xmr_amount_piconero) } }
                tr { th { "Amount received (piconero)" } td { (order.amount_received_piconero) } }
                tr { th { "Status" } td { (super::state_badge(&order.status)) } }
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
            div class="table-scroll" { table class="payments-table" {
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
            } }
        }
    }
}

pub fn detail_page(chrome: &PageChrome, data: &OrderDetailViewModel) -> Markup {
    let extra_head = html! {
        style { (PreEscaped(PAGE_STYLE)) }
    };

    let body = html! {
        div class="wrap" {
            (super::store_breadcrumb(&data.connection_id, &data.display_name, true))
            @if let Some(order) = &data.order {
                h1 class="order-title" {
                    span { "Order · " code class="order-title-id" title=(order.order_id) { (order.order_id) } }
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
                    "Share the payment link (icon above) with whoever needs to pay this order. "
                    (super::reload_button(&chrome.current_path))
                    " "
                    (super::logs_link(chrome, "order.id", &order.order_id, "Logs for this order"))
                    @if order.from_pos && chrome.is_admin {
                        " "
                        a class="logs-link" href=(format!("/dashboard/admin/logs/pos?order={}", url::form_urlencoded::byte_serialize(order.order_id.as_bytes()).collect::<String>())) {
                            "POS session"
                        }
                    }
                }
                (live_fragment(order))
                // Streams the part above as the order changes, when
                // JavaScript is on (fixi starts it as soon as it's seen).
                span hidden fx-action=(format!("/dashboard/stores/{}/orders/{}/events", data.connection_id, order.order_id))
                    fx-trigger="fx:inited" fx-swap="none" fx-sse-reconnect {}

            } @else {
                h1 { "Order not found" }
                p { "This order does not exist." }
            }
        }
        @if data.order.is_some() {
            script { (PreEscaped(SHARE_SCRIPT)) }
        }
    };

    let title = match &data.order {
        Some(order) => format!("Order {} - {} - Monokulo", order.order_id, data.display_name),
        None => format!("Order not found - {} - Monokulo", data.display_name),
    };
    layout_with_head(chrome, &title, extra_head, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chrome() -> PageChrome {
        PageChrome::from_user(None, "/dashboard/stores/conn_1/orders")
    }

    #[test]
    fn list_page_links_to_each_order_detail_page() {
        let data = OrdersViewModel {
            connection_id: "conn_1".to_string(),
            display_name: "shop.example.com".to_string(),
            orders: vec![OrderRowViewModel {
                order_id: "pay_xyz".to_string(),
                reference: Some("wc-1042".to_string()),
                source: "WooCommerce".to_string(),
                status: "paid".to_string(),
                amount: "25.00".to_string(),
                currency: "USD".to_string(),
                created_at: 1000,
            }],
            search: String::new(),
            page: 0,
            has_more: false,
        };
        let html = list_page(&chrome(), &data).into_string();
        assert!(html.contains("pay_xyz"));
        assert!(html.contains(r#"href="/dashboard/stores/conn_1/orders/pay_xyz""#));
        assert!(html.contains(r#"href="/dashboard/stores/conn_1""#), "expected a back-to-store link");
    }

    fn test_order_detail_data(double_spend_detected_at: Option<i64>) -> OrderDetailData {
        OrderDetailData {
            from_pos: false,
            order_id: "pay_abc123".to_string(),
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
        let data = OrderDetailViewModel { connection_id: "conn_1".to_string(), display_name: "shop.example.com".to_string(), order: Some(test_order_detail_data(None)) };
        let html = detail_page(&chrome(), &data).into_string();
        // The real point of this follow-up: no dash, no row at all - a
        // permanently-visible "Double-spend detected at" label reads as a
        // warning even when it says nothing happened.
        assert!(!html.contains("Double-spend detected at"), "expected the row fully absent when no double-spend occurred, got: {html}");
    }

    #[test]
    fn detail_page_uses_order_breadcrumb() {
        let data = OrderDetailViewModel { connection_id: "conn_1".to_string(), display_name: "shop.example.com".to_string(), order: Some(test_order_detail_data(None)) };
        let html = detail_page(&chrome(), &data).into_string();
        assert!(html.contains(r#"<nav class="context-nav" aria-label="Breadcrumb"><a href="/dashboard/stores/conn_1" title="shop.example.com">shop.example.com</a><span class="breadcrumb-sep" aria-hidden="true">›</span><a href="/dashboard/stores/conn_1/orders">Orders</a></nav>"#));
        assert!(html.contains("Order · <code class=\"order-title-id\" title=\"pay_abc123\">pay_abc123</code>"));
    }

    #[test]
    fn detail_page_shows_the_double_spend_row_when_one_was_detected() {
        let data =
            OrderDetailViewModel { connection_id: "conn_1".to_string(), display_name: "shop.example.com".to_string(), order: Some(test_order_detail_data(Some(1_700_000_000))) };
        let html = detail_page(&chrome(), &data).into_string();
        assert!(html.contains("Double-spend detected at"), "expected the row present when a double-spend was detected, got: {html}");
        assert!(html.contains("1700000000"), "expected the real detected-at timestamp shown, got: {html}");
    }

    #[test]
    fn detail_page_shows_a_not_found_state_when_order_is_none() {
        let data = OrderDetailViewModel { connection_id: "conn_1".to_string(), display_name: "shop.example.com".to_string(), order: None };
        let html = detail_page(&chrome(), &data).into_string();
        assert!(html.to_lowercase().contains("not found"));
        // The nav's own status-dot poll script always renders regardless -
        // it's the order-specific share-button script that must be absent
        // with no order to attach it to.
        assert!(!html.contains("share-payment-link"), "no order means no share button to enhance");
    }
}
