//! `GET /dashboard/connections/{id}` - `http::orders::store_detail`/
//! `render_store_detail_page`.

use maud::{html, Markup};

use super::orders::OrderRowViewModel;
use super::{layout, PageChrome};

/// One row of the FX-provider settings dropdown.
pub struct FxProviderOption {
    pub name: String,
    pub selected: bool,
}

/// One custom confirmation threshold.
pub struct ConfirmationThresholdView {
    pub id: String,
    pub unit_amount: String,
    pub confirmations_required: u64,
}

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
    /// Set only when the "create an order" form on this page was just
    /// rejected - the engine's own validation error, surfaced verbatim.
    /// `None` on a plain page load.
    pub order_creation_error: Option<String>,
    /// Every currency the "create an order" form's dropdown can offer right
    /// now - always includes `"XMR"` first.
    pub order_currency_options: Vec<String>,
    /// `true` exactly when `order_currency_options` is `["XMR"]` alone (no
    /// fiat provider enabled/configured for this store) - shows a plain
    /// `readonly` "XMR" field instead of a one-option dropdown.
    pub order_currency_is_locked_to_xmr: bool,
    /// The tenant's current confirmation threshold - `0` when the engine is
    /// currently unreachable.
    pub confirmations_required: u64,
    pub fx_provider: String,
    pub fx_provider_options: Vec<FxProviderOption>,
    pub base_currency: String,
    pub base_currency_options: Vec<crate::currencies::CurrencyOptionView>,
    pub confirmation_thresholds: Vec<ConfirmationThresholdView>,
    /// `true` once this store already has 5 custom thresholds - the
    /// add-threshold form hides itself rather than accepting a submission
    /// the server would just reject anyway.
    pub confirmation_thresholds_at_max: bool,
    /// This store's current zero-conf ceiling, formatted as an XMR decimal
    /// string - empty when disabled.
    pub zero_conf_max_xmr: String,
    /// Set only when the "update settings" form on this page was just
    /// rejected - shared by every settings sub-form on this page, since
    /// only one can ever be submitted at a time. `None` on a plain load.
    pub settings_error: Option<String>,
}

pub struct StoreDetailViewModel {
    pub store: Option<StoreDetailData>,
}

