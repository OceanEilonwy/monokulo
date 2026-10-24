//! Setting up a store (`/setup`, `http::setup`): Store, then Wallet, then
//! Done. The store step asks where payments are taken, the store's name and
//! its site; the wallet step (`views::wallets`, shared with adding a wallet
//! from the Account page) opens into its own small steps; Done says what's
//! left before the store takes payments. Every screen works without
//! JavaScript except making a new wallet, whose recovery phrase is made in
//! the browser.

use maud::{html, Markup, PreEscaped};

use super::PageChrome;
use crate::plugins::{ConnectingPlugin, Plugin};
use crate::stores::Site;
use ui::{WizardState, WizardStep as Segment, WizardSteps};

/// The three steps of setup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Store,
    Wallet,
    Done,
}

/// Which way a wallet is added: made new in the browser, or brought in with
/// its keys. Each has its own small steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalletPath {
    New,
    Keys,
}

impl WalletPath {
    fn labels(self) -> &'static [&'static str] {
        match self {
            WalletPath::New => &["Kind", "Back up", "Check"],
            WalletPath::Keys => &["Kind", "Keys"],
        }
    }
}

/// Where in the wallet's own small steps a screen is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalletAt {
    pub path: WalletPath,
    /// Which of `path`'s steps, from 0.
    pub at: usize,
}

/// Setup progress uses completed tasks, not the index of the active screen.
/// Done is reached only once the store exists.
pub fn steps(at: Step, wallet: Option<WalletAt>) -> Markup {
    let all = [
        (Step::Store, "Store"),
        (Step::Wallet, "Wallet"),
        (Step::Done, "Done"),
    ];
    let current = all.iter().position(|(s, _)| *s == at).unwrap_or(0);
    let segments = all
        .iter()
        .enumerate()
        .map(|(i, (step, label))| {
            let complete = i < current || at == Step::Done;
            let state = if i == current {
                WizardState::Current
            } else if complete {
                WizardState::Done
            } else {
                WizardState::Next
            };
            match (complete, i == current, step, wallet) {
                (true, ..) => segment(label, "Complete".into(), 1, 1, state),
                (false, true, Step::Wallet, Some(w)) => wallet_segment(w),
                (false, true, ..) => segment(label, "In progress".into(), 0, 1, state),
                _ => segment(label, "Next".into(), 0, 1, state),
            }
        })
        .collect::<Vec<_>>();
    html! { (WizardSteps::new("Setup progress", segments)) }
}

/// The wallet's small steps on their own, for adding a wallet outside
/// setup (`/account/wallets/...`).
pub fn wallet_steps(wallet: WalletAt) -> Markup {
    html! { (WizardSteps::new("Wallet progress", vec![wallet_segment(wallet)])) }
}

fn segment(label: &str, detail: String, done: usize, tasks: usize, state: WizardState) -> Segment {
    Segment {
        label: label.into(),
        detail,
        done,
        tasks,
        state,
    }
}

/// The Wallet segment while on it: the task in hand and its position, the
/// bar counting the tasks before it.
fn wallet_segment(wallet: WalletAt) -> Segment {
    let labels = wallet.path.labels();
    let task = labels.get(wallet.at).copied().unwrap_or_default();
    segment(
        "Wallet",
        format!("{task} · step {} of {}", wallet.at + 1, labels.len()),
        wallet.at.min(labels.len()),
        labels.len(),
        WizardState::Current,
    )
}

/// The store step's answers, carried through the wallet step's screens
/// (as hidden fields and in links) until the store is made.
pub struct SetupContext {
    pub store_name: String,
    /// Every answer and the plugin's request, as form fields.
    pub fields: Vec<(&'static str, String)>,
}

impl SetupContext {
    /// The answers as a query string, for a link.
    pub fn query(&self) -> String {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        for (name, value) in &self.fields {
            query.append_pair(name, value);
        }
        query.finish()
    }

