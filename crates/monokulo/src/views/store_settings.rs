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

use super::{layout_with_head, PageChrome};

/// One row of the FX-provider settings dropdown.
pub struct FxProviderOption {
    pub name: String,
    pub selected: bool,
}

/// Haveno's per-store limits as the settings form shows them (see
/// `crate::fx_provider_settings`).
pub struct HavenoSettingsView {
    /// Comma-separated currency codes; empty means every currency.
    pub currencies: String,
    pub max_spread_pct: String,
    pub min_offers_per_side: String,
    pub min_depth_xmr_per_side: String,
}

impl From<&crate::fx_provider_settings::HavenoSettings> for HavenoSettingsView {
    fn from(settings: &crate::fx_provider_settings::HavenoSettings) -> Self {
        HavenoSettingsView {
            currencies: settings.currencies.join(", "),
            max_spread_pct: settings.max_spread_pct.to_string(),
            min_offers_per_side: settings.min_offers_per_side.to_string(),
            min_depth_xmr_per_side: settings.min_depth_xmr_per_side.to_string(),
        }
    }
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
    /// The zone the page shows times in (the signed-in user's).
    pub clock: super::time::Clock,
    pub connection_id: crate::db::ConnectionId,
    pub display_name: String,
    /// The tenant's current confirmation threshold -
    /// `confirmation_thresholds::FALLBACK_CONFIRMATIONS` when the engine is
    /// currently unreachable.
    pub confirmations_required: u64,
    /// Every provider this instance offers: the store's enabled ones first,
    /// in its order, then the rest.
    pub fx_provider_options: Vec<FxProviderOption>,
    /// `Some` while this instance offers Haveno - its per-store limits.
    pub haveno_settings: Option<HavenoSettingsView>,
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
    /// The engine couldn't be asked for the webhooks: the section says so
    /// rather than showing none, and the rest of the page still works.
    pub webhooks_unavailable: bool,
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
    /// Where the store's keys are kept and where they could move (part 5);
    /// `None` when there's nowhere else to move them.
    pub key_storage: Option<KeyStorageView>,
    /// The section a form was just posted from: its error (or new webhook
    /// secret) shows there. `None` on a plain load.
    pub active_section: Option<StoreSection>,
    /// The store sends client logs (`db::Db::client_logging`).
    pub client_logging: bool,
}

/// The page's sections, each saved (and, with fixi, swapped back) on its
/// own (structured_logging.md part 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreSection {
    BaseCurrency,
    Confirmations,
    FxProvider,
    KeyStorage,
    Domains,
    Webhooks,
    Diagnostics,
}

impl StoreSection {
    pub fn from_id(id: &str) -> Option<Self> {
        [
            Self::BaseCurrency,
            Self::Confirmations,
            Self::FxProvider,
            Self::KeyStorage,
            Self::Domains,
            Self::Webhooks,
            Self::Diagnostics,
        ]
        .into_iter()
        .find(|section| section.id() == id)
    }

    pub fn id(self) -> &'static str {
        match self {
            StoreSection::BaseCurrency => "base-currency",
            StoreSection::Confirmations => "confirmation-thresholds",
            StoreSection::FxProvider => "fx-provider",
            StoreSection::KeyStorage => "key-storage",
            StoreSection::Domains => "verified-domains",
            StoreSection::Webhooks => "webhooks",
            StoreSection::Diagnostics => "diagnostics",
        }
    }

    /// Other sections a save here changes too (sent out of band): a new
    /// base currency deletes the custom thresholds and renames their unit.
    pub fn also_changes(self) -> &'static [StoreSection] {
        match self {
            StoreSection::BaseCurrency => &[StoreSection::Confirmations],
            _ => &[],
        }
    }
}

/// fixi attributes posting `action` and swapping `section` back.
fn fx(action: &str, section: StoreSection) -> (String, String) {
    (action.to_string(), format!("#{}", section.id()))
}

pub struct KeyStorageView {
    /// A description of the backend holding the keys now.
    pub current: String,
    /// The current backend is turned off: the store isn't being scanned.
    pub current_disabled: bool,
    /// The backends the keys could move to (never the current one).
    pub move_to: Vec<super::connect::CustodyChoice>,
    /// Encrypted key entry, when the keys may move to SEV-SNP key storage.
    pub snp_entry: Option<super::key_entry::SnpKeyEntry>,
}

