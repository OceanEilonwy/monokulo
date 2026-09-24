//! `GET /dashboard/connections/{id}/settings` -
//! `http::orders::store_settings`/`render_store_settings_page`.
//!
//! Everything about a store that isn't day-to-day order activity: currency
//! and confirmation-threshold config, the exchange rate provider, and
//! webhooks - split out of the store overview page (which used to carry all
//! of this inline, making it the busiest page in the dashboard) into one
//! dedicated settings page, reached via the "Settings" link next to that
//! page's own "help" disclosure.

use maud::{html, Markup};

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

/// One row of the webhooks table - mirrors the engine's own `WebhookView`
/// field-for-field. Formerly `views::webhooks::WebhookRowViewModel`, now
/// that webhooks live on this page rather than their own.
pub struct WebhookRowViewModel {
    pub webhook_id: String,
    pub url: String,
    pub enabled: bool,
    pub created_at: i64,
}

pub struct StoreSettingsData {
    pub connection_id: String,
    pub display_name: String,
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
    /// string - empty when disabled. Doubles as the "0-conf enabled"
    /// checkbox's own checked state (non-empty = enabled) on the Default
    /// (fallback) row - see [`page`]'s own rendering of that row.
    pub zero_conf_max_xmr: String,
    pub webhooks: Vec<WebhookRowViewModel>,
    /// Set only immediately after a successful webhook creation - the
    /// engine hands back a real signing secret exactly once, at creation
    /// time, with no way to ever fetch it again after this moment. `None`
    /// on a plain `GET`, and gone again the moment the page is reloaded.
    pub created_webhook_signing_secret: Option<String>,
    /// Set only when a form on this page was just rejected - shared by
    /// every sub-form here, since only one can ever be submitted at a time.
    /// `None` on a plain load.
    pub settings_error: Option<String>,
}

pub struct StoreSettingsViewModel {
    pub store: Option<StoreSettingsData>,
}

