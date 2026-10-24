//! `GET /stores/{id}/settings` -
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

use super::settings::{plain_card, toast_region, Card, Field, Save, Toast, ToastKind};
use super::PageChrome;
use ui::{Choice, Select};

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

pub struct StoreSettingsData {
    /// The zone the page shows times in (the signed-in user's).
    pub clock: super::time::Clock,
    pub connection_id: crate::db::ConnectionId,
    pub display_name: String,
    /// Its site's host; empty when it has none.
    pub site: String,
    /// What its payments are for, shown on the checkout when an order has
    /// no description of its own; empty when it has none.
    pub payment_description: String,
    /// The website and the plugins connected to the store
    /// (`views::store_site`).
    pub sites: super::store_site::SiteView,
    /// The devices connected to the store (`views::connected_apps`).
    pub apps: super::connected_apps::AppsView,
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
    pub embed_domains: Vec<super::domains::DomainView>,
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
    /// The tab shown ([`StoreTab::pick`]).
    pub tab: StoreTab,
}

/// "Wallet": the wallet a store takes payments into, the others it could
/// change to, and the ones it used before.
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

/// The page's tabs, each a page of its own (`?tab=`), each the cards of one
/// job: the store itself, how it takes payments, where they go, and what
/// it's connected to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreTab {
    General,
    Payments,
    Wallet,
    Connections,
}

impl StoreTab {
    pub const ALL: [StoreTab; 4] = [
        StoreTab::General,
        StoreTab::Payments,
        StoreTab::Wallet,
        StoreTab::Connections,
    ];