/// The error a form in `section` was just refused with, shown in the
/// section when it's swapped back in place (`in_place`) and focused. (A
/// whole page shows it at the top instead.)
fn section_error(store: &StoreSettingsData, section: StoreSection, in_place: bool) -> Markup {
    html! {
        @if in_place && store.active_section == Some(section) {
            @if let Some(error) = &store.settings_error {
                div class="error" role="alert" data-fx-focus tabindex="-1" { (error) }
            } @else {
                p class="success settings-saved" role="status" data-settings-saved { "Settings saved." }
            }
        }
    }
}

/// "Key storage": where the store's view key is kept, and a form to move it
/// (task 5.6). The keys are entered again - they're never read back from
/// anywhere - and the fields are always empty on render.
fn key_storage_section(
    store: &StoreSettingsData,
    key_storage: &KeyStorageView,
    in_place: bool,
    oob: bool,
) -> Markup {
    let connection_id = &store.connection_id;
    let (action, target) = fx(
        &format!("/dashboard/stores/{connection_id}/settings/key-custody"),
        StoreSection::KeyStorage,
    );
    html! {
      section id=(StoreSection::KeyStorage.id()) data-fx-oob[oob] {
        h2 { "Key storage" }
        (section_error(store, StoreSection::KeyStorage, in_place))
        p { strong { "Kept: " } (key_storage.current) }
        @if key_storage.current_disabled {
            p class="error" {
                "This way of storing keys has been turned off on this instance, so payments to this store aren't "
                "being detected. Move the keys below to start again."
            }
        }
        form method="post" action=(action) fx-action=(action) fx-method="POST" fx-target=(target) {
            label {
                "Move to"
                select name="backend" {
                    @for choice in &key_storage.move_to {
                        option value=(choice.backend) selected[choice.selected] { (choice.label) }
                    }
                }
            }
            (super::key_entry::key_fields(
                "",
                "",
                key_storage.snp_entry.as_ref(),
                html! { "This store's private view key, entered again: keys are never copied between storage backends." },
                html! { "They must be the same wallet this store already uses - it's checked before anything moves." },
            ))
            @if let Some(entry) = &key_storage.snp_entry {
                (super::key_entry::snp_section(entry, Some("backend")))
            }
            button type="submit" { "Move keys" }
        }
      }
    }
}

pub struct StoreSettingsViewModel {
    pub store: Option<StoreSettingsData>,
}