pub fn page(chrome: &PageChrome, data: &StoreSettingsViewModel) -> Markup {
    let body = html! {
        div class="wrap" {
            @if let Some(store) = &data.store {
                p { a href=(format!("/dashboard/connections/{}", store.connection_id)) { "← back to store" } }
                h1 { "Settings" " " span class="muted" { (store.display_name) } }

                @if let Some(error) = &store.settings_error {
                    div class="error" { (error) }
                }

                h2 { "Base currency" }
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

                h2 { "Confirmation thresholds" }
                p class="hint" {
                    "How many blocks a payment needs before this store's orders read as paid. The default "
                    "below is the fallback used whenever no custom threshold applies; custom thresholds let a higher-value "
                    "order require more confirmations (or a lower-value one fewer) based on its amount in this store's base "
                    "currency."
                }
                form method="post" action=(format!("/dashboard/connections/{}/settings/confirmation-thresholds/save", store.connection_id)) {
                    table {
                        thead { tr { th { "Amount (" (store.base_currency) ")" } th { "Confirmations required" } th { "Delete" } } }
                        tbody {
                            tr {
                                td class="muted" { "Default (fallback)" }
                                td {
                                    input type="text" name="confirmations_required" value=(store.confirmations_required) required;
                                    label class="zero-conf-toggle" {
                                        input type="checkbox" name="zero_conf_enabled" checked[!store.zero_conf_max_xmr.is_empty()];
                                        " Also accept unconfirmed (0-conf) payments up to "
                                        input type="text" name="zero_conf_max_xmr" value=(store.zero_conf_max_xmr) placeholder="0.00" size="6";
                                        " XMR"
                                        span class="help-icon" tabindex="0" title="An order at or under this XMR amount can read as paid the moment its transaction reaches this store's node's mempool, before any block confirms it - useful for fast, low-value, in-person sales. This is a real double-spend risk for exactly that amount of XMR (an attacker who can out-race the transaction to a miner keeps both the goods and the coin) - keep this at the smallest amount you're actually willing to lose." { "?" }
                                    }
                                }
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

                h2 { "Exchange rate provider" }
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
                                "XMR (which always works, needing no provider at all). \"coingecko\" looks up a "
                                "live market rate (cached briefly before the next lookup refreshes it)."
                            }
                        }
                        button type="submit" { "Update" }
                    }
                }

                h2 { "Webhooks" }
                @if let Some(secret) = &store.created_webhook_signing_secret {
                    div class="box" {
                        h3 { "Webhook created" }
                        p {
                            "Its signing secret (verify the " code { "X-Monokulo-Signature" } " header with this - shown once, right now, and never again):"
                        }
                        pre { (secret) }
                        p class="hint" { "Store it somewhere safe before leaving this page. If you lose it, delete this webhook and create a new one." }
                    }
                }
                table {
                    thead { tr { th { "URL" } th { "Enabled" } th { "Created" } th {} } }
                    tbody {
                        @for webhook in &store.webhooks {
                            tr {
                                td { (webhook.url) }
                                td {
                                    @if webhook.enabled {
                                        span class="tag tag-ok" { "enabled" }
                                    } @else {
                                        span class="tag tag-unknown" { "disabled" }
                                    }
                                }
                                td { (webhook.created_at) }
                                td {
                                    form method="post"
                                        action=(format!("/dashboard/connections/{}/settings/webhooks/{}/delete", store.connection_id, webhook.webhook_id))
                                        onsubmit="return confirm('Delete this webhook? Anything relying on it will stop receiving events immediately.');" {
                                        button type="submit" class="btn-secondary" { "Delete" }
                                    }
                                }
                            }
                        }
                    }
                }
                @if store.webhooks.is_empty() {
                    p class="muted" { "No webhooks yet." }
                }
                div class="box" {
                    h3 { "Add a webhook" }
                    form method="post" action=(format!("/dashboard/connections/{}/settings/webhooks", store.connection_id)) {
                        label {
                            "URL"
                            input type="url" name="url" placeholder="https://your-endpoint.example.com/monokulo-webhook" required;
                            span class="field-help" {
                                "A plain " code { "http(s)://" } " URL your endpoint controls. Private/loopback addresses are "
                                "checked at delivery time, not registration - registering one won't error here, but nothing will ever actually be "
                                "delivered to it."
                            }
                        }
                        label {
                            "Custom headers (optional)"
                            textarea name="extra_headers" rows="3" placeholder="X-Api-Key: your-value\nAnother-Header: another-value" {}
                            span class="field-help" {
                                "One " code { "Header-Name: value" } " pair per line - sent with every delivery to this "
                                "webhook, alongside the signature headers Monokulo always includes."
                            }
                        }
                        button type="submit" { "Add webhook" }
                    }
                }
            } @else {
                h1 { "Store not found" }
                p { "This store doesn't exist, or isn't connected to your account." }
            }
        }
    };
    layout(chrome, "Settings - Monokulo", body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chrome() -> PageChrome {
        PageChrome::from_user(None, "/dashboard/connections/conn_1/settings")
    }

    fn base_store() -> StoreSettingsData {
        StoreSettingsData {
            connection_id: "conn_1".to_string(),
            display_name: "shop.example.com".to_string(),
            confirmations_required: 10,
            fx_provider: "coingecko".to_string(),
            fx_provider_options: vec![FxProviderOption { name: "coingecko".to_string(), selected: true }],
            base_currency: "XMR".to_string(),
            base_currency_options: vec![],
            confirmation_thresholds: vec![],
            confirmation_thresholds_at_max: false,
            zero_conf_max_xmr: String::new(),
            webhooks: vec![],
            created_webhook_signing_secret: None,
            settings_error: None,
        }
    }

    #[test]
    fn renders_not_found_state_when_store_is_none() {
        let html = page(&chrome(), &StoreSettingsViewModel { store: None }).into_string();
        assert!(html.to_lowercase().contains("not found"));
    }

    #[test]
    fn the_default_threshold_row_has_no_delete_control() {
        let html = page(&chrome(), &StoreSettingsViewModel { store: Some(base_store()) }).into_string();
        assert!(html.contains("Default (fallback)"));
        assert!(html.contains(r#"value="10""#));
    }

    #[test]
    fn zero_conf_checkbox_is_unchecked_and_amount_blank_when_disabled() {
        let html = page(&chrome(), &StoreSettingsViewModel { store: Some(base_store()) }).into_string();
        assert!(
            html.contains(r#"<input type="checkbox" name="zero_conf_enabled">"#),
            "expected the 0-conf checkbox unchecked when no ceiling is set, got: {html}"
        );
    }

    #[test]
    fn zero_conf_checkbox_is_checked_and_amount_shown_when_enabled() {
        let store = StoreSettingsData { zero_conf_max_xmr: "0.05".to_string(), ..base_store() };
        let html = page(&chrome(), &StoreSettingsViewModel { store: Some(store) }).into_string();
        assert!(html.contains("checked"));
        assert!(html.contains(r#"value="0.05""#));
    }

    #[test]
    fn shows_the_settings_error_when_present() {
        let store = StoreSettingsData { settings_error: Some("Enter a whole number of confirmations.".to_string()), ..base_store() };
        let html = page(&chrome(), &StoreSettingsViewModel { store: Some(store) }).into_string();
        assert!(html.contains("Enter a whole number of confirmations."));
    }

    #[test]
    fn lists_webhooks_with_a_delete_form_each() {
        let store = StoreSettingsData {
            webhooks: vec![WebhookRowViewModel { webhook_id: "wh_1".to_string(), url: "https://example.com/hook".to_string(), enabled: true, created_at: 1000 }],
            ..base_store()
        };
        let html = page(&chrome(), &StoreSettingsViewModel { store: Some(store) }).into_string();
        assert!(html.contains("https://example.com/hook"));
        assert!(html.contains("tag-ok"));
        assert!(html.contains(r#"action="/dashboard/connections/conn_1/settings/webhooks/wh_1/delete""#));
    }

    #[test]
    fn shows_the_webhook_signing_secret_exactly_once_after_creation() {
        let store = StoreSettingsData { created_webhook_signing_secret: Some("whsec_abc123".to_string()), ..base_store() };
        let html = page(&chrome(), &StoreSettingsViewModel { store: Some(store) }).into_string();
        assert!(html.contains("whsec_abc123"));
        assert!(html.contains("Webhook created"));
    }
}
