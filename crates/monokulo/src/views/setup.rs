//! Setting up a store (`/setup`, `http::setup`): Store, then Wallet, then
//! Done. The store step asks where payments are taken, the store's name and
//! its site; the wallet step (`views::wallets`, shared with adding a wallet
//! from the Account page) opens into its own small steps; Done says what's
//! left before the store takes payments. Every screen works without
//! JavaScript except making a new wallet, whose recovery phrase is made in
//! the browser.

use maud::{html, Markup};

use super::{layout, PageChrome};
use crate::stores::StoreKind;

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

/// The wallet's small steps as a timeline: done ones green, the current
/// one orange, the rest neutral, joined by lines. On a phone it gives way
/// to one line ("Wallet · 2 of 3: back up").
fn wallet_timeline(wallet: WalletAt) -> Markup {
    let labels = wallet.path.labels();
    html! {
        ol class="sub-timeline" aria-label="Wallet steps" {
            @for (i, label) in labels.iter().enumerate() {
                @if i == wallet.at {
                    li aria-current="step" { span class="pill" { (label) } }
                } @else if i < wallet.at {
                    li class="done" { span class="pill" { (label) } }
                } @else {
                    li { span class="pill" { (label) } }
                }
            }
        }
    }
}

/// The one-line form of the wallet's small steps, for a phone.
fn wallet_line(wallet: WalletAt) -> Markup {
    let labels = wallet.path.labels();
    html! {
        p class="sub-crumbs" {
            "Wallet · " (wallet.at + 1) " of " (labels.len()) ": "
            (labels.get(wallet.at).copied().unwrap_or_default().to_lowercase())
        }
    }
}

/// The steps above every setup screen: Store, Wallet, Done. While on
/// Wallet, its own small steps sit beside it. Done is only ever reached
/// once the store exists.
pub fn steps(at: Step, wallet: Option<WalletAt>) -> Markup {
    let all = [
        (Step::Store, "Store"),
        (Step::Wallet, "Wallet"),
        (Step::Done, "Done"),
    ];
    let current = all.iter().position(|(s, _)| *s == at).unwrap_or(0);
    html! {
        ol class="setup-steps" aria-label="Setup progress" {
            @for (i, (step, label)) in all.iter().enumerate() {
                @if i == current {
                    @match (step, wallet) {
                        (Step::Wallet, Some(wallet)) => {
                            li class="wallet-group" aria-current="step" {
                                span class="lbl" { span class="n" { (i + 1) } (label) }
                                (wallet_timeline(wallet))
                            }
                        }
                        _ => li aria-current="step" { span class="n" { (i + 1) } (label) },
                    }
                } @else if i < current {
                    li class="done" { span class="n" { (i + 1) } (label) }
                } @else {
                    li { span class="n" { (i + 1) } (label) }
                }
            }
        }
        @if let (Step::Wallet, Some(wallet)) = (at, wallet) {
            (wallet_line(wallet))
        }
    }
}

/// The wallet's small steps on their own, for adding a wallet outside
/// setup (`/account/wallets/...`).
pub fn wallet_steps(wallet: WalletAt) -> Markup {
    html! {
        (wallet_timeline(wallet))
        (wallet_line(wallet))
    }
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
    pub kind: StoreKind,
    pub name: String,
    pub site: String,
    /// The plugin's request, when a WooCommerce plugin sent the merchant
    /// here: its shop's host, which is the site and can't be changed, and
    /// its fields, carried through.
    pub plugin_host: Option<String>,
    pub carried: Vec<(&'static str, String)>,
    pub name_error: Option<String>,
    pub site_error: Option<SiteError>,
}

fn globe_icon() -> Markup {
    html! {
        svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" {
            circle cx="12" cy="12" r="9" {}
            path d="M3 12h18M12 3c2.6 2.6 3.8 5.6 3.8 9s-1.2 6.4-3.8 9M12 3C9.4 5.6 8.2 8.6 8.2 12s1.2 6.4 3.8 9" {}
        }
    }
}