/// "Base currency".
fn base_currency_section(store: &StoreSettingsData, in_place: bool, oob: bool) -> Markup {
    html! {
        section id=(StoreSection::BaseCurrency.id()) data-fx-oob[oob] {
                h2 { "Base currency" }
                (section_error(store, StoreSection::BaseCurrency, in_place))
                form method="post" action=(format!("/dashboard/stores/{}/settings/base-currency", store.connection_id)) fx-action=(format!("/dashboard/stores/{}/settings/base-currency", store.connection_id)) fx-method="POST" fx-target="#base-currency" {
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
        }
    }
}

/// "Confirmation thresholds": the default and the custom ones.
fn confirmations_section(store: &StoreSettingsData, in_place: bool, oob: bool) -> Markup {
    html! {
        section id=(StoreSection::Confirmations.id()) data-fx-oob[oob] {
                h2 { "Confirmation thresholds" }
                (section_error(store, StoreSection::Confirmations, in_place))
                p class="hint" {
                    "How many blocks a payment needs before this store's orders read as paid. The default "
                    "below is the fallback used whenever no custom threshold applies; custom thresholds let a higher-value "
                    "order require more confirmations (or a lower-value one fewer) based on its amount in this store's base "
                    "currency."
                }
                form id="default-confirmations" method="post" action=(format!("/dashboard/stores/{}/settings/confirmations", store.connection_id)) fx-action=(format!("/dashboard/stores/{}/settings/confirmations", store.connection_id)) fx-method="POST" fx-target="#confirmation-thresholds" {
                    input type="hidden" name="zero_conf_checkbox_present" value="true";
                }
                form method="post" action=(format!("/dashboard/stores/{}/settings/confirmation-thresholds/save", store.connection_id)) fx-action=(format!("/dashboard/stores/{}/settings/confirmation-thresholds/save", store.connection_id)) fx-method="POST" fx-target="#confirmation-thresholds" {
                    table class="thresholds-table table-stack" {
                        thead { tr { th { "Amount (" (store.base_currency) ")" } th { "Confirmations required" } th { "Action" } } }
                        tbody {
                            tr {
                                td class="muted" { "Default (fallback)" }
                                td data-label="Confirmations required" {
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
                                    td data-label=(format!("Amount ({})", store.base_currency)) { (threshold.unit_amount) }
                                    td data-label="Confirmations required" { (threshold.confirmations_required) }
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
                                        button type="submit" class="btn-primary" { "Add" }
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
        }
    }
}

/// "Exchange rate providers": which providers this store uses, and in what
/// order. Plain form fields (a checkbox and a position number per provider),
/// so it works with no JavaScript.
fn fx_provider_section(store: &StoreSettingsData, in_place: bool, oob: bool) -> Markup {
    let action = format!(
        "/dashboard/stores/{}/settings/fx-provider",
        store.connection_id
    );
    html! {
        section id=(StoreSection::FxProvider.id()) data-fx-oob[oob] {
                h2 { "Exchange rate providers" }
                (section_error(store, StoreSection::FxProvider, in_place))
                @if store.fx_provider_options.is_empty() {
                    p { strong { "Exchange rate providers:" } " " span class="muted" { "none enabled on this instance" } }
                    p class="hint" {
                        "Only XMR-denominated orders can be created until an admin of this Monokulo instance "
                        "enables a provider (e.g. Coingecko)."
                    }
                } @else {
                    form method="post" action=(action) fx-action=(action) fx-method="POST" fx-target="#fx-provider" {
                        span class="field-help" {
                            "Where this store's orders get their live market rate from, for any currency other than "
                            "XMR (which always works, needing no provider at all). Tick the providers to use and "
                            "number them in order of preference: the first one that is reachable and has a rate for "
                            "the order's currency is used, otherwise the next. The provider that priced each order is "
                            "recorded on it. Changes apply to the next order created."
                        }
                        table class="fx-providers" {
                            thead { tr { th { "Use" } th { "Provider" } th { "Order" } } }
                            tbody {
                                @for (index, opt) in store.fx_provider_options.iter().enumerate() {
                                    tr {
                                        td { input type="checkbox" name=(format!("use_{}", opt.name)) value="on" checked[opt.selected] aria-label=(format!("Use {}", opt.name)); }
                                        td { (opt.name) }
                                        td { input type="number" name=(format!("position_{}", opt.name)) value=(index + 1) min="1" max=(store.fx_provider_options.len()) aria-label=(format!("Preference order of {}", opt.name)); }
                                    }
                                }
                            }
                        }
                        @if let Some(haveno) = &store.haveno_settings {
                            fieldset class="haveno-settings" {
                                legend { "Haveno (RetoSwap) limits" }
                                span class="field-help" {
                                    "Haveno prices from a thin peer-to-peer order book, so it only quotes when the book "
                                    "meets the limits below; otherwise the next provider in your order is used. These "
                                    "only matter while Haveno is ticked above."
                                }
                                label {
                                    "Currencies Haveno may quote"
                                    input type="text" name=(crate::fx_provider_settings::HAVENO_CURRENCIES) value=(haveno.currencies) placeholder="USD, EUR, GBP" autocomplete="off";
                                    span class="field-help" { "Comma-separated. Leave empty to allow every currency." }
                                }
                                label {
                                    "Maximum spread (%)"
                                    input type="number" name=(crate::fx_provider_settings::HAVENO_MAX_SPREAD_PCT) value=(haveno.max_spread_pct) min="0.01" max="100" step="any";
                                    span class="field-help" { "The largest gap between the best buy and sell offer, as a percentage of their midpoint." }
                                }
                                label {
                                    "Minimum offers on each side"
                                    input type="number" name=(crate::fx_provider_settings::HAVENO_MIN_OFFERS_PER_SIDE) value=(haveno.min_offers_per_side) min="1" max="1000" step="1";
                                    span class="field-help" { "Buy offers and sell offers must each number at least this many." }
                                }
                                label {
                                    "Minimum XMR on each side"
                                    input type="number" name=(crate::fx_provider_settings::HAVENO_MIN_DEPTH_XMR_PER_SIDE) value=(haveno.min_depth_xmr_per_side) min="0" max="1000000" step="any";
                                    span class="field-help" { "Total XMR offered on each side must be at least this much. 0 turns the check off." }
                                }
                            }
                        }
                        button type="submit" { "Update" }
                    }
                }
        }
    }
}

/// "Webhooks", with a new webhook's signing secret right after it is made.
fn webhooks_section(store: &StoreSettingsData, in_place: bool, oob: bool) -> Markup {
    html! {
        section id=(StoreSection::Webhooks.id()) data-fx-oob[oob] {
                h2 { "Webhooks" }
                (section_error(store, StoreSection::Webhooks, in_place))
                @if let Some(secret) = &store.created_webhook_signing_secret {
                    div class="box" data-webhook-secret {
                        h3 { "Webhook created" }
                        p {
                            "Its signing secret (verify the " code { "X-Monokulo-Signature" } " header with this - shown once, right now, and never again):"
                        }
                        pre { (secret) }
                        p class="hint" { "Store it somewhere safe before leaving this page. If you lose it, delete this webhook and create a new one." }
                    }
                }
                table class="table-stack" {
                    thead { tr { th { "URL" } th { "Enabled" } th { "Created" } th {} } }
                    tbody {
                        @for webhook in &store.webhooks {
                            tr {
                                td data-label="URL" { (webhook.url) }
                                td {
                                    @if webhook.enabled {
                                        span class="tag tag-ok" { "enabled" }
                                    } @else {
                                        span class="tag tag-unknown" { "disabled" }
                                    }
                                }
                                td data-label="Created" { (store.clock.time(webhook.created_at)) }
                                td {
                                    form method="post"
                                        action=(format!("/dashboard/stores/{}/settings/webhooks/{}/delete", store.connection_id, webhook.webhook_id))
                                        fx-action=(format!("/dashboard/stores/{}/settings/webhooks/{}/delete", store.connection_id, webhook.webhook_id))
                                        fx-method="POST" fx-target="#webhooks"
                                        onsubmit="return confirm('Delete this webhook? Anything relying on it will stop receiving events immediately.');" {
                                        button type="submit" class="btn-secondary" { "Delete" }
                                    }
                                }
                            }
                        }
                    }
                }
                @if store.webhooks_unavailable {
                    div class="error" role="alert" { "Couldn't reach the engine to list this store's webhooks. Reload the page to try again." }
                } @else if store.webhooks.is_empty() {
                    p class="muted" { "No webhooks yet." }
                }
                div class="box" {
                    h3 { "Add a webhook" }
                    form method="post" action=(format!("/dashboard/stores/{}/settings/webhooks", store.connection_id)) fx-action=(format!("/dashboard/stores/{}/settings/webhooks", store.connection_id)) fx-method="POST" fx-target="#webhooks" {
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
                        button type="submit" class="btn-primary" { "Add webhook" }
                    }
                }
        }
    }
}

/// "Diagnostics": whether this store's browsers, POS and plugin may send
/// logs to this instance. Off by default.
fn diagnostics_section(store: &StoreSettingsData, in_place: bool, oob: bool) -> Markup {
    let (action, target) = fx(
        &format!(
            "/dashboard/stores/{}/settings/diagnostics",
            store.connection_id
        ),
        StoreSection::Diagnostics,
    );
    html! {
        section id=(StoreSection::Diagnostics.id()) data-fx-oob[oob] {
            h2 { "Diagnostics" }
            (section_error(store, StoreSection::Diagnostics, in_place))
            form method="post" action=(action) fx-action=(action) fx-method="POST" fx-target=(target) {
                @if store.client_logging {
                    p {
                        span class="tag tag-ok" { "On" } " "
                        strong { "This store sends diagnostic logs." }
                        " Script errors from its dashboard pages and checkout, a timeline of each POS session (connection "
                        "drops, the app going to the background, orders created, backgrounded and completed), and the "
                        "WooCommerce plugin's errors when its \"Send errors to Monokulo\" option is on, all go to this "
                        "instance's logs, where its admins can read them."
                    }
                    input type="hidden" name="client_logging" value="off";
                    button type="submit" class="btn-secondary" { "Turn off" }
                } @else {
                    p {
                        span class="tag tag-unknown" { "Off" } " "
                        strong { "This store sends no diagnostic logs." }
                        " Turn this on while tracking down a problem with the POS, the checkout or the WooCommerce plugin: "
                        "script errors, a timeline of each POS session and the plugin's errors then go to this instance's "
                        "logs. Customer notes, addresses and keys are never included."
                    }
                    input type="hidden" name="client_logging" value="on";
                    button type="submit" { "Send diagnostic logs" }
                }
            }
        }
    }
}

/// One section of the page, as fixi swaps it back after a save there.
/// `oob` marks it to replace the page's copy wherever that is (a section
/// another save changed too).
pub fn section(store: &StoreSettingsData, which: StoreSection, oob: bool) -> Markup {
    match which {
        StoreSection::BaseCurrency => base_currency_section(store, true, oob),
        StoreSection::Confirmations => confirmations_section(store, true, oob),
        StoreSection::FxProvider => fx_provider_section(store, true, oob),
        StoreSection::KeyStorage => match &store.key_storage {
            Some(key_storage) => key_storage_section(store, key_storage, true, oob),
            None => html! { section id=(StoreSection::KeyStorage.id()) data-fx-oob[oob] {} },
        },
        StoreSection::Domains => verified_domains(store, true, oob),
        StoreSection::Webhooks => webhooks_section(store, true, oob),
        StoreSection::Diagnostics => diagnostics_section(store, true, oob),
    }
}

pub fn page(chrome: &PageChrome, data: &StoreSettingsViewModel) -> Markup {
    let body = html! {
        div class="wrap" data-store-settings data-active-section=(data.store.as_ref().and_then(|s| s.active_section).map(|s| s.id()).unwrap_or("")) {
            @if let Some(store) = &data.store {
                (super::store_breadcrumb(store.connection_id.as_str(), &store.display_name, false))
                h1 { "Settings" }
                @if store.active_section.is_some() && store.settings_error.is_none() {
                    p class="success settings-saved" role="status" data-settings-saved { "Settings saved." }
                }

                @if let Some(error) = &store.settings_error {
                    div class="error" role="alert" {
                        (error)
                        @if let Some(section) = store.active_section {
                            " " a href=(format!("#{}", section.id())) { "Go to the form" }
                        }
                    }
                }
                (section(store, StoreSection::BaseCurrency, false))
                (section(store, StoreSection::Confirmations, false))
                (section(store, StoreSection::FxProvider, false))
                @if let Some(key_storage) = &store.key_storage {
                    (key_storage_section(store, key_storage, true, false))
                }
                (section(store, StoreSection::Domains, false))
                (section(store, StoreSection::Webhooks, false))
                (section(store, StoreSection::Diagnostics, false))
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
    let head =
        html! { script { (maud::PreEscaped(include_str!("../../static/settings-dialogs.js"))) } };
    layout_with_head(chrome, &title, head, body)
}

/// Adding, checking and removing the domains this store has proved it owns
/// (`crate::embed_domains`).
fn verified_domains(store: &StoreSettingsData, in_place: bool, oob: bool) -> Markup {
    html! {
      section id=(StoreSection::Domains.id()) data-fx-oob[oob] {
        h2 { "Verified domains" }
        (section_error(store, StoreSection::Domains, in_place))
        p class="hint" {
            "Prove you own the websites that show this store's checkout. Add a domain, publish the TXT record shown here in "
            "that domain's DNS settings, then check it. A verified domain covers all of its subdomains. Onion addresses can't "
            "be verified, because they have no DNS."
        }
        form class="embed-restriction" method="post" action=(format!("/dashboard/stores/{}/settings/embed-restriction", store.connection_id))
            fx-action=(format!("/dashboard/stores/{}/settings/embed-restriction", store.connection_id)) fx-method="POST" fx-target="#verified-domains" {
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
            table class="domains-table table-stack" {
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
                            td data-label="Last checked" { (domain.last_checked) }
                            td class="domain-actions" {
                                form method="post" action=(format!("/dashboard/stores/{}/settings/domains/{}/check", store.connection_id, domain.id)) fx-action=(format!("/dashboard/stores/{}/settings/domains/{}/check", store.connection_id, domain.id)) fx-method="POST" fx-target="#verified-domains" {
                                    button type="submit" { "Check now" }
                                }
                                form method="post" action=(format!("/dashboard/stores/{}/settings/domains/{}/delete", store.connection_id, domain.id)) fx-action=(format!("/dashboard/stores/{}/settings/domains/{}/delete", store.connection_id, domain.id)) fx-method="POST" fx-target="#verified-domains"
                                    onsubmit="return confirm('Remove this domain? You would need a new DNS record to verify it again.');" {
                                    button type="submit" class="btn-secondary" { "Remove" }
                                }
                            }
                        }
                    }
                }
            }
        }
        form method="post" action=(format!("/dashboard/stores/{}/settings/domains", store.connection_id)) fx-action=(format!("/dashboard/stores/{}/settings/domains", store.connection_id)) fx-method="POST" fx-target="#verified-domains" {
            label {
                "Domain"
                input type="text" name="domain" placeholder="shop.example" required autocomplete="off" spellcheck="false";
                span class="field-help" { "Just the domain, like shop.example. Its subdomains are covered too." }
            }
            button type="submit" class="btn-primary" { "Add domain" }
        }
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
            clock: crate::views::time::Clock::utc(0),
            connection_id: shared::ids::ConnectionId::new("conn_1".to_string()),
            display_name: "shop.example.com".to_string(),
            confirmations_required: 10,
            fx_provider_options: vec![FxProviderOption {
                name: "coingecko".to_string(),
                selected: true,
            }],
            haveno_settings: None,
            base_currency: "XMR".to_string(),
            base_currency_options: vec![],
            confirmation_thresholds: vec![],
            confirmation_thresholds_at_max: false,
            zero_conf_enabled: false,
            webhooks: vec![],
            created_webhook_signing_secret: None,
            settings_error: None,
            webhooks_unavailable: false,
            embed_domains: vec![],
            embed_restricted: false,
            embed_can_restrict: false,
            key_storage: None,
            active_section: None,
            client_logging: false,
        }
    }

    fn with_haveno(settings: crate::fx_provider_settings::HavenoSettings) -> String {
        let store = StoreSettingsData {
            fx_provider_options: vec![
                FxProviderOption {
                    name: "coingecko".to_string(),
                    selected: true,
                },
                FxProviderOption {
                    name: "haveno".to_string(),
                    selected: false,
                },
            ],
            haveno_settings: Some(HavenoSettingsView::from(&settings)),
            ..base_store()
        };
        page(&chrome(), &StoreSettingsViewModel { store: Some(store) }).into_string()
    }

    #[test]
    fn the_haveno_limits_are_shown_only_while_the_instance_offers_haveno() {
        let html = page(
            &chrome(),
            &StoreSettingsViewModel {
                store: Some(base_store()),
            },
        )
        .into_string();
        assert!(
            !html.contains("haveno_max_spread_pct") && !html.contains("Haveno (RetoSwap) limits"),
            "got: {html}"
        );

        let html = with_haveno(Default::default());
        assert!(html.contains("Haveno (RetoSwap) limits"), "got: {html}");
        for name in [
            "haveno_currencies",
            "haveno_max_spread_pct",
            "haveno_min_offers_per_side",
            "haveno_min_depth_xmr_per_side",
        ] {
            assert!(
                html.contains(&format!(r#"name="{name}""#)),
                "missing {name}: {html}"
            );
        }
    }

    #[test]
    fn the_haveno_limits_show_the_stores_current_values() {
        let html = with_haveno(crate::fx_provider_settings::HavenoSettings {
            currencies: vec!["USD".to_string(), "EUR".to_string()],
            max_spread_pct: 2.5,
            min_offers_per_side: 3,
            min_depth_xmr_per_side: 1.5,
        });
        assert!(
            html.contains(r#"name="haveno_currencies" value="USD, EUR""#),
            "got: {html}"
        );
        assert!(
            html.contains(r#"name="haveno_max_spread_pct" value="2.5""#),
            "got: {html}"
        );
        assert!(
            html.contains(r#"name="haveno_min_offers_per_side" value="3""#),
            "got: {html}"
        );
        assert!(
            html.contains(r#"name="haveno_min_depth_xmr_per_side" value="1.5""#),
            "got: {html}"
        );
    }

    #[test]
    fn the_default_haveno_limits_render_as_plain_numbers() {
        let html = with_haveno(Default::default());
        assert!(
            html.contains(r#"name="haveno_currencies" value="""#),
            "got: {html}"
        );
        assert!(
            html.contains(r#"name="haveno_max_spread_pct" value="5""#),
            "got: {html}"
        );
        assert!(
            html.contains(r#"name="haveno_min_offers_per_side" value="1""#),
            "got: {html}"
        );
        assert!(
            html.contains(r#"name="haveno_min_depth_xmr_per_side" value="0""#),
            "got: {html}"
        );
    }

    #[test]
    fn a_hostile_currency_list_is_escaped() {
        let html = with_haveno(crate::fx_provider_settings::HavenoSettings {
            currencies: vec![r#""><script>x</script>"#.to_string()],
            ..Default::default()
        });
        assert!(!html.contains("<script>x</script>"), "got: {html}");
    }

    #[test]
    fn diagnostics_are_off_until_turned_on() {
        let html = page(
            &chrome(),
            &StoreSettingsViewModel {
                store: Some(base_store()),
            },
        )
        .into_string();
        assert!(
            html.contains(r#"<section id="diagnostics"><h2>Diagnostics</h2>"#),
            "got: {html}"
        );
        assert!(html.contains("This store sends no diagnostic logs."));
        assert!(html.contains(r#"<input type="hidden" name="client_logging" value="on">"#));
        assert!(html.contains(r##"fx-target="#diagnostics""##));

        let store = StoreSettingsData {
            client_logging: true,
            ..base_store()
        };
        let html = page(&chrome(), &StoreSettingsViewModel { store: Some(store) }).into_string();
        assert!(html.contains("This store sends diagnostic logs."));
        assert!(html.contains(r#"<input type="hidden" name="client_logging" value="off">"#));
    }

    #[test]
    fn the_restriction_switch_needs_a_verified_domain_to_turn_on() {
        let html = page(
            &chrome(),
            &StoreSettingsViewModel {
                store: Some(base_store()),
            },
        )
        .into_string();
        assert!(html.contains(r#"<input type="hidden" name="restricted" value="on"><button type="submit" disabled>Only allow my verified domains</button>"#), "got: {html}");

        let store = StoreSettingsData {
            embed_can_restrict: true,
            ..base_store()
        };
        let html = page(&chrome(), &StoreSettingsViewModel { store: Some(store) }).into_string();
        assert!(html.contains(r#"<button type="submit">Only allow my verified domains</button>"#));

        let store = StoreSettingsData {
            embed_can_restrict: true,
            embed_restricted: true,
            ..base_store()
        };
        let html = page(&chrome(), &StoreSettingsViewModel { store: Some(store) }).into_string();
        assert!(html.contains("Only my verified domains can show this checkout."));
        assert!(html.contains(r#"<input type="hidden" name="restricted" value="off">"#));
    }

    #[test]
    fn verified_domains_show_the_record_to_publish_until_verified() {
        let html = page(
            &chrome(),
            &StoreSettingsViewModel {
                store: Some(base_store()),
            },
        )
        .into_string();
        assert!(html.contains(r#"<section id="verified-domains"><h2>Verified domains</h2>"#));
        assert!(html.contains("No domains yet."));
        assert!(html.contains(r#"action="/dashboard/stores/conn_1/settings/domains""#));

        let domain =
            |id: &str, name: &str, state_label: &'static str, show_record: bool| EmbedDomainView {
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
            embed_domains: vec![
                domain("d1", "shop.example", "Verified", false),
                domain("d2", "new.example", "Waiting for DNS", true),
            ],
            ..base_store()
        };
        let html = page(&chrome(), &StoreSettingsViewModel { store: Some(store) }).into_string();
        assert!(
            !html.contains("monokulo-verify=token-d1"),
            "a verified domain doesn't need its record shown"
        );
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
        let html = page(
            &chrome(),
            &StoreSettingsViewModel {
                store: Some(base_store()),
            },
        )
        .into_string();
        assert!(
            html.contains(r#"<nav class="context-nav" aria-label="Breadcrumb"><a href="/dashboard/stores/conn_1" title="shop.example.com">shop.example.com</a></nav><h1>Settings</h1>"#),
            "expected a store link above the Settings heading, got: {html}"
        );
    }

    #[test]
    fn the_default_threshold_row_has_no_delete_control() {
        let html = page(
            &chrome(),
            &StoreSettingsViewModel {
                store: Some(base_store()),
            },
        )
        .into_string();
        assert!(html.contains("Default (fallback)"));
        assert!(html.contains(r#"value="10""#));
    }

    #[test]
    fn default_and_custom_confirmation_controls_submit_to_separate_forms() {
        let html = page(
            &chrome(),
            &StoreSettingsViewModel {
                store: Some(base_store()),
            },
        )
        .into_string();
        assert!(html.contains(r##"<form id="default-confirmations" method="post" action="/dashboard/stores/conn_1/settings/confirmations" fx-action="/dashboard/stores/conn_1/settings/confirmations" fx-method="POST" fx-target="#confirmation-thresholds">"##), "{html}");
        assert!(html.contains(r#"name="zero_conf_checkbox_present" value="true""#));
        assert!(html.contains(r#"maxlength="3" required form="default-confirmations""#));
        assert!(
            html.contains(r#"<button type="submit" form="default-confirmations">Save</button>"#)
        );
        assert!(html.contains(r#"<form method="post" action="/dashboard/stores/conn_1/settings/confirmation-thresholds/save""#), "{html}");
        assert!(html.contains(r#"<button type="submit" class="btn-primary">Add</button>"#));
    }

    #[test]
    fn zero_conf_checkbox_is_unchecked_when_disabled() {
        let html = page(
            &chrome(),
            &StoreSettingsViewModel {
                store: Some(base_store()),
            },
        )
        .into_string();
        assert!(
            html.contains(
                r#"<input type="checkbox" name="zero_conf_enabled" form="default-confirmations">"#
            ),
            "expected the 0-conf checkbox unchecked when no ceiling is set, got: {html}"
        );
    }

    #[test]
    fn zero_conf_checkbox_is_checked_when_enabled() {
        let store = StoreSettingsData {
            zero_conf_enabled: true,
            ..base_store()
        };
        let html = page(&chrome(), &StoreSettingsViewModel { store: Some(store) }).into_string();
        assert!(html.contains(r#"<input type="checkbox" name="zero_conf_enabled" checked form="default-confirmations">"#), "got: {html}");
    }

    #[test]
    fn confirmations_required_input_is_narrow_and_capped_at_three_digits() {
        let html = page(
            &chrome(),
            &StoreSettingsViewModel {
                store: Some(base_store()),
            },
        )
        .into_string();
        assert!(html.contains(r#"class="confirmations-input" name="confirmations_required" value="10" size="3" maxlength="3""#), "got: {html}");
    }

    #[test]
    fn add_row_is_part_of_the_table_and_uses_descriptive_placeholders() {
        let html = page(
            &chrome(),
            &StoreSettingsViewModel {
                store: Some(base_store()),
            },
        )
        .into_string();
        assert!(html.contains("<th>Action</th>"), "got: {html}");
        assert!(!html.contains("<th>Delete</th>"));
        assert!(!html.contains("threshold-gap-row"));
        assert!(html.contains(r#"<tr class="new-threshold-row"><td><input type="text" name="new_unit_amount" placeholder="Minimum Amount (XMR)">"#), "got: {html}");
        assert!(
            html.contains(r##"placeholder="# Confirmations""##),
            "got: {html}"
        );
        assert!(html.contains(r#"<button type="submit" class="btn-primary">Add</button>"#));
    }

    #[test]
    fn existing_threshold_rows_have_save_buttons_even_at_the_limit() {
        let store = StoreSettingsData {
            confirmation_thresholds: vec![ConfirmationThresholdView {
                id: "threshold_1".to_string(),
                unit_amount: "50.00".to_string(),
                confirmations_required: 20,
            }],
            confirmation_thresholds_at_max: true,
            ..base_store()
        };
        let html = page(&chrome(), &StoreSettingsViewModel { store: Some(store) }).into_string();
        assert!(html.contains("Maximum of 5 custom thresholds reached"));
        assert!(html.contains(r#"name="delete_threshold_1""#), "got: {html}");
        assert!(
            html.contains(r#"<button type="submit">Save</button>"#),
            "existing thresholds need a Save button, got: {html}"
        );
        assert!(!html.contains(r#"<button type="submit" class="btn-primary">Add</button>"#));
    }

    #[test]
    fn says_when_the_webhooks_could_not_be_listed() {
        let store = StoreSettingsData {
            webhooks_unavailable: true,
            ..base_store()
        };
        let html = page(&chrome(), &StoreSettingsViewModel { store: Some(store) }).into_string();
        assert!(html.contains("list this store's webhooks"), "{html}");
        assert!(!html.contains("No webhooks yet."));
        assert!(html.contains("Add a webhook"), "the form still works");
    }

    #[test]
    fn shows_the_settings_error_when_present() {
        let store = StoreSettingsData {
            settings_error: Some("Enter a whole number of confirmations.".to_string()),
            ..base_store()
        };
        let html = page(&chrome(), &StoreSettingsViewModel { store: Some(store) }).into_string();
        assert!(html.contains("Enter a whole number of confirmations."));
    }

    #[test]
    fn lists_webhooks_with_a_delete_form_each() {
        let store = StoreSettingsData {
            webhooks: vec![WebhookRowViewModel {
                webhook_id: "wh_1".to_string(),
                url: "https://example.com/hook".to_string(),
                enabled: true,
                created_at: 1000,
            }],
            ..base_store()
        };
        let html = page(&chrome(), &StoreSettingsViewModel { store: Some(store) }).into_string();
        assert!(html.contains("https://example.com/hook"));
        assert!(html.contains("tag-ok"));
        assert!(html.contains(r#"action="/dashboard/stores/conn_1/settings/webhooks/wh_1/delete""#));
    }

    #[test]
    fn shows_the_webhook_signing_secret_exactly_once_after_creation() {
        let store = StoreSettingsData {
            created_webhook_signing_secret: Some("whsec_abc123".to_string()),
            ..base_store()
        };
        let html = page(&chrome(), &StoreSettingsViewModel { store: Some(store) }).into_string();
        assert!(html.contains("whsec_abc123"));
        assert!(html.contains("Webhook created"));
    }
}
