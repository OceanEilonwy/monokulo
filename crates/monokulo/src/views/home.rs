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
    pub status: super::OrderStatus,
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
                    "Connect a shop plugin, or your own site.",
                    html! { (PreEscaped(icons::PLUS)) },
                )
                .href(crate::base::url("/setup")))
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
                        "guided flow for a shop plugin, or the advanced form if you're integrating something custom."
                    }
                    (ui::FormActions::new(ui::LinkButton::new(html! { (PreEscaped(icons::PLUS)) "Add a store" }, crate::base::url("/setup")).variant(ui::ButtonVariant::Primary)))
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
    let base = crate::base::url(format!("/stores/{}", store.connection_id));
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
            a href=(crate::base::url(format!("/stores/{}/orders/{}", order.connection_id, order.order_id))) {
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