    /// What `?tab=` names it by.
    pub fn id(self) -> &'static str {
        match self {
            StoreTab::General => "general",
            StoreTab::Payments => "payments",
            StoreTab::Wallet => "wallet",
            StoreTab::Connections => "connections",
        }
    }

    fn label(self) -> &'static str {
        match self {
            StoreTab::General => "General",
            StoreTab::Payments => "Payments",
            StoreTab::Wallet => "Wallet",
            StoreTab::Connections => "Connections",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tab| tab.id() == id)
    }

    /// The tab a card is on.
    pub fn of(section: StoreSection) -> Self {
        match section {
            StoreSection::Store | StoreSection::BaseCurrency | StoreSection::Diagnostics => {
                StoreTab::General
            }
            StoreSection::Confirmations | StoreSection::FxProvider => StoreTab::Payments,
            StoreSection::Wallet | StoreSection::KeyStorage => StoreTab::Wallet,
            StoreSection::Connections | StoreSection::Domains | StoreSection::Webhooks => {
                StoreTab::Connections
            }
        }
    }

    /// The tab to show: the one asked for, else the one the save or action
    /// the page answers was on (so it lands where it was done), else
    /// General.
    pub fn pick(asked: Option<StoreTab>, outcome: Option<&StoreOutcome>) -> Self {
        let done = match outcome {
            Some(StoreOutcome::Saved(sections)) => sections.first().copied(),
            Some(StoreOutcome::Refused { section, .. }) => Some(*section),
            _ => None,
        };
        asked
            .or(done.map(StoreTab::of))
            .unwrap_or(StoreTab::General)
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
            (ui::Alert::new(message, ui::Tone::Error))
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
    let action = crate::base::url(format!(
        "/stores/{}/settings/key-custody",
        store.connection_id
    ));
    plain_card(
        StoreSection::KeyStorage.id(),
        "Key storage",
        html! {
            (action_error(store, StoreSection::KeyStorage))
            p { strong { "Kept: " } (key_storage.current) }
            (ui::Hint::new("The keys belong to this store's wallet: moving them moves the wallet, for every store that uses it."))
            @if key_storage.current_disabled {
                (ui::Alert::new(
                    "This way of storing keys has been turned off on this instance, so payments to this store aren't \
                     being detected. Move the keys below to start again.",
                    ui::Tone::Error,
                )
                .quiet(true))
            }
            form method="post" action=(action) {
                label {
                    "Move to"
                    (Select::new(
                        "backend",
                        key_storage
                            .move_to
                            .iter()
                            .map(|choice| Choice::new(&choice.backend, &choice.label).selected(choice.selected))
                            .collect::<Vec<_>>(),
                    ))
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
                (ui::FormActions::new(ui::Button::new("Move keys").kind(ui::ButtonType::Submit)))
            }
        },
    )
}

/// "Wallet": which wallet the store takes payments into, as a full-width
/// dropdown of the account's wallets (the current one marked Current, since
/// when in the help), and, collapsed, the wallets it used before. Picking
/// another wallet asks first, saying what happens to the orders already
/// open: an action of its own, not part of the settings form. With
/// JavaScript the pick posts at once; without, Change wallet does.
fn wallet_card(store: &StoreSettingsData) -> Markup {
    let wallet = &store.wallet;
    let action = crate::base::url(format!("/stores/{}/settings/wallet", store.connection_id));
    let picked = wallet
        .pending
        .as_ref()
        .map(|p| p.wallet_id.as_str())
        .or(wallet.current.as_ref().map(|c| c.id.as_str()));
    let confirm = |text: String| {
        ui::Button::new(text)
            .variant(ui::ButtonVariant::Primary)
            .kind(ui::ButtonType::Submit)
            .attr("name", "confirm")
            .attr("value", "yes")
    };
    plain_card(
        StoreSection::Wallet.id(),
        "Wallet",
        html! {
            (action_error(store, StoreSection::Wallet))
            @if let Some(changed) = &wallet.changed {
                (ui::Alert::new(changed.as_str(), ui::Tone::Success))
            }
            // Picking a wallet sends it at once (static/fx-glue.js), to ask.
            form method="post" action=(action) fx-submit-on-change {
                (ui::Setting::new(
                    "Wallet",
                    "store-wallet",
                    Select::new(
                        "wallet_id",
                        picked
                            .is_none()
                            .then(|| Choice::prompt("Choose a wallet…", true))
                            .into_iter()
                            .chain(wallet.choices.iter().map(|choice| wallet_choice(choice, picked)))
                            .collect::<Vec<_>>(),
                    )
                    .id("store-wallet")
                    .required(true)
                    .attr("aria-describedby", "store-wallet-help"),
                )
                .help(html! {
                    @if let Some(current) = &wallet.current {
                        "New payments go into this wallet, since " (store.clock.time(current.since))
                        ". A store can only change to a wallet on its own network."
                    } @else {
                        "This store isn't linked to one of your wallets yet. Pick the one it takes payments into."
                    }
                })
                .help_id("store-wallet-help")
                .layout(ui::SettingLayout::Stacked))
                @if let (Some(pending), Some(current)) = (&wallet.pending, &wallet.current) {
                    div class="change-confirm" role="status" {
                        h3 { "Change to " (pending.wallet_name) "?" }
                        p {
                            "New orders take payments into " strong { (pending.wallet_name) } ". "
                            (open_orders_line(pending.open_orders, &current.name))
                        }
                        (ui::FormActions::new(confirm(format!("Change to {}", pending.wallet_name))))
                    }
                } @else if let Some(pending) = &wallet.pending {
                    div class="change-confirm" role="status" {
                        h3 { "Take payments into " (pending.wallet_name) "?" }
                        (ui::FormActions::new(confirm(format!("Use {}", pending.wallet_name))))
                    }
                } @else {
                    // With JavaScript, picking a wallet sends it.
                    (ui::FormActions::new(ui::Button::new("Change wallet").kind(ui::ButtonType::Submit))
                        .attr("data-only", "no-script"))
                }
            }
            @if !wallet.history.is_empty() {
                (ui::Disclosure::new(
                    format!("Wallet history ({})", wallets_label(wallet.history.len())),
                    ui::Table::new(html! {
                            @for period in &wallet.history {
                                tr class=[period.until.is_none().then_some("current")] aria-current=[period.until.is_none().then_some("true")] {
                                    td {
                                        @match (&period.wallet_id, &period.wallet_name) {
                                            (Some(id), Some(name)) => {
                                                a href=(crate::base::url(format!("/account/wallets/{id}"))) { (name) }
                                                @if period.wallet_retired { " " (ui::Tag::new("Retired").tone(ui::TagTone::Unknown)) }
                                            },
                                            _ => span class="muted" { "A deleted wallet" },
                                        }
                                    }
                                    td { (store.clock.time(period.from)) }
                                    td { @match period.until { Some(until) => (store.clock.time(until)), None => "now" } }
                                    td class="num" { (period.orders) }
                                }
                            }
                    })
                    .head(html! { tr { th { "Wallet" } th { "From" } th { "Until" } th class="num" { "Orders" } } })
                    .scroll(false),
                )
                .attr("data-disclosure", "wallet-history"))
            }
        },
    )
}

fn wallet_choice(choice: &WalletChoice, picked: Option<&str>) -> Choice {
    let note = match (choice.current, choice.other_stores) {
        (true, 0) => "this store only".to_owned(),
        (_, 0) => "no other stores".to_owned(),
        (_, 1) => "1 other store".to_owned(),
        (_, n) => format!("{n} other stores"),
    };
    Choice::new(&choice.id, &choice.name)
        .detail(&choice.short_address)
        .network(&choice.network)
        .current(choice.current)
        .note(note)
        .selected(picked == Some(choice.id.as_str()))
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

/// "Store": its name, its website, optional, and its payment description,
/// optional. Any store takes payments at the till; a website lets it show
/// the checkout on its pages and connect a shop plugin.
fn store_card(store: &StoreSettingsData) -> Markup {
    let name = store.shown("store_name", &store.display_name);
    let site = store.shown("store_site", &store.site);
    let payment_description = store.shown("payment_description", &store.payment_description);
    settings_card(
        store,
        StoreSection::Store,
        "Store",
        html! {
            (Field::new("Store name", "store-name")
                .help(None, html! { "Shown on your dashboard, the till and the checkout." })
                .render(html! {
                    (ui::Input::new("store_name")
                        .id("store-name")
                        .value(name.as_str())
                        .required(true)
                        .attr("maxlength", crate::stores::MAX_NAME_LEN.to_string())
                        .maybe_attr("data-saved", store.saved_attr(&store.display_name)))
                }))
            (super::store_site::website_field(&store.sites, &site))
            (Field::new("Payment description (optional)", "payment-description")
                .help(None, html! {
                    "Shown to the payer under your store's name when an order has no description of its own. "
                    "Up to 140 characters. Anyone with a payment link can read it."
                })
                .render(html! {
                    (ui::Input::new("payment_description")
                        .id("payment-description")
                        .value(payment_description.as_str())
                        .attr("maxlength", crate::stores::MAX_DESCRIPTION_CHARS.to_string())
                        .maybe_attr("data-saved", store.saved_attr(&store.payment_description)))
                }))
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
                "What the confirmation tiers' amounts (on the Payments tab) are in. Changing this deletes every "
                "custom tier this store has - an amount in a currency you're no longer using means nothing."
            })
            .render(html! {
                (Select::new(
                    "base_currency",
                    store
                        .base_currency_options
                        .iter()
                        .map(|opt| Choice::new(&opt.code, &opt.description).detail(&opt.code).selected(opt.code == shown))
                        .collect::<Vec<_>>(),
                )
                .id("base-currency")
                .maybe_attr("data-saved", store.saved_attr(&store.base_currency)))
            }),
    )
}

/// About how long `confirmations` blocks take: one every 2 minutes.
fn wait(confirmations: u64) -> String {
    match confirmations * 2 {
        0 => "no wait".to_string(),
        minutes if minutes < 90 => format!("about {minutes} min"),
        minutes => format!("about {} h", (minutes + 30) / 60),
    }
}

/// The tiers, smallest amount first under the default: each removable, and
/// a blank one to add while there's room (one per save).
fn confirmation_tiers(store: &StoreSettingsData) -> Markup {
    let currency = store.base_currency.as_str();
    let mut tiers: Vec<&ConfirmationThresholdView> = store.confirmation_thresholds.iter().collect();
    tiers.sort_by(|a, b| {
        let amount =
            |t: &ConfirmationThresholdView| t.unit_amount.parse::<f64>().unwrap_or(f64::MAX);
        amount(a).total_cmp(&amount(b))
    });
    let confirmations = |n: u64| {
        if n == 1 {
            "1 confirmation".to_string()
        } else {
            format!("{n} confirmations")
        }
    };
    let below = tiers.first().map_or_else(
        || "Every order".to_string(),
        |lowest| format!("Under {} {currency}", lowest.unit_amount),
    );
    let rows = tiers
        .iter()
        .map(|tier| {
            let name = format!("delete_{}", tier.id);
            ui::EditorRow {
                label: format!("the {} {currency} tier", tier.unit_amount),
                fields: html! {
                    strong { "From " (tier.unit_amount) " " (currency) }
                    span { (confirmations(tier.confirmations_required)) }
                    span class="muted" { (wait(tier.confirmations_required)) }
                }
                .into(),
                removed: store.ticked(&name, false),
                name,
            }
        })
        .collect::<Vec<_>>();
    let new_row = (!store.confirmation_thresholds_at_max).then(|| {
        html! {
            "From"
            (ui::Input::new("new_unit_amount")
                .value(store.shown("new_unit_amount", ""))
                .label(format!("New tier's amount ({currency})"))
                .attr("inputmode", "decimal")
                .maybe_attr("data-saved", store.saved_attr("")))
            (currency) " need"
            (ui::Input::new("new_confirmations_required")
                .value(store.shown("new_confirmations_required", ""))
                .label("New tier's confirmations")
                .attr("data-input", "confirmations")
                .attr("size", "3")
                .attr("maxlength", "3")
                .attr("inputmode", "numeric")
                .maybe_attr("data-saved", store.saved_attr("")))
            "confirmations"
        }
    });
    let count = store.confirmation_thresholds.len();
    let note = if store.confirmation_thresholds_at_max {
        "5 of 5 tiers. Remove one to add another.".to_string()
    } else {
        format!("{count} of 5 tiers.")
    };
    // The ladder as a picture: the default from nothing, then each tier
    // from its amount.
    let scale = ui::StepScale::new(
        std::iter::once(ui::ScaleStep {
            label: format!("{} conf", store.confirmations_required),
            from: format!("0 {currency}"),
        })
        .chain(tiers.iter().map(|tier| ui::ScaleStep {
            label: format!("{} conf", tier.confirmations_required),
            from: tier.unit_amount.clone(),
        }))
        .collect::<Vec<_>>(),
    )
    .end("and up")
    .attr("data-scale", "tiers");
    let mut editor = ui::RowsEditor::new("Confirmation tiers", rows)
        .add_label("Add a tier")
        .note(note)
        .attr("data-list", "tiers");
    if let Some(new_row) = new_row {
        editor = editor.new_row(new_row);
    }
    html! {
        (scale)
        p class="tier-default" {
            strong { (below) } ": the default, "
            (confirmations(store.confirmations_required)) " "
            span class="muted" { "(" (wait(store.confirmations_required)) ")" }
        }
        (editor)
    }
}

/// "Confirmation thresholds": the default, accepting unconfirmed payments,
/// and the tiers by amount (each can be removed, and one added, per save).
fn confirmations_card(store: &StoreSettingsData) -> Markup {
    let default = store.confirmations_required.to_string();
    let zero_conf = store.ticked("zero_conf_enabled", store.zero_conf_enabled);
    settings_card(
        store,
        StoreSection::Confirmations,
        "Confirmation thresholds",
        html! {
            (ui::Hint::new(
                "How many blocks a payment needs before this store's orders read as paid. The default \
                 is the fallback used whenever no custom threshold applies; custom thresholds let a higher-value \
                 order require more confirmations (or a lower-value one fewer) based on its amount in this store's base \
                 currency.",
            ))
            (Field::new("Default confirmations", "confirmations-required")
                .help(Some("confirmations-required-help"), html! { "Blocks a payment waits before an order counts as paid, from 0 to 720." })
                .render(html! {
                    (ui::Input::new("confirmations_required")
                        .id("confirmations-required")
                        .value(store.shown("confirmations_required", &default))
                        .described_by("confirmations-required-help")
                        .required(true)
                        .attr("data-input", "confirmations")
                        .attr("size", "3")
                        .attr("maxlength", "3")
                        .attr("inputmode", "numeric")
                        .maybe_attr("data-saved", store.saved_attr(&default)))
                }))
            (Field::new("Accept unconfirmed (0-conf) payments", "zero-conf")
                .help(Some("zero-conf-help"), html! {
                    "An order under the default can read as paid the moment its transaction reaches this store's node's "
                    "mempool, before any block confirms it - useful for fast, low-value, in-person sales. This is a real "
                    "double-spend risk (an attacker who can out-race the transaction to a miner keeps both the goods and the "
                    "coin). Add a custom threshold to keep larger orders requiring real confirmations."
                })
                .render(super::controls::switch("zero_conf_enabled", "zero-conf", zero_conf, Some("zero-conf-help"), store.saved_box(store.zero_conf_enabled).map(|s| s == "on"))))
            (Field::group("Confirmations by order amount", "confirmation-tiers")
                .help(Some("confirmation-tiers-help"), html! {
                    "Bigger orders can wait for more blocks before they count as paid. Each tier asks an order of at "
                    "least its amount, in " (store.base_currency) ", for its confirmations; smaller orders take the "
                    "default. A block comes about every 2 minutes. Up to 5 tiers."
                })
                .stacked()
                .render(confirmation_tiers(store)))
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
                (ui::Hint::new(
                    "Only XMR-denominated orders can be created until an admin of this Monokulo instance \
                     enables a provider (e.g. Coingecko).",
                ))
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
                        (ui::Table::new(html! {
                                @for (index, opt) in store.fx_provider_options.iter().enumerate() {
                                    @let use_name = format!("use_{}", opt.name);
                                    @let position_name = format!("position_{}", opt.name);
                                    @let position = (index + 1).to_string();
                                    tr {
                                        td {
                                            (ui::Checkbox::new()
                                                .name(use_name.as_str())
                                                .checked(store.ticked(&use_name, opt.selected))
                                                .label(format!("Use {}", opt.name))
                                                .maybe_attr("data-saved", store.saved_box(opt.selected)))
                                        }
                                        td { (opt.name) }
                                        td {
                                            (ui::Input::new(position_name.as_str())
                                                .kind(ui::InputType::Number)
                                                .value(store.shown(&position_name, &position))
                                                .label(format!("Preference order of {}", opt.name))
                                                .attr("min", "1")
                                                .attr("max", count.to_string())
                                                .maybe_attr("data-saved", store.saved_attr(&position)))
                                        }
                                    }
                                }
                        })
                        .head(html! { tr { th { "Use" } th { "Provider" } th { "Order" } } })
                        .scroll(false))
                    }))
                @if let Some(haveno) = &store.haveno_settings {
                    h4 { "Haveno (RetoSwap) limits" }
                    (ui::Hint::new(
                        "Haveno prices from a thin peer-to-peer order book, so it only quotes when the book \
                         meets the limits below; otherwise the next provider in your order is used. These \
                         only matter while Haveno is ticked above.",
                    ))
                    (haveno_field(store, crate::fx_provider_settings::HAVENO_CURRENCIES, "Currencies Haveno may quote", &haveno.currencies,
                        "Comma-separated. Leave empty to allow every currency.",
                        |input| input.placeholder("USD, EUR, GBP").autocomplete("off")))
                    (haveno_field(store, crate::fx_provider_settings::HAVENO_MAX_SPREAD_PCT, "Maximum spread (%)", &haveno.max_spread_pct,
                        "The largest gap between the best buy and sell offer, as a percentage of their midpoint.",
                        |input| number(input, "0.01", "100", "any")))
                    (haveno_field(store, crate::fx_provider_settings::HAVENO_MIN_OFFERS_PER_SIDE, "Minimum offers on each side", &haveno.min_offers_per_side,
                        "Buy offers and sell offers must each number at least this many.",
                        |input| number(input, "1", "1000", "1")))
                    (haveno_field(store, crate::fx_provider_settings::HAVENO_MIN_DEPTH_XMR_PER_SIDE, "Minimum XMR on each side", &haveno.min_depth_xmr_per_side,
                        "Total XMR offered on each side must be at least this much. 0 turns the check off.",
                        |input| number(input, "0", "1000000", "any")))
                }
            }
        },
    )
}

