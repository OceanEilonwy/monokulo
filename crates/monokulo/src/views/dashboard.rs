//! `GET /dashboard` - `http::home::dashboard_home`.

use maud::{html, Markup};

use super::{layout, layout_with_head, PageChrome};

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
    pub payment_id: String,
    pub status: String,
    pub amount: String,
    pub currency: String,
    pub created_at: i64,
}

/// One entry in the dashboard-home "syncing" banner
/// (`docs/order_rescan_wbs.md` Phase 3.4).
pub struct DashboardRescanRow {
    pub connection_id: String,
    pub payment_id: String,
    pub percent_complete: u8,
    pub stalled: bool,
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
    /// `docs/order_rescan_wbs.md` Phase 3.4 - every currently-`running`
    /// rescan across every one of this user's connected stores. Drives the
    /// "syncing" banner and its tightened meta-refresh.
    pub active_rescans: Vec<DashboardRescanRow>,
    /// e.g. `"1 order"`/`"2 orders"` - precomputed by the caller. Empty (and
    /// never read - the banner is gated on `active_rescans` being
    /// non-empty) when nothing is running.
    pub active_rescans_count_label: String,
}

pub fn page(chrome: &PageChrome, data: &DashboardViewModel) -> Markup {
    let body = html! {
        div class="wrap" {
            h1 { "Dashboard" }

            @if !data.active_rescans.is_empty() {
                div class="box" {
                    span class="tag tag-syncing" { "Syncing" }
                    " " (data.active_rescans_count_label) " for possible late payments - this page "
                    "refreshes every 5s while that's true."
                    ul {
                        @for rescan in &data.active_rescans {
                            li {
                                a href=(format!("/dashboard/connections/{}/orders/{}", rescan.connection_id, rescan.payment_id)) {
                                    (rescan.payment_id)
                                    " (" (rescan.percent_complete) "%"
                                    @if rescan.stalled { ", stalled" }
                                    ") →"
                                }
                            }
                        }
                    }
                }
            }

            @if data.has_stores {
                div class="box" {
                    strong { "Total received (detected):" }
                    " " (data.total_received_xmr) " XMR across all connected stores"
                }

                h2 { "Your stores" }
                table {
                    thead {
                        tr { th { "Store" } th { "Platform" } th { "Public key" } th { "Status" } th {} }
                    }
                    tbody {
                        @for store in &data.stores {
                            tr {
                                td { (store.display_name) }
                                td { (store.platform) }
                                td { code class="ellipsis" { (store.public_key) } }
                                td { span class=(format!("tag tag-{}", store.health)) { (store.health_label) } }
                                td { a href=(format!("/dashboard/connections/{}", store.connection_id)) { "view →" } }
                            }
                        }
                    }
                }
                a class="btn btn-secondary" href="/dashboard/connections/new" { "+ add another store" }

                h2 { "Recent orders" }
                @if data.recent_orders.is_empty() {
                    p class="muted" { "No orders yet." }
                } @else {
                    table {
                        thead {
                            tr { th { "Store" } th { "Payment ID" } th { "Status" } th { "Amount" } th { "Created" } }
                        }
                        tbody {
                            @for order in &data.recent_orders {
                                tr {
                                    td { (order.display_name) }
                                    td {
                                        a href=(format!("/dashboard/connections/{}/orders/{}", order.connection_id, order.payment_id)) {
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
            } @else {
                div class="box" {
                    h2 { "Connect your first store" }
                    p {
                        "You don't have any stores connected yet. Adding one takes a couple of minutes - pick the "
                        "guided flow for WooCommerce, or the advanced form if you're integrating something custom."
                    }
                    a class="btn" href="/dashboard/connections/new" { "+ add a store" }
                }
            }
        }
    };

    if data.active_rescans.is_empty() {
        layout(chrome, "Dashboard - Monokulo", body)
    } else {
        let extra_head = html! { meta http-equiv="refresh" content="5"; };
        layout_with_head(chrome, "Dashboard - Monokulo", extra_head, body)
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
            active_rescans: vec![],
            active_rescans_count_label: String::new(),
        }
    }

    #[test]
    fn shows_the_add_store_cta_when_the_user_has_no_stores() {
        let html = page(&chrome(), &empty_data()).into_string();
        assert!(html.contains(r#"href="/dashboard/connections/new""#));
        assert!(!html.contains("<table"), "an empty dashboard shouldn't render a store table at all");
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
                payment_id: "pay_xyz".to_string(),
                status: "paid".to_string(),
                amount: "25.00".to_string(),
                currency: "USD".to_string(),
                created_at: 1000,
            }],
            total_received_xmr: "1.234567890123".to_string(),
            active_rescans: vec![],
            active_rescans_count_label: String::new(),
        };
        let html = page(&chrome(), &data).into_string();
        assert!(html.contains("pk_abc123"));
        assert!(html.contains("1.234567890123"));
        assert!(html.contains("tag-ok"));
        assert!(html.contains(r#"href="/dashboard/connections/conn_1""#));
        assert!(html.contains("pay_xyz"));
        assert!(html.contains(r#"href="/dashboard/connections/conn_1/orders/pay_xyz""#));
    }

    #[test]
    fn shows_a_meta_refresh_only_while_a_rescan_is_active() {
        let html = page(&chrome(), &empty_data()).into_string();
        assert!(!html.contains(r#"<meta http-equiv="refresh""#));

        let mut data = empty_data();
        data.active_rescans = vec![DashboardRescanRow {
            connection_id: "conn_1".to_string(),
            payment_id: "pay_1".to_string(),
            percent_complete: 40,
            stalled: false,
        }];
        data.active_rescans_count_label = "1 order".to_string();
        let html = page(&chrome(), &data).into_string();
        assert!(html.contains(r#"<meta http-equiv="refresh" content="5">"#));
        assert!(html.contains("pay_1"));
        assert!(html.contains("40%"));
    }
}
