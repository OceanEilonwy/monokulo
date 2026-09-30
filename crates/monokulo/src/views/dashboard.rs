//! `GET /dashboard` - `http::home::dashboard_home`.

use maud::{html, Markup};

use super::{layout, PageChrome};

/// One connected store as shown on the dashboard home page - a much smaller
/// projection of [`crate::db::StoreConnectionRow`] than the full order/
/// webhook views need, plus two fields no engine or database call directly
/// hands back: `display_name` (there is no user-facing name column on
/// `store_connections` - see `Db::list_store_connections_for_user`'s own
/// doc comment for why this derives one from `site_url` instead of adding
/// one) and `health` (`"ok"` if `EngineClient::get_tenant` on this
/// connection's decrypted `sk_...` succeeded, `"error"` otherwise - the only
/// signal actually available without this service also probing the
/// merchant's own `site_url`, which nothing here does).
pub struct DashboardStoreRow {
    pub connection_id: String,
    pub display_name: String,
    pub platform: String,
    pub site_url: String,
    pub public_key: String,
    pub health: String,
    pub health_label: String,
}

/// One order row on the dashboard home page - like an orders-list row but
/// also carrying which store it belongs to, since the dashboard shows
/// orders across every connected store, not just one.
pub struct DashboardOrderRow {
    pub connection_id: String,
    pub display_name: String,
    pub order_id: String,
    pub status: super::DisplayStatus,
    pub amount: String,
    pub currency: String,
    pub created_at: i64,
}

/// `has_stores` is redundant with `!stores.is_empty()` but kept as an
/// explicit field rather than computed in the view - documentation of that
/// fact at the call site rather than a second source of truth to drift.
pub struct DashboardViewModel {
    pub has_stores: bool,
    pub stores: Vec<DashboardStoreRow>,
    pub recent_orders: Vec<DashboardOrderRow>,
    /// Sum of every order's `amount_received_piconero` across every
    /// connected store - "total received XMR that has been detected",
    /// deliberately the *detected* amount rather than only orders in a
    /// `paid`/`overpaid` status, since a partially-paid or still-confirming
    /// order has still genuinely had funds detected for it.
    pub total_received_xmr: String,
}

pub fn page(chrome: &PageChrome, data: &DashboardViewModel) -> Markup {
    let body = html! {
        div class="wrap" {
            h1 { "Dashboard" }

            @if data.has_stores {
                div class="box" {
                    strong { "Total received (detected):" }
                    " " (data.total_received_xmr) " XMR across all connected stores"
                }

                h2 { "Your stores" }
                // On a phone only the name, health and link stay (`.col-optional`).
                div class="table-scroll" { table {
                    thead {
                        tr { th { "Store" } th class="col-optional" { "Platform" } th class="col-optional" { "Public key" } th { "Status" } th {} }
                    }
                    tbody {
                        @for store in &data.stores {
                            tr {
                                td { (store.display_name) }
                                td class="col-optional" { (store.platform) }
                                td class="col-optional" { code class="ellipsis" { (store.public_key) } }
                                td { span class=(format!("tag tag-{}", store.health)) { (store.health_label) } }
                                td class="nowrap" { a href=(format!("/dashboard/stores/{}", store.connection_id)) { "view →" } }
                            }
                        }
                    }
                } }
                a class="btn btn-secondary" href="/dashboard/stores/new" { "+ add another store" }

                h2 { "Recent orders" }
                @if data.recent_orders.is_empty() {
                    p class="muted" { "No orders yet." }
                } @else {
                    // On a phone: order, status and amount; the order page has the rest.
                    div class="table-scroll" { table {
                        thead {
                            tr { th class="col-optional" { "Store" } th { "Order ID" } th { "Status" } th { "Amount" } th class="col-optional" { "Created" } }
                        }
                        tbody {
                            @for order in &data.recent_orders {
                                tr {
                                    td class="col-optional" { (order.display_name) }
                                    td {
                                        a class="order-id" href=(format!("/dashboard/stores/{}/orders/{}", order.connection_id, order.order_id)) {
                                            (super::order_id_short(&order.order_id))
                                        }
                                    }
                                    td { (super::state_badge(&order.status)) }
                                    td class="nowrap" { (super::display_amount(&order.amount, &order.currency)) }
                                    td class="col-optional" { (chrome.clock.time(order.created_at)) }
                                }
                            }
                        }
                    } }
                }
            }
            @if !data.has_stores {
                div class="box" {
                    h2 { "Connect your first store" }
                    p {
                        "You don't have any stores connected yet. Adding one takes a couple of minutes - pick the "
                        "guided flow for WooCommerce, or the advanced form if you're integrating something custom."
                    }
                    a class="btn btn-primary" href="/dashboard/stores/new" { "+ add a store" }
                }
            }
            (timezone_setting(chrome))
        }
    };

    layout(chrome, "Dashboard - Monokulo", body)
}

