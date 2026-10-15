//! Setting up a store (`/setup`, `http::setup`): Store, then Wallet, then
//! Done. The store step asks where payments are taken, the store's name and
//! its site; the wallet step (`views::wallets`, shared with adding a wallet
//! from the Account page) opens into its own small steps; Done says what's
//! left before the store takes payments. Every screen works without
//! JavaScript except making a new wallet, whose recovery phrase is made in
//! the browser.

use maud::{html, Markup};

use super::PageChrome;
use ui::{WizardSteps, WizardStep as Segment, WizardState};

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
    /// The plugin's request, when a WooCommerce plugin sent the merchant
    /// here: its shop's host, which is the site and can't be changed, and
    /// its fields, carried through.
    pub plugin_host: Option<String>,
    /// What the store prices in: the answer, or a guess from the browser.
    pub currency: String,
    pub currencies: Vec<crate::currencies::CurrencyOptionView>,
    pub carried: Vec<(&'static str, String)>,
    pub name_error: Option<String>,
    pub site_error: Option<SiteError>,
}

/// `GET`/`POST /setup`: the store's name and, optionally, its website.
/// Any store takes payments at the till; a website lets it show the
/// checkout on its pages and connect the WooCommerce plugin too.
pub fn store_page(chrome: &PageChrome, data: &StoreStepViewModel) -> Markup {
    let title = if data.plugin_host.is_some() { "Connect your WooCommerce shop" } else { "Set up a store" };
    let header = ui::PageHeader::new(title).progress(steps(Step::Store, None));
    let body = html! {
        @if let Some(host) = &data.plugin_host {
            p { "The WooCommerce plugin on " strong { (host) } " asked to connect to your account. Name the store, then choose where its money goes." }
        }
        form method="post" action="/setup" class="store-form" novalidate {
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
                    @if data.plugin_host.is_none() { (ui::Hint::new("(optional)").inline(true)) }
                }
                (field_help("Needed for the checkout on your pages or for the WooCommerce plugin. Leave it empty to take payments at the till only."))
                (ui::Input::new("store_site")
                    .id("store-site")
                    .value(data.site.clone())
                    .placeholder("shop.example")
                    .autocomplete("url")
                    .readonly(data.plugin_host.is_some())
                    .invalid(data.site_error.is_some())
                    .maybe_described_by(data.site_error.as_ref().map(|_| "store-site-error".to_string()))
                    .attr("inputmode", "url"))
                @match &data.site_error {
                    Some(SiteError::Invalid(why)) => (ui::FieldNote::new(html! { "✕ " (why) }).tone(ui::NoteTone::Problem).id("store-site-error")),
                    Some(SiteError::Yours { id, name, site }) => (ui::FieldNote::new(html! { "✕ Your store " a href=(format!("/dashboard/stores/{id}")) { (name) } " already uses " (site) ". Open it, or use a different site." }).tone(ui::NoteTone::Problem).id("store-site-error")),
                    Some(SiteError::Someone { site }) => (ui::FieldNote::new(html! { "✕ " (site) " is already connected to Monokulo here. If it's yours, ask the person who connected it." }).tone(ui::NoteTone::Problem).id("store-site-error")),
                    None => {}
                }
            }
            (ui::Hint::new(
                "Checkouts on any page of the site work. Subdomains (pay.shop.example) work too, once you verify the domain in the store's settings."
            ))
            div class="step-foot" {
                (ui::LinkButton::new("Cancel", "/"))
                span class="spacer" {}
                (ui::Button::new("Next").variant(ui::ButtonVariant::Primary).kind(ui::ButtonType::Submit))
            }
        }
    };
    super::Page::new("Set up a store - Monokulo", header).class("setup-store").render(chrome, body)
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
    /// Its host, or empty when it has none.
    pub site: String,
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
/// (`POST /connect/{platform}`).
pub struct PluginReturn {
    pub platform: String,
    pub site_url: String,
    pub return_url: String,
    pub nonce: String,
}

fn settings_link(id: &str) -> String {
    format!("/dashboard/stores/{id}/settings")
}

fn summary(data: &DoneViewModel) -> Markup {
    html! {
        "Payments will go to " a href="/account?tab=wallets" { strong { (data.wallet_name) } } " " (super::network_badge(&data.wallet_network)) ", priced in "
        (data.base_currency) ". " a href=(settings_link(&data.store_id)) { "Change these in the store's settings." }
    }
}

/// A form back to the plugin: the button mints the store's key and sends
/// it there.
pub fn plugin_return_form(store_id: &str, plugin: &PluginReturn, label: &str) -> Markup {
    html! {
        form method="post" action=(format!("/connect/{}", plugin.platform)) class="inline-form" {
            input type="hidden" name="connection_id" value=(store_id);
            input type="hidden" name="site_url" value=(plugin.site_url);
            input type="hidden" name="return_url" value=(plugin.return_url);
            input type="hidden" name="nonce" value=(plugin.nonce);
            (ui::Button::new(label).variant(ui::ButtonVariant::Primary).kind(ui::ButtonType::Submit))
        }
    }
}

