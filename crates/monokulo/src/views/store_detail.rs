//! `GET /dashboard/stores/{id}` - `http::orders::store_detail`/
//! `render_store_detail_page`.

use maud::{html, Markup};

use super::orders::{lookup_payment_card, orders_table, OrderRowViewModel};
use super::PageChrome;

pub struct StoreDetailData {
    pub connection_id: crate::db::ConnectionId,
    pub display_name: String,
    /// The host its checkout runs on; empty for a store with no site.
    pub site: String,
    pub public_key: String,
    pub base_currency: String,
    pub health: String,
    pub health_label: String,
    pub created_at: i64,
    pub recent_orders: Vec<OrderRowViewModel>,
    /// `docs/txid_lookup_and_scan_chunking_wbs.md` Part B.3 - re-populates the
    /// "look up a payment" card's own input after a submission, empty for a
    /// plain page view.
    pub lookup_txid_value: String,
    /// The lookup's own plain-text result, `None` until a lookup has actually
    /// been submitted.
    pub lookup_message: Option<String>,
    /// `Some(order_id)` only when the lookup found a real match - a link to
    /// the now-updated order, alongside `lookup_message`.
    pub lookup_found_order_id: Option<String>,
    pub embed_warnings: EmbedWarnings,
    /// The wallet the store takes payments into: its id and name.
    pub wallet: Option<(String, String)>,
}

/// A verified domain whose DNS record has gone missing
/// (`http::embed_domains::store_page_warnings`).
pub struct FailingDomainWarning {
    pub domain: String,
    pub record_name: String,
    /// How long the record has been missing, e.g. "5h 20m".
    pub missing_for: String,
    /// Past the grace period, so it no longer counts as verified.
    pub lapsed: bool,
    /// How much longer it still counts as verified (when not lapsed).
    pub counts_for: String,
}

/// The store page's embed warnings: "any website can show this checkout"
/// while embedding isn't restricted (shrunk to one line once dismissed), and,
/// never dismissable, one per failing domain plus one when the restriction
/// is on but no domain counts any more.
/// The one-line reminder left once the "any website" warning is
/// dismissed; also what fixi swaps in for the warning on Dismiss.
pub fn compact_embed_warning(connection_id: &str) -> Markup {
    let settings = format!("/dashboard/stores/{connection_id}/settings#verified-domains");
    html! {
        (ui::Alert::new(html! { a href=(settings) { "Verify your domains" } }, ui::Tone::Warning)
            .title("Any website can show this store's checkout.")
            .quiet(true)
            .attr("id", "embed-warning"))
    }
}

pub struct EmbedWarnings {
    pub restricted: bool,
    pub any_site_dismissed: bool,
    /// Restricted, with no domain counting as verified: no website can show
    /// the checkout.
    pub shown_nowhere: bool,
    pub failing: Vec<FailingDomainWarning>,
}

pub struct StoreDetailViewModel {
    pub store: Option<StoreDetailData>,
}

