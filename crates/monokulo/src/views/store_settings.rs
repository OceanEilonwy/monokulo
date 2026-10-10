//! `GET /dashboard/stores/{id}/settings` -
//! `http::orders::store_settings`/`render_store_settings_page`.
//!
//! Everything about a store that isn't day-to-day order activity: currency
//! and confirmation-threshold config, the exchange rate provider, and
//! webhooks - split out of the store overview page (which used to carry all
//! of this inline, making it the busiest page in the dashboard) into one
//! dedicated settings page, reached via the "Settings" link next to that
//! page's own "help" disclosure.
//!
//! The settings are cards in one settings form (`views::settings`), saved
//! together with the save bar (`http::store_settings`): the base currency,
//! the confirmation thresholds, the exchange rate providers and diagnostics.
//! What isn't a setting but an action with its own button (changing the
//! wallet, moving the keys, the verified domains, the webhooks) is a card of
//! the same look outside the form. Every save and action posts and comes
//! back to the page with a toast.

use maud::{html, Markup};

use super::controls::Choice;
use super::settings::{plain_card, toast_region, Card, Field, Save, Toast, ToastKind};
use super::{layout, PageChrome};

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
    /// Its site's host; empty when it has none.
    pub site: String,
    /// The website and the plugins connected to the store
    /// (`views::store_site`).
    pub sites: super::store_site::SiteView,
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
    /// The store's webhooks and their deliveries (`views::webhooks`),
    /// with a just-made webhook's signing secret, shown that once.
    pub webhooks: super::webhooks::WebhooksCard,
    /// What the save or the action this page answers did. `None` on a
    /// plain load.
    pub outcome: Option<StoreOutcome>,
    /// The settings form's fields as sent, after a save it refused: shown
    /// in place of the saved values, to fix or discard.
    pub sent: Option<Vec<(String, String)>>,
    pub embed_domains: Vec<EmbedDomainView>,
    /// "Only my verified domains can show this checkout" is on.
    pub embed_restricted: bool,
    /// At least one domain counts as verified, so it can be turned on.
    pub embed_can_restrict: bool,
    /// Where the store's keys are kept and where they could move (part 5);
    /// `None` when there's nowhere else to move them.
    pub key_storage: Option<KeyStorageView>,
    /// The store sends client logs (`db::Db::client_logging`).
    pub client_logging: bool,
    /// The wallet it takes payments into, and the ones it could change to.
    pub wallet: StoreWalletView,
}

/// "Wallet": the wallet a store takes payments into, the others it could
/// change to, and the ones it used before (docs/wallets.md, "Changing a
/// store's wallet").
#[derive(Default)]
pub struct StoreWalletView {
    /// `None` for a store made before wallets, not yet matched to one.
    pub current: Option<CurrentWallet>,
    /// Every wallet of the account: the current one first.
    pub choices: Vec<WalletChoice>,
    /// A change picked and waiting to be confirmed.
    pub pending: Option<PendingWalletChange>,
    /// Newest first; the first is the current one.
    pub history: Vec<WalletPeriodView>,
    /// Just changed: what the section says.
    pub changed: Option<String>,
}

pub struct CurrentWallet {
    pub id: String,
    pub name: String,
    pub since: i64,
}

pub struct WalletChoice {
    pub id: String,
    pub name: String,
    pub short_address: String,
    pub current: bool,
    /// Its Monero network: the store's own, as only those are offered.
    pub network: String,
    /// Stores other than this one that use it.
    pub other_stores: u64,
}

pub struct PendingWalletChange {
    pub wallet_id: String,
    pub wallet_name: String,
    /// The store's open orders, which stay on the current wallet; `None`
    /// when the engine couldn't say.
    pub open_orders: Option<usize>,
}

pub struct WalletPeriodView {
    pub wallet_id: Option<String>,
    /// `None` once the wallet was deleted.
    pub wallet_name: Option<String>,
    pub wallet_retired: bool,
    pub from: i64,
    pub until: Option<i64>,
    pub orders: u64,
}

/// The page's cards: the settings form's (`SETTINGS_CARDS`), and the
/// actions'.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreSection {
    Store,
    Connections,
    Wallet,
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
            Self::Store,
            Self::Connections,
            Self::Wallet,
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

    /// Its card's name (`card-{id}` its element id), and what `saved=`
    /// names it by.
    pub fn id(self) -> &'static str {
        match self {
            StoreSection::Store => "store",
            StoreSection::Connections => "connections",
            StoreSection::Wallet => "wallet",
            StoreSection::BaseCurrency => "base-currency",
            StoreSection::Confirmations => "confirmation-thresholds",
            StoreSection::FxProvider => "fx-provider",
            StoreSection::KeyStorage => "key-storage",
            StoreSection::Domains => "verified-domains",
            StoreSection::Webhooks => "webhooks",
            StoreSection::Diagnostics => "diagnostics",
        }
    }
}

/// What the save or action the page answers did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreOutcome {
    /// Nothing in the settings form had changed.
    Unchanged,
    /// These cards were saved (or this action done).
    Saved(Vec<StoreSection>),
    /// `section` was refused, for `message`; the cards in `saved` were
    /// saved before it was (none: nothing was saved).
    Refused {
        section: StoreSection,
        message: String,
        saved: Vec<StoreSection>,
    },
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

pub struct StoreSettingsViewModel {
    pub store: Option<StoreSettingsData>,
}