/// The zone every date and time is shown in (the nav's "tz: ..." link
/// comes here). Automatic follows the browser.
fn timezone_setting(chrome: &PageChrome) -> Markup {
    let clock = &chrome.clock;
    let automatic = if clock.is_automatic() && clock.name() != "UTC" {
        format!("Automatic (this browser: {})", clock.name())
    } else {
        "Automatic (this browser's zone, UTC until it's known)".to_string()
    };
    html! {
        section class="box" id="timezone" {
            h2 { "Time zone" }
            form method="post" action="/dashboard/timezone" class="setting-field" {
                label for="timezone-select" { "Show dates and times in" }
                select id="timezone-select" name="timezone" {
                    option value="" selected[clock.is_automatic()] { (automatic) }
                    @for name in super::time::zone_names() {
                        option value=(name) selected[!clock.is_automatic() && name == clock.name()] { (name) }
                    }
                }
                button type="submit" { "Save" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chrome() -> PageChrome {
        PageChrome::from_user(None, "/dashboard")
    }

    fn empty_data() -> DashboardViewModel {
        DashboardViewModel {
            has_stores: false,
            stores: vec![],
            recent_orders: vec![],
            total_received_xmr: "0".to_string(),
        }
    }

    #[test]
    fn shows_the_add_store_cta_when_the_user_has_no_stores() {
        let html = page(&chrome(), &empty_data()).into_string();
        assert!(html.contains(r#"href="/dashboard/stores/new""#));
        assert!(
            !html.contains("<table"),
            "an empty dashboard shouldn't render a store table at all"
        );
    }

    #[test]
    fn lists_stores_and_recent_orders_when_present() {
        let data = DashboardViewModel {
            has_stores: true,
            stores: vec![DashboardStoreRow {
                connection_id: "conn_1".to_string(),
                display_name: "shop.example.com".to_string(),
                platform: "woocommerce".to_string(),
                site_url: "https://shop.example.com".to_string(),
                public_key: "pk_abc123".to_string(),
                health: "ok".to_string(),
                health_label: "healthy".to_string(),
            }],
            recent_orders: vec![DashboardOrderRow {
                connection_id: "conn_1".to_string(),
                display_name: "shop.example.com".to_string(),
                order_id: "pay_xyz".to_string(),
                status: shared::order_status::OrderStatus::Paid.into(),
                amount: "25.00".to_string(),
                currency: "USD".to_string(),
                created_at: 1000,
            }],
            total_received_xmr: "1.234567890123".to_string(),
        };
        let html = page(&chrome(), &data).into_string();
        assert!(html.contains("pk_abc123"));
        assert!(html.contains("1.234567890123"));
        assert!(html.contains("tag-ok"));
        assert!(html.contains(r#"href="/dashboard/stores/conn_1""#));
        assert!(html.contains("pay_xyz"));
        assert!(html.contains(r#"href="/dashboard/stores/conn_1/orders/pay_xyz""#));
    }

    #[test]
    fn never_shows_a_meta_refresh() {
        // `docs/txid_lookup_and_scan_chunking_wbs.md` Part C.8 - the dashboard's
        // only meta-refresh was the rescan syncing banner's; with that feature
        // gone, this page never auto-refreshes at all.
        let html = page(&chrome(), &empty_data()).into_string();
        assert!(!html.contains(r#"<meta http-equiv="refresh""#));
    }
}