fn embed_warnings(connection_id: &str, warnings: &EmbedWarnings) -> Markup {
    let settings = format!("/dashboard/stores/{connection_id}/settings#verified-domains");
    let dismiss = format!("/dashboard/stores/{connection_id}/embed-warning/dismiss");
    html! {
        @if warnings.shown_nowhere {
            (ui::Alert::new(html! {
                "Only verified domains are allowed, and none is verified right now. "
                a href=(settings) { "Check your domains in Settings" } ", or turn the restriction off there."
            }, ui::Tone::Error)
            .title("No website can show this store's checkout")
            .block(true))
        }
        @for failing in &warnings.failing {
            @if failing.lapsed {
                (ui::Alert::new(html! {
                    "The TXT record at " code { (failing.record_name) } " has been missing for " (failing.missing_for) ". "
                    "Publish it again, then " a href=(settings) { "check it in Settings" } "."
                }, ui::Tone::Error)
                .title(format!("{} is no longer verified", failing.domain))
                .block(true))
            } @else {
                (ui::Alert::new(html! {
                    "The TXT record at " code { (failing.record_name) } " has been missing for " (failing.missing_for) ". "
                    "If it isn't back within " (failing.counts_for) ", " (failing.domain) " will stop counting as verified"
                    @if warnings.restricted { " and stop being able to show this checkout" }
                    ". " a href=(settings) { "Check it in Settings" } "."
                }, ui::Tone::Error)
                .title(format!("{} failed its DNS check", failing.domain))
                .block(true))
            }
        }
        @if warnings.restricted {
        } @else if warnings.any_site_dismissed {
            (compact_embed_warning(connection_id))
        } @else {
            (ui::Alert::new(html! {
                "If a scam site embeds it, that site's victims would pay you, and you'd be the one they come to. "
                a href=(settings) { "Verify your domains" } " in Settings and allow only them."
            }, ui::Tone::Warning)
            .title("Any website can show this store's checkout")
            .actions(html! {
                form method="post" action=(dismiss) fx-action=(dismiss) fx-method="POST" fx-target="#embed-warning" {
                    (ui::Button::new("Dismiss").kind(ui::ButtonType::Submit).size(ui::ControlSize::Compact))
                }
            })
            .block(true)
            .quiet(true)
            .attr("id", "embed-warning"))
        }
    }
}

/// A store's health as a tag: `"ok"` or `"error"`
/// (`http::orders::health_of_tenant_lookup`), in words.
pub fn health_tag(health: &str, label: &str) -> ui::Tag {
    let tone = if health == "ok" {
        ui::TagTone::Ok
    } else {
        ui::TagTone::Error
    };
    ui::Tag::new(label).tone(tone)
}

/// A row of the store's facts: its name, then its value.
fn fact_row(name: &str, value: Markup) -> ui::Slot {
    ui::Row::new(ui::Slot::many([
        ui::Cell::new(name).header(true).into(),
        ui::Slot::from(ui::Cell::new(value)),
    ]))
    .into()
}

/// A keypad, for the POS tile.
fn pos_icon() -> Markup {
    html! {
        svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"
            stroke-linejoin="round" aria-hidden="true" focusable="false" {
            rect x="4" y="3" width="16" height="18" rx="2" {}
            circle cx="8.5" cy="8.5" r="0.6" fill="currentColor" stroke="none" {}
            circle cx="12" cy="8.5" r="0.6" fill="currentColor" stroke="none" {}
            circle cx="15.5" cy="8.5" r="0.6" fill="currentColor" stroke="none" {}
            circle cx="8.5" cy="12" r="0.6" fill="currentColor" stroke="none" {}
            circle cx="12" cy="12" r="0.6" fill="currentColor" stroke="none" {}
            circle cx="15.5" cy="12" r="0.6" fill="currentColor" stroke="none" {}
            circle cx="8.5" cy="15.5" r="0.6" fill="currentColor" stroke="none" {}
            circle cx="12" cy="15.5" r="0.6" fill="currentColor" stroke="none" {}
            circle cx="15.5" cy="15.5" r="0.6" fill="currentColor" stroke="none" {}
            line x1="8" y1="18.5" x2="16" y2="18.5" {}
        }
    }
}

/// A page with a plus, for the new order tile.
fn new_order_icon() -> Markup {
    html! {
        svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"
            stroke-linejoin="round" aria-hidden="true" focusable="false" {
            path d="M6 3h9l3 3v15a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1z" {}
            path d="M15 3v3h3" {}
            line x1="12" y1="11" x2="12" y2="17" {}
            line x1="9" y1="14" x2="15" y2="14" {}
        }
    }
}