/// The settings form's cards, in the order they're saved.
pub const SETTINGS_CARDS: [StoreSection; 5] = [
    StoreSection::Store,
    StoreSection::BaseCurrency,
    StoreSection::Confirmations,
    StoreSection::FxProvider,
    StoreSection::Diagnostics,
];

impl StoreSettingsData {
    /// What the last save or action refused on `section`, and why.
    fn refusal(&self, section: StoreSection) -> Option<&str> {
        match &self.outcome {
            Some(StoreOutcome::Refused {
                section: refused,
                message,
                ..
            }) if *refused == section => Some(message),
            _ => None,
        }
    }

    fn was_saved(&self, section: StoreSection) -> bool {
        match &self.outcome {
            Some(StoreOutcome::Saved(sections)) => sections.contains(&section),
            Some(StoreOutcome::Refused { saved, .. }) => saved.contains(&section),
            _ => false,
        }
    }

    /// The settings form's values were refused: it shows what was sent.
    fn refused_settings(&self) -> bool {
        self.sent.is_some()
    }

    /// What the form shows in `name`: what was sent after a refused save,
    /// `saved` otherwise.
    fn shown(&self, name: &str, saved: &str) -> String {
        match &self.sent {
            Some(sent) => sent
                .iter()
                .rev()
                .find(|(key, _)| key == name)
                .map_or_else(String::new, |(_, value)| value.clone()),
            None => saved.to_string(),
        }
    }

    /// Whether the checkbox `name` shows ticked: as sent after a refused
    /// save, `saved` otherwise.
    fn ticked(&self, name: &str, saved: bool) -> bool {
        match &self.sent {
            Some(sent) => sent.iter().any(|(key, _)| key == name),
            None => saved,
        }
    }

    /// `data-saved` for a control showing what was sent: the saved value,
    /// so the settings form knows the change is still unsaved.
    fn saved_attr(&self, saved: &str) -> Option<String> {
        self.refused_settings().then(|| saved.to_string())
    }

    fn saved_box(&self, saved: bool) -> Option<&'static str> {
        self.refused_settings()
            .then_some(if saved { "on" } else { "off" })
    }

    /// The toast the save or action this page answers leaves.
    fn toast(&self) -> Option<Toast> {
        Some(match self.outcome.as_ref()? {
            StoreOutcome::Unchanged => {
                Toast::new(ToastKind::Neutral, "Nothing to save").line("Nothing had changed.")
            }
            StoreOutcome::Saved(sections) => {
                let title = match sections.as_slice() {
                    [StoreSection::Wallet] => "Wallet changed",
                    [StoreSection::Store] => "Store saved",
                    [StoreSection::Connections] => "Plugin disconnected",
                    [StoreSection::KeyStorage] => "Keys moved",
                    [StoreSection::Domains] => "Verified domains updated",
                    [StoreSection::Webhooks] if self.webhooks.created_secret.is_some() => {
                        "Webhook created"
                    }
                    [StoreSection::Webhooks] => "Webhooks updated",
                    _ => "Settings saved",
                };
                Toast::new(ToastKind::Success, title)
            }
            StoreOutcome::Refused {
                section,
                message,
                saved,
            } => Toast::new(
                ToastKind::Error,
                if saved.is_empty() {
                    "Not saved"
                } else {
                    "Partly saved"
                },
            )
            .line(message.clone())
            .show(section.id()),
        })
    }
}

/// An action card's refusal, at the top of its body.
fn action_error(store: &StoreSettingsData, section: StoreSection) -> Markup {
    html! {
        @if let Some(message) = store.refusal(section) {
            p class="error" role="alert" { (message) }
        }
    }
}

/// A card of the settings form, as the last save left it.
fn settings_card(
    store: &StoreSettingsData,
    section: StoreSection,
    title: &str,
    body: Markup,
) -> Markup {
    let failure = store.refusal(section);
    Card::new(section.id(), title)
        .failed(failure.is_some(), failure)
        .saved(store.was_saved(section).then_some(""), false)
        .render(body)
}

