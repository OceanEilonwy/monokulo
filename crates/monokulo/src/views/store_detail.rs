//! `GET /dashboard/connections/{id}` - `http::orders::store_detail`/
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
    pub endpoint: String,
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
    /// `Some(payment_id)` only when the lookup found a real match - a link to
    /// the now-updated order, alongside `lookup_message`.
    pub lookup_found_payment_id: Option<String>,
}

pub struct StoreDetailViewModel {
    pub store: Option<StoreDetailData>,
}

pub fn page(chrome: &PageChrome, data: &StoreDetailViewModel) -> Markup {
    let body = html! {
        div class="wrap" {
            @if let Some(store) = &data.store {
                h1 { (store.display_name) }

                div class="store-header-row" {
                    details class="help-disclosure" {
                        summary {
                            span {
                                span class=(format!("tag tag-{}", store.health)) { (store.health_label) }
                                "\u{a0}" span class="muted" { (store.platform) } "\u{a0}·\u{a0}"
                                a href=(store.site_url) { (store.site_url) }
                            }
                            span class="hint" { "help" }
                        }
                        (super::integration_help::fragment(&store.public_key, &store.endpoint, store.is_woocommerce))
                    }
                    a class="btn-secondary settings-link" href=(format!("/dashboard/connections/{}/settings", store.connection_id)) { "Settings" }
                }

                table {
                    tr { th { "Public key" } td { code { (store.public_key) } } }
                    tr { th { "Engine endpoint" } td { code { (store.endpoint) } } }
                    tr { th { "Connected" } td { (store.created_at) } }
                }

                div class="widget-links" {
                    a class="btn widget-link" href=(format!("/dashboard/connections/{}/pos", store.connection_id)) {
                        span class="widget-link-title" { "POS Terminal" }
                        span class="widget-link-hint" { "Full-screen keypad for in-person sales" }
                    }
                    a class="btn widget-link" href=(format!("/dashboard/connections/{}/orders/new", store.connection_id)) {
                        span class="widget-link-title" { "Create an order" }
                        span class="widget-link-hint" { "Creates a real order and opens its payment page" }
                    }
                }

                h2 { "Recent orders" }
                (lookup_payment_card(
                    &format!("/dashboard/connections/{}/orders/lookup", store.connection_id),
                    &store.lookup_txid_value,
                    &store.lookup_message,
                    &store.lookup_found_payment_id,
                    |payment_id| format!("/dashboard/connections/{}/orders/{}", store.connection_id, payment_id),
                ))
                @if store.recent_orders.is_empty() {
                    p class="muted" { "No orders yet." }
                } @else {
                    table {
                        thead { tr { th { "Payment ID" } th { "Status" } th { "Amount" } th { "Created" } } }
                        tbody {
                            @for order in &store.recent_orders {
                                tr {
                                    td {
                                        a href=(format!("/dashboard/connections/{}/orders/{}", store.connection_id, order.payment_id)) {
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
                p {
                    a href=(format!("/dashboard/connections/{}/orders", store.connection_id)) { "all orders →" }
                }
            } @else {
                h1 { "Store not found" }
                p { "This store doesn't exist, or isn't connected to your account." }
            }
        }
    };
    layout(chrome, "Store - Monokulo", body)
}

pub fn woocommerce_instructions_page(chrome: &PageChrome) -> Markup {
    let body = html! {
        div class="wrap" {
            h1 { "Connect a WooCommerce store" }
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
        PageChrome::from_user(None, "/dashboard/connections/conn_1")
    }

    fn base_store(is_woocommerce: bool) -> StoreDetailData {
        StoreDetailData {
            connection_id: "conn_1".to_string(),
            display_name: "shop.example.com".to_string(),
            platform: if is_woocommerce { "woocommerce".to_string() } else { "custom".to_string() },
            site_url: "https://shop.example.com".to_string(),
            public_key: "pk_abc123".to_string(),
            endpoint: "http://127.0.0.1:8080".to_string(),
            health: "ok".to_string(),
            health_label: "healthy".to_string(),
            created_at: 1000,
            recent_orders: vec![],
            is_woocommerce,
            lookup_txid_value: String::new(),
            lookup_message: None,
            lookup_found_payment_id: None,
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
        // store's own public_key/endpoint, not some stale or empty value -
        // the exact same fragment the post-connect success page uses
        // (`views::connect`'s own tests), so the two can never drift on
        // what "integrate this store" means.
        assert!(html.contains("pk_abc123"));
        assert!(html.contains("http://127.0.0.1:8080"));
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
    fn links_to_the_settings_page_and_widget_pages() {
        let html = page(&chrome(), &StoreDetailViewModel { store: Some(base_store(false)) }).into_string();
        assert!(html.contains(r#"href="/dashboard/connections/conn_1/settings""#), "expected a Settings link, got: {html}");
        assert!(html.contains(r#"href="/dashboard/connections/conn_1/pos""#));
        assert!(html.contains(r#"href="/dashboard/connections/conn_1/orders/new""#));
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