pub fn page(chrome: &PageChrome, data: &StoreDetailViewModel) -> Markup {
    let body = html! {
        div class="wrap" {
            @if let Some(store) = &data.store {
                h1 { (store.display_name) }

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

                table {
                    tr { th { "Public key" } td { code { (store.public_key) } } }
                    tr { th { "Engine endpoint" } td { code { (store.endpoint) } } }
                    tr { th { "Connected" } td { (store.created_at) } }
                }

                h2 { "Recent orders" }
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
                    "\u{a0}·\u{a0}"
                    a href=(format!("/dashboard/connections/{}/webhooks", store.connection_id)) { "webhooks →" }
                    "\u{a0}·\u{a0}"
                    a href=(format!("/dashboard/connections/{}/pos", store.connection_id)) { "POS terminal →" }
                }

                h2 { "Create an order" }
                p class="hint" { "Creates a real order on the engine and takes you straight to its payment page." }
                @if let Some(error) = &store.order_creation_error {
                    div class="error" { (error) }
                }
                form method="post" action=(format!("/dashboard/connections/{}/orders/new", store.connection_id)) {
                    label for="amount" { "Amount" }
                    input type="text" id="amount" name="amount" value="10.00" required;
                    label for="currency" { "Currency" }
                    @if store.order_currency_is_locked_to_xmr {
                        input type="text" id="currency" name="currency" value="XMR" readonly;
                        span class="field-help" {
                            "\"XMR\" - the only currency this instance can price an order in right now. Enable an "
                            "exchange rate provider (below) to offer others."
                        }
                    } @else {
                        select id="currency" name="currency" {
                            @for currency in &store.order_currency_options {
                                option value=(currency) { (currency) }
                            }
                        }
                        span class="field-help" {
                            "\"XMR\" always works, with no provider needed. Any other currency here comes from this "
                            "store's own exchange rate provider (below)."
                        }
                    }
                    label for="merchant_order_id" { "Merchant order ID " span class="field-help" style="display:inline" { "(optional)" } }
                    input type="text" id="merchant_order_id" name="merchant_order_id";
                    span class="field-help" {
                        "Your own order/cart id, if you have one - shown on this order's detail page so you can "
                        "match it back to your own records."
                    }
                    button type="submit" { "Create order" }
                }

                h2 { "Settings" }
                @if let Some(error) = &store.settings_error {
                    div class="error" { (error) }
                }

                h2 { "Confirmation Thresholds" }
                p class="hint" {
                    "How many blocks a payment needs before this store's orders read as paid. The default "
                    "below is the fallback used whenever no custom threshold applies; custom thresholds let a higher-value "
                    "order require more confirmations (or a lower-value one fewer) based on its amount in this store's base "
                    "currency."
                }

                h3 { "Base currency" }
                form method="post" action=(format!("/dashboard/connections/{}/settings/base-currency", store.connection_id)) {
                    label {
                        "Base currency"
                        select name="base_currency" {
                            @for opt in &store.base_currency_options {
                                option value=(opt.code) selected[opt.selected] { (opt.description) " (" (opt.code) ")" }
                            }
                        }
                        span class="field-help" {
                            "What custom threshold amounts below are denominated in. Changing this deletes every "
                            "custom threshold this store currently has - an amount in a currency you're no longer using means nothing."
                        }
                    }
                    button type="submit" { "Update" }
                }

                form method="post" action=(format!("/dashboard/connections/{}/settings/confirmation-thresholds/save", store.connection_id)) {
                    table {
                        thead { tr { th { "Amount (" (store.base_currency) ")" } th { "Confirmations required" } th { "Delete" } } }
                        tbody {
                            tr {
                                td class="muted" { "Default (fallback)" }
                                td { input type="text" name="confirmations_required" value=(store.confirmations_required) required; }
                                td { "-" }
                            }
                            @for threshold in &store.confirmation_thresholds {
                                tr {
                                    td { (threshold.unit_amount) }
                                    td { (threshold.confirmations_required) }
                                    td { label { input type="checkbox" name=(format!("delete_{}", threshold.id)); " delete" } }
                                }
                            }
                            @if !store.confirmation_thresholds_at_max {
                                tr {
                                    td { input type="text" name="new_unit_amount" placeholder="50.00"; }
                                    td { input type="text" name="new_confirmations_required" placeholder="20"; }
                                    td class="muted" { "new" }
                                }
                            }
                        }
                    }
                    span class="field-help" {
                        "\"Default (fallback)\" applies whenever an order's amount doesn't fall under any custom "
                        "threshold above it (or there are none) - it always exists and can't be deleted. Each custom threshold makes a "
                        "higher-value order (by amount in this store's base currency) require more confirmations, or a lower-value one "
                        "fewer."
                        @if store.confirmation_thresholds_at_max {
                            " Maximum of 5 custom thresholds reached - delete one to add "
                            "another."
                        }
                    }
                    button type="submit" { "Save" }
                }

                h3 { "Zero-confirmation payments" }
                form method="post" action=(format!("/dashboard/connections/{}/settings/zero-conf", store.connection_id)) {
                    label {
                        "Accept unconfirmed payments up to (XMR)"
                        input type="text" name="zero_conf_max_xmr" value=(store.zero_conf_max_xmr) placeholder="0.00 (disabled)";
                        span class="field-help" {
                            "An order at or under this XMR amount can read as paid the moment its transaction "
                            "reaches this store's node's mempool, before any block confirms it - useful for fast, low-value, in-person "
                            "sales. This is a real double-spend risk for exactly that amount of XMR (an attacker who can out-race the "
                            "transaction to a miner keeps both the goods and the coin) - keep this at the smallest amount you're actually "
                            "willing to lose. Leave blank to require the confirmations above for every order, with no exception."
                        }
                    }
                    button type="submit" { "Update" }
                }

                @if store.fx_provider_options.is_empty() {
                    p { strong { "Exchange rate provider:" } " " span class="muted" { "none enabled on this instance" } }
                    p class="hint" {
                        "Only XMR-denominated orders can be created until an admin of this Monokulo instance "
                        "enables a provider (e.g. Coingecko)."
                    }
                } @else {
                    form method="post" action=(format!("/dashboard/connections/{}/settings/fx-provider", store.connection_id)) {
                        label {
                            "Exchange rate provider"
                            select name="fx_provider" {
                                @for opt in &store.fx_provider_options {
                                    option value=(opt.name) selected[opt.selected] { (opt.name) }
                                }
                            }
                            span class="field-help" {
                                "Where this store's orders get their live market rate from, for any currency other than "
                                "XMR (which always works, needing no provider at all - see \"create an order\" above). \"coingecko\" looks up a "
                                "live market rate (cached briefly before the next lookup refreshes it)."
                            }
                        }
                        button type="submit" { "Update" }
                    }
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
            order_creation_error: None,
            confirmations_required: 10,
            order_currency_options: vec!["XMR".to_string(), "USD".to_string()],
            order_currency_is_locked_to_xmr: false,
            fx_provider: "coingecko".to_string(),
            fx_provider_options: vec![FxProviderOption { name: "coingecko".to_string(), selected: true }],
            base_currency: "XMR".to_string(),
            base_currency_options: vec![],
            confirmation_thresholds: vec![],
            confirmation_thresholds_at_max: false,
            zero_conf_max_xmr: String::new(),
            settings_error: None,
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
    fn shows_the_create_order_error_when_present() {
        let store =
            StoreDetailData { order_creation_error: Some("unsupported currency: XYZ".to_string()), ..base_store(false) };
        let html = page(&chrome(), &StoreDetailViewModel { store: Some(store) }).into_string();
        assert!(html.contains("unsupported currency: XYZ"), "expected the real error surfaced, got: {html}");
        assert!(html.contains("<form"), "the create-order form must still be present on error");
    }

    #[test]
    fn woocommerce_instructions_page_renders() {
        let html = woocommerce_instructions_page(&chrome()).into_string();
        assert!(html.to_lowercase().contains("woocommerce"));
        assert!(html.contains(r#"href="/dashboard/connect""#));
    }
}