pub fn page(chrome: &PageChrome, data: &StoreDetailViewModel) -> Markup {
    let header = match &data.store {
        Some(store) => {
            ui::PageHeader::new(store.display_name.as_str()).crumbs(super::crumbs(&[("/", "Home")]))
        }
        None => ui::PageHeader::new("Store not found"),
    };
    let body = html! {
        @if let Some(store) = &data.store {
            div class="store-header-row" {
                // One line: the site shows as its host, cut with an ellipsis
                // rather than broken mid-word; the link keeps the full URL.
                span class="store-header-status" {
                    (health_tag(&store.health, &store.health_label))
                    // The site is a host (`crate::stores::normalize_site`),
                    // so the link is built, never the merchant's text.
                    @if !store.site.is_empty() {
                        span class="muted" aria-hidden="true" { "·" }
                        a class="store-site" href=(crate::stores::site_link(&store.site)) title=(store.site) { (store.site) }
                    }
                }
                (ui::LinkButton::new(html! { "Settings " span aria-hidden="true" { "→" } }, format!("/dashboard/stores/{}/settings", store.connection_id))
                    .attr("data-store-link", "settings"))
                (ui::LinkButton::new(html! { "Help " (super::external_link_icon()) }, super::DOCS_URL)
                    .attr("target", "_blank")
                    .attr("rel", "noopener")
                    .attr("data-store-link", "help"))
            }

            (embed_warnings(store.connection_id.as_str(), &store.embed_warnings))

            (ui::Table::new(ui::Slot::many([
                fact_row("Wallet", html! {
                    @match &store.wallet {
                        Some((id, name)) => a href=(format!("/account/wallets/{id}")) { (name) },
                        None => span class="muted" { "-" },
                    }
                }),
                fact_row("Base currency", html! { (store.base_currency) }),
                fact_row("Public key", html! { code { (store.public_key) } }),
                fact_row("Connected", chrome.clock.time(store.created_at)),
            ]))
            .look(ui::TableLook::Facts)
            .scroll(false))
            @if chrome.is_admin {
                p { (super::logs_link(chrome, "store.id", store.connection_id.as_str(), "This store's logs")) }
            }

            // The POS needs JavaScript: without it, its tile says so.
            div class="widget-links" {
                (ui::LaunchTile::new("POS Terminal", "Full-screen keypad for in-person sales", pos_icon())
                    .href(format!("/dashboard/stores/{}/pos", store.connection_id))
                    .attr("data-only", "script"))
                (ui::LaunchTile::new("POS Terminal", "Requires JavaScript", pos_icon())
                    .attr("data-only", "no-script"))
                (ui::LaunchTile::new("Create an order", "For one-off custom payments", new_order_icon())
                    .href(format!("/dashboard/stores/{}/orders/new", store.connection_id)))
            }

            h2 { "Recent orders" }
            @if store.recent_orders.is_empty() {
                p class="muted" { "No orders yet." }
            } @else {
                (orders_table(store.connection_id.as_str(), &store.recent_orders, &chrome.clock))
            }
            p {
                a href=(format!("/dashboard/stores/{}/orders", store.connection_id)) { "all orders →" }
            }

            (lookup_payment_card(
                &format!("/dashboard/stores/{}/orders/lookup", store.connection_id),
                &store.lookup_txid_value,
                &store.lookup_message,
                &store.lookup_found_order_id,
                |order_id| format!("/dashboard/stores/{}/orders/{}", store.connection_id, order_id),
            ))
        } @else {
            p { "This store doesn't exist, or isn't connected to your account." }
        }
    };
    let title = match &data.store {
        Some(store) => format!("{} - Monokulo", store.display_name),
        None => "Store not found - Monokulo".to_string(),
    };
    super::Page::new(title, header).render(chrome, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chrome() -> PageChrome {
        PageChrome::from_user(None, "/dashboard/stores/conn_1")
    }

    fn base_store() -> StoreDetailData {
        StoreDetailData {
            wallet: None,
            connection_id: shared::ids::ConnectionId::new("conn_1".to_string()),
            display_name: "Corner shop".to_string(),
            site: "shop.example.com".to_string(),
            public_key: "pk_abc123".to_string(),
            base_currency: "XMR".to_string(),
            health: "ok".to_string(),
            health_label: "healthy".to_string(),
            created_at: 1000,
            recent_orders: vec![],
            lookup_txid_value: String::new(),
            lookup_message: None,
            lookup_found_order_id: None,
            embed_warnings: EmbedWarnings {
                restricted: false,
                any_site_dismissed: false,
                shown_nowhere: false,
                failing: vec![],
            },
        }
    }

    #[test]
    fn the_any_site_warning_can_be_dismissed_to_one_line_but_a_failing_domain_warning_cannot() {
        let html = page(
            &chrome(),
            &StoreDetailViewModel {
                store: Some(base_store()),
            },
        )
        .into_string();
        assert!(html.contains(r#"<div class="warning" id="embed-warning"><strong class="alert-title">Any website can show this store's checkout</strong>"#), "got: {html}");
        assert!(html.contains(r#"action="/dashboard/stores/conn_1/embed-warning/dismiss""#));
        assert!(html.contains(r#"href="/dashboard/stores/conn_1/settings#verified-domains""#));

        let store = StoreDetailData {
            wallet: None,
            embed_warnings: EmbedWarnings {
                restricted: false,
                any_site_dismissed: true,
                shown_nowhere: false,
                failing: vec![
                    FailingDomainWarning {
                        domain: "shop.example".to_string(),
                        record_name: "_monokulo.shop.example".to_string(),
                        missing_for: "5h".to_string(),
                        lapsed: false,
                        counts_for: "2d 19h".to_string(),
                    },
                    FailingDomainWarning {
                        domain: "old.example".to_string(),
                        record_name: "_monokulo.old.example".to_string(),
                        missing_for: "4d".to_string(),
                        lapsed: true,
                        counts_for: "any moment".to_string(),
                    },
                ],
            },
            ..base_store()
        };
        let html = page(&chrome(), &StoreDetailViewModel { store: Some(store) }).into_string();
        assert!(html.contains(r#"<p class="warning" id="embed-warning"><strong class="alert-title">Any website can show this store's checkout.</strong>"#), "got: {html}");
        assert!(
            !html.contains("embed-warning/dismiss"),
            "a dismissed warning offers nothing more to dismiss"
        );
        assert!(html.contains("shop.example failed its DNS check"));
        assert!(html.contains(
            "If it isn't back within 2d 19h, shop.example will stop counting as verified."
        ));
        assert!(html.contains("old.example is no longer verified"));
        assert_eq!(
            html.matches(r#"<div class="error" role="alert"><strong class="alert-title">"#)
                .count(),
            2
        );
    }

    #[test]
    fn a_restricted_store_drops_the_any_site_warning_but_warns_when_no_domain_counts() {
        let warnings = |shown_nowhere| EmbedWarnings {
            restricted: true,
            any_site_dismissed: false,
            shown_nowhere,
            failing: vec![],
        };
        let store = StoreDetailData {
            wallet: None,
            embed_warnings: warnings(false),
            ..base_store()
        };
        let html = page(&chrome(), &StoreDetailViewModel { store: Some(store) }).into_string();
        assert!(!html.contains(r#"class="alert-title""#), "got: {html}");

        let store = StoreDetailData {
            wallet: None,
            embed_warnings: warnings(true),
            ..base_store()
        };
        let html = page(&chrome(), &StoreDetailViewModel { store: Some(store) }).into_string();
        assert!(html.contains("No website can show this store's checkout"));
    }

    #[test]
    fn renders_not_found_state_when_store_is_none() {
        let html = page(&chrome(), &StoreDetailViewModel { store: None }).into_string();
        assert!(html.to_lowercase().contains("not found"));
    }

    #[test]
    fn the_store_is_called_by_its_name_and_its_site_is_a_link_to_the_host() {
        let html = page(
            &chrome(),
            &StoreDetailViewModel {
                store: Some(base_store()),
            },
        )
        .into_string();
        assert!(html.contains("<h1>Corner shop</h1>"), "{html}");
        assert!(
            html.contains(
                r#"href="https://shop.example.com" title="shop.example.com">shop.example.com</a>"#
            ),
            "{html}"
        );
        assert!(html.contains("pk_abc123"));

        // A store with no site has none to show.
        let html = page(
            &chrome(),
            &StoreDetailViewModel {
                store: Some(StoreDetailData {
                    site: String::new(),
                    ..base_store()
                }),
            },
        )
        .into_string();
        assert!(!html.contains("store-site"), "{html}");
    }

    #[test]
    fn shows_the_base_currency_at_the_top_of_the_table_and_hides_the_engine_endpoint() {
        let store = StoreDetailData {
            wallet: None,
            base_currency: "USD".to_string(),
            ..base_store()
        };
        let html = page(&chrome(), &StoreDetailViewModel { store: Some(store) }).into_string();
        assert!(
            html.contains(r#"<th>Base currency</th><td>USD</td>"#),
            "expected the base currency row, got: {html}"
        );
        let base_currency_pos = html
            .find("Base currency")
            .expect("base currency row missing");
        let public_key_pos = html.find("Public key").expect("public key row missing");
        assert!(
            base_currency_pos < public_key_pos,
            "expected Base currency above Public key, got: {html}"
        );
        assert!(
            !html.contains("Engine endpoint"),
            "the engine endpoint row should no longer render on this page, got: {html}"
        );
    }

    #[test]
    fn links_to_the_settings_page_and_widget_pages() {
        let html = page(
            &chrome(),
            &StoreDetailViewModel {
                store: Some(base_store()),
            },
        )
        .into_string();
        assert!(
            html.contains(r#"href="/dashboard/stores/conn_1/settings""#),
            "expected a Settings link, got: {html}"
        );
        let help = html.find(r#"data-store-link="help""#).unwrap();
        let settings = html.find(r#"data-store-link="settings""#).unwrap();
        assert!(
            settings < help,
            "Help should be the rightmost control in the store header: {html}"
        );
        // Help is the docs, in a new tab.
        assert!(
            html.contains(&format!(
                r#"<a class="mk-btn" href="{}" target="_blank" rel="noopener" data-store-link="help">Help "#,
                crate::views::DOCS_URL
            )),
            "expected Help to link to the docs: {html}"
        );
        assert!(html.find("class=\"store-header-status\"").unwrap() < help);
        assert!(
            html.contains("Settings <span aria-hidden=\"true\">→</span>"),
            "expected the Settings arrow: {html}"
        );
        // The POS needs JavaScript: its tile is a link only with it.
        assert!(html.contains(r#"<a class="mk-launch-tile" href="/dashboard/stores/conn_1/pos" data-only="script">"#), "{html}");
        assert!(
            html.contains(
                r#"<a class="mk-launch-tile" aria-disabled="true" data-only="no-script">"#
            ),
            "{html}"
        );
        assert!(html.contains("Requires JavaScript"));
        assert!(html.contains(r#"href="/dashboard/stores/conn_1/orders/new""#));
    }

    #[test]
    fn shows_the_lookup_message_when_present() {
        let store = StoreDetailData {
            wallet: None,
            lookup_txid_value: "abc123".to_string(),
            lookup_message: Some(
                "No transaction with that ID was found on the network.".to_string(),
            ),
            ..base_store()
        };
        let html = page(&chrome(), &StoreDetailViewModel { store: Some(store) }).into_string();
        assert!(html.contains("No transaction with that ID was found on the network."));
        assert!(html.contains(r#"value="abc123""#));
    }
}
