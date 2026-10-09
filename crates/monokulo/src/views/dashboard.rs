//! `GET /` (signed in) - `http::home::home`.

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
    pub connection_id: crate::db::ConnectionId,
    pub display_name: String,
    /// Its site's host; empty for a store with no site.
    pub site: String,
    pub public_key: String,
    pub health: String,
    pub health_label: String,
    /// The wallet it takes payments into: its name and network. `None`
    /// for a store not yet matched to one of the account's wallets.
    pub wallet: Option<(String, String)>,
}

/// One order row on the dashboard home page - like an orders-list row but
/// also carrying which store it belongs to, since the dashboard shows
/// orders across every connected store, not just one.
pub struct DashboardOrderRow {
    pub connection_id: crate::db::ConnectionId,
    pub display_name: String,
    pub order_id: crate::db::OrderId,
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
                    // The engine lists a store's latest orders, not all
                    // of them: the figure says so rather than claim a total.
                    " " (data.total_received_xmr) " XMR across each connected store's latest 50 orders"
                }

                h2 { "Your stores" }
                // On a phone only the name, health and link stay (`.col-optional`).
                div class="table-scroll" { table {
                    thead {
                        tr { th { "Store" } th { "Wallet" } th class="col-optional" { "Site" } th class="col-optional" { "Public key" } th { "Status" } th {} }
                    }
                    tbody {
                        @for store in &data.stores {
                            tr {
                                td { (store.display_name) }
                                td {
                                    @if let Some((name, network)) = &store.wallet {
                                        (name) " " (super::network_badge(network))
                                    } @else {
                                        span class="muted" { "Not linked yet" }
                                    }
                                }
                                td class="col-optional" { (store.site) }
                                td class="col-optional" { code { (super::short_id(&store.public_key)) } }
                                td { span class=(format!("tag tag-{}", store.health)) { (store.health_label) } }
                                td class="nowrap" { a href=(format!("/dashboard/stores/{}", store.connection_id)) { "view →" } }
                            }
                        }
                    }
                } }
                a class="btn btn-secondary" href="/setup" { "+ add another store" }

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
                                        a href=(format!("/dashboard/stores/{}/orders/{}", order.connection_id, order.order_id)) {
                                            (super::short_id(order.order_id.as_str()))
                                        }
                                    }
                                    td { (super::state_badge(order.status)) }
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
                    a class="btn btn-primary" href="/setup" { "+ add a store" }
                }
            }
        }
    };

    layout(chrome, "Dashboard - Monokulo", body)
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
        assert!(html.contains(r#"href="/setup""#));
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
                connection_id: shared::ids::ConnectionId::new("conn_1".to_string()),
                display_name: "shop.example.com".to_string(),
                site: "shop.example.com".to_string(),
                public_key: "pk_abc123".to_string(),
                health: "ok".to_string(),
                health_label: "healthy".to_string(),
                wallet: Some(("Feather test".to_string(), "stagenet".to_string())),
            }],
            recent_orders: vec![DashboardOrderRow {
                connection_id: shared::ids::ConnectionId::new("conn_1".to_string()),
                display_name: "shop.example.com".to_string(),
                order_id: shared::ids::OrderId::new("pay_xyz".to_string()),
                status: shared::order_status::OrderStatus::Paid.into(),
                amount: "25.00".to_string(),
                currency: "USD".to_string(),
                created_at: 1000,
            }],
            total_received_xmr: "1.234567890123".to_string(),
        };
        let html = page(&chrome(), &data).into_string();
        assert!(html.contains("pk_abc123"));
        assert!(
            html.contains(r#"<td>Feather test <span class="tag-network is-test">"#),
            "the store's wallet, with its network: {html}"
        );
        assert!(html.contains("1.234567890123"));
        assert!(html.contains("tag-ok"));
        assert!(html.contains(r#"href="/dashboard/stores/conn_1""#));
        assert!(html.contains("pay_xyz"));
        assert!(html.contains(r#"href="/dashboard/stores/conn_1/orders/pay_xyz""#));
    }

    /// A real public key and order id are long: each shows as
    /// `views::short_id` writes it, the whole in reach, the order id a
    /// plain link.
    #[test]
    fn a_long_public_key_and_order_id_are_shortened_with_the_whole_in_reach() {
        let public_key = format!("pk_b4c4e8{}3fa21c", "0".repeat(36));
        let order_id = "order_a8723b2e45b0d44ea8723b2e45b0d44e";
        let data = DashboardViewModel {
            has_stores: true,
            stores: vec![DashboardStoreRow {
                connection_id: shared::ids::ConnectionId::new("conn_1".to_string()),
                display_name: "shop.example.com".to_string(),
                site: "shop.example.com".to_string(),
                public_key: public_key.clone(),
                health: "ok".to_string(),
                health_label: "healthy".to_string(),
                wallet: None,
            }],
            recent_orders: vec![DashboardOrderRow {
                connection_id: shared::ids::ConnectionId::new("conn_1".to_string()),
                display_name: "shop.example.com".to_string(),
                order_id: shared::ids::OrderId::new(order_id.to_string()),
                status: shared::order_status::OrderStatus::Paid.into(),
                amount: "25.00".to_string(),
                currency: "USD".to_string(),
                created_at: 1000,
            }],
            total_received_xmr: "0".to_string(),
        };
        let html = page(&chrome(), &data).into_string();
        assert!(html.contains(&format!(
            r#"<td class="col-optional"><code><span class="short-value" title="{public_key}"><span class="short-value-text" aria-hidden="true">pk_b4c4e8…3fa21c</span><span class="short-value-full">{public_key}</span></span></code></td>"#
        )), "{html}");
        assert!(html.contains(&format!(
            r#"<a href="/dashboard/stores/conn_1/orders/{order_id}"><span class="short-value" title="{order_id}"><span class="short-value-text" aria-hidden="true">a8723b…b0d44e</span><span class="short-value-full">{order_id}</span></span></a>"#
        )), "{html}");
        assert!(!html.contains("..."), "{html}");
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
