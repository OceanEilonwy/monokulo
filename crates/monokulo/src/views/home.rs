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
    pub health: String,
    pub health_label: String,
    /// The wallet it takes payments into: its name and network. `None`
    /// for a store not yet matched to one of the account's wallets.
    pub wallet: Option<(String, String)>,
    /// XMR detected across its latest orders (the engine lists 50).
    pub received_xmr: String,
    /// Its orders made in the last 24 hours, of those.
    pub orders_last_day: usize,
    /// When its latest order was made, if it has one.
    pub last_order_at: Option<i64>,
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
            // A card each: its name and health, what it's taking, and what
            // to do with it; adding another is the last of them.
            div class="mk-card-grid" data-list="stores" {
                @for store in &data.stores { (store_card(chrome, store)) }
                (ui::LaunchTile::new(
                    "Add another store",
                    "Connect a WooCommerce shop, or your own site.",
                    html! { (PreEscaped(icons::PLUS)) },
                )
                .href("/setup"))
            }

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

/// A store as a card: its name (to its page) and health, its site; then
/// its wallet and how it's doing, with the till, a new order and its
/// settings to hand. A store with no wallet can't take payments, so it
/// says so, and linking one is what it offers.
fn store_card(chrome: &PageChrome, store: &HomeStoreRow) -> ui::Card {
    let base = format!("/dashboard/stores/{}", store.connection_id);
    let link = |icon: &str, label: &str, href: String| {
        ui::Slot::from(
            ui::LinkButton::new(html! { (PreEscaped(icon)) (label) }, href)
                .size(ui::ControlSize::Compact),
        )
    };
    let settings = ui::LinkButton::new(
        html! { (PreEscaped(icons::SETTINGS)) "Settings" },
        format!("{base}/settings"),
    )
    .size(ui::ControlSize::Compact)
    .attr("aria-label", format!("{} settings", store.display_name));
    let fact = |term: &str, value: Markup| ui::Fact {
        term: term.to_string(),
        value: value.into(),
    };
    let card = ui::Card::new(html! { a href=(base) data-store-link { (store.display_name) } })
        .subtitle(if store.site.is_empty() {
            "No website"
        } else {
            store.site.as_str()
        })
        .status(super::store_detail::health_tag(
            &store.health,
            &store.health_label,
        ))
        .attr("data-store", store.connection_id.as_str());
    match &store.wallet {
        Some((name, network)) => card
            .body(ui::Facts::new(vec![
                fact(
                    "Wallet",
                    html! { (super::network_icon(network)) " " (name) },
                ),
                fact(
                    "Received",
                    html! { (super::trim_xmr(&store.received_xmr)) " XMR" },
                ),
                fact("Orders in 24 h", html! { (store.orders_last_day) }),
                fact(
                    "Last order",
                    html! {
                        @if let Some(at) = store.last_order_at { (chrome.clock.relative(at)) }
                        @else { span class="muted" { "None yet" } }
                    },
                ),
            ]))
            .action(ui::FormActions::new(ui::Slot::many([
                link(icons::POS, "Open POS", format!("{base}/pos")),
                link(icons::ORDER_NEW, "New order", format!("{base}/orders/new")),
                settings.into(),
            ]))),
        None => card
            .body(ui::Hint::new(
                "This store can't take payments until a wallet is linked.",
            ))
            .action(ui::FormActions::new(ui::Slot::many([
                ui::LinkButton::new(
                    html! { (PreEscaped(icons::WALLET)) "Link a wallet" },
                    format!("{base}/settings?tab=wallet"),
                )
                .variant(ui::ButtonVariant::Primary)
                .size(ui::ControlSize::Compact)
                .into(),
                settings.into(),
            ]))),
    }
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

    fn store(wallet: Option<(String, String)>) -> HomeStoreRow {
        HomeStoreRow {
            connection_id: shared::ids::ConnectionId::new("conn_1".to_string()),
            display_name: "Kafe Kolo".to_string(),
            site: "kafekolo.example".to_string(),
            health: "ok".to_string(),
            health_label: "healthy".to_string(),
            wallet,
            received_xmr: "3.412000000000".to_string(),
            orders_last_day: 7,
            last_order_at: Some(chrome().clock.now() - 720),
        }
    }

    /// Each store is a card: its name to its page, health and site; its
    /// wallet and how it's doing; the till, a new order and its settings,
    /// each with its icon. Adding another store is the grid's last tile.
    #[test]
    fn lists_stores_as_cards_and_recent_orders_when_present() {
        let data = HomeViewModel {
            has_stores: true,
            stores: vec![store(Some((
                "Feather test".to_string(),
                "stagenet".to_string(),
            )))],
            recent_orders: vec![HomeOrderRow {
                connection_id: shared::ids::ConnectionId::new("conn_1".to_string()),
                display_name: "Kafe Kolo".to_string(),
                order_id: shared::ids::OrderId::new("pay_xyz".to_string()),
                status: shared::order_status::OrderStatus::Paid.into(),
                amount: "25.00".to_string(),
                currency: "USD".to_string(),
                created_at: 1000,
            }],
            total_received_xmr: "1.234567890123".to_string(),
        };
        let html = page(&chrome(), &data).into_string();
        assert!(html.contains(r#"<div class="mk-card-grid" data-list="stores"><article class="mk-card" data-store="conn_1">"#), "{html}");
        assert!(html.contains(r#"<h3 class="mk-card-title"><a href="/dashboard/stores/conn_1" data-store-link>Kafe Kolo</a></h3><p class="mk-card-sub">kafekolo.example</p>"#), "{html}");
        assert!(html.contains("tag-ok"));
        assert!(
            html.contains(r#"<dt>Wallet</dt><dd><span class="tag-network is-test" data-size="icon" title="Stagenet">"#)
                && html.contains("</span></span> Feather test</dd>"),
            "the store's wallet, with its network: {html}"
        );
        assert!(
            html.contains("<dt>Received</dt><dd>3.412 XMR</dd>"),
            "{html}"
        );
        assert!(html.contains("<dt>Orders in 24 h</dt><dd>7</dd>"), "{html}");
        assert!(
            html.contains("<dt>Last order</dt><dd>12 min ago</dd>"),
            "{html}"
        );
        for (href, label) in [
            ("/dashboard/stores/conn_1/pos", "Open POS"),
            ("/dashboard/stores/conn_1/orders/new", "New order"),
            ("/dashboard/stores/conn_1/settings", "Settings"),
        ] {
            let at = html
                .find(&format!(r#"href="{href}""#))
                .unwrap_or_else(|| panic!("{href}: {html}"));
            let link = &html[at..at + html[at..].find("</a>").unwrap()];
            assert!(
                link.contains("<svg") && link.contains(label),
                "{label} with its icon: {link}"
            );
        }
        assert!(
            html.contains(r#"aria-label="Kafe Kolo settings""#),
            "{html}"
        );
        assert!(
            html.contains(r#"<a class="mk-launch-tile" href="/setup""#),
            "{html}"
        );
        assert!(html.contains("1.234567890123"));
        assert!(html.contains(r#"href="/dashboard/stores/conn_1/orders/pay_xyz""#));
    }

    /// A store with no wallet can't take payments: its card says so and
    /// offers to link one, the page's one primary action, instead of its
    /// figures and the till.
    #[test]
    fn a_store_without_a_wallet_offers_to_link_one() {
        let data = HomeViewModel {
            has_stores: true,
            stores: vec![store(None)],
            recent_orders: vec![],
            total_received_xmr: "0".to_string(),
        };
        let html = page(&chrome(), &data).into_string();
        assert!(
            html.contains("This store can't take payments until a wallet is linked."),
            "{html}"
        );
        assert!(
            html.contains(r#"href="/dashboard/stores/conn_1/settings?tab=wallet""#),
            "{html}"
        );
        assert_eq!(
            html.matches(r#"data-variant="primary""#).count(),
            1,
            "{html}"
        );
        assert!(!html.contains("Open POS"), "{html}");
        assert!(!html.contains("<dt>Received</dt>"), "{html}");
    }

    /// A long order id shows as `views::short_id` writes it, the whole in
    /// reach, a plain link.
    #[test]
    fn a_long_order_id_is_shortened_with_the_whole_in_reach() {
        let order_id = "order_a8723b2e45b0d44ea8723b2e45b0d44e";
        let data = HomeViewModel {
            has_stores: true,
            stores: vec![store(None)],
            recent_orders: vec![HomeOrderRow {
                connection_id: shared::ids::ConnectionId::new("conn_1".to_string()),
                display_name: "Kafe Kolo".to_string(),
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
