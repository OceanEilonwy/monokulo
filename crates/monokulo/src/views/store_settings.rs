//! `GET /dashboard/stores/{id}/settings` -
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

/// One verified-embed domain row (`http::embed_domains::domain_views`).
pub struct EmbedDomainView {
    pub id: String,
    pub domain: String,
    /// `tag-*` class suffix: `ok`, `error` or `unknown`.
    pub state_tag: &'static str,
    pub state_label: &'static str,
    /// A line explaining a failing or lapsed domain's grace period.
    pub detail: Option<String>,
    /// The TXT record to publish - shown until the domain is verified, and
    /// again while it's failing.
    pub show_record: bool,
    pub record_name: String,
    pub record_value: String,
    pub last_checked: String,
    /// The latest failed check's reason; `None` once verified.
    pub last_error: Option<String>,
}

pub struct StoreSettingsData {
    pub connection_id: String,
    pub display_name: String,
    /// The tenant's current confirmation threshold - `10` when the engine is
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
    /// Whether the Default (fallback) row's own "accept unconfirmed
    /// payments" checkbox is currently on - no amount to enter: the engine
    /// enforces the same tier boundaries every other threshold already does,
    /// so this is implicit in whether a merchant has set a custom threshold
    /// above whatever amount they don't want 0-conf applied to.
    pub zero_conf_enabled: bool,
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
    pub embed_domains: Vec<EmbedDomainView>,
    /// "Only my verified domains can show this checkout" is on.
    pub embed_restricted: bool,
    /// At least one domain counts as verified, so it can be turned on.
    pub embed_can_restrict: bool,
}

pub struct StoreSettingsViewModel {
    pub store: Option<StoreSettingsData>,
}