/// WooCommerce's mark: its speech bubble with "Woo" in it.
fn woo_icon() -> Markup {
    html! {
        svg class="woo" viewBox="0 0 32 24" {
            path class="woo-bubble" d="M4 3h24a3 3 0 0 1 3 3v9a3 3 0 0 1-3 3H17l-5 4 1-4H4a3 3 0 0 1-3-3V6a3 3 0 0 1 3-3z" {}
            text class="woo-text" x="16" y="14.6" text-anchor="middle" { "Woo" }
        }
    }
}

fn shop_icon() -> Markup {
    html! {
        svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round" stroke-linecap="round" {
            path d="M3 9l1.5-5h15L21 9" {}
            path d="M3 9c0 1.4 1 2.4 2.25 2.4S7.5 10.4 7.5 9c0 1.4 1 2.4 2.25 2.4S12 10.4 12 9c0 1.4 1 2.4 2.25 2.4S16.5 10.4 16.5 9c0 1.4 1 2.4 2.25 2.4S21 10.4 21 9" {}
            path d="M4.5 11.4V20h15v-8.6" {}
            path d="M10 20v-5h4v5" {}
        }
    }
}

/// One whole-card choice of where payments are taken: a radio, hidden but
/// there, inside a label, so it works without JavaScript and by keyboard.
fn kind_card(kind: StoreKind, checked: bool) -> Markup {
    let (title, icon, description) = match kind {
        StoreKind::Website => (
            "My own website",
            globe_icon(),
            "A checkout on your pages, with a script tag or from your server.",
        ),
        StoreKind::WooCommerce => (
            "WooCommerce",
            woo_icon(),
            "Install the plugin in WordPress; it connects back here.",
        ),
        StoreKind::InPerson => (
            "In person only",
            shop_icon(),
            "The point of sale on a phone or tablet. No website.",
        ),
    };
    html! {
        label class="kind-card" {
            input class="visually-hidden" type="radio" name="kind" value=(kind.key()) checked[checked];
            span class="kc-title" { (title) }
            span class="kind-icon" aria-hidden="true" { (icon) }
            span class="kc-desc" { (description) }
        }
    }
}

/// `GET`/`POST /setup`: where payments are taken, the store's name and its
/// site.
pub fn store_page(chrome: &PageChrome, data: &StoreStepViewModel) -> Markup {
    let body = html! {
        div class="wrap setup-store" {
            (steps(Step::Store, None))
            @if let Some(host) = &data.plugin_host {
                h1 { "Connect your WooCommerce shop" }
                p { "The WooCommerce plugin on " strong { (host) } " asked to connect to your account. Name the store, then choose where its money goes." }
            } @else {
                h1 { "Where will you take payments?" }
            }
            form method="post" action="/setup" class="store-form" novalidate {
                @for (name, value) in &data.carried {
                    input type="hidden" name=(name) value=(value);
                }
                @if data.plugin_host.is_some() {
                    input type="hidden" name="kind" value=(StoreKind::WooCommerce.key());
                } @else {
                    fieldset class="kind-grid" {
                        legend class="visually-hidden" { "Where will you take payments?" }
                        @for kind in StoreKind::ALL {
                            (kind_card(kind, kind == data.kind))
                        }
                    }
                }
                div class="two-fields" {
                    div class="setting-field" {
                        div class="setting-label-row" { label class="setting-label" for="store-name" { "Store name" } }
                        p class="field-help hint" { "Shown on your dashboard, the till and the checkout." }
                        input id="store-name" type="text" name="store_name" value=(data.name) maxlength=(crate::stores::MAX_NAME_LEN)
                            autocomplete="organization" required aria-invalid=[data.name_error.as_ref().map(|_| "true")]
                            aria-describedby=[data.name_error.as_ref().map(|_| "store-name-error")];
                        @if let Some(error) = &data.name_error {
                            span class="field-check bad" id="store-name-error" { "✕ " (error) }
                        }
                    }
                    div class="setting-field site-field" {
                        div class="setting-label-row" { label class="setting-label" for="store-site" { "Site" } }
                        p class="field-help hint" { "The website the checkout runs on. Any page on it works." }
                        input id="store-site" type="text" name="store_site" value=(data.site) inputmode="url"
                            autocomplete="url" placeholder="shop.example" readonly[data.plugin_host.is_some()]
                            aria-invalid=[data.site_error.as_ref().map(|_| "true")]
                            aria-describedby=[data.site_error.as_ref().map(|_| "store-site-error")];
                        @match &data.site_error {
                            Some(SiteError::Invalid(why)) => span class="field-check bad" id="store-site-error" { "✕ " (why) },
                            Some(SiteError::Yours { id, name, site }) => span class="field-check bad" id="store-site-error" {
                                "✕ Your store " a href=(format!("/dashboard/stores/{id}")) { (name) } " already uses " (site) ". Open it, or use a different site."
                            },
                            Some(SiteError::Someone { site }) => span class="field-check bad" id="store-site-error" {
                                "✕ " (site) " is already connected to Monokulo here. If it's yours, ask the person who connected it."
                            },
                            None => {}
                        }
                    }
                }
                p class="hint site-hint" {
                    "Checkouts on any page of the site work. Subdomains (pay.shop.example) work too, once you verify the domain in the store's settings."
                }
                div class="step-foot" {
                    a class="btn" href="/" { "Cancel" }
                    span class="spacer" {}
                    button type="submit" class="btn-primary" { "Next" }
                }
            }
        }
    };
    layout(chrome, "Set up a store - Monokulo", body)
}