/// "Key storage": where the store's view key is kept, and a form to move it
/// (task 5.6). The keys are entered again - they're never read back from
/// anywhere - and the fields are always empty on render. An action of its
/// own, not part of the settings form.
fn key_storage_card(store: &StoreSettingsData, key_storage: &KeyStorageView) -> Markup {
    let action = format!(
        "/dashboard/stores/{}/settings/key-custody",
        store.connection_id
    );
    plain_card(
        StoreSection::KeyStorage.id(),
        "Key storage",
        html! {
            (action_error(store, StoreSection::KeyStorage))
            p { strong { "Kept: " } (key_storage.current) }
            p class="hint" {
                "The keys belong to this store's wallet: moving them moves the wallet, for every store that uses it."
            }
            @if key_storage.current_disabled {
                p class="error" {
                    "This way of storing keys has been turned off on this instance, so payments to this store aren't "
                    "being detected. Move the keys below to start again."
                }
            }
            form method="post" action=(action) {
                label {
                    "Move to"
                    mk-select {
                        select name="backend" {
                            @for choice in &key_storage.move_to {
                                (Choice::new(&choice.backend, &choice.label).selected(choice.selected))
                            }
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
        },
    )
}

/// "Wallet": which wallet the store takes payments into, a dropdown of the
/// account's wallets to change it (the current one marked Current), and,
/// collapsed, the wallets it used before. Picking another wallet asks first,
/// saying what happens to the orders already open: an action of its own,
/// not part of the settings form.
fn wallet_card(store: &StoreSettingsData) -> Markup {
    let wallet = &store.wallet;
    let action = format!("/dashboard/stores/{}/settings/wallet", store.connection_id);
    let picked = wallet
        .pending
        .as_ref()
        .map(|p| p.wallet_id.as_str())
        .or(wallet.current.as_ref().map(|c| c.id.as_str()));
    plain_card(
        StoreSection::Wallet.id(),
        "Wallet",
        html! {
            (action_error(store, StoreSection::Wallet))
            @if let Some(changed) = &wallet.changed {
                p class="success" role="status" { (changed) }
            }
            @if let Some(current) = &wallet.current {
                p class="settings-summary-line" {
                    "Payments go to " strong { a href=(format!("/account/wallets/{}", current.id)) { (current.name) } }
                    " since " (store.clock.time(current.since)) "."
                }
            } @else {
                p class="hint" { "This store isn't linked to one of your wallets yet. Pick the one it takes payments into." }
            }
            // Picking a wallet sends it at once (static/fx-glue.js), to ask.
            form method="post" action=(action) fx-submit-on-change {
                label {
                    "Wallet"
                    mk-select {
                        select name="wallet_id" required {
                            @if picked.is_none() { (Choice::prompt("Choose a wallet…", true)) }
                            @for choice in &wallet.choices {
                                (wallet_choice(choice, picked))
                            }
                        }
                    }
                    span class="field-help" {
                        "The store takes new payments into the wallet picked here."
                        @if wallet.current.is_some() { " A store can only change to a wallet on its own network." }
                    }
                }
                @if let (Some(pending), Some(current)) = (&wallet.pending, &wallet.current) {
                    div class="change-confirm" role="status" {
                        h3 { "Change to " (pending.wallet_name) "?" }
                        p {
                            "New orders take payments into " strong { (pending.wallet_name) } ". "
                            (open_orders_line(pending.open_orders, &current.name))
                        }
                        button type="submit" name="confirm" value="yes" class="btn-primary" { "Change to " (pending.wallet_name) }
                    }
                } @else if let Some(pending) = &wallet.pending {
                    div class="change-confirm" role="status" {
                        h3 { "Take payments into " (pending.wallet_name) "?" }
                        button type="submit" name="confirm" value="yes" class="btn-primary" { "Use " (pending.wallet_name) }
                    }
                } @else {
                    button type="submit" { "Change wallet" }
                }
            }
            @if !wallet.history.is_empty() {
                details class="wallet-history" {
                    summary { "Wallet history (" (wallets_label(wallet.history.len())) ")" }
                    table {
                        thead { tr { th { "Wallet" } th { "From" } th { "Until" } th class="num" { "Orders" } } }
                        tbody {
                            @for period in &wallet.history {
                                tr class=[period.until.is_none().then_some("current")] aria-current=[period.until.is_none().then_some("true")] {
                                    td {
                                        @match (&period.wallet_id, &period.wallet_name) {
                                            (Some(id), Some(name)) => {
                                                a href=(format!("/account/wallets/{id}")) { (name) }
                                                @if period.wallet_retired { " " span class="tag tag-unknown" { "Retired" } }
                                            },
                                            _ => span class="muted" { "A deleted wallet" },
                                        }
                                    }
                                    td { (store.clock.time(period.from)) }
                                    td { @match period.until { Some(until) => (store.clock.time(until)), None => "now" } }
                                    td class="num" { (period.orders) }
                                }
                            }
                        }
                    }
                }
            }
        },
    )
}

fn wallet_choice(choice: &WalletChoice, picked: Option<&str>) -> Markup {
    let note = match (choice.current, choice.other_stores) {
        (true, 0) => "this store only".to_owned(),
        (_, 0) => "no other stores".to_owned(),
        (_, 1) => "1 other store".to_owned(),
        (_, n) => format!("{n} other stores"),
    };
    let option = Choice::new(&choice.id, &choice.name)
        .detail(&choice.short_address)
        .network(&choice.network)
        .current(choice.current)
        .note(note)
        .selected(picked == Some(choice.id.as_str()));
    html! { (option) }
}

/// What happens to the orders already open, said before the change.
fn open_orders_line(open: Option<usize>, current: &str) -> String {
    match open {
        Some(0) => format!("No orders are open on {current}; any that close late are still watched there."),
        Some(1) => format!("The 1 order still open on {current} keeps being paid into it, and is watched until it closes."),
        Some(n) => format!("The {n} orders still open on {current} keep being paid into it, and are watched until they close."),
        None => format!("Orders already open keep being paid into {current}, and are watched until they close."),
    }
}

fn wallets_label(n: usize) -> String {
    if n == 1 {
        "1 wallet".to_owned()
    } else {
        format!("{n} wallets")
    }
}

/// "Store": its name, and its website, optional. Any store takes payments
/// at the till; a website lets it show the checkout on its pages and
/// connect the WooCommerce plugin.
fn store_card(store: &StoreSettingsData) -> Markup {
    let name = store.shown("store_name", &store.display_name);
    let site = store.shown("store_site", &store.site);
    settings_card(
        store,
        StoreSection::Store,
        "Store",
        html! {
            (Field::new("Store name", "store-name")
                .help(None, html! { "Shown on your dashboard, the till and the checkout." })
                .render(html! {
                    input type="text" id="store-name" name="store_name" value=(name) maxlength=(crate::stores::MAX_NAME_LEN) required
                        data-saved=[store.saved_attr(&store.display_name)];
                }))
            (super::store_site::website_field(&store.sites, &site))
        },
    )
}

/// "Base currency".
fn base_currency_card(store: &StoreSettingsData) -> Markup {
    let shown = store.shown("base_currency", &store.base_currency);
    settings_card(
        store,
        StoreSection::BaseCurrency,
        "Base currency",
        Field::new("Base currency", "base-currency")
            .help(None, html! {
                "What custom threshold amounts below are denominated in. Changing this deletes every "
                "custom threshold this store currently has - an amount in a currency you're no longer using means nothing."
            })
            .render(html! {
                mk-select {
                    select name="base_currency" id="base-currency" data-saved=[store.saved_attr(&store.base_currency)] {
                        @for opt in &store.base_currency_options {
                            (Choice::new(&opt.code, &opt.description).detail(&opt.code).selected(opt.code == shown))
                        }
                    }
                }
            }),
    )
}

/// "Confirmation thresholds": the default, accepting unconfirmed payments,
/// and the custom ones (each can be deleted, and one added, per save).
fn confirmations_card(store: &StoreSettingsData) -> Markup {
    let default = store.confirmations_required.to_string();
    let zero_conf = store.ticked("zero_conf_enabled", store.zero_conf_enabled);
    settings_card(
        store,
        StoreSection::Confirmations,
        "Confirmation thresholds",
        html! {
            p class="hint" {
                "How many blocks a payment needs before this store's orders read as paid. The default "
                "is the fallback used whenever no custom threshold applies; custom thresholds let a higher-value "
                "order require more confirmations (or a lower-value one fewer) based on its amount in this store's base "
                "currency."
            }
            (Field::new("Default confirmations", "confirmations-required")
                .help(Some("confirmations-required-help"), html! { "Blocks a payment waits before an order counts as paid, from 0 to 720." })
                .render(html! {
                    input type="text" class="confirmations-input" id="confirmations-required" name="confirmations_required"
                        value=(store.shown("confirmations_required", &default)) size="3" maxlength="3" inputmode="numeric" required
                        aria-describedby="confirmations-required-help" data-saved=[store.saved_attr(&default)];
                }))
            (Field::new("Accept unconfirmed (0-conf) payments", "zero-conf")
                .help(Some("zero-conf-help"), html! {
                    "An order under the default can read as paid the moment its transaction reaches this store's node's "
                    "mempool, before any block confirms it - useful for fast, low-value, in-person sales. This is a real "
                    "double-spend risk (an attacker who can out-race the transaction to a miner keeps both the goods and the "
                    "coin). Add a custom threshold to keep larger orders requiring real confirmations."
                })
                .render(super::controls::switch("zero_conf_enabled", "zero-conf", zero_conf, Some("zero-conf-help"), store.saved_box(store.zero_conf_enabled).map(|s| s == "on"))))
            (Field::group("Custom thresholds", "custom-thresholds")
                .help(Some("custom-thresholds-help"), html! {
                    "Each custom threshold makes an order of at least its amount (in this store's base currency) "
                    "require its number of confirmations. Up to 5; tick Delete to remove one when you save."
                })
                .render(html! {
                    table class="thresholds-table table-stack" {
                        thead { tr { th { "Amount (" (store.base_currency) ")" } th { "Confirmations required" } th { "Delete" } } }
                        tbody {
                            @for threshold in &store.confirmation_thresholds {
                                @let name = format!("delete_{}", threshold.id);
                                tr {
                                    td data-label=(format!("Amount ({})", store.base_currency)) { (threshold.unit_amount) }
                                    td data-label="Confirmations required" { (threshold.confirmations_required) }
                                    td {
                                        label class="inline" {
                                            input type="checkbox" name=(name) checked[store.ticked(&name, false)] data-saved=[store.saved_box(false)];
                                            " Delete"
                                        }
                                    }
                                }
                            }
                            tr class="new-threshold-row" {
                                @if store.confirmation_thresholds_at_max {
                                    td colspan="3" class="muted" { "Maximum of 5 custom thresholds reached - delete one to add another." }
                                } @else {
                                    td {
                                        input type="text" name="new_unit_amount" value=(store.shown("new_unit_amount", ""))
                                            placeholder=(format!("Minimum Amount ({})", store.base_currency))
                                            aria-label=(format!("New threshold's amount ({})", store.base_currency)) data-saved=[store.saved_attr("")];
                                    }
                                    td {
                                        input type="text" class="confirmations-input" name="new_confirmations_required" value=(store.shown("new_confirmations_required", ""))
                                            size="3" maxlength="3" inputmode="numeric" placeholder="# Confirmations"
                                            aria-label="New threshold's confirmations" data-saved=[store.saved_attr("")];
                                    }
                                    td class="muted" { "New" }
                                }
                            }
                        }
                    }
                }))
        },
    )
}

/// "Exchange rate providers": which providers this store uses, and in what
/// order. Plain form fields (a checkbox and a position number per provider),
/// so it works with no JavaScript.
fn fx_provider_card(store: &StoreSettingsData) -> Markup {
    let count = store.fx_provider_options.len();
    settings_card(
        store,
        StoreSection::FxProvider,
        "Exchange rate providers",
        html! {
            @if store.fx_provider_options.is_empty() {
                p { strong { "Exchange rate providers:" } " " span class="muted" { "none enabled on this instance" } }
                p class="hint" {
                    "Only XMR-denominated orders can be created until an admin of this Monokulo instance "
                    "enables a provider (e.g. Coingecko)."
                }
            } @else {
                (Field::group("Providers", "fx-providers")
                    .help(Some("fx-providers-help"), html! {
                        "Where this store's orders get their live market rate from, for any currency other than "
                        "XMR (which always works, needing no provider at all). Tick the providers to use and "
                        "number them in order of preference: the first one that is reachable and has a rate for "
                        "the order's currency is used, otherwise the next. The provider that priced each order is "
                        "recorded on it. Changes apply to the next order created."
                    })
                    .render(html! {
                        table class="fx-providers" {
                            thead { tr { th { "Use" } th { "Provider" } th { "Order" } } }
                            tbody {
                                @for (index, opt) in store.fx_provider_options.iter().enumerate() {
                                    @let use_name = format!("use_{}", opt.name);
                                    @let position_name = format!("position_{}", opt.name);
                                    @let position = (index + 1).to_string();
                                    tr {
                                        td { input type="checkbox" name=(use_name) value="on" checked[store.ticked(&use_name, opt.selected)] aria-label=(format!("Use {}", opt.name)) data-saved=[store.saved_box(opt.selected)]; }
                                        td { (opt.name) }
                                        td { input type="number" name=(position_name) value=(store.shown(&position_name, &position)) min="1" max=(count) aria-label=(format!("Preference order of {}", opt.name)) data-saved=[store.saved_attr(&position)]; }
                                    }
                                }
                            }
                        }
                    }))
                @if let Some(haveno) = &store.haveno_settings {
                    h4 { "Haveno (RetoSwap) limits" }
                    p class="hint" {
                        "Haveno prices from a thin peer-to-peer order book, so it only quotes when the book "
                        "meets the limits below; otherwise the next provider in your order is used. These "
                        "only matter while Haveno is ticked above."
                    }
                    (haveno_field(store, crate::fx_provider_settings::HAVENO_CURRENCIES, "Currencies Haveno may quote", &haveno.currencies,
                        "Comma-separated. Leave empty to allow every currency.",
                        |name, id, value, saved| html! { input type="text" name=(name) value=(value) id=(id) placeholder="USD, EUR, GBP" autocomplete="off" data-saved=[saved]; }))
                    (haveno_field(store, crate::fx_provider_settings::HAVENO_MAX_SPREAD_PCT, "Maximum spread (%)", &haveno.max_spread_pct,
                        "The largest gap between the best buy and sell offer, as a percentage of their midpoint.",
                        |name, id, value, saved| html! { input type="number" name=(name) value=(value) id=(id) min="0.01" max="100" step="any" data-saved=[saved]; }))
                    (haveno_field(store, crate::fx_provider_settings::HAVENO_MIN_OFFERS_PER_SIDE, "Minimum offers on each side", &haveno.min_offers_per_side,
                        "Buy offers and sell offers must each number at least this many.",
                        |name, id, value, saved| html! { input type="number" name=(name) value=(value) id=(id) min="1" max="1000" step="1" data-saved=[saved]; }))
                    (haveno_field(store, crate::fx_provider_settings::HAVENO_MIN_DEPTH_XMR_PER_SIDE, "Minimum XMR on each side", &haveno.min_depth_xmr_per_side,
                        "Total XMR offered on each side must be at least this much. 0 turns the check off.",
                        |name, id, value, saved| html! { input type="number" name=(name) value=(value) id=(id) min="0" max="1000000" step="any" data-saved=[saved]; }))
                }
            }
        },
    )
}

/// One of Haveno's limits: `control(name, id, value shown, data-saved)`.
fn haveno_field(
    store: &StoreSettingsData,
    name: &str,
    label: &str,
    saved: &str,
    help: &str,
    control: impl Fn(&str, &str, &str, Option<String>) -> Markup,
) -> Markup {
    let id = name.replace('_', "-");
    let help_id = format!("{id}-help");
    Field::new(label, &id)
        .help(Some(&help_id), html! { (help) })
        .render(control(
            name,
            &id,
            &store.shown(name, saved),
            store.saved_attr(saved),
        ))
}

/// "Webhooks" (`views::webhooks`): actions of their own, not part of the
/// settings form.
fn webhooks_card(store: &StoreSettingsData) -> Markup {
    super::webhooks::card(&store.webhooks, action_error(store, StoreSection::Webhooks))
}

/// "Diagnostics": whether this store's browsers, POS and plugin may send
/// logs to this instance. Off by default.
fn diagnostics_card(store: &StoreSettingsData) -> Markup {
    let on = store.ticked("client_logging", store.client_logging);
    settings_card(
        store,
        StoreSection::Diagnostics,
        "Diagnostics",
        Field::new("Send diagnostic logs", "client-logging")
            .help(Some("client-logging-help"), html! {
                "Turn this on while tracking down a problem with the POS, the checkout or the WooCommerce plugin. "
                "Script errors from this store's dashboard pages and checkout, a timeline of each POS session (connection "
                "drops, the app going to the background, orders created, backgrounded and completed), and the "
                "WooCommerce plugin's errors when its \"Send errors to Monokulo\" option is on, then go to this "
                "instance's logs, where its admins can read them. Customer notes, addresses and keys are never included."
            })
            .render(super::controls::switch(
                "client_logging",
                "client-logging",
                on,
                Some("client-logging-help"),
                store.saved_box(store.client_logging).map(|s| s == "on"),
            )),
    )
}

/// The save bar: what saving does, or why the last save didn't.
fn save_bar(store: &StoreSettingsData) -> Markup {
    let refused = match &store.outcome {
        Some(StoreOutcome::Refused {
            section,
            message,
            saved,
        }) if SETTINGS_CARDS.contains(section) => Some((section, message, !saved.is_empty())),
        _ => None,
    };
    let message = html! {
        @if let Some((section, message, partly)) = refused {
            strong { @if partly { "Changes partly saved." } @else { "Nothing saved." } } " " (message) " "
            a href=(format!("#{}", super::settings::card_id(section.id()))) data-show-card=(section.id()) { "Show" }
        } @else {
            "Saving applies the changes to this store's next orders."
        }
    };
    super::settings::save_bar(
        refused.is_some(),
        false,
        message,
        &format!("/dashboard/stores/{}/settings", store.connection_id),
    )
}

pub fn page(chrome: &PageChrome, data: &StoreSettingsViewModel) -> Markup {
    let body = html! {
        div class="wrap settings-page" {
            @if let Some(store) = &data.store {
                (super::store_breadcrumb(store.connection_id.as_str(), &store.display_name, false))
                h1 { "Settings" }
                (wallet_card(store))
                (super::settings::form(
                    &format!("/dashboard/stores/{}/settings", store.connection_id),
                    Save::Reload,
                    "Store settings",
                    html! {
                        (store_card(store))
                        (base_currency_card(store))
                        (confirmations_card(store))
                        (fx_provider_card(store))
                        (diagnostics_card(store))
                        (save_bar(store))
                    },
                ))
                (super::store_site::connections_card(&store.sites))
                @if let Some(key_storage) = &store.key_storage {
                    (key_storage_card(store, key_storage))
                }
                (verified_domains_card(store))
                (webhooks_card(store))
                (toast_region(store.toast().as_ref(), false))
                (super::store_site::dialogs(&store.sites))
                (super::webhooks::dialogs(&store.webhooks))
                (super::script("confirm-dialogs.js", super::Load::Defer))
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
/// (`crate::embed_domains`), and whether only they may show its checkout:
/// actions of their own, not part of the settings form (turning the
/// restriction on needs a verified domain first).
fn verified_domains_card(store: &StoreSettingsData) -> Markup {
    let base = format!("/dashboard/stores/{}/settings", store.connection_id);
    plain_card(
        StoreSection::Domains.id(),
        "Verified domains",
        html! {
            (action_error(store, StoreSection::Domains))
            p class="hint" {
                "Prove you own the websites that show this store's checkout. Add a domain, publish the TXT record shown here in "
                "that domain's DNS settings, then check it. A verified domain covers all of its subdomains. Onion addresses can't "
                "be verified, because they have no DNS."
            }
            form class="embed-restriction" method="post" action=(format!("{base}/embed-restriction")) {
                @if store.embed_restricted {
                    p {
                        span class="tag tag-ok" { "On" } " "
                        strong { "Only my verified domains can show this checkout." }
                        " Browsers won't show it on any other website, and orders from other websites are refused. Pages on "
                        "this server (the POS, the payment link) always work."
                    }
                    input type="hidden" name="restricted" value="off";
                    button type="submit" { "Turn off" }
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
                                    form method="post" action=(format!("{base}/domains/{}/check", domain.id)) {
                                        button type="submit" { "Check now" }
                                    }
                                    form method="post" action=(format!("{base}/domains/{}/delete", domain.id))
                                        onsubmit="return confirm('Remove this domain? You would need a new DNS record to verify it again.');" {
                                        button type="submit" { "Remove" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            form method="post" action=(format!("{base}/domains")) {
                (Field::new("Domain", "new-domain")
                    .help(Some("new-domain-help"), html! {
                        "Just the domain, like shop.example. Its subdomains are covered too."
                    })
                    .render(html! {
                        input type="text" name="domain" id="new-domain" aria-describedby="new-domain-help"
                            placeholder="shop.example" required autocomplete="off" spellcheck="false";
                    }))
                div class="form-actions" {
                    button type="submit" { "Add domain" }
                }
            }
        },
    )
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
            site: "shop.example.com".to_string(),
            sites: crate::views::store_site::SiteView {
                store_id: "conn_1".into(),
                store_name: "shop.example.com".into(),
                site: "shop.example.com".into(),
                active: None,
                past: Vec::new(),
                open_orders: None,
                plugin_open_orders: None,
                site_domain: false,
            },
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
            webhooks: super::super::webhooks::WebhooksCard {
                store_id: "conn_1".into(),
                store_name: "shop.example.com".into(),
                clock: crate::views::time::Clock::utc(0),
                max_attempts: 8,
                webhooks: vec![],
                unavailable: false,
                created_secret: None,
            },
            outcome: None,
            sent: None,
            embed_domains: vec![],
            embed_restricted: false,
            embed_can_restrict: false,
            key_storage: None,
            client_logging: false,
            wallet: StoreWalletView::default(),
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

    fn render(store: StoreSettingsData) -> String {
        page(&chrome(), &StoreSettingsViewModel { store: Some(store) }).into_string()
    }

    #[test]
    fn diagnostics_are_a_switch_off_until_turned_on() {
        let html = render(base_store());
        assert!(
            html.contains(r#"<mk-settings-card id="card-diagnostics" class="settings-card" name="diagnostics""#),
            "got: {html}"
        );
        assert!(html.contains(r#"<input type="hidden" name="switches" value="client_logging">"#));
        assert!(html.contains(r#"<input type="checkbox" role="switch" name="client_logging" value="true" id="client-logging" aria-describedby="client-logging-help">"#), "{html}");
        let html = render(StoreSettingsData {
            client_logging: true,
            ..base_store()
        });
        assert!(
            html.contains(r#"name="client_logging" value="true" id="client-logging" checked"#),
            "{html}"
        );
    }

    /// The settings are cards of one form with one save bar; what has a
    /// button of its own (the wallet, the domains, the webhooks) is a card
    /// outside it.
    #[test]
    fn the_settings_are_one_form_and_the_actions_are_cards_beside_it() {
        let html = render(base_store());
        let at = |needle: &str| {
            html.find(needle)
                .unwrap_or_else(|| panic!("{needle} in {html}"))
        };
        let wallet = at(r#"<section id="card-wallet" class="settings-card""#);
        let form = at(
            r#"<mk-settings-form label="Store settings"><form method="post" action="/dashboard/stores/conn_1/settings" id="settings-form">"#,
        );
        let base = at(r#"<mk-settings-card id="card-base-currency""#);
        let confirmations = at(r#"<mk-settings-card id="card-confirmation-thresholds""#);
        let fx = at(r#"<mk-settings-card id="card-fx-provider""#);
        let diagnostics = at(r#"<mk-settings-card id="card-diagnostics""#);
        let bar = at(r#"<mk-save-bar id="save-bar""#);
        let end = at("</form></mk-settings-form>");
        let domains = at(r#"<section id="card-verified-domains" class="settings-card""#);
        let webhooks = at(r#"<section id="card-webhooks" class="settings-card""#);
        assert!(
            wallet < form
                && form < base
                && base < confirmations
                && confirmations < fx
                && fx < diagnostics
                && diagnostics < bar
                && bar < end
                && end < domains
                && domains < webhooks,
            "{html}"
        );
        // Only the website's confirm dialogs, and one orange button: the
        // bar's Save.
        assert_eq!(html.matches("<dialog").count(), 2, "{html}");
        assert!(
            html.contains(r#"<dialog id="website-dialog" class="settings-dialog confirm-dialog""#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<dialog id="website-remove-dialog" class="settings-dialog confirm-dialog""#
            ),
            "{html}"
        );
        assert!(!html.contains("settings-dialogs"), "{html}");
        assert_eq!(html.matches("btn-primary").count(), 1, "{html}");
        assert!(html.contains(r#"<a class="btn" href="/dashboard/stores/conn_1/settings" data-discard-all>Discard changes</a>"#), "{html}");
    }

    #[test]
    fn a_refused_save_shows_why_on_its_card_and_keeps_what_was_sent() {
        let store = StoreSettingsData {
            confirmation_thresholds: vec![ConfirmationThresholdView {
                id: "t1".to_string(),
                unit_amount: "50".to_string(),
                confirmations_required: 20,
            }],
            outcome: Some(StoreOutcome::Refused {
                section: StoreSection::Confirmations,
                message: "Enter a whole number of confirmations.".to_string(),
                saved: Vec::new(),
            }),
            sent: Some(vec![
                ("confirmations_required".to_string(), "abc".to_string()),
                ("switches".to_string(), "zero_conf_enabled".to_string()),
                ("delete_t1".to_string(), "on".to_string()),
                ("new_unit_amount".to_string(), "9".to_string()),
                ("switches".to_string(), "client_logging".to_string()),
                ("client_logging".to_string(), "true".to_string()),
            ]),
            ..base_store()
        };
        let html = render(store);
        assert!(
            html.contains(r#"<mk-settings-card id="card-confirmation-thresholds" class="settings-card is-failed""#),
            "{html}"
        );
        assert!(html.contains(r#"<div class="card-body"><p class="error" role="alert">Enter a whole number of confirmations.</p>"#), "{html}");
        assert!(
            html.contains(r#"name="confirmations_required" value="abc""#),
            "{html}"
        );
        assert!(html.contains(r#"data-saved="10""#), "{html}");
        assert!(
            html.contains(r#"<input type="checkbox" name="delete_t1" checked data-saved="off">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"name="new_unit_amount" value="9""#),
            "{html}"
        );
        assert!(html.contains(r#"name="client_logging" value="true" id="client-logging" checked aria-describedby="client-logging-help" data-saved="off">"#), "{html}");
        assert!(
            html.contains(r#"<mk-save-bar id="save-bar" class="save-bar is-failed""#),
            "{html}"
        );
        assert!(html.contains(r##"<strong>Nothing saved.</strong> Enter a whole number of confirmations. <a href="#card-confirmation-thresholds" data-show-card="confirmation-thresholds">Show</a>"##), "{html}");
        assert!(
            html.contains(r#"<div class="toast toast-error" role="alert" data-toast>"#),
            "{html}"
        );
    }

    #[test]
    fn a_save_says_which_cards_it_saved() {
        let html = render(StoreSettingsData {
            outcome: Some(StoreOutcome::Saved(vec![
                StoreSection::BaseCurrency,
                StoreSection::Diagnostics,
            ])),
            ..base_store()
        });
        assert_eq!(html.matches("data-card-saved>Saved<").count(), 2, "{html}");
        assert!(html.contains("<strong>Settings saved</strong>"), "{html}");
        let html = render(StoreSettingsData {
            outcome: Some(StoreOutcome::Unchanged),
            ..base_store()
        });
        assert!(html.contains("<strong>Nothing to save</strong>"), "{html}");
    }

    #[test]
    fn a_refused_action_shows_why_in_its_own_card() {
        let html = render(StoreSettingsData {
            outcome: Some(StoreOutcome::Refused {
                section: StoreSection::Webhooks,
                message: "Enter a webhook URL.".to_string(),
                saved: Vec::new(),
            }),
            ..base_store()
        });
        assert!(
            html.contains(
                r#"<div class="card-body"><p class="error" role="alert">Enter a webhook URL.</p>"#
            ),
            "{html}"
        );
        // The settings form's bar isn't about it.
        assert!(!html.contains("save-bar is-failed"), "{html}");
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
        assert!(html.contains(r#"<section id="card-verified-domains" class="settings-card" aria-labelledby="card-verified-domains-title"><header class="card-head"><h3 id="card-verified-domains-title">Verified domains</h3>"#));
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
        assert!(html.contains("Default confirmations"));
        assert!(html.contains(r#"value="10""#));
    }

    #[test]
    fn accepting_unconfirmed_payments_is_a_switch() {
        let html = render(base_store());
        assert!(
            html.contains(r#"<input type="hidden" name="switches" value="zero_conf_enabled">"#),
            "{html}"
        );
        assert!(html.contains(r#"<input type="checkbox" role="switch" name="zero_conf_enabled" value="true" id="zero-conf" aria-describedby="zero-conf-help">"#), "{html}");
        let html = render(StoreSettingsData {
            zero_conf_enabled: true,
            ..base_store()
        });
        assert!(
            html.contains(r#"name="zero_conf_enabled" value="true" id="zero-conf" checked"#),
            "{html}"
        );
    }

    #[test]
    fn confirmations_required_input_is_narrow_and_capped_at_three_digits() {
        let html = render(base_store());
        assert!(html.contains(r#"class="confirmations-input" id="confirmations-required" name="confirmations_required" value="10" size="3" maxlength="3" inputmode="numeric" required"#), "got: {html}");
    }

    #[test]
    fn a_custom_threshold_is_added_in_the_tables_last_row() {
        let html = render(base_store());
        assert!(html.contains("<th>Delete</th>"), "got: {html}");
        assert!(html.contains(r#"<tr class="new-threshold-row"><td><input type="text" name="new_unit_amount" value="" placeholder="Minimum Amount (XMR)""#), "got: {html}");
        assert!(
            html.contains(r##"placeholder="# Confirmations""##),
            "got: {html}"
        );
    }

    #[test]
    fn existing_thresholds_are_deleted_by_ticking_them_even_at_the_limit() {
        let store = StoreSettingsData {
            confirmation_thresholds: vec![ConfirmationThresholdView {
                id: "threshold_1".to_string(),
                unit_amount: "50.00".to_string(),
                confirmations_required: 20,
            }],
            confirmation_thresholds_at_max: true,
            ..base_store()
        };
        let html = render(store);
        assert!(html.contains("Maximum of 5 custom thresholds reached"));
        assert!(
            html.contains(r#"<input type="checkbox" name="delete_threshold_1">"#),
            "got: {html}"
        );
        assert!(!html.contains("new_unit_amount"), "{html}");
    }

    /// The Verified domains form is a settings field: its name, its help,
    /// then the input, described by the help.
    #[test]
    fn the_verified_domains_form_uses_the_settings_field() {
        let html = page(
            &chrome(),
            &StoreSettingsViewModel {
                store: Some(base_store()),
            },
        )
        .into_string();
        assert!(
            html.contains(r#"<mk-setting class="setting-field"><div class="setting-label-row"><label class="setting-label" for="new-domain">Domain</label><span class="changed-mark">changed</span></div><p class="field-help" id="new-domain-help">Just the domain, like shop.example. Its subdomains are covered too.</p><input type="text" name="domain" id="new-domain" aria-describedby="new-domain-help""#),
            "{html}"
        );
        assert!(html.contains(
            r#"<div class="form-actions"><button type="submit">Add domain</button></div>"#
        ));
    }

    #[test]
    fn says_when_the_webhooks_could_not_be_read() {
        let mut store = base_store();
        store.webhooks.unavailable = true;
        let html = page(&chrome(), &StoreSettingsViewModel { store: Some(store) }).into_string();
        assert!(html.contains("read this store's webhooks"), "{html}");
        assert!(!html.contains("No webhooks yet."));
        assert!(html.contains("Add a webhook"), "the form still works");
    }

    #[test]
    fn shows_the_webhook_signing_secret_exactly_once_after_creation() {
        let mut store = base_store();
        store.webhooks.created_secret = Some("whsec_abc123".to_string());
        store.outcome = Some(StoreOutcome::Saved(vec![StoreSection::Webhooks]));
        let html = page(&chrome(), &StoreSettingsViewModel { store: Some(store) }).into_string();
        assert!(html.contains("whsec_abc123"));
        assert!(html.contains("Webhook created"));
    }
}