/// One of Haveno's limits: its input, named, valued and marked with what's
/// saved, finished by `control`.
fn haveno_field(
    store: &StoreSettingsData,
    name: &str,
    label: &str,
    saved: &str,
    help: &str,
    control: impl Fn(ui::Input) -> ui::Input,
) -> Markup {
    let id = name.replace('_', "-");
    let help_id = format!("{id}-help");
    let input = ui::Input::new(name)
        .id(id.as_str())
        .value(store.shown(name, saved))
        .maybe_attr("data-saved", store.saved_attr(saved));
    Field::new(label, &id)
        .help(Some(&help_id), html! { (help) })
        .render(html! { (control(input)) })
}

/// A number input between `min` and `max`, in steps of `step`.
fn number(field: ui::Input, min: &str, max: &str, step: &str) -> ui::Input {
    field
        .kind(ui::InputType::Number)
        .attr("min", min)
        .attr("max", max)
        .attr("step", step)
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
                "Turn this on while tracking down a problem with the POS, the checkout or a shop plugin. "
                "Script errors from this store's dashboard pages and checkout, a timeline of each POS session (connection "
                "drops, the app going to the background, orders created, backgrounded and completed), and the "
                "errors from a shop plugin whose \"Send errors to Monokulo\" option is on, then go to this "
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
        &format!("/stores/{}/settings", store.connection_id),
    )
}

