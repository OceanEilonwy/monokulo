//! `GET /dashboard/stores/{id}` - `http::orders::store_detail`/
//! `render_store_detail_page`.

use maud::{html, Markup};

use super::orders::{lookup_payment_card, OrderRowViewModel};
use super::{layout, PageChrome};

pub struct StoreDetailData {
    pub connection_id: String,
    pub display_name: String,
    pub platform: String,
    pub site_url: String,
    pub public_key: String,
    /// No longer shown anywhere on this page (an internal detail, not
    /// something a merchant needs day to day) - kept only because
    /// `integration_help::fragment` still takes it as a parameter, for
    /// signature parity with the old partial it replaced; that fragment
    /// itself never actually renders it (`let _ = endpoint;`).
    pub endpoint: String,
    pub base_currency: String,
    pub health: String,
    pub health_label: String,
    pub created_at: i64,
    pub recent_orders: Vec<OrderRowViewModel>,
    /// Drives which half of the integration-help fragment renders.
    pub is_woocommerce: bool,
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
}

pub struct StoreDetailViewModel {
    pub store: Option<StoreDetailData>,
}

pub fn page(chrome: &PageChrome, data: &StoreDetailViewModel) -> Markup {
    let body = html! {
        div class="wrap" {
            @if let Some(store) = &data.store {
                nav class="context-nav" aria-label="Breadcrumb" { a href="/dashboard" { "Dashboard" } }
                h1 { (store.display_name) }

                div class="store-header-row" {
                    span class="store-header-status" {
                        span class=(format!("tag tag-{}", store.health)) { (store.health_label) }
                        "\u{a0}" span class="muted" { (store.platform) } "\u{a0}·\u{a0}"
                        a href=(store.site_url) { (store.site_url) }
                    }
                    a class="btn btn-secondary settings-link" href=(format!("/dashboard/stores/{}/settings", store.connection_id)) {
                        "Settings " span aria-hidden="true" { "→" }
                    }
                    details class="help-disclosure" {
                        summary class="btn btn-secondary help-control" { "Help" }
                        div class="store-help-content" {
                            (super::integration_help::fragment(&store.public_key, &store.endpoint, store.is_woocommerce))
                        }
                    }
                }

                table {
                    tr { th { "Base currency" } td { (store.base_currency) } }
                    tr { th { "Public key" } td { code { (store.public_key) } } }
                    tr { th { "Connected" } td { (store.created_at) } }
                }

                div class="widget-links" {
                    a class="btn widget-link pos-launch-disabled" id="pos-launch" data-href=(format!("/dashboard/stores/{}/pos", store.connection_id)) aria-disabled="true" tabindex="-1" {
                        span class="widget-link-icon" {
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
                        span class="widget-link-text" {
                            span class="widget-link-title" { "POS Terminal" }
                            span class="widget-link-hint" id="pos-launch-hint" { "Requires JS" }
                        }
                    }
                    script { (maud::PreEscaped("(function(){var link=document.getElementById('pos-launch');if(!link)return;link.href=link.dataset.href;link.removeAttribute('aria-disabled');link.removeAttribute('tabindex');link.classList.remove('pos-launch-disabled');document.getElementById('pos-launch-hint').textContent='Full-screen keypad for in-person sales';})();")) }
                    a class="btn widget-link" href=(format!("/dashboard/stores/{}/orders/new", store.connection_id)) {
                        span class="widget-link-icon" {
                            svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"
                                stroke-linejoin="round" aria-hidden="true" focusable="false" {
                                path d="M6 3h9l3 3v15a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1z" {}
                                path d="M15 3v3h3" {}
                                line x1="12" y1="11" x2="12" y2="17" {}
                                line x1="9" y1="14" x2="15" y2="14" {}
                            }
                        }
                        span class="widget-link-text" {
                            span class="widget-link-title" { "Create an order" }
                            span class="widget-link-hint" { "For one-off custom payments" }
                        }
                    }
                }

                h2 { "Recent orders" }
                @if store.recent_orders.is_empty() {
                    p class="muted" { "No orders yet." }
                } @else {
                    table {
                        thead { tr { th { "Order ID" } th { "Status" } th { "Amount" } th { "Created" } } }
                        tbody {
                            @for order in &store.recent_orders {
                                tr {
                                    td {
                                        a href=(format!("/dashboard/stores/{}/orders/{}", store.connection_id, order.order_id)) {
                                            (order.order_id)
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
                h1 { "Store not found" }
                p { "This store doesn't exist, or isn't connected to your account." }
            }
        }
    };
    let title = match &data.store {
        Some(store) => format!("{} - Monokulo", store.display_name),
        None => "Store not found - Monokulo".to_string(),
    };
    layout(chrome, &title, body)
}

pub fn woocommerce_instructions_page(chrome: &PageChrome) -> Markup {
    let body = html! {
        div class="wrap" {
            nav class="context-nav" aria-label="Breadcrumb" { a href="/dashboard/stores/new" { "Add a store" } }
            h1 { "Set up WooCommerce" }
            p class="hint" {
                "This flow runs from inside WordPress, not from here - it needs your store's own "
                "URL and a plugin-issued token to hand back to it, which only WordPress itself can provide. Follow "
                "these steps from your WordPress admin:"
            }
            ol class="steps" {
                li { "Install and activate the " strong { "Monokulo" } " plugin (WordPress admin → Plugins → Add New, search \"Monokulo\")." }
                li { "Go to " strong { "WooCommerce → Settings → Payments" } " and enable " strong { "Monokulo" } "." }
                li { "Open its settings and click " strong { "Connect to Monokulo" } "." }
                li {
                    "You'll land back here to paste in your view key and spend public key (the same \"custom\" "
                    "form the advanced flow uses) - your WooCommerce site is remembered automatically, so nothing "
                    "else needs typing."
                }
                li { "Once confirmed, you're sent straight back to your WooCommerce settings, already connected." }
            }
            div class="box" {
                p class="hint" {
                    "Don't have the plugin installed yet, or just want to see what the advanced form "
                    "looks like first? You can also "
                    a href="/dashboard/connect" { "connect manually" }
                    " right now and enter your WooCommerce site's URL yourself."
                }
            }
        }
    };
    layout(chrome, "Connect WooCommerce - Monokulo", body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chrome() -> PageChrome {
        PageChrome::from_user(None, "/dashboard/stores/conn_1")
    }

    fn base_store(is_woocommerce: bool) -> StoreDetailData {
        StoreDetailData {
            connection_id: "conn_1".to_string(),
            display_name: "shop.example.com".to_string(),
            platform: if is_woocommerce { "woocommerce".to_string() } else { "custom".to_string() },
            site_url: "https://shop.example.com".to_string(),
            public_key: "pk_abc123".to_string(),
            endpoint: "http://127.0.0.1:8080".to_string(),
            base_currency: "XMR".to_string(),
            health: "ok".to_string(),
            health_label: "healthy".to_string(),
            created_at: 1000,
            recent_orders: vec![],
            is_woocommerce,
            lookup_txid_value: String::new(),
            lookup_message: None,
            lookup_found_order_id: None,
        }
    }

    #[test]
    fn renders_not_found_state_when_store_is_none() {
        let html = page(&chrome(), &StoreDetailViewModel { store: None }).into_string();
        assert!(html.to_lowercase().contains("not found"));
    }

    #[test]
    fn renders_integration_help_with_the_right_public_key_via_the_shared_fragment() {
        let store = StoreDetailData { health: "error".to_string(), health_label: "unreachable".to_string(), ..base_store(true) };
        let html = page(&chrome(), &StoreDetailViewModel { store: Some(store) }).into_string();
        // Proves the integration_help fragment actually received this
        // store's own public_key, not some stale or empty value - the exact
        // same fragment the post-connect success page uses
        // (`views::connect`'s own tests), so the two can never drift on
        // what "integrate this store" means. `endpoint` isn't asserted here -
        // `integration_help::fragment` never actually renders it (`let _ =
        // endpoint;` in that function, kept only for signature parity), and
        // this page's own "Engine endpoint" table row is gone by design.
        assert!(html.contains("pk_abc123"));
        assert!(html.contains("tag-error"));
        assert!(html.contains("Integrate this store"));
        // is_woocommerce: true must render the "already connected" copy,
        // not the "install the plugin" onboarding steps - real bug: this
        // used to always show WooCommerce onboarding instructions even for
        // stores connected through the advanced/custom form.
        assert!(html.contains("already connected via the WooCommerce plugin"));
        assert!(!html.contains("Install the"), "should not show plugin-install instructions for an already-connected store");
    }

    /// The other half of the same real bug: a store connected via the
    /// advanced (custom) form must show generic direct-API instructions,
    /// never the WooCommerce-specific onboarding steps - it was never
    /// connected through the plugin at all.
    #[test]
    fn shows_generic_integration_help_for_a_non_woocommerce_store() {
        let html = page(&chrome(), &StoreDetailViewModel { store: Some(base_store(false)) }).into_string();
        assert!(html.contains("Install the"), "expected the WooCommerce onboarding steps to still be offered, got: {html}");
        assert!(!html.contains("already connected via the WooCommerce plugin"));
    }

    #[test]
    fn shows_the_base_currency_at_the_top_of_the_table_and_hides_the_engine_endpoint() {
        let store = StoreDetailData { base_currency: "USD".to_string(), ..base_store(false) };
        let html = page(&chrome(), &StoreDetailViewModel { store: Some(store) }).into_string();
        assert!(html.contains(r#"<th>Base currency</th><td>USD</td>"#), "expected the base currency row, got: {html}");
        let base_currency_pos = html.find("Base currency").expect("base currency row missing");
        let public_key_pos = html.find("Public key").expect("public key row missing");
        assert!(base_currency_pos < public_key_pos, "expected Base currency above Public key, got: {html}");
        assert!(!html.contains("Engine endpoint"), "the engine endpoint row should no longer render on this page, got: {html}");
    }

    #[test]
    fn links_to_the_settings_page_and_widget_pages() {
        let html = page(&chrome(), &StoreDetailViewModel { store: Some(base_store(false)) }).into_string();
        assert!(html.contains(r#"href="/dashboard/stores/conn_1/settings""#), "expected a Settings link, got: {html}");
        let help = html.find("class=\"btn btn-secondary help-control\"").unwrap();
        let settings = html.find("class=\"btn btn-secondary settings-link\"").unwrap();
        assert!(settings < help, "Help should be the rightmost control in the store header: {html}");
        assert!(html.contains(r#"<summary class="btn btn-secondary help-control">Help</summary>"#), "expected Help to be the disclosure's only summary content: {html}");
        let summary_start = html.find("<summary").unwrap();
        assert!(html.find("class=\"store-header-status\"").unwrap() < summary_start);
        assert!(settings < summary_start, "Settings must sit outside the Help summary: {html}");
        assert!(html.contains("Settings <span aria-hidden=\"true\">→</span>"), "expected the Settings arrow: {html}");
        assert!(html.contains(r#"data-href="/dashboard/stores/conn_1/pos""#));
        assert!(html.contains("Requires JS"));
        assert!(html.contains(r#"aria-disabled="true""#));
        assert!(html.contains(r#"href="/dashboard/stores/conn_1/orders/new""#));
    }

    #[test]
    fn shows_the_lookup_message_when_present() {
        let store = StoreDetailData {
            lookup_txid_value: "abc123".to_string(),
            lookup_message: Some("No transaction with that ID was found on the network.".to_string()),
            ..base_store(false)
        };
        let html = page(&chrome(), &StoreDetailViewModel { store: Some(store) }).into_string();
        assert!(html.contains("No transaction with that ID was found on the network."));
        assert!(html.contains(r#"value="abc123""#));
    }

    #[test]
    fn woocommerce_instructions_page_renders() {
        let html = woocommerce_instructions_page(&chrome()).into_string();
        assert!(html.to_lowercase().contains("woocommerce"));
        assert!(html.contains(r#"href="/dashboard/connect""#));
    }
}