/// `GET /setup/done/{id}`: every way the store can start taking payments,
/// its state deciding what's ready. The checkout needs a site; the plugin
/// goes back to WooCommerce when it sent the merchant here; the till is
/// always there. One orange button: Back to WooCommerce from the plugin,
/// else Open the guide when there's a site, else Open the till. Nothing
/// claims the store takes payments: a site with no orders yet says it
/// doesn't.
pub fn done_page(chrome: &PageChrome, data: &DoneViewModel) -> Markup {
    let id = &data.store_id;
    let has_site = !data.site.is_empty();
    let primary = |on: bool| on.then_some(ui::ButtonVariant::Primary);
    let till_primary = data.plugin.is_none() && !has_site;
    let guide_primary = data.plugin.is_none() && has_site;
    let title = if data.plugin.is_some() { "is connected" } else { "is set up" };
    let header = ui::PageHeader::new(html! { (data.name) " " (title) }).progress(steps(Step::Done, None));
    let body = html! {
        p class="store-summary" {
            @if has_site && !data.has_orders {
                (ui::Tag::new("Not taking payments yet").tone(ui::TagTone::Unknown).attr("data-not-yet", "")) " "
            }
            (summary(data))
        }
        @if data.skipped_backup { (skipped_warning()) }
        ol class="todo" {
            li {
                span class="num" { "1" }
                p {
                    strong { "Add the checkout to your site" }
                    @if has_site {
                        (ui::Hint::new(format!("A script tag and a button on {}, or a call from your server. The guide shows both, and how to verify the domain.", data.site)).inline(true))
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
                span class="num" { "2" }
                p {
                    strong { "Install the WooCommerce plugin" }
                    @if data.plugin.is_some() {
                        (ui::Hint::new(format!(
                            "Go back to WooCommerce to give the plugin on {} its key. Monero then shows at checkout once you \
                             turn it on in WooCommerce → Settings → Payments.",
                            data.site
                        )).inline(true))
                    } @else {
                        (ui::Hint::new(
                            "In WordPress: Plugins, Add New, search for Monokulo, then Connect it from WooCommerce → Settings → Payments. \
                             It finds this store by its site."
                        ).inline(true))
                    }
                }
                @if let Some(plugin) = &data.plugin {
                    (plugin_return_form(id, plugin, "Back to WooCommerce"))
                } @else {
                    (ui::LinkButton::new(html! { "Plugin guide " (super::external_link_icon()) }, super::docs_url("woocommerce/"))
                        .attr("target", "_blank")
                        .attr("rel", "noopener"))
                }
            }
            li {
                span class="num" { "3" }
                p {
                    strong { "Open the till (POS)" }
                    (ui::Hint::new("On the phone or tablet you take payments with. Customers scan a QR code.").inline(true))
                }
                (ui::LinkButton::new("Open the till", format!("/dashboard/stores/{id}/pos")).maybe_variant(primary(till_primary)))
            }
        }
        @if has_site { (public_key_help(&data.public_key)) }
        div class="step-foot" {
            (ui::LinkButton::new("Store settings", settings_link(id)))
            span class="spacer" {}
            a href=(format!("/dashboard/stores/{id}")) { "Go to " (data.name) "'s page" }
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
    fn bringing_your_own_wallet_has_two_small_steps() {
        let html = wallet_steps(WalletAt {
            path: WalletPath::Keys,
            at: 1,
        })
        .into_string();
        assert!(html.contains("<small>Keys · step 2 of 2</small>"));
        assert!(html.contains(r#"value="1" max="2""#));
        assert!(!html.contains("sub-timeline"));
        assert!(!html.contains("Back up"));
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
            plugin_host: None,
            carried: vec![],
            name_error: None,
            site_error: None,
        }
    }

    /// One screen: the store's name, and its website, optional. No kinds.
    #[test]
    fn the_store_step_asks_a_name_and_an_optional_website() {
        let html = store_page(&chrome(), &store_data()).into_string();
        assert!(
            !html.contains("kind-card") && !html.contains(r#"name="kind""#),
            "{html}"
        );
        assert!(html.contains("<h1>Set up a store</h1>"), "{html}");
        assert!(html.contains(r#"name="store_name" id="store-name" value="Bakery""#));
        assert!(html.contains(r#"name="store_site" id="store-site" value="shop.example""#));
        assert!(
            html.contains(r#"<label class="setting-label" for="store-site">Your website</label><span class="hint">(optional)</span>"#),
            "{html}"
        );
        assert!(
            html.contains("Needed for the checkout on your pages or for the WooCommerce plugin."),
            "{html}"
        );
        assert!(!html.to_lowercase().contains("in person"), "{html}");
        assert!(html.contains(r#"action="/setup""#));
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
        assert!(html.contains(r#"Your store <a href="/dashboard/stores/c_1">Corner shop</a> already uses shop.example."#), "{html}");
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
        assert!(!html.contains("/dashboard/stores/"));
    }

    #[test]
    fn from_the_plugin_the_site_is_the_shops_and_fixed() {
        let html = store_page(
            &chrome(),
            &StoreStepViewModel {
                plugin_host: Some("bakery.example".into()),
                site: "bakery.example".into(),
                carried: vec![("return_url", "https://bakery.example/wp-admin".into())],
                ..store_data()
            },
        )
        .into_string();
        assert!(html.contains("<h1>Connect your WooCommerce shop</h1>"));
        assert!(html.contains(r#"name="return_url" value="https://bakery.example/wp-admin""#));
        assert!(html.contains("readonly"));
        assert!(!html.contains("(optional)"), "{html}");
    }

    fn done(site: &str) -> DoneViewModel {
        DoneViewModel {
            store_id: "c_1".into(),
            name: "Bakery".into(),
            site: site.into(),
            has_orders: false,
            public_key: "pk_abc".into(),
            base_currency: "XMR".into(),
            wallet_name: "Bakery takings".into(),
            wallet_network: "mainnet".into(),
            skipped_backup: false,
            plugin: None,
        }
    }

    fn plugin() -> PluginReturn {
        PluginReturn {
            platform: "woocommerce".into(),
            site_url: "https://bakery.example".into(),
            return_url: "https://bakery.example/wp-admin/x".into(),
            nonce: "n1".into(),
        }
    }

    /// The one orange button on Done.
    fn primary(html: &str) -> &str {
        const PRIMARY: &str = r#"data-variant="primary""#;
        assert_eq!(
            html.matches(PRIMARY).count(),
            1,
            "one primary button: {html}"
        );
        let at = html.find(PRIMARY).unwrap();
        let rest = &html[at..];
        &rest[..rest.find("</").unwrap()]
    }

    /// Every way to start, whatever the store: the checkout, the plugin and
    /// the till.
    #[test]
    fn done_lists_the_checkout_the_plugin_and_the_till() {
        for data in [
            done("bakery.example"),
            done(""),
            DoneViewModel {
                plugin: Some(plugin()),
                ..done("bakery.example")
            },
        ] {
            let html = done_page(&chrome(), &data).into_string();
            assert!(
                html.contains("<strong>Add the checkout to your site</strong>"),
                "{html}"
            );
            assert!(
                html.contains("<strong>Install the WooCommerce plugin</strong>"),
                "{html}"
            );
            assert!(
                html.contains("<strong>Open the till (POS)</strong>"),
                "{html}"
            );
            assert!(
                html.contains(r#"href="/dashboard/stores/c_1/pos""#),
                "{html}"
            );
            assert!(
                !html.to_lowercase().contains("ready to take payments"),
                "{html}"
            );
            assert!(!html.to_lowercase().contains("in person"), "{html}");
            assert!(html
                .contains(r#"<li aria-current="step"><span>Done<small>Complete</small></span>"#));
        }
    }

    /// With a site and no plugin: the guide is the primary, and the store
    /// isn't taking payments on it yet.
    #[test]
    fn with_a_site_the_guide_is_the_primary() {
        let html = done_page(&chrome(), &done("bakery.example")).into_string();
        assert!(primary(&html).contains("Open the guide"), "{html}");
        assert!(html.contains(&format!(r#"href="{}""#, crate::views::DOCS_URL)));
        assert!(html.contains("Not taking payments yet"), "{html}");
        assert!(
            html.contains("A script tag and a button on bakery.example"),
            "{html}"
        );
        assert!(html.contains("Your store's public key"), "{html}");
        // Once an order was made, it no longer says so.
        let html = done_page(
            &chrome(),
            &DoneViewModel {
                has_orders: true,
                ..done("bakery.example")
            },
        )
        .into_string();
        assert!(!html.contains("Not taking payments yet"), "{html}");
    }

    /// With no site: the till is the primary; the checkout says to add a
    /// site in the store's settings first.
    #[test]
    fn with_no_site_the_till_is_the_primary() {
        let html = done_page(&chrome(), &done("")).into_string();
        assert!(primary(&html).contains("Open the till"), "{html}");
        assert!(
            html.contains("add your site in the store's settings first"),
            "{html}"
        );
        assert!(html.contains(r#"<a class="mk-btn" href="/dashboard/stores/c_1/settings#card-store">Add your site</a>"#), "{html}");
        assert!(!html.contains("Open the guide"), "{html}");
        assert!(!html.contains("Not taking payments yet"), "{html}");
    }

    /// From the plugin: Back to WooCommerce is the primary, a form that
    /// mints the store's key.
    #[test]
    fn from_the_plugin_back_to_woocommerce_is_the_primary() {
        let html = done_page(
            &chrome(),
            &DoneViewModel {
                plugin: Some(plugin()),
                ..done("bakery.example")
            },
        )
        .into_string();
        assert!(primary(&html).contains("Back to WooCommerce"), "{html}");
        assert!(html.contains(r#"action="/connect/woocommerce""#));
        assert!(html.contains(r#"name="connection_id" value="c_1""#));
        assert!(html.contains("<h1>Bakery is connected</h1>"), "{html}");
        assert!(
            html.contains("Open the guide"),
            "the guide is still there: {html}"
        );
    }
}