/// The tabs, links to the page's own other tabs. A store with no wallet
/// can't take payments, and its Wallet tab says so.
fn tab_links(store: &StoreSettingsData) -> Markup {
    let links = StoreTab::ALL
        .into_iter()
        .map(|tab| ui::TabLink {
            href: crate::base::url(format!(
                "/stores/{}/settings?tab={}",
                store.connection_id,
                tab.id()
            )),
            label: tab.label().to_string(),
            current: tab == store.tab,
            count: (tab == StoreTab::Wallet && store.wallet.current.is_none())
                .then(|| "No wallet".to_string()),
        })
        .collect::<Vec<_>>();
    html! { (ui::TabLinks::new("Store settings", links)) }
}

/// The settings form of a tab: its cards, saved together with the save
/// bar. It says which tab it is, so a save that changed nothing comes back
/// to it.
fn settings_form(store: &StoreSettingsData, cards: Markup) -> Markup {
    super::settings::form(
        &format!("/stores/{}/settings", store.connection_id),
        Save::Reload,
        "Store settings",
        html! {
            input type="hidden" name="tab" value=(store.tab.id());
            (cards)
            (save_bar(store))
        },
    )
}

pub fn page(chrome: &PageChrome, data: &StoreSettingsViewModel) -> Markup {
    let header = match &data.store {
        Some(store) => ui::PageHeader::new("Settings").crumbs(super::store_crumbs(
            store.connection_id.as_str(),
            &store.display_name,
            false,
        )),
        None => ui::PageHeader::new("Store not found"),
    };
    let body = html! {
        @if let Some(store) = &data.store {
            (tab_links(store))
            @match store.tab {
                StoreTab::General => (settings_form(store, html! {
                    (store_card(store))
                    (base_currency_card(store))
                    (diagnostics_card(store))
                })),
                StoreTab::Payments => (settings_form(store, html! {
                    (confirmations_card(store))
                    (fx_provider_card(store))
                })),
                StoreTab::Wallet => {
                    (wallet_card(store))
                    @if let Some(key_storage) = &store.key_storage {
                        (key_storage_card(store, key_storage))
                    }
                },
                StoreTab::Connections => {
                    (super::store_site::connections_card(&store.sites))
                    (super::connected_apps::apps_card(&store.apps))
                    (verified_domains_card(store))
                    (webhooks_card(store))
                },
            }
            (toast_region(store.toast().as_ref(), false))
            (super::store_site::dialogs(&store.sites))
            (super::webhooks::dialogs(&store.webhooks))
        } @else {
            p { "This store doesn't exist, or isn't connected to your account." }
        }
    };
    let title = match &data.store {
        Some(store) => format!("Settings - {} - Monokulo", store.display_name),
        None => "Store not found - Monokulo".to_string(),
    };
    super::Page::new(title, header)
        .class("settings-page")
        .render(chrome, body)
}