// -- Done ----------------------------------------------------------------------

/// The store that was just made, and what it takes payments into.
pub struct DoneViewModel {
    pub store_id: String,
    pub name: String,
    pub kind: StoreKind,
    pub site: String,
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
            button type="submit" class="btn-primary" { (label) }
        }
    }
}

/// `GET /setup/done/{id}`: what's left. A website's store never says it can
/// take payments yet: it lists the checkout, the domain and a test payment.
pub fn done_page(chrome: &PageChrome, data: &DoneViewModel) -> Markup {
    let id = &data.store_id;
    let body = html! {
        div class="wrap setup-done" {
            (steps(Step::Done, None))
            @match (data.kind, &data.plugin) {
                (_, Some(plugin)) => {
                    h1 { (data.name) " is connected" }
                    p {
                        "Go back to WooCommerce to give the plugin on " strong { (data.site) } " its key. WooCommerce then shows "
                        "Monero at checkout as soon as you turn it on in " strong { "WooCommerce → Settings → Payments" } "."
                    }
                    p class="store-summary" { (summary(data)) }
                    @if data.skipped_backup { (skipped_warning()) }
                    div class="step-foot" {
                        a class="btn" href=(settings_link(id)) { "Store settings" }
                        span class="spacer" {}
                        (plugin_return_form(id, plugin, "Back to WooCommerce"))
                    }
                }
                (StoreKind::InPerson, None) => {
                    h1 { (data.name) " is ready" }
                    p {
                        "Open the till on the phone or tablet you'll take payments with. Customers scan a QR code; it's paid "
                        "when it lands in your wallet."
                    }
                    p class="store-summary" { (summary(data)) }
                    @if data.skipped_backup { (skipped_warning()) }
                    div class="step-foot" {
                        a class="btn" href=(settings_link(id)) { "Store settings" }
                        span class="spacer" {}
                        a class="btn btn-primary" href=(format!("/dashboard/stores/{id}/pos")) { "Open the till" }
                    }
                }
                (StoreKind::WooCommerce, None) => {
                    h1 { (data.name) " is set up" }
                    p class="store-summary" { span class="tag tag-unknown not-yet" { "Not taking payments yet" } " " (summary(data)) }
                    @if data.skipped_backup { (skipped_warning()) }
                    ol class="todo" {
                        li {
                            span class="num" { "1" }
                            p {
                                strong { "Install the Monokulo plugin on " (data.site) }
                                span class="hint" { "In WordPress: Plugins, Add New, search for Monokulo, then activate it." }
                            }
                            span {}
                        }
                        li {
                            span class="num" { "2" }
                            p {
                                strong { "Connect it" }
                                span class="hint" {
                                    "WooCommerce, Settings, Payments, Monokulo, then Connect. It finds this store by its site, so "
                                    "there's nothing to type."
                                }
                            }
                            span {}
                        }
                        li {
                            span class="num" { "3" }
                            p {
                                strong { "Take a test payment" }
                                span class="hint" { "Buy something from your shop. This store's page shows the order when it arrives." }
                            }
                            a class="btn" href=(format!("/dashboard/stores/{id}")) { "Store page" }
                        }
                    }
                    (public_key_help(&data.public_key))
                }
                (StoreKind::Website, None) => {
                    h1 { (data.name) " is set up" }
                    p class="store-summary" { span class="tag tag-unknown not-yet" { "Not taking payments yet" } " " (summary(data)) }
                    @if data.skipped_backup { (skipped_warning()) }
                    ol class="todo" {
                        li {
                            span class="num" { "1" }
                            p {
                                strong { "Add the checkout to " (data.site) }
                                span class="hint" { "A script tag and a button, or a call from your server. The guide shows both." }
                            }
                            a class="btn btn-primary docs-link" href=(super::DOCS_URL) target="_blank" rel="noopener" {
                                "Open the guide " (super::external_link_icon())
                            }
                        }
                        li {
                            span class="num" { "2" }
                            p {
                                strong { "Verify " (data.site) }
                                span class="hint" { "A DNS record, so only your site can show the checkout. Optional, recommended." }
                            }
                            a class="btn" href=(format!("/dashboard/stores/{id}/settings#verified-domains")) { "Verify domain" }
                        }
                        li {
                            span class="num" { "3" }
                            p {
                                strong { "Take a test payment" }
                                span class="hint" { "Make an order from your site. This store's page shows it when it arrives." }
                            }
                            a class="btn" href=(super::docs_url("walkthrough-geomart/#step-6")) target="_blank" rel="noopener" { "How to test" }
                        }
                    }
                    (public_key_help(&data.public_key))
                }
            }
            p { a href=(format!("/dashboard/stores/{id}")) { "Go to " (data.name) "'s page" } }
        }
    };
    layout(chrome, &format!("{} is set up - Monokulo", data.name), body)
}

