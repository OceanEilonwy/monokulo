//! `GET /dashboard/stores/{id}/orders` and
//! `/dashboard/stores/{id}/orders/{order_id}` -
//! `http::orders::orders_list`/`order_detail`/`lookup_payment`.

use maud::{html, Markup, PreEscaped};

use super::PageChrome;

/// One row of the orders list page - just the fields the table shows, not
/// the full engine `OrderView`.
pub struct OrderRowViewModel {
    pub order_id: crate::db::OrderId,
    /// The shop's own reference (a POS note, a WooCommerce order number) -
    /// caller-supplied text, always escaped.
    pub reference: Option<String>,
    /// Where the order came from: "POS", "WooCommerce", "Website", ...
    pub source: String,
    /// The engine's status, or "cancelled" for an unpaid order the POS
    /// cancelled (the engine does not know about POS cancellations).
    pub status: super::DisplayStatus,
    pub amount: String,
    pub currency: String,
    pub created_at: i64,
}

pub struct OrdersViewModel {
    pub connection_id: crate::db::ConnectionId,
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
pub fn orders_table(
    connection_id: &str,
    orders: &[OrderRowViewModel],
    clock: &super::time::Clock,
) -> Markup {
    // On a phone each row is a card (`TableLook::Cards`): order and
    // status, then amount and date, then reference and source.
    let head = ui::Row::new(ui::Slot::many(
        [
            "Order",
            "Reference",
            "Source",
            "Status",
            "Amount",
            "Created",
        ]
        .map(|name| ui::Slot::from(ui::Cell::new(name).header(true))),
    ));
    let rows = orders.iter().map(|order| {
        ui::Slot::from(ui::Row::new(ui::Slot::many([
            ui::Cell::new(html! { a href=(format!("/dashboard/stores/{connection_id}/orders/{}", order.order_id)) { (super::short_id(order.order_id.as_str())) } })
                .part(ui::CellPart::Title)
                .into(),
            ui::Cell::new(html! { @if let Some(reference) = &order.reference { (reference) } @else { (super::no_reference()) } })
                .part(ui::CellPart::Meta)
                .into(),
            ui::Cell::new(order.source.as_str()).part(ui::CellPart::Meta).into(),
            ui::Cell::new(super::state_badge(order.status)).part(ui::CellPart::Status).into(),
            ui::Cell::new(html! { span class="nowrap" { (super::display_amount(&order.amount, &order.currency)) } })
                .part(ui::CellPart::Amount)
                .into(),
            ui::Slot::from(ui::Cell::new(clock.time(order.created_at)).part(ui::CellPart::When)),
        ])))
    });
    html! {
        (ui::Table::new(ui::Slot::many(rows))
            .head(head)
            .look(ui::TableLook::Cards)
            .attr("data-table", "orders"))
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
            (ui::Hint::new(
                "Payments are watched for while an order is open and for a while after it's paid or expires. \
                 A payment sent later than that isn't picked up by itself: look it up here to record it.",
            ))
            form method="post" action=(action) fx-action=(action) fx-method="POST" fx-target="#payment-lookup" {
                (ui::Input::new("txid")
                    .value(txid_value)
                    .placeholder("Transaction ID (64 hex characters)")
                    .required(true)
                    .attr("pattern", "[0-9a-fA-F]{64}")
                    .attr("maxlength", "64"))
                (ui::FormActions::new(ui::Button::new("Look up").variant(ui::ButtonVariant::Primary).kind(ui::ButtonType::Submit)))
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
pub fn list_results(data: &OrdersViewModel, clock: &super::time::Clock) -> Markup {
    let base = orders_base(data);
    let page_link = |page: u32| {
        let mut query = Vec::new();
        if !data.search.is_empty() {
            query.push(format!("q={}", urlencoding(&data.search)));
        }
        if page > 0 {
            query.push(format!("page={page}"));
        }
        if query.is_empty() {
            base.clone()
        } else {
            format!("{base}?{}", query.join("&"))
        }
    };
    html! {
        div id="orders-results" {
            @if data.orders.is_empty() {
                p class="muted" {
                    @if data.search.is_empty() { "No orders yet." } @else { "No orders match “" (data.search) "”." }
                }
            } @else {
                (orders_table(data.connection_id.as_str(), &data.orders, clock))
            }
            @if data.page > 0 || data.has_more {
                (ui::Pagination::new("Orders pages")
                    .maybe_previous((data.page > 0).then(|| page_link(data.page - 1)))
                    .maybe_next(data.has_more.then(|| page_link(data.page + 1)))
                    .previous_label("Newer")
                    .next_label("Older")
                    .fixi_target("#orders-results"))
            }
        }
    }
}

pub fn list_page(chrome: &PageChrome, data: &OrdersViewModel) -> Markup {
    let base = orders_base(data);
    let header = ui::PageHeader::new("Orders").crumbs(super::store_crumbs(
        data.connection_id.as_str(),
        &data.display_name,
        false,
    ));
    let body = html! {
        form method="get" action=(base) class="orders-search" role="search"
            fx-action=(base) fx-target="#orders-results" fx-push-url fx-replace {
            label for="orders-search" class="sr-only" { "Search orders" }
            (ui::Input::new("q")
                .kind(ui::InputType::Search)
                .id("orders-search")
                .value(data.search.as_str())
                .placeholder("Search by reference or order ID")
                .attr("maxlength", "120"))
            (ui::Button::new("Search").variant(ui::ButtonVariant::Primary).kind(ui::ButtonType::Submit))
            @if !data.search.is_empty() { " " a href=(base) { "Clear" } }
        }
        (list_results(data, &chrome.clock))
    };
    super::Page::new(format!("Orders - {} - Monokulo", data.display_name), header)
        .render(chrome, body)
}

/// Percent-encodes a query value (a search term).
fn urlencoding(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// One payment row inside the order detail page's `payments` table - mirrors
/// the engine's own `PaymentView`, but with every timestamp/optional field
/// already rendered to a display string (`Option<i64>` -> human-readable UTC
/// or a muted dash) rather than left for the view to interpret. Values that
/// may be a dash are `Markup` (`templates::display_or_dash`), escaped when
/// built, so nothing here is rendered unescaped from a string.
pub struct PaymentRowViewModel {
    pub txid: String,
    pub output_index: i64,
    pub amount_piconero: u64,
    pub first_seen_at: i64,
    pub block_height_display: maud::Markup,
    pub voided_at: Option<i64>,
    /// Voided for another payment of the same output, not a double spend.
    pub superseded: bool,
}

pub struct OrderDetailData {
    /// Taken on the POS: admins get a link to its session's timeline.
    pub from_pos: bool,
    pub order_id: crate::db::OrderId,
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
    pub status: super::DisplayStatus,
    pub confirmations: u64,
    pub confirmations_required_display: String,
    pub base_currency_display: String,
    pub base_currency_rate_display: String,
    pub double_spend_detected_at: Option<i64>,
    /// Same caller-supplied-text caveat as `merchant_order_id` above.
    pub refund_address: Option<String>,
    pub created_at: i64,
    pub expires_at: i64,
    pub updated_at: i64,
    pub payments: Vec<PaymentRowViewModel>,
    pub payment_link: String,
    pub scan_range_display: maud::Markup,
}

pub struct OrderDetailViewModel {
    pub connection_id: crate::db::ConnectionId,
    pub display_name: String,
    pub order: Option<OrderDetailData>,
}

const PAGE_STYLE: &str = r#"
.kv-table th { width: 14em; }
/* Codes (address, ids, txids) break anywhere; words and badges never do. */
.kv-table td, [data-table="payments"] td { overflow-wrap: anywhere; }
.kv-table td code, [data-table="payments"] td code { word-break: break-all; }
[data-table="payments"] th { min-width: 8em; }
/* The order's id in the title: its type, at the title's size, no box. */
.order-title-id { font-family: var(--font-mono); font-weight: 600; letter-spacing: 0; }
.mk-btn .share-icon { width: 1.1em; height: 1.1em; flex: none; }
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
pub fn live_fragment(order: &OrderDetailData, clock: &super::time::Clock) -> Markup {
    let fact = |name: &str, value: Markup| {
        ui::Slot::from(ui::Row::new(ui::Slot::many([
            ui::Cell::new(name).header(true).into(),
            ui::Slot::from(ui::Cell::new(value)),
        ])))
    };
    let dash = || html! { span class="muted" { "-" } };
    let mut facts = vec![
        fact("Order ID", html! { code { (order.order_id) } }),
        fact(
            "Merchant Reference",
            match &order.merchant_order_id {
                Some(reference) => html! { (reference) },
                None => dash(),
            },
        ),
        fact("Address", html! { code { (order.address) } }),
        fact(
            "Amount",
            html! { (super::display_amount(&order.amount, &order.currency)) },
        ),
        fact("Exchange rate", html! { (order.rate_display) }),
        fact("Rate provider", html! { (order.rate_provider) }),
        fact(
            "XMR amount (piconero)",
            html! { (order.xmr_amount_piconero) },
        ),
        fact(
            "Amount received (piconero)",
            html! { (order.amount_received_piconero) },
        ),
        fact("Status", super::state_badge(order.status)),
        fact("Confirmations", html! { (order.confirmations) }),
        fact(
            "Confirmations required",
            html! { (order.confirmations_required_display) },
        ),
        fact(
            "Store base currency (at order creation)",
            html! { (order.base_currency_display) },
        ),
        fact(
            "Base currency rate used",
            html! { (order.base_currency_rate_display) },
        ),
    ];
    if let Some(at) = order.double_spend_detected_at {
        facts.push(fact("Double-spend detected at", clock.time(at)));
    }
    facts.extend([
        fact(
            "Refund address",
            match &order.refund_address {
                Some(address) => html! { code { (address) } },
                None => dash(),
            },
        ),
        fact("Created at", clock.time(order.created_at)),
        fact("Expires at", clock.time(order.expires_at)),
        fact("Updated at", clock.time(order.updated_at)),
        fact("Scan range", order.scan_range_display.clone()),
    ]);
    let payments = order.payments.iter().map(|payment| {
        ui::Slot::from(ui::Row::new(ui::Slot::many([
            ui::Cell::new(html! { code { (payment.txid) } }).into(),
            ui::Cell::new(payment.output_index.to_string()).into(),
            ui::Cell::new(payment.amount_piconero.to_string()).into(),
            ui::Cell::new(clock.time(payment.first_seen_at)).into(),
            ui::Cell::new(payment.block_height_display.clone()).into(),
            ui::Slot::from(ui::Cell::new(html! {
                (clock.time_or_dash(payment.voided_at))
                @if payment.superseded {
                    " " span class="muted" { "(the same output, paid in another transaction, is the one counted)" }
                }
            })),
        ])))
    });
    html! {
        div id="order-live" {
            (ui::Table::new(ui::Slot::many(facts))
                .look(ui::TableLook::Facts)
                .scroll(false))
            h2 { "Payments" }
            (ui::Table::new(ui::Slot::many(payments))
                .head(ui::Row::new(ui::Slot::many(
                    ["Txid", "Output index", "Amount (piconero)", "First seen", "Block height", "Voided at"]
                        .map(|name| ui::Slot::from(ui::Cell::new(name).header(true))),
                )))
                .attr("data-table", "payments"))
        }
    }
}

pub fn detail_page(chrome: &PageChrome, data: &OrderDetailViewModel) -> Markup {
    let extra_head = html! {
        style { (PreEscaped(PAGE_STYLE)) }
    };

    let crumbs = super::store_crumbs(data.connection_id.as_str(), &data.display_name, true);
    let header = match &data.order {
        Some(order) => ui::PageHeader::new(html! {
            "Order " span class="order-title-id" { (super::short_id(order.order_id.as_str())) }
        })
        .crumbs(crumbs)
        .actions(html! {
            // With JavaScript, the device's share sheet; without, the
            // payment page in a new tab.
            (ui::LinkButton::new(html! {
                svg class="share-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"
                    stroke-linejoin="round" aria-hidden="true" focusable="false" {
                    circle cx="18" cy="5" r="3" {}
                    circle cx="6" cy="12" r="3" {}
                    circle cx="18" cy="19" r="3" {}
                    line x1="8.59" y1="13.51" x2="15.42" y2="17.49" {}
                    line x1="15.41" y1="6.51" x2="8.59" y2="10.49" {}
                }
                "Share"
            }, order.payment_link.as_str())
            .attr("id", "share-payment-link")
            .attr("target", "_blank")
            .attr("rel", "noopener")
            .attr("title", "Share the payment link"))
            (super::reload_button(&chrome.current_path))
        }),
        None => ui::PageHeader::new("Order not found").crumbs(crumbs),
    };
    let body = html! {
        @if let Some(order) = &data.order {
            (ui::Hint::new(html! {
                "Share the payment link with whoever needs to pay this order. "
                " "
                (super::logs_link(chrome, "order.id", order.order_id.as_str(), "Logs for this order"))
                @if order.from_pos && chrome.is_admin {
                    " "
                    a class="logs-link" href=(format!("/dashboard/admin/logs/pos?order={}", url::form_urlencoded::byte_serialize(order.order_id.as_str().as_bytes()).collect::<String>())) {
                        "POS session"
                    }
                }
            }))
            (live_fragment(order, &chrome.clock))
            // Streams the part above as the order changes, when
            // JavaScript is on (fixi starts it as soon as it's seen).
            span hidden fx-action=(format!("/dashboard/stores/{}/orders/{}/events", data.connection_id, order.order_id))
                fx-trigger="fx:inited" fx-swap="none" fx-sse-reconnect {}

        } @else {
            p { "This order does not exist." }
        }
        @if data.order.is_some() {
            script { (PreEscaped(SHARE_SCRIPT)) }
        }
    };

    let title = match &data.order {
        Some(order) => format!(
            "Order {} - {} - Monokulo",
            order.order_id, data.display_name
        ),
        None => format!("Order not found - {} - Monokulo", data.display_name),
    };
    super::Page::new(title, header)
        .head(extra_head)
        .render(chrome, body)
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
            connection_id: shared::ids::ConnectionId::new("conn_1".to_string()),
            display_name: "shop.example.com".to_string(),
            orders: vec![OrderRowViewModel {
                order_id: shared::ids::OrderId::new("pay_xyz".to_string()),
                reference: Some("wc-1042".to_string()),
                source: "WooCommerce".to_string(),
                status: shared::order_status::OrderStatus::Paid.into(),
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
        assert!(
            html.contains(r#"href="/dashboard/stores/conn_1""#),
            "expected a back-to-store link"
        );
    }

    /// A middle page of a search links to the newer and older pages, the
    /// search kept, fixi swapping each into the results.
    #[test]
    fn a_middle_page_links_newer_and_older_keeping_the_search() {
        let data = OrdersViewModel {
            connection_id: shared::ids::ConnectionId::new("conn_1".to_string()),
            display_name: "shop.example.com".to_string(),
            orders: Vec::new(),
            search: "wc".to_string(),
            page: 2,
            has_more: true,
        };
        let html = list_results(&data, &chrome().clock).into_string();
        assert!(html.contains(concat!(
            r##"<nav class="mk-pagination" aria-label="Orders pages">"##,
            r##"<a href="/dashboard/stores/conn_1/orders?q=wc&amp;page=1" rel="prev" fx-action="/dashboard/stores/conn_1/orders?q=wc&amp;page=1" fx-target="#orders-results" fx-push-url="">← Newer</a>"##,
            r##"<a href="/dashboard/stores/conn_1/orders?q=wc&amp;page=3" rel="next" fx-action="/dashboard/stores/conn_1/orders?q=wc&amp;page=3" fx-target="#orders-results" fx-push-url="">Older →</a></nav>"##,
        )), "{html}");

        // The first and only page has nowhere to go.
        let html = list_results(
            &OrdersViewModel {
                page: 0,
                has_more: false,
                ..data
            },
            &chrome().clock,
        )
        .into_string();
        assert!(!html.contains("mk-pagination"), "{html}");
    }

    /// A real order id is long: the list shows it as `views::short_id`
    /// writes it, inside a plain link, the whole id in reach.
    #[test]
    fn list_page_shortens_a_long_order_id_inside_a_plain_link() {
        let order_id = "order_a8723b2e45b0d44ea8723b2e45b0d44e";
        let data = OrdersViewModel {
            connection_id: shared::ids::ConnectionId::new("conn_1".to_string()),
            display_name: "shop.example.com".to_string(),
            orders: vec![OrderRowViewModel {
                order_id: shared::ids::OrderId::new(order_id.to_string()),
                reference: None,
                source: "WooCommerce".to_string(),
                status: shared::order_status::OrderStatus::Paid.into(),
                amount: "25.00".to_string(),
                currency: "USD".to_string(),
                created_at: 1000,
            }],
            search: String::new(),
            page: 0,
            has_more: false,
        };
        let html = list_page(&chrome(), &data).into_string();
        assert!(html.contains(&format!(
            r#"<td class="card-title"><a href="/dashboard/stores/conn_1/orders/{order_id}"><span class="short-value" title="{order_id}"><span class="short-value-text" aria-hidden="true">a8723b…b0d44e</span><span class="short-value-full">{order_id}</span></span></a></td>"#
        )), "{html}");
        assert!(!html.contains("..."), "{html}");
    }

    fn test_order_detail_data(double_spend_detected_at: Option<i64>) -> OrderDetailData {
        OrderDetailData {
            from_pos: false,
            order_id: shared::ids::OrderId::new("pay_abc123".to_string()),
            merchant_order_id: None,
            address: "86hiL7n5RcVJJKBztLP1UFjCSXJZTSa276LaNaXcQuw1ZcauZJShLbB61YabbizKYVB3jHh7K3s1GCLwLVs6AwMX9FGCnfC".to_string(),
            currency: "XMR".to_string(),
            amount: "0.5".to_string(),
            rate_display: "1 XMR per 1 XMR".to_string(),
            rate_provider: "xmr".to_string(),
            xmr_amount_piconero: 500_000_000_000,
            amount_received_piconero: 0,
            status: shared::order_status::OrderStatus::Pending.into(),
            confirmations: 0,
            confirmations_required_display: "10".to_string(),
            base_currency_display: "XMR".to_string(),
            base_currency_rate_display: "same as order currency".to_string(),
            double_spend_detected_at,
            refund_address: None,
            created_at: 1000,
            expires_at: 2000,
            updated_at: 1000,
            payments: vec![],
            payment_link: "http://127.0.0.1:8081/pay/pk_abc123/orders/pay_abc123/share".to_string(),
            scan_range_display: maud::html! { span class="muted" { "-" } },
        }
    }

    #[test]
    fn detail_page_hides_the_double_spend_row_entirely_when_none_was_detected() {
        let data = OrderDetailViewModel {
            connection_id: shared::ids::ConnectionId::new("conn_1".to_string()),
            display_name: "shop.example.com".to_string(),
            order: Some(test_order_detail_data(None)),
        };
        let html = detail_page(&chrome(), &data).into_string();
        // The real point of this follow-up: no dash, no row at all - a
        // permanently-visible "Double-spend detected at" label reads as a
        // warning even when it says nothing happened.
        assert!(
            !html.contains("Double-spend detected at"),
            "expected the row fully absent when no double-spend occurred, got: {html}"
        );
    }

    #[test]
    fn detail_page_uses_order_breadcrumb() {
        let data = OrderDetailViewModel {
            connection_id: shared::ids::ConnectionId::new("conn_1".to_string()),
            display_name: "shop.example.com".to_string(),
            order: Some(test_order_detail_data(None)),
        };
        let html = detail_page(&chrome(), &data).into_string();
        assert!(html.contains(r#"<nav class="mk-breadcrumbs" aria-label="Breadcrumb"><a href="/dashboard/stores/conn_1" title="shop.example.com">shop.example.com</a><a href="/dashboard/stores/conn_1/orders" title="Orders">Orders</a></nav>"#));
        assert!(
            html.contains(r#"<h1>Order <span class="order-title-id">pay_abc123</span></h1>"#),
            "{html}"
        );
    }

    #[test]
    fn detail_page_shows_the_double_spend_row_when_one_was_detected() {
        let data = OrderDetailViewModel {
            connection_id: shared::ids::ConnectionId::new("conn_1".to_string()),
            display_name: "shop.example.com".to_string(),
            order: Some(test_order_detail_data(Some(1_700_000_000))),
        };
        let html = detail_page(&chrome(), &data).into_string();
        assert!(
            html.contains("Double-spend detected at"),
            "expected the row present when a double-spend was detected, got: {html}"
        );
        assert!(
            html.contains(r#"datetime="2023-11-14T22:13:20Z""#)
                && html.contains(">14 Nov 2023, 22:13</time>"),
            "expected the real detected-at time shown, got: {html}"
        );
    }

    #[test]
    fn detail_page_shows_a_not_found_state_when_order_is_none() {
        let data = OrderDetailViewModel {
            connection_id: shared::ids::ConnectionId::new("conn_1".to_string()),
            display_name: "shop.example.com".to_string(),
            order: None,
        };
        let html = detail_page(&chrome(), &data).into_string();
        assert!(html.to_lowercase().contains("not found"));
        // The nav's own status-dot poll script always renders regardless -
        // it's the order-specific share-button script that must be absent
        // with no order to attach it to.
        assert!(
            !html.contains("share-payment-link"),
            "no order means no share button to enhance"
        );
    }
}