/// Adding, checking and removing the domains this store has proved it owns
/// (`crate::embed_domains`, `views::domains`), and whether only they may
/// show its checkout: actions of their own, not part of the settings form
/// (turning the restriction on needs a verified domain first).
fn verified_domains_card(store: &StoreSettingsData) -> Markup {
    let store_ref = super::domains::StoreRef {
        id: store.connection_id.as_str(),
        name: &store.display_name,
    };
    plain_card(
        StoreSection::Domains.id(),
        "Verified domains",
        html! {
            (action_error(store, StoreSection::Domains))
            (super::domains::domains_body(&store_ref, &store.embed_domains, store.embed_restricted, store.embed_can_restrict))
            @if store.embed_domains.iter().any(|domain| domain.status != super::domains::DomainStatus::Verified) {
                (super::script("domains.js", super::Load::Defer))
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chrome() -> PageChrome {
        PageChrome::from_user(None, "/stores/conn_1/settings")
    }

    fn base_store() -> StoreSettingsData {
        StoreSettingsData {
            tab: StoreTab::General,
            clock: crate::views::time::Clock::utc(0),
            connection_id: shared::ids::ConnectionId::new("conn_1".to_string()),
            display_name: "shop.example.com".to_string(),
            site: "shop.example.com".to_string(),
            payment_description: String::new(),
            apps: crate::views::connected_apps::AppsView {
                store_id: "conn_1".into(),
                store_name: "shop.example.com".into(),
                apps: Vec::new(),
            },
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
                test: None,
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
            ..on(StoreTab::Payments)
        };
        page(&chrome(), &StoreSettingsViewModel { store: Some(store) }).into_string()
    }

    #[test]
    fn the_haveno_limits_are_shown_only_while_the_instance_offers_haveno() {
        let html = page(
            &chrome(),
            &StoreSettingsViewModel {
                store: Some(on(StoreTab::Payments)),
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
            html.contains(r#"name="haveno_currencies" id="haveno-currencies" value="USD, EUR""#),
            "got: {html}"
        );
        assert!(
            html.contains(r#"name="haveno_max_spread_pct" id="haveno-max-spread-pct" value="2.5""#),
            "got: {html}"
        );
        assert!(
            html.contains(
                r#"name="haveno_min_offers_per_side" id="haveno-min-offers-per-side" value="3""#
            ),
            "got: {html}"
        );
        assert!(
            html.contains(r#"name="haveno_min_depth_xmr_per_side" id="haveno-min-depth-xmr-per-side" value="1.5""#),
            "got: {html}"
        );
    }

    #[test]
    fn the_default_haveno_limits_render_as_plain_numbers() {
        let html = with_haveno(Default::default());
        assert!(
            html.contains(r#"name="haveno_currencies" id="haveno-currencies" value="""#),
            "got: {html}"
        );
        assert!(
            html.contains(r#"name="haveno_max_spread_pct" id="haveno-max-spread-pct" value="5""#),
            "got: {html}"
        );
        assert!(
            html.contains(
                r#"name="haveno_min_offers_per_side" id="haveno-min-offers-per-side" value="1""#
            ),
            "got: {html}"
        );
        assert!(
            html.contains(r#"name="haveno_min_depth_xmr_per_side" id="haveno-min-depth-xmr-per-side" value="0""#),
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

    /// The base store, on `tab`.
    fn on(tab: StoreTab) -> StoreSettingsData {
        StoreSettingsData {
            tab,
            ..base_store()
        }
    }

    fn render(store: StoreSettingsData) -> String {
        page(&chrome(), &StoreSettingsViewModel { store: Some(store) }).into_string()
    }

    #[test]
    fn diagnostics_are_a_switch_off_until_turned_on() {
        let html = render(base_store());
        assert!(
            html.contains(
                r#"<section id="card-diagnostics" class="settings-card" data-name="diagnostics""#
            ),
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
    /// Four tabs, each a page of its own: General and Payments each their
    /// settings form's cards with its save bar; Wallet and Connections the
    /// actions' cards. The current tab is marked.
    #[test]
    fn each_tab_shows_its_own_cards() {
        let tab = |tab: StoreTab| render(on(tab));
        let order = |html: &str, needles: &[&str]| {
            let at: Vec<usize> = needles
                .iter()
                .map(|needle| {
                    html.find(needle)
                        .unwrap_or_else(|| panic!("{needle} in {html}"))
                })
                .collect();
            assert!(
                at.windows(2).all(|pair| pair[0] < pair[1]),
                "{needles:?} in order: {html}"
            );
        };
        let form = r#"<form method="post" action="/stores/conn_1/settings" id="settings-form""#;

        let html = tab(StoreTab::General);
        assert!(html.contains(r#"<nav class="mk-tabs" aria-label="Store settings"><a href="/stores/conn_1/settings?tab=general" aria-current="page">General</a><a href="/stores/conn_1/settings?tab=payments">Payments</a><a href="/stores/conn_1/settings?tab=wallet">Wallet<span class="mk-tab-count">No wallet</span></a><a href="/stores/conn_1/settings?tab=connections">Connections</a></nav>"#), "{html}");
        order(
            &html,
            &[
                form,
                r#"<input type="hidden" name="tab" value="general">"#,
                r#"<section id="card-store""#,
                r#"<section id="card-base-currency""#,
                r#"<section id="card-diagnostics""#,
                r#"<div id="save-bar""#,
            ],
        );
        for other in [
            "card-confirmation-thresholds",
            "card-fx-provider",
            "card-wallet",
            "card-verified-domains",
            "card-webhooks",
        ] {
            assert!(
                !html.contains(&format!(r#"id="{other}""#)),
                "{other} on General: {html}"
            );
        }
        // The website's confirm dialogs, and one orange button: the bar's Save.
        assert_eq!(html.matches("<dialog").count(), 2, "{html}");
        assert_eq!(
            html.matches("btn-primary").count() + html.matches(r#"data-variant="primary""#).count(),
            1,
            "{html}"
        );
        assert!(
            html.contains(r#"<a class="mk-btn" href="/stores/conn_1/settings" data-discard-all"#),
            "{html}"
        );

        let html = tab(StoreTab::Payments);
        assert!(
            html.contains(
                r#"<a href="/stores/conn_1/settings?tab=payments" aria-current="page">Payments</a>"#
            ),
            "{html}"
        );
        order(
            &html,
            &[
                form,
                r#"<input type="hidden" name="tab" value="payments">"#,
                r#"<section id="card-confirmation-thresholds""#,
                r#"<section id="card-fx-provider""#,
                r#"<div id="save-bar""#,
            ],
        );
        assert!(!html.contains(r#"id="card-store""#), "{html}");

        let html = tab(StoreTab::Wallet);
        assert!(
            html.contains(r#"<section id="card-wallet" class="settings-card""#),
            "{html}"
        );
        assert!(!html.contains(form), "no settings form on Wallet: {html}");

        let html = tab(StoreTab::Connections);
        order(
            &html,
            &[
                r#"<section id="card-verified-domains" class="settings-card""#,
                r#"<section id="card-webhooks" class="settings-card""#,
            ],
        );
        assert!(
            !html.contains(form),
            "no settings form on Connections: {html}"
        );
    }

    /// A save or an action lands on its card's tab, unless a tab was asked
    /// for; a plain load is on General.
    #[test]
    fn the_tab_shown_is_the_one_asked_for_or_the_one_just_saved_on() {
        let saved = |sections: Vec<StoreSection>| Some(StoreOutcome::Saved(sections));
        assert_eq!(StoreTab::pick(None, None), StoreTab::General);
        assert_eq!(
            StoreTab::pick(None, saved(vec![StoreSection::Wallet]).as_ref()),
            StoreTab::Wallet
        );
        assert_eq!(
            StoreTab::pick(None, saved(vec![StoreSection::Webhooks]).as_ref()),
            StoreTab::Connections
        );
        assert_eq!(
            StoreTab::pick(
                None,
                Some(&StoreOutcome::Refused {
                    section: StoreSection::FxProvider,
                    message: String::new(),
                    saved: Vec::new()
                })
            ),
            StoreTab::Payments
        );
        assert_eq!(
            StoreTab::pick(
                Some(StoreTab::Payments),
                saved(vec![StoreSection::Wallet]).as_ref()
            ),
            StoreTab::Payments
        );
        assert_eq!(
            StoreTab::pick(None, Some(&StoreOutcome::Unchanged)),
            StoreTab::General
        );
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
            ..on(StoreTab::Payments)
        };
        let html = render(store);
        assert!(
            html.contains(
                r#"<section id="card-confirmation-thresholds" class="settings-card is-failed""#
            ),
            "{html}"
        );
        assert!(
            html.contains(r#"<div class="card-body"><p class="error" role="alert""#)
                && html.contains(">Enter a whole number of confirmations.</p>"),
            "{html}"
        );
        assert!(
            html.contains(
                r#"name="confirmations_required" id="confirmations-required" value="abc""#
            ),
            "{html}"
        );
        assert!(html.contains(r#"data-saved="10""#), "{html}");
        // The tier marked to remove stays marked.
        assert!(
            html.contains(
                r#"<input type="checkbox" name="delete_t1" value="remove" data-rows-remove="" checked>"#
            ),
            "{html}"
        );
        assert!(
            html.contains(r#"name="new_unit_amount" value="9""#),
            "{html}"
        );
        assert!(
            html.contains(r#"<div id="save-bar" class="save-bar is-failed""#),
            "{html}"
        );
        assert!(html.contains(r##"<strong>Nothing saved.</strong> Enter a whole number of confirmations. <a href="#card-confirmation-thresholds" data-show-card="confirmation-thresholds">Show</a>"##), "{html}");
        assert!(
            html.contains(r#"<div class="toast toast-error" role="alert" data-toast data-mk="1""#),
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
        assert_eq!(
            html.matches(r#"data-card-saved data-mk="4">Saved<"#)
                .count(),
            2,
            "{html}"
        );
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
            ..on(StoreTab::Connections)
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

    /// The restriction banner's form: its state, and the one button that
    /// changes it.
    fn banner(html: &str) -> &str {
        html.split(r#"<form id="embed-restriction""#)
            .nth(1)
            .and_then(|rest| rest.split("</form>").next())
            .unwrap_or_else(|| panic!("the restriction banner in {html}"))
    }

    #[test]
    fn the_restriction_banner_needs_a_verified_domain_to_turn_on() {
        let html = render(on(StoreTab::Connections));
        let off = banner(&html);
        assert!(
            off.contains(r#"<input type="hidden" name="restricted" value="on">"#),
            "{off}"
        );
        assert!(off.contains(r#"data-tone="warning""#), "{off}");
        assert!(
            off.contains("<strong>Any website can show this checkout.</strong>"),
            "{off}"
        );
        assert!(
            off.contains("A domain must be verified before you can allow only your own."),
            "{off}"
        );
        // Nothing verified: the button is the plain disabled one.
        let button = off.split("<button").last().unwrap();
        assert!(
            button.contains(" disabled") && !button.contains("data-variant"),
            "{button}"
        );

        let html = render(StoreSettingsData {
            embed_can_restrict: true,
            ..on(StoreTab::Connections)
        });
        let off = banner(&html);
        let button = off.split("<button").last().unwrap();
        assert!(
            button.contains(r#"data-variant="primary""#) && button.contains(r#"type="submit""#),
            "{button}"
        );
        assert!(!button.contains(" disabled"), "{button}");
        assert!(button.contains("Only allow my domains"), "{button}");

        let html = render(StoreSettingsData {
            embed_can_restrict: true,
            embed_restricted: true,
            ..on(StoreTab::Connections)
        });
        let on = banner(&html);
        assert!(on.contains(r#"data-tone="success""#), "{on}");
        assert!(
            on.contains("<strong>Only your verified domains can show this checkout.</strong>"),
            "{on}"
        );
        assert!(
            on.contains(r#"<input type="hidden" name="restricted" value="off">"#),
            "{on}"
        );
        // Allowing any website is a caution, and asks first.
        assert!(on.contains(r#"data-variant="caution""#), "{on}");
        assert!(on.contains(r#"data-opens-dialog="allow-any""#), "{on}");
        assert!(html.contains(r#"<dialog id="allow-any""#), "{html}");
        assert!(
            html.contains("Allow any website to show this checkout?"),
            "{html}"
        );
    }

    #[test]
    fn verified_domains_are_cards_with_their_actions_at_the_top() {
        let html = render(on(StoreTab::Connections));
        assert!(html.contains(r#"<section id="card-verified-domains" class="settings-card" aria-labelledby="card-verified-domains-title"><header class="card-head"><h3 id="card-verified-domains-title">Verified domains</h3>"#));
        assert!(html.contains("<strong>No domains yet</strong>"), "{html}");
        // With nothing verified, adding a domain is the card's primary.
        assert!(
            html.contains(r#"href="/stores/conn_1/settings/domains/new" data-variant="primary">"#),
            "{html}"
        );
        let script = crate::assets::url("domains.js");
        assert!(!html.contains(&script), "nothing is waiting: {html}");

        let domain = |id: &str, name: &str, status: super::super::domains::DomainStatus| {
            super::super::domains::DomainView {
                id: id.to_string(),
                domain: name.to_string(),
                status,
                meta: "Checked 3 h ago".to_string(),
                record_host: "_monokulo",
                record_value: format!("monokulo-verify=token-{id}"),
                reason: (status != super::super::domains::DomainStatus::Verified)
                    .then(|| "No TXT record yet.".to_string()),
                wait: None,
            }
        };
        let html = render(StoreSettingsData {
            embed_domains: vec![
                domain(
                    "d1",
                    "shop.example",
                    super::super::domains::DomainStatus::Verified,
                ),
                domain(
                    "d2",
                    "new.example",
                    super::super::domains::DomainStatus::Waiting,
                ),
            ],
            embed_can_restrict: true,
            ..on(StoreTab::Connections)
        });
        let card = |id: &str| {
            html.split(&format!(r#"id="domain-{id}""#))
                .nth(1)
                .and_then(|rest| rest.split("</article>").next())
                .unwrap_or_else(|| panic!("domain {id}'s card in {html}"))
                .to_string()
        };
        let verified = card("d1");
        assert!(
            !verified.contains("monokulo-verify=token-d1"),
            "a verified domain's record isn't shown: {verified}"
        );
        assert!(!verified.contains("/check"), "nothing to check: {verified}");
        assert!(
            verified.contains(r#"action="/stores/conn_1/settings/domains/d1/delete""#),
            "{verified}"
        );
        assert!(
            verified.contains(r#"data-variant="danger""#),
            "Remove is red: {verified}"
        );
        assert!(verified.contains(">Verified<"), "{verified}");

        let waiting = card("d2");
        assert!(waiting.contains(">Not verified<"), "{waiting}");
        assert!(waiting.contains("monokulo-verify=token-d2"), "{waiting}");
        assert!(waiting.contains("_monokulo"), "{waiting}");
        assert!(waiting.contains("No TXT record yet."), "{waiting}");
        // Check again swaps this card, and checks again by itself.
        assert!(
            waiting.contains(r#"fx-action="/stores/conn_1/settings/domains/d2/check""#),
            "{waiting}"
        );
        assert!(waiting.contains(r##"fx-target="#domain-d2""##), "{waiting}");
        assert!(waiting.contains("data-recheck"), "{waiting}");
        // The actions come before the meta line: in the card's head.
        assert!(
            waiting.find("Check again").unwrap() < waiting.find("Checked 3 h ago").unwrap(),
            "{waiting}"
        );
        assert!(html.contains(&script), "a domain is waiting: {html}");
        // Something is verified: adding another isn't the primary.
        assert!(
            html.contains(r#"<a class="mk-btn" href="/stores/conn_1/settings/domains/new">"#),
            "{html}"
        );
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
            html.contains(r#"<nav class="mk-breadcrumbs" aria-label="Breadcrumb"><a href="/stores/conn_1" title="shop.example.com">shop.example.com</a></nav><div class="mk-page-title"><h1>Settings</h1>"#),
            "expected a store link above the Settings heading, got: {html}"
        );
    }

    /// The default heads the tiers, in the same words, and has no remove:
    /// it's set in its own field.
    #[test]
    fn the_default_heads_the_tiers_without_a_remove() {
        let html = render(on(StoreTab::Payments));
        assert!(html.contains("Default confirmations"));
        assert!(html.contains(r#"value="10""#));
        assert!(
            html.contains(r#"<p class="tier-default"><strong>Every order</strong>: the default, 10 confirmations <span class="muted">(about 20 min)</span></p>"#),
            "{html}"
        );
    }
    #[test]
    fn accepting_unconfirmed_payments_is_a_switch() {
        let html = render(on(StoreTab::Payments));
        assert!(
            html.contains(r#"<input type="hidden" name="switches" value="zero_conf_enabled">"#),
            "{html}"
        );
        assert!(html.contains(r#"<input type="checkbox" role="switch" name="zero_conf_enabled" value="true" id="zero-conf" aria-describedby="zero-conf-help">"#), "{html}");
        let html = render(StoreSettingsData {
            zero_conf_enabled: true,
            ..on(StoreTab::Payments)
        });
        assert!(
            html.contains(r#"name="zero_conf_enabled" value="true" id="zero-conf" checked"#),
            "{html}"
        );
    }

    #[test]
    fn confirmations_required_input_is_narrow_and_capped_at_three_digits() {
        let html = render(on(StoreTab::Payments));
        assert!(html.contains(r#"name="confirmations_required" id="confirmations-required" value="10" aria-describedby="confirmations-required-help" required data-input="confirmations" size="3" maxlength="3" inputmode="numeric""#), "got: {html}");
    }

    /// A tier is added in the blank row, each control named for screen
    /// readers; the note says how many there are of how many.
    #[test]
    fn a_tier_is_added_in_the_blank_row() {
        let html = render(on(StoreTab::Payments));
        assert!(
            html.contains(r#"data-island="RowsEditor""#) && html.contains(r#"data-list="tiers""#),
            "{html}"
        );
        assert!(
            html.contains(r#"<li class="mk-rows-row mk-rows-new">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<input class="mk-input" type="text" name="new_unit_amount" value="" aria-label="New tier's amount (XMR)" inputmode="decimal">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"aria-label="New tier's confirmations""#),
            "{html}"
        );
        assert!(html.contains(">Add a tier</button>"), "{html}");
        assert!(
            html.contains(r#"<p class="mk-rows-note">0 of 5 tiers.</p>"#),
            "{html}"
        );
    }
    /// Tiers read smallest first under the default, each with about how
    /// long it waits, and each can be removed even at the limit, when
    /// there's no blank row to add another.
    #[test]
    fn tiers_read_smallest_first_and_can_be_removed_even_at_the_limit() {
        let tier = |id: &str, amount: &str, confirmations| ConfirmationThresholdView {
            id: id.to_string(),
            unit_amount: amount.to_string(),
            confirmations_required: confirmations,
        };
        let store = StoreSettingsData {
            confirmation_thresholds: vec![
                tier("threshold_2", "500", 30),
                tier("threshold_1", "50.00", 20),
            ],
            confirmation_thresholds_at_max: true,
            ..on(StoreTab::Payments)
        };
        let html = render(store);
        assert!(
            html.contains("<strong>Under 50.00 XMR</strong>: the default"),
            "{html}"
        );
        let small = html
            .find("<strong>From 50.00 XMR</strong>")
            .expect("the 50 tier");
        let big = html
            .find("<strong>From 500 XMR</strong>")
            .expect("the 500 tier");
        assert!(small < big, "{html}");
        assert!(
            html.contains(
                r#"<span>20 confirmations</span><span class="muted">about 40 min</span>"#
            ),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<span>30 confirmations</span><span class="muted">about 60 min</span>"#
            ),
            "{html}"
        );
        assert!(
            html.contains(r#"<input type="checkbox" name="delete_threshold_1" value="remove" data-rows-remove="">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"aria-label="Remove the 50.00 XMR tier""#),
            "{html}"
        );
        assert!(
            html.contains("5 of 5 tiers. Remove one to add another."),
            "{html}"
        );
        assert!(!html.contains("new_unit_amount"), "{html}");
    }
    #[test]
    fn says_when_the_webhooks_could_not_be_read() {
        let mut store = on(StoreTab::Connections);
        store.webhooks.unavailable = true;
        let html = page(&chrome(), &StoreSettingsViewModel { store: Some(store) }).into_string();
        assert!(html.contains("read this store's webhooks"), "{html}");
        assert!(!html.contains("No webhooks yet."));
        assert!(html.contains("Add a webhook"), "the form still works");
    }

    #[test]
    fn shows_the_webhook_signing_secret_exactly_once_after_creation() {
        let mut store = on(StoreTab::Connections);
        store.webhooks.created_secret = Some("whsec_abc123".to_string());
        store.outcome = Some(StoreOutcome::Saved(vec![StoreSection::Webhooks]));
        let html = page(&chrome(), &StoreSettingsViewModel { store: Some(store) }).into_string();
        assert!(html.contains("whsec_abc123"));
        assert!(html.contains("Webhook created"));
    }
}