    /// The answers as hidden fields, for a form.
    pub fn hidden(&self) -> Markup {
        html! {
            @for (name, value) in &self.fields {
                input type="hidden" name=(name) value=(value);
            }
        }
    }
}

// -- Store ---------------------------------------------------------------------

/// Why the site can't be used.
pub enum SiteError {
    /// Not a web address.
    Invalid(String),
    /// One of the merchant's own stores already has it.
    Yours {
        id: String,
        name: String,
        site: String,
    },
    /// Another account's store has it.
    Someone { site: String },
}

pub struct StoreStepViewModel {
    pub name: String,
    pub site: String,
    /// The plugin and its shop, when a shop plugin sent the merchant here:
    /// the shop's site is the store's and can't be changed, and the
    /// plugin's fields are carried through.
    pub from_plugin: Option<ConnectingPlugin>,
    /// What the store prices in: the answer, or a guess from the browser.
    pub currency: String,
    pub currencies: Vec<crate::currencies::CurrencyOptionView>,
    pub carried: Vec<(&'static str, String)>,
    pub name_error: Option<String>,
    pub site_error: Option<SiteError>,
}

/// `GET`/`POST /setup`: the store's name and, optionally, its website.
/// Any store takes payments at the till; a website lets it show the
/// checkout on its pages and connect a shop plugin too.
pub fn store_page(chrome: &PageChrome, data: &StoreStepViewModel) -> Markup {
    let title = if let Some(from) = &data.from_plugin {
        format!("Connect your {} shop", from.plugin)
    } else {
        "Set up a store".to_owned()
    };
    let header = ui::PageHeader::new(title).progress(steps(Step::Store, None));
    let body = html! {
        @if let Some(from) = &data.from_plugin {
            p { "The " (from.plugin) " plugin on " strong { (from.site) } " asked to connect to your account. Name the store, then choose where its money goes." }
        }
        form method="post" action=(crate::base::url("/setup")) class="store-form" novalidate {
            @for (name, value) in &data.carried {
                input type="hidden" name=(name) value=(value);
            }
            div class="two-fields" {
                div class="setting-field" {
                    div class="setting-label-row" { label class="setting-label" for="store-name" { "Store name" } }
                    (field_help("Shown on your dashboard, the till and the checkout."))
                    (ui::Input::new("store_name")
                        .id("store-name")
                        .value(data.name.clone())
                        .autocomplete("organization")
                        .required(true)
                        .invalid(data.name_error.is_some())
                        .maybe_described_by(data.name_error.as_ref().map(|_| "store-name-error".to_string()))
                        .attr("maxlength", crate::stores::MAX_NAME_LEN.to_string()))
                    @if let Some(error) = &data.name_error {
                        (ui::FieldNote::new(html! { "✕ " (error) }).tone(ui::NoteTone::Problem).id("store-name-error"))
                    }
                }
                div class="setting-field" {
                    div class="setting-label-row" { label class="setting-label" for="store-currency" { "Prices in" } }
                    (field_help("What the till and your orders charge in. You can change it later in the store's settings."))
                    (ui::Select::new(
                        "base_currency",
                        data.currencies
                            .iter()
                            .map(|c| ui::Choice::new(&c.code, &c.description).detail(&c.code).selected(c.code == data.currency))
                            .collect::<Vec<_>>(),
                    )
                    .id("store-currency"))
                }
            }
            div class="setting-field site-field" {
                div class="setting-label-row" {
                    label class="setting-label" for="store-site" { "Your website" }
                    @if data.from_plugin.is_none() { (ui::Hint::new("(optional)").inline(true)) }
                }
                (field_help("Needed for the checkout on your pages or for a shop plugin. Leave it empty to take payments at the till only."))
                (ui::Input::new("store_site")
                    .id("store-site")
                    .value(data.site.clone())
                    .placeholder("shop.example")
                    .autocomplete("url")
                    .readonly(data.from_plugin.is_some())
                    .invalid(data.site_error.is_some())
                    .maybe_described_by(data.site_error.as_ref().map(|_| "store-site-error".to_string()))
                    .attr("inputmode", "url"))
                @match &data.site_error {
                    Some(SiteError::Invalid(why)) => (ui::FieldNote::new(html! { "✕ " (why) }).tone(ui::NoteTone::Problem).id("store-site-error")),
                    Some(SiteError::Yours { id, name, site }) => (ui::FieldNote::new(html! { "✕ Your store " a href=(crate::base::url(format!("/stores/{id}"))) { (name) } " already uses " (site) ". Open it, or use a different site." }).tone(ui::NoteTone::Problem).id("store-site-error")),
                    Some(SiteError::Someone { site }) => (ui::FieldNote::new(html! { "✕ " (site) " is already connected to Monokulo here. If it's yours, ask the person who connected it." }).tone(ui::NoteTone::Problem).id("store-site-error")),
                    None => {}
                }
            }
            (ui::Hint::new(
                "Checkouts on any page of the site work. Subdomains (pay.shop.example) work too, once you verify the domain in the store's settings."
            ))
            div class="step-foot" {
                (ui::LinkButton::new("Cancel", crate::base::url("/")))
                span class="spacer" {}
                (ui::Button::new("Next").variant(ui::ButtonVariant::Primary).kind(ui::ButtonType::Submit))
            }
        }
    };
    super::Page::new("Set up a store - Monokulo", header)
        .class("setup-store")
        .render(chrome, body)
}

/// What a field on the store step is for, between its label and its input.
fn field_help(text: &str) -> ui::FieldNote {
    ui::FieldNote::new(text)
}

// -- Done ----------------------------------------------------------------------

/// The store that was just made, and what it takes payments into.
pub struct DoneViewModel {
    pub store_id: String,
    pub name: String,
    /// Its site, if it has one.
    pub site: Option<Site>,
    /// Whether any order has been made for it yet: until then, a store
    /// with a site isn't taking payments on it.
    pub has_orders: bool,
    pub public_key: String,
    pub base_currency: String,
    pub wallet_name: String,
    pub wallet_network: String,
    /// The wallet's recovery phrase wasn't backed up.
    pub skipped_backup: bool,
    /// The plugin that asked to connect, to go back to with the store's key.
    pub plugin: Option<PluginReturn>,
}

/// Going back to the plugin: a form that mints its key
/// (`POST /plugins/connect/{platform}`).
pub struct PluginReturn {
    pub plugin: Plugin,
    pub site_url: String,
    pub return_url: String,
    pub nonce: String,
}

fn settings_link(id: &str) -> String {
    crate::base::url(format!("/stores/{id}/settings"))
}

/// What the store is set up with, as facts: whether it takes payments yet
/// (only said while a site has no orders, so nothing claims it does), the
/// wallet and its network, the currency it prices in, and where to change
/// them.
fn summary(data: &DoneViewModel) -> Markup {
    let fact = |term: &str, value: Markup| ui::Fact {
        term: term.into(),
        value: value.into(),
    };
    let not_yet = data.site.is_some() && !data.has_orders;
    let facts: Vec<ui::Fact> = not_yet
        .then(|| {
            fact(
                "Status",
                html! { (ui::Tag::new("Not taking payments yet").tone(ui::TagTone::Unknown).attr("data-not-yet", "")) },
            )
        })
        .into_iter()
        .chain([
            fact(
                "Wallet",
                html! { a href=(crate::base::url("/account?tab=wallets")) { (data.wallet_name) } " " (super::network_badge(&data.wallet_network)) },
            ),
            fact("Prices in", html! { (data.base_currency) }),
        ])
        .collect();
    html! {
        (ui::Facts::new(facts).attr("data-store-summary", ""))
        (ui::FormActions::new(
            ui::LinkButton::new(
                html! { "Change in the store's settings" (PreEscaped(ui::assets::icons::ARROW_RIGHT)) },
                settings_link(&data.store_id),
            )
            .variant(ui::ButtonVariant::Quiet)
            .size(ui::ControlSize::Compact),
        ))
    }
}

/// A form back to the plugin: the button mints the store's key and sends
/// it there.
pub fn plugin_return_form(store_id: &str, plugin: &PluginReturn, label: &str) -> Markup {
    html! {
        form method="post" action=(crate::base::url(format!("/plugins/connect/{}", plugin.plugin.slug()))) class="inline-form" {
            input type="hidden" name="connection_id" value=(store_id);
            input type="hidden" name="site_url" value=(plugin.site_url);
            input type="hidden" name="return_url" value=(plugin.return_url);
            input type="hidden" name="nonce" value=(plugin.nonce);
            @if let Some(name) = plugin.plugin.name() {
                input type="hidden" name="name" value=(name.as_str());
            }
            (ui::Button::new(label).variant(ui::ButtonVariant::Primary).kind(ui::ButtonType::Submit))
        }
    }
}

/// `GET /setup/done/{id}`: every way the store can start taking payments,
/// its state deciding what's ready. The checkout needs a site; the plugin
/// goes back to its shop when it sent the merchant here; the till is
/// always there. One orange button: Back to the plugin's shop from it,
/// else Open the guide when there's a site, else Open the till. Nothing
/// claims the store takes payments: a site with no orders yet says it
/// doesn't.
pub fn done_page(chrome: &PageChrome, data: &DoneViewModel) -> Markup {
    let id = &data.store_id;
    let site = crate::stores::stored_site(data.site.as_ref());
    let has_site = data.site.is_some();
    let primary = |on: bool| on.then_some(ui::ButtonVariant::Primary);
    let till_primary = data.plugin.is_none() && !has_site;
    let guide_primary = data.plugin.is_none() && has_site;
    let title = if data.plugin.is_some() {
        "is connected"
    } else {
        "is set up"
    };
    let header =
        ui::PageHeader::new(html! { (data.name) " " (title) }).progress(steps(Step::Done, None));
    let body = html! {
        (summary(data))
        @if data.skipped_backup { (skipped_warning()) }
        ul class="todo" {
            li {
                p {
                    strong { "Add the checkout to your site" }
                    @if has_site {
                        (ui::Hint::new(format!("A script tag and a button on {}, or a call from your server. The guide shows both, and how to verify the domain.", site)).inline(true))
                    } @else {
                        (ui::Hint::new("It runs on your website: add your site in the store's settings first.").inline(true))
                    }
                }
                @if has_site {
                    (ui::LinkButton::new(html! { "Open the guide " (super::external_link_icon()) }, super::DOCS_URL)
                        .maybe_variant(primary(guide_primary))
                        .attr("target", "_blank")
                        .attr("rel", "noopener"))
                } @else {
                    (ui::LinkButton::new("Add your site", format!("{}#card-store", settings_link(id))))
                }
            }
            li {
                @if let Some(plugin) = &data.plugin {
                    @let name = plugin.plugin.to_string();
                    p {
                        strong { "Connect " (name) }
                        (ui::Hint::new(format!(
                            "Go back to {name} to give the plugin on {} its key. Monero then shows at checkout once you \
                             turn it on in the shop's payment settings.",
                            site
                        )).inline(true))
                    }
                    (plugin_return_form(id, plugin, &format!("Back to {name}")))
                } @else {
                    p {
                        strong { "Connect a shop plugin" }
                        (ui::Hint::new(
                            "Install Monokulo's plugin for your shop, then connect it from the shop's payment settings. \
                             It finds this store by its site."
                        ).inline(true))
                    }
                    (ui::LinkButton::new(html! { "Find your plugin " (super::external_link_icon()) }, super::docs_url("plugins/"))
                        .attr("target", "_blank")
                        .attr("rel", "noopener"))
                }
            }
            li {
                p {
                    strong { "Open the till (POS)" }
                    (ui::Hint::new("On the phone or tablet you take payments with. Customers scan a QR code.").inline(true))
                }
                (ui::LinkButton::new("Open the till", crate::base::url(format!("/stores/{id}/pos"))).maybe_variant(primary(till_primary)))
            }
        }
        @if has_site { (public_key_help(&data.public_key)) }
        div class="step-foot" {
            (ui::LinkButton::new("Store settings", settings_link(id)))
            span class="spacer" {}
            a href=(crate::base::url(format!("/stores/{id}"))) { "Go to " (data.name) "'s page" }
        }
    };
    super::Page::new(format!("{} is set up - Monokulo", data.name), header)
        .class("setup-done")
        .render(chrome, body)
}

fn skipped_warning() -> Markup {
    html! {
        (ui::Alert::new(
            "The wallet's recovery phrase was not backed up. Payments to it can't be spent unless you have it.",
            ui::Tone::Warning,
        )
        .quiet(true))
    }
}

fn public_key_help(public_key: &str) -> Markup {
    html! {
        (ui::Disclosure::new("Your store's public key", html! {
            pre { (public_key) }
            (ui::Hint::new("The guide tells you where it goes. It's safe to publish: it names the store and can't move money."))
        })
        .look(ui::DisclosureLook::Boxed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chrome() -> PageChrome {
        PageChrome::from_user(None, "/setup")
    }

    #[test]
    fn the_steps_never_say_done_before_the_store_exists() {
        let store = steps(Step::Store, None).into_string();
        assert!(
            store.contains(
                r#"<li aria-current="step"><span>Store<small>In progress</small></span>"#
            ),
            "{store}"
        );
        assert!(!store.contains(r#"class="done""#));
        assert!(!store.to_lowercase().contains("ready"));
        let wallet = steps(
            Step::Wallet,
            Some(WalletAt {
                path: WalletPath::New,
                at: 1,
            }),
        )
        .into_string();
        assert!(wallet.contains(r#"<li class="done"><span>Store<small>Complete</small></span>"#));
        assert!(wallet.contains(r#"<li aria-current="step"><span>Wallet"#));
        assert!(wallet.contains("<small>Back up · step 2 of 3</small>"));
        assert!(!wallet.contains("sub-timeline"));
        assert!(!wallet.contains("sub-crumbs"));
        assert!(wallet.contains(r#"<li><span>Done<small>Next</small></span>"#));
    }

    #[test]
    fn progress_counts_completed_wallet_tasks() {
        for (path, total) in [(WalletPath::New, 3), (WalletPath::Keys, 2)] {
            for at in 0..total {
                let html = steps(Step::Wallet, Some(WalletAt { path, at })).into_string();
                assert!(html.contains(&format!("value=\"{at}\" max=\"{total}\" aria-label=\"Wallet: {at} of {total} tasks complete\"")));
            }
        }
        let done = steps(Step::Done, None).into_string();
        assert_eq!(done.matches("value=\"1\" max=\"1\"").count(), 3);
        let start = steps(Step::Store, None).into_string();
        assert_eq!(start.matches("value=\"0\" max=\"1\"").count(), 3);
    }

    fn store_data() -> StoreStepViewModel {
        StoreStepViewModel {
            name: "Bakery".into(),
            currency: "XMR".into(),
            currencies: Vec::new(),
            site: "shop.example".into(),
            from_plugin: None,
            carried: vec![],
            name_error: None,
            site_error: None,
        }
    }

    #[test]
    fn a_site_one_of_your_stores_has_links_to_it_and_someone_elses_does_not() {
        let html = store_page(
            &chrome(),
            &StoreStepViewModel {
                site_error: Some(SiteError::Yours {
                    id: "c_1".into(),
                    name: "Corner shop".into(),
                    site: "shop.example".into(),
                }),
                ..store_data()
            },
        )
        .into_string();
        assert!(
            html.contains(
                r#"Your store <a href="/stores/c_1">Corner shop</a> already uses shop.example."#
            ),
            "{html}"
        );
        let html = store_page(
            &chrome(),
            &StoreStepViewModel {
                site_error: Some(SiteError::Someone {
                    site: "shop.example".into(),
                }),
                ..store_data()
            },
        )
        .into_string();
        assert!(html.contains("shop.example is already connected to Monokulo here."));
        assert!(!html.contains("/stores/"));
    }
}
