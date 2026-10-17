//! `GET /` (signed in) - `http::home::home`.

use maud::{html, Markup, PreEscaped};
use ui::assets::icons;

use super::PageChrome;

/// One connected store as shown on the home page - a much smaller
/// projection of [`crate::db::StoreConnectionRow`] than the full order/
/// webhook views need, plus two fields no engine or database call directly
/// hands back: `display_name` (there is no user-facing name column on
/// `store_connections` - see `Db::list_store_connections_for_user`'s own
/// doc comment for why this derives one from `site_url` instead of adding
/// one) and `health` (`"ok"` if `EngineClient::get_tenant` on this
/// connection's decrypted `sk_...` succeeded, `"error"` otherwise - the only
/// signal actually available without this service also probing the
/// merchant's own `site_url`, which nothing here does).
pub struct HomeStoreRow {
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

/// One order row on the home page - like an orders-list row but
/// also carrying which store it belongs to, since the home page shows
/// orders across every connected store, not just one.
pub struct HomeOrderRow {
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
pub struct HomeViewModel {
    pub has_stores: bool,
    pub stores: Vec<HomeStoreRow>,
    pub recent_orders: Vec<HomeOrderRow>,
    /// Sum of every order's `amount_received_piconero` across every
    /// connected store - "total received XMR that has been detected",
    /// deliberately the *detected* amount rather than only orders in a
    /// `paid`/`overpaid` status, since a partially-paid or still-confirming
    /// order has still genuinely had funds detected for it.
    pub total_received_xmr: String,
}

pub fn page(chrome: &PageChrome, data: &HomeViewModel) -> Markup {
    let header = ui::PageHeader::new("Home");
    let body = html! {

        @if data.has_stores {
            (ui::Panel::new(html! {
                strong { "Total received (detected):" }
                // The engine lists a store's latest orders, not all
                // of them: the figure says so rather than claim a total.
                " " (data.total_received_xmr) " XMR across each connected store's latest 50 orders"
            }))

            h2 { "Your stores" }
            (ui::FormActions::new(ui::LinkButton::new(html! { (PreEscaped(icons::PLUS)) "Add another store" }, "/setup")))
            // On a phone each store is a card: its name and health, its
            // wallet and link, then its site.
            (ui::Table::new(ui::Slot::many(data.stores.iter().map(store_row)))
                .head(ui::Row::new(ui::Slot::many([
                    ui::Cell::new("Store").header(true).into(),
                    ui::Cell::new("Wallet").header(true).into(),
                    ui::Cell::new("Site").header(true).optional(true).into(),
                    ui::Cell::new("Public key").header(true).optional(true).into(),
                    ui::Cell::new("Status").header(true).into(),
                    ui::Slot::from(ui::Cell::new("").header(true)),
                ])))
                .look(ui::TableLook::Cards))

            h2 { "Recent orders" }
            @if data.recent_orders.is_empty() {
                p class="muted" { "No orders yet." }
            } @else {
                // On a phone each order is a card: order and status, amount
                // and date, then its store.
                (ui::Table::new(ui::Slot::many(data.recent_orders.iter().map(|order| order_row(chrome, order))))
                    .head(ui::Row::new(ui::Slot::many([
                        ui::Cell::new("Store").header(true).optional(true).into(),
                        ui::Cell::new("Order ID").header(true).into(),
                        ui::Cell::new("Status").header(true).into(),
                        ui::Cell::new("Amount").header(true).into(),
                        ui::Slot::from(ui::Cell::new("Created").header(true)),
                    ])))
                    .look(ui::TableLook::Cards))
            }
        }
        @if !data.has_stores {
            // A card like the settings pages': a head with an icon and the
            // title, then the body.
            section id="card-first-store" class="settings-card" aria-labelledby="card-first-store-title" {
                header class="card-head" {
                    (PreEscaped(icons::STORE))
                    h2 id="card-first-store-title" { "Connect your first store" }
                }
                div class="card-body" {
                    p {
                        "You don't have any stores connected yet. Adding one takes a couple of minutes - pick the "
                        "guided flow for WooCommerce, or the advanced form if you're integrating something custom."
                    }
                    (ui::FormActions::new(ui::LinkButton::new(html! { (PreEscaped(icons::PLUS)) "Add a store" }, "/setup").variant(ui::ButtonVariant::Primary)))
                }
            }
        }
    };
    super::Page::new("Home - Monokulo", header).render(chrome, body)
}

fn store_row(store: &HomeStoreRow) -> ui::Slot {
    ui::Row::new(ui::Slot::many([
        ui::Cell::new(store.display_name.as_str()).part(ui::CellPart::Title).into(),
        ui::Cell::new(html! {
            @if let Some((name, network)) = &store.wallet {
                (super::network_icon(network)) " " (name)
            } @else {
                span class="muted" { "Not linked yet" }
            }
        })
        .part(ui::CellPart::Detail)
        .into(),
        ui::Cell::new(store.site.as_str()).part(ui::CellPart::Meta).into(),
        ui::Cell::new(html! { code { (super::short_id(&store.public_key)) } }).optional(true).into(),
        ui::Cell::new(super::store_detail::health_tag(&store.health, &store.health_label)).part(ui::CellPart::Status).into(),
        ui::Slot::from(ui::Cell::new(html! {
            a class="nowrap" href=(format!("/dashboard/stores/{}", store.connection_id)) { "view →" }
        }).part(ui::CellPart::When)),
    ]))
    .into()
}

fn order_row(chrome: &PageChrome, order: &HomeOrderRow) -> ui::Slot {
    ui::Row::new(ui::Slot::many([
        ui::Cell::new(order.display_name.as_str()).part(ui::CellPart::Meta).into(),
        ui::Cell::new(html! {
            a href=(format!("/dashboard/stores/{}/orders/{}", order.connection_id, order.order_id)) {
                (super::short_id(order.order_id.as_str()))
            }
        })
        .part(ui::CellPart::Title)
        .into(),
        ui::Cell::new(super::state_badge(order.status)).part(ui::CellPart::Status).into(),
        ui::Cell::new(html! { span class="nowrap" { (super::display_amount(&order.amount, &order.currency)) } })
            .part(ui::CellPart::Amount)
            .into(),
        ui::Slot::from(ui::Cell::new(chrome.clock.time(order.created_at)).part(ui::CellPart::When)),
    ]))
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chrome() -> PageChrome {
        PageChrome::from_user(None, "/dashboard")
    }

    fn empty_data() -> HomeViewModel {
        HomeViewModel {
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
        // A card head with the shop icon and the title, then the body.
        assert!(
            html.contains(r#"<section id="card-first-store" class="settings-card" aria-labelledby="card-first-store-title"><header class="card-head"><svg xmlns="http://www.w3.org/2000/svg" class="mk-icon""#),
            "{html}"
        );
        assert!(
            html.contains(r#"<h2 id="card-first-store-title">Connect your first store</h2></header><div class="card-body">"#),
            "{html}"
        );
        assert!(html.contains("</svg>Add a store</a>"), "{html}");
        assert!(!html.contains("+ add"), "{html}");
        assert!(
            !html.contains("<table"),
            "an empty home page shouldn't render a store table at all"
        );
    }

    #[test]
    fn lists_stores_and_recent_orders_when_present() {
        let data = HomeViewModel {
            has_stores: true,
            stores: vec![HomeStoreRow {
                connection_id: shared::ids::ConnectionId::new("conn_1".to_string()),
                display_name: "shop.example.com".to_string(),
                site: "shop.example.com".to_string(),
                public_key: "pk_abc123".to_string(),
                health: "ok".to_string(),
                health_label: "healthy".to_string(),
                wallet: Some(("Feather test".to_string(), "stagenet".to_string())),
            }],
            recent_orders: vec![HomeOrderRow {
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
            html.contains(r#"<td class="card-detail"><span class="tag-network is-test" data-size="icon" title="Stagenet">"#)
                && html.contains("</span></span> Feather test</td>"),
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
        let data = HomeViewModel {
            has_stores: true,
            stores: vec![HomeStoreRow {
                connection_id: shared::ids::ConnectionId::new("conn_1".to_string()),
                display_name: "shop.example.com".to_string(),
                site: "shop.example.com".to_string(),
                public_key: public_key.clone(),
                health: "ok".to_string(),
                health_label: "healthy".to_string(),
                wallet: None,
            }],
            recent_orders: vec![HomeOrderRow {
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
        // `docs/txid_lookup_and_scan_chunking_wbs.md` Part C.8 - the home page's
        // only meta-refresh was the rescan syncing banner's; with that feature
        // gone, this page never auto-refreshes at all.
        let html = page(&chrome(), &empty_data()).into_string();
        assert!(!html.contains(r#"<meta http-equiv="refresh""#));
    }
}