fn skipped_warning() -> Markup {
    html! {
        p class="warning" { "The wallet's recovery phrase was not backed up. Payments to it can't be spent unless you have it." }
    }
}

fn public_key_help(public_key: &str) -> Markup {
    html! {
        details class="keys-help" {
            summary { "Your store's public key" }
            div class="keys-help-body" {
                pre { (public_key) }
                p class="hint" { "The guide tells you where it goes. It's safe to publish: it names the store and can't move money." }
            }
        }
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
            store.contains(r#"<li aria-current="step"><span class="n">1</span>Store</li>"#),
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
        assert!(wallet.contains(r#"<li class="done"><span class="n">1</span>Store</li>"#));
        assert!(wallet.contains(r#"<li class="wallet-group" aria-current="step">"#));
        // The wallet's own steps: Kind done, Back up current, Check to come.
        assert!(wallet.contains(r#"<li class="done"><span class="pill">Kind</span></li><li aria-current="step"><span class="pill">Back up</span></li><li><span class="pill">Check</span></li>"#), "{wallet}");
        assert!(wallet.contains("Wallet · 2 of 3: back up"));
        assert!(wallet.contains(r#"<li><span class="n">3</span>Done</li>"#));
    }

    #[test]
    fn bringing_your_own_wallet_has_two_small_steps() {
        let html = wallet_steps(WalletAt {
            path: WalletPath::Keys,
            at: 1,
        })
        .into_string();
        assert!(html.contains(r#"<li aria-current="step"><span class="pill">Keys</span></li>"#));
        assert!(html.contains("Wallet · 2 of 2: keys"));
        assert!(!html.contains("Back up"));
    }

    fn store_data() -> StoreStepViewModel {
        StoreStepViewModel {
            kind: StoreKind::Website,
            name: "Bakery".into(),
            site: "shop.example".into(),
            plugin_host: None,
            carried: vec![],
            name_error: None,
            site_error: None,
        }
    }

    #[test]
    fn the_store_step_offers_three_whole_card_kinds_and_a_name_apart_from_the_site() {
        let html = store_page(&chrome(), &store_data()).into_string();
        assert_eq!(html.matches(r#"class="kind-card""#).count(), 3);
        assert!(
            html.contains(
                r#"<input class="visually-hidden" type="radio" name="kind" value="web" checked>"#
            ),
            "{html}"
        );
        assert!(html.contains(r#"name="store_name" value="Bakery""#));
        assert!(html.contains(r#"name="store_site" value="shop.example""#));
        assert!(html.contains("Checkouts on any page of the site work"));
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
    fn from_the_plugin_the_kind_and_site_are_fixed() {
        let html = store_page(
            &chrome(),
            &StoreStepViewModel {
                kind: StoreKind::WooCommerce,
                plugin_host: Some("bakery.example".into()),
                site: "bakery.example".into(),
                carried: vec![("return_url", "https://bakery.example/wp-admin".into())],
                ..store_data()
            },
        )
        .into_string();
        assert!(!html.contains("kind-card"));
        assert!(html.contains(r#"<input type="hidden" name="kind" value="woocommerce">"#));
        assert!(html.contains(r#"name="return_url" value="https://bakery.example/wp-admin""#));
        assert!(html.contains("readonly"));
    }

    fn done(kind: StoreKind) -> DoneViewModel {
        DoneViewModel {
            store_id: "c_1".into(),
            name: "Bakery".into(),
            kind,
            site: "bakery.example".into(),
            public_key: "pk_abc".into(),
            base_currency: "XMR".into(),
            wallet_name: "Bakery takings".into(),
            wallet_network: "mainnet".into(),
            skipped_backup: false,
            plugin: None,
        }
    }

    #[test]
    fn a_website_is_never_said_to_take_payments_before_its_checkout_is_added() {
        let html = done_page(&chrome(), &done(StoreKind::Website)).into_string();
        assert!(html.contains("Not taking payments yet"));
        assert!(!html.to_lowercase().contains("ready to take payments"));
        assert!(html.contains(&format!(r#"href="{}""#, crate::views::DOCS_URL)));
        assert!(html.contains("Open the guide"));
        assert!(html.contains("Verify bakery.example"));
        assert!(html.contains("Take a test payment"));
        assert_eq!(html.matches("btn-primary").count(), 1, "one primary button");
        assert!(html.contains(r#"<li aria-current="step"><span class="n">3</span>Done</li>"#));
    }

    #[test]
    fn a_till_opens_and_a_plugin_gets_its_key_back() {
        let html = done_page(&chrome(), &done(StoreKind::InPerson)).into_string();
        assert!(
            html.contains(r#"href="/dashboard/stores/c_1/pos">Open the till"#),
            "{html}"
        );
        let html = done_page(
            &chrome(),
            &DoneViewModel {
                plugin: Some(PluginReturn {
                    platform: "woocommerce".into(),
                    site_url: "https://bakery.example".into(),
                    return_url: "https://bakery.example/wp-admin/x".into(),
                    nonce: "n1".into(),
                }),
                ..done(StoreKind::WooCommerce)
            },
        )
        .into_string();
        assert!(html.contains(r#"action="/connect/woocommerce""#));
        assert!(html.contains(r#"name="connection_id" value="c_1""#));
        assert!(html.contains("Back to WooCommerce"));
    }
}