pub fn page(chrome: &PageChrome, data: &StoreSettingsViewModel) -> Markup {
    let body = html! {
        div class="wrap" {
            @if let Some(store) = &data.store {
                (super::store_breadcrumb(&store.connection_id, &store.display_name, false))
                h1 { "Settings" }

                @if let Some(error) = &store.settings_error {
                    div class="error" { (error) }
                }

                h2 { "Base currency" }
                form method="post" action=(format!("/dashboard/stores/{}/settings/base-currency", store.connection_id)) {
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
                form id="default-confirmations" method="post" action=(format!("/dashboard/stores/{}/settings/confirmations", store.connection_id)) {
                    input type="hidden" name="zero_conf_checkbox_present" value="true";
                }
                form method="post" action=(format!("/dashboard/stores/{}/settings/confirmation-thresholds/save", store.connection_id)) {
                    table class="thresholds-table" {
                        thead { tr { th { "Amount (" (store.base_currency) ")" } th { "Confirmations required" } th { "Action" } } }
                        tbody {
                            tr {
                                td class="muted" { "Default (fallback)" }
                                td {
                                    input type="text" class="confirmations-input" name="confirmations_required"
                                        value=(store.confirmations_required) size="3" maxlength="3" required form="default-confirmations";
                                    label class="zero-conf-toggle" {
                                        input type="checkbox" name="zero_conf_enabled" checked[store.zero_conf_enabled] form="default-confirmations";
                                        " Accept unconfirmed (0-conf) payments"
                                    }
                                    span class="help-icon" tabindex="0" title="An order that falls under this default tier can read as paid the moment its transaction reaches this store's node's mempool, before any block confirms it - useful for fast, low-value, in-person sales. This is a real double-spend risk (an attacker who can out-race the transaction to a miner keeps both the goods and the coin). No amount to set here - it's implicit in not creating a custom threshold above whatever you don't want treated this way; add one below to keep larger orders requiring real confirmations." { "?" }
                                }
                                td { button type="submit" form="default-confirmations" { "Save" } }
                            }
                            @for threshold in &store.confirmation_thresholds {
                                tr {
                                    td { (threshold.unit_amount) }
                                    td { (threshold.confirmations_required) }
                                    td {
                                        label { input type="checkbox" name=(format!("delete_{}", threshold.id)); " delete" }
                                        button type="submit" { "Save" }
                                    }
                                }
                            }
                            tr class="new-threshold-row" {
                                @if store.confirmation_thresholds_at_max {
                                    td colspan="2" class="muted" { "Maximum of 5 custom thresholds reached - delete one to add another." }
                                } @else {
                                    td { input type="text" name="new_unit_amount" placeholder=(format!("Minimum Amount ({})", store.base_currency)); }
                                    td {
                                        input type="text" class="confirmations-input" name="new_confirmations_required"
                                            size="3" maxlength="3" placeholder="# Confirmations";
                                    }
                                }
                                td {
                                    @if !store.confirmation_thresholds_at_max {
                                        button type="submit" { "Add" }
                                    }
                                }
                            }
                        }
                    }
                    span class="field-help" {
                        "\"Default (fallback)\" applies whenever an order's amount doesn't fall under any custom "
                        "threshold above it (or there are none) - it always exists and can't be deleted. Each custom threshold makes a "
                        "higher-value order (by amount in this store's base currency) require more confirmations, or a lower-value one "
                        "fewer."
                    }
                }

                h2 { "Exchange rate provider" }
                @if store.fx_provider_options.is_empty() {
                    p { strong { "Exchange rate provider:" } " " span class="muted" { "none enabled on this instance" } }
                    p class="hint" {
                        "Only XMR-denominated orders can be created until an admin of this Monokulo instance "
                        "enables a provider (e.g. Coingecko)."
                    }
                } @else {
                    form method="post" action=(format!("/dashboard/stores/{}/settings/fx-provider", store.connection_id)) {
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

                (verified_domains(store))

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
                                        action=(format!("/dashboard/stores/{}/settings/webhooks/{}/delete", store.connection_id, webhook.webhook_id))
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
                    form method="post" action=(format!("/dashboard/stores/{}/settings/webhooks", store.connection_id)) {
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
    let title = match &data.store {
        Some(store) => format!("Settings - {} - Monokulo", store.display_name),
        None => "Store not found - Monokulo".to_string(),
    };
    layout(chrome, &title, body)
}

/// Adding, checking and removing the domains this store has proved it owns
/// (`crate::embed_domains`).
fn verified_domains(store: &StoreSettingsData) -> Markup {
    html! {
        h2 id="verified-domains" { "Verified domains" }
        p class="hint" {
            "Prove you own the websites that show this store's checkout. Add a domain, publish the TXT record shown here in "
            "that domain's DNS settings, then check it. A verified domain covers all of its subdomains. Onion addresses can't "
            "be verified, because they have no DNS."
        }
        form class="embed-restriction" method="post" action=(format!("/dashboard/stores/{}/settings/embed-restriction", store.connection_id)) {
            @if store.embed_restricted {
                p {
                    span class="tag tag-ok" { "On" } " "
                    strong { "Only my verified domains can show this checkout." }
                    " Browsers won't show it on any other website, and orders from other websites are refused. Pages on "
                    "this server (the POS, the payment link) always work."
                }
                input type="hidden" name="restricted" value="off";
                button type="submit" class="btn-secondary" { "Turn off" }
            } @else {
                p {
                    span class="tag tag-unknown" { "Off" } " "
                    strong { "Any website can show this checkout." }
                    " Turn this on to allow only the verified domains below, and their subdomains."
                }
                input type="hidden" name="restricted" value="on";
                @if store.embed_can_restrict {
                    button type="submit" { "Only allow my verified domains" }
                } @else {
                    button type="submit" disabled { "Only allow my verified domains" }
                    span class="field-help" { "Verify a domain first." }
                }
            }
        }
        @if store.embed_domains.is_empty() {
            p class="muted" { "No domains yet." }
        } @else {
            table class="domains-table" {
                thead { tr { th { "Domain" } th { "Status" } th { "Last checked" } th {} } }
                tbody {
                    @for domain in &store.embed_domains {
                        tr {
                            td {
                                strong { (domain.domain) }
                                @if let Some(detail) = &domain.detail { div class="hint" { (detail) } }
                                @if let Some(error) = &domain.last_error { div class="hint" { (error) } }
                                @if domain.show_record {
                                    dl class="dns-record" {
                                        dt { "Type" } dd { code { "TXT" } }
                                        dt { "Name" } dd { code { (domain.record_name) } }
                                        dt { "Value" } dd { code { (domain.record_value) } }
                                    }
                                }
                            }
                            td { span class=(format!("tag tag-{}", domain.state_tag)) { (domain.state_label) } }
                            td { (domain.last_checked) }
                            td class="domain-actions" {
                                form method="post" action=(format!("/dashboard/stores/{}/settings/domains/{}/check", store.connection_id, domain.id)) {
                                    button type="submit" { "Check now" }
                                }
                                form method="post" action=(format!("/dashboard/stores/{}/settings/domains/{}/delete", store.connection_id, domain.id))
                                    onsubmit="return confirm('Remove this domain? You would need a new DNS record to verify it again.');" {
                                    button type="submit" class="btn-secondary" { "Remove" }
                                }
                            }
                        }
                    }
                }
            }
        }
        form method="post" action=(format!("/dashboard/stores/{}/settings/domains", store.connection_id)) {
            label {
                "Domain"
                input type="text" name="domain" placeholder="shop.example" required autocomplete="off" spellcheck="false";
                span class="field-help" { "Just the domain, like shop.example. Its subdomains are covered too." }
            }
            button type="submit" { "Add domain" }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chrome() -> PageChrome {
        PageChrome::from_user(None, "/dashboard/stores/conn_1/settings")
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
            zero_conf_enabled: false,
            webhooks: vec![],
            created_webhook_signing_secret: None,
            settings_error: None,
            embed_domains: vec![],
            embed_restricted: false,
            embed_can_restrict: false,
        }
    }

    #[test]
    fn the_restriction_switch_needs_a_verified_domain_to_turn_on() {
        let html = page(&chrome(), &StoreSettingsViewModel { store: Some(base_store()) }).into_string();
        assert!(html.contains(r#"<input type="hidden" name="restricted" value="on"><button type="submit" disabled>Only allow my verified domains</button>"#), "got: {html}");

        let store = StoreSettingsData { embed_can_restrict: true, ..base_store() };
        let html = page(&chrome(), &StoreSettingsViewModel { store: Some(store) }).into_string();
        assert!(html.contains(r#"<button type="submit">Only allow my verified domains</button>"#));

        let store = StoreSettingsData { embed_can_restrict: true, embed_restricted: true, ..base_store() };
        let html = page(&chrome(), &StoreSettingsViewModel { store: Some(store) }).into_string();
        assert!(html.contains("Only my verified domains can show this checkout."));
        assert!(html.contains(r#"<input type="hidden" name="restricted" value="off">"#));
    }

    #[test]
    fn verified_domains_show_the_record_to_publish_until_verified() {
        let html = page(&chrome(), &StoreSettingsViewModel { store: Some(base_store()) }).into_string();
        assert!(html.contains(r#"<h2 id="verified-domains">Verified domains</h2>"#));
        assert!(html.contains("No domains yet."));
        assert!(html.contains(r#"action="/dashboard/stores/conn_1/settings/domains""#));

        let domain = |id: &str, name: &str, state_label: &'static str, show_record: bool| EmbedDomainView {
            id: id.to_string(),
            domain: name.to_string(),
            state_tag: "unknown",
            state_label,
            detail: None,
            show_record,
            record_name: format!("_monokulo.{name}"),
            record_value: format!("monokulo-verify=token-{id}"),
            last_checked: "never".to_string(),
            last_error: None,
        };
        let store = StoreSettingsData {
            embed_domains: vec![domain("d1", "shop.example", "Verified", false), domain("d2", "new.example", "Waiting for DNS", true)],
            ..base_store()
        };
        let html = page(&chrome(), &StoreSettingsViewModel { store: Some(store) }).into_string();
        assert!(!html.contains("monokulo-verify=token-d1"), "a verified domain doesn't need its record shown");
        assert!(html.contains("<code>_monokulo.new.example</code>"));
        assert!(html.contains("<code>monokulo-verify=token-d2</code>"));
        assert!(html.contains(r#"action="/dashboard/stores/conn_1/settings/domains/d2/check""#));
        assert!(html.contains(r#"action="/dashboard/stores/conn_1/settings/domains/d1/delete""#));
    }

    #[test]
    fn renders_not_found_state_when_store_is_none() {
        let html = page(&chrome(), &StoreSettingsViewModel { store: None }).into_string();
        assert!(html.to_lowercase().contains("not found"));
    }

    #[test]
    fn shows_store_context_above_the_settings_heading() {
        let html = page(&chrome(), &StoreSettingsViewModel { store: Some(base_store()) }).into_string();
        assert!(
            html.contains(r#"<nav class="context-nav" aria-label="Breadcrumb"><a href="/dashboard/stores/conn_1" title="shop.example.com">shop.example.com</a></nav><h1>Settings</h1>"#),
            "expected a store link above the Settings heading, got: {html}"
        );
    }

    #[test]
    fn the_default_threshold_row_has_no_delete_control() {
        let html = page(&chrome(), &StoreSettingsViewModel { store: Some(base_store()) }).into_string();
        assert!(html.contains("Default (fallback)"));
        assert!(html.contains(r#"value="10""#));
    }

    #[test]
    fn default_and_custom_confirmation_controls_submit_to_separate_forms() {
        let html = page(&chrome(), &StoreSettingsViewModel { store: Some(base_store()) }).into_string();
        assert!(html.contains(r#"<form id="default-confirmations" method="post" action="/dashboard/stores/conn_1/settings/confirmations">"#));
        assert!(html.contains(r#"name="zero_conf_checkbox_present" value="true""#));
        assert!(html.contains(r#"maxlength="3" required form="default-confirmations""#));
        assert!(html.contains(r#"<button type="submit" form="default-confirmations">Save</button>"#));
        assert!(html.contains(r#"<form method="post" action="/dashboard/stores/conn_1/settings/confirmation-thresholds/save">"#));
        assert!(html.contains(r#"<button type="submit">Add</button>"#));
    }

    #[test]
    fn zero_conf_checkbox_is_unchecked_when_disabled() {
        let html = page(&chrome(), &StoreSettingsViewModel { store: Some(base_store()) }).into_string();
        assert!(
            html.contains(r#"<input type="checkbox" name="zero_conf_enabled" form="default-confirmations">"#),
            "expected the 0-conf checkbox unchecked when no ceiling is set, got: {html}"
        );
    }

    #[test]
    fn zero_conf_checkbox_is_checked_when_enabled() {
        let store = StoreSettingsData { zero_conf_enabled: true, ..base_store() };
        let html = page(&chrome(), &StoreSettingsViewModel { store: Some(store) }).into_string();
        assert!(html.contains(r#"<input type="checkbox" name="zero_conf_enabled" checked form="default-confirmations">"#), "got: {html}");
    }

    #[test]
    fn confirmations_required_input_is_narrow_and_capped_at_three_digits() {
        let html = page(&chrome(), &StoreSettingsViewModel { store: Some(base_store()) }).into_string();
        assert!(html.contains(r#"class="confirmations-input" name="confirmations_required" value="10" size="3" maxlength="3""#), "got: {html}");
    }

    #[test]
    fn add_row_is_part_of_the_table_and_uses_descriptive_placeholders() {
        let html = page(&chrome(), &StoreSettingsViewModel { store: Some(base_store()) }).into_string();
        assert!(html.contains("<th>Action</th>"), "got: {html}");
        assert!(!html.contains("<th>Delete</th>"));
        assert!(!html.contains("threshold-gap-row"));
        assert!(html.contains(r#"<tr class="new-threshold-row"><td><input type="text" name="new_unit_amount" placeholder="Minimum Amount (XMR)">"#), "got: {html}");
        assert!(html.contains(r##"placeholder="# Confirmations""##), "got: {html}");
        assert!(html.contains(r#"<button type="submit">Add</button>"#));
    }

    #[test]
    fn existing_threshold_rows_have_save_buttons_even_at_the_limit() {
        let store = StoreSettingsData {
            confirmation_thresholds: vec![ConfirmationThresholdView { id: "threshold_1".to_string(), unit_amount: "50.00".to_string(), confirmations_required: 20 }],
            confirmation_thresholds_at_max: true,
            ..base_store()
        };
        let html = page(&chrome(), &StoreSettingsViewModel { store: Some(store) }).into_string();
        assert!(html.contains("Maximum of 5 custom thresholds reached"));
        assert!(html.contains(r#"name="delete_threshold_1""#), "got: {html}");
        assert!(html.contains(r#"<button type="submit">Save</button>"#), "existing thresholds need a Save button, got: {html}");
        assert!(!html.contains(r#"<button type="submit">Add</button>"#));
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
        assert!(html.contains(r#"action="/dashboard/stores/conn_1/settings/webhooks/wh_1/delete""#));
    }

    #[test]
    fn shows_the_webhook_signing_secret_exactly_once_after_creation() {
        let store = StoreSettingsData { created_webhook_signing_secret: Some("whsec_abc123".to_string()), ..base_store() };
        let html = page(&chrome(), &StoreSettingsViewModel { store: Some(store) }).into_string();
        assert!(html.contains("whsec_abc123"));
        assert!(html.contains("Webhook created"));
    }
}
