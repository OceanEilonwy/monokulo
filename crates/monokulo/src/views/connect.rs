//! `GET`/`POST /dashboard/connect`, `/connect/{platform}`, and
//! `/dashboard/stores/new` - `http::dashboard::render_connect_form`/
//! `render_connect_success`, `http::connect::render_confirm_form`,
//! `http::home::new_store_picker`.
//!
//! A store takes payments into one of the merchant's wallets
//! (docs/wallets.md): these forms pick one rather than asking for keys.

use maud::{html, Markup};

use super::wallets::{add_wallet_links, wallet_select};
use super::{layout, PageChrome};
use crate::db::WalletSummary;

/// Either `error` is set (re-rendering the form after it was refused) or
/// `public_key` is set (a successful connection, showing the confirmation
/// view instead of the form) - never both, and a plain `GET` gets neither.
pub struct ConnectViewModel {
    pub error: Option<String>,
    pub public_key: Option<String>,
    pub connection_id: Option<String>,
    /// This instance's public address, when set - used by the integration
    /// help shown alongside `public_key`.
    pub public_url: Option<String>,
    /// Echoed back into the form after a refused submission.
    pub site_url: String,
    pub currency_options: Vec<crate::currencies::CurrencyOptionView>,
    /// The merchant's wallets, to pick the one the store takes payments into.
    pub wallets: Vec<WalletSummary>,
    pub selected_wallet: Option<String>,
}

/// One key custody backend a new wallet's keys can go to (part 5).
pub struct CustodyChoice {
    pub backend: String,
    pub label: String,
    pub selected: bool,
}

/// Which screen of the plugin connect flow: the question ("is this shop
/// already a store?"), or one of its two answers.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ConnectMode {
    Ask,
    New,
    Existing,
}

/// The plugin connect flow's page: the values that must survive the round
/// trip as hidden fields (`return_url`/`nonce`) or be shown (`site_url`),
/// and `platform` so the form posts back to the same `/connect/{platform}`.
pub struct PlatformConnectViewModel {
    pub platform: String,
    pub site_url: String,
    pub return_url: String,
    pub nonce: String,
    pub error: Option<String>,
    pub mode: ConnectMode,
    pub currency_options: Vec<crate::currencies::CurrencyOptionView>,
    pub wallets: Vec<WalletSummary>,
    pub selected_wallet: Option<String>,
    /// Every store this user already has connected (any platform), for
    /// "add it to an existing store".
    pub existing_stores: Vec<ExistingStoreOption>,
    /// Set when this instance can't connect plugins at all right now (no
    /// public address configured): the reason is shown and no form is.
    pub unavailable: Option<String>,
}

/// One entry in the "use an existing store" picker.
pub struct ExistingStoreOption {
    pub connection_id: crate::db::ConnectionId,
    pub display_name: String,
    pub platform: String,
}

/// The key storage choice, shown only when this instance offers more than
/// one backend.
pub(crate) fn custody_select(choices: &[CustodyChoice]) -> Markup {
    html! {
        @if choices.len() > 1 {
            label {
                "Key storage"
                select name="key_custody_backend" {
                    @for choice in choices {
                        option value=(choice.backend) selected[choice.selected] { (choice.label) }
                    }
                }
                span class="field-help" {
                    "Where this wallet's view key is kept. You can move it later from a store's settings, "
                    "by entering the keys again."
                }
            }
        }
    }
}

fn currency_select(options: &[crate::currencies::CurrencyOptionView]) -> Markup {
    html! {
        select name="base_currency" {
            @for opt in options {
                option value=(opt.code) selected[opt.selected] { (opt.description) " (" (opt.code) ")" }
            }
        }
    }
}

/// No wallet yet: a store needs one, so say so and offer the two ways.
fn needs_a_wallet(next: &str) -> Markup {
    let next = url::form_urlencoded::byte_serialize(next.as_bytes()).collect::<String>();
    html! {
        div class="notice" {
            p { strong { "A store needs a wallet to take payments into." } " Set one up first; you'll come straight back here." }
            p {
                a class="btn btn-primary" href=(format!("/dashboard/wallets/setup?next={next}")) { "Set up a wallet" }
            }
        }
    }
}

pub fn page(chrome: &PageChrome, data: &ConnectViewModel) -> Markup {
    let body = html! {
        div class="wrap" {
            @if let Some(public_key) = &data.public_key {
                nav class="context-nav" aria-label="Breadcrumb" { a href="/dashboard" { "Dashboard" } }
                h1 { "Store connected" }
                @if let Some(connection_id) = &data.connection_id {
                    p { a class="btn btn-primary" href=(format!("/dashboard/stores/{connection_id}")) { "View store →" } }
                }
                p { "Your public key: " code { (public_key) } }
                p class="hint" {
                    "Keep this value handy. Your secret token is stored securely and is never shown "
                    "here - the control plane keeps it on your behalf for the calls it makes on your store's behalf."
                }
                (super::integration_help::fragment(public_key, data.public_url.as_deref(), false))
            } @else {
                nav class="context-nav" aria-label="Breadcrumb" {
                    a href="/dashboard/stores/new" { "Add a store" }
                }
                h1 { "Custom store" }
                p class="hint" {
                    "For your own website, the POS, or anything that talks to the API. Payments go to the wallet you "
                    "pick; Monokulo only ever holds its watch-only keys."
                }
                @if let Some(error) = &data.error {
                    p class="error" role="alert" { (error) }
                }
                @if data.wallets.is_empty() {
                    (needs_a_wallet("/dashboard/connect"))
                } @else {
                    form method="post" action="/dashboard/connect" id="connect-form" {
                        label {
                            "Site URL"
                            input type="url" name="site_url" id="site_url" value=(data.site_url) placeholder="https://shop.example.com" required;
                            span class="field-help" {
                                "Your storefront's own URL, shown on your dashboard. Its domain is added to "
                                "your store's domains, ready for you to verify in Settings."
                            }
                        }
                        (wallet_select(&data.wallets, data.selected_wallet.as_deref()))
                        (add_wallet_links("/dashboard/connect"))
                        label {
                            "Base currency"
                            (currency_select(&data.currency_options))
                            span class="field-help" {
                                "What custom confirmation thresholds are set in, and what an order's own currency "
                                "is converted into to pick one."
                            }
                        }
                        button type="submit" class="btn-primary" { "Connect" }
                    }
                }
            }
        }
    };
    layout(chrome, "Connect your store - Monokulo", body)
}

impl PlatformConnectViewModel {
    /// This page with the plugin's request, at `mode`.
    pub fn link(&self, mode: Option<&str>) -> String {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query
            .append_pair("site_url", &self.site_url)
            .append_pair("return_url", &self.return_url)
            .append_pair("nonce", &self.nonce);
        if let Some(mode) = mode {
            query.append_pair("mode", mode);
        }
        format!("/connect/{}?{}", self.platform, query.finish())
    }

    fn hidden_fields(&self) -> Markup {
        html! {
            input type="hidden" name="site_url" value=(self.site_url);
            input type="hidden" name="return_url" value=(self.return_url);
            input type="hidden" name="nonce" value=(self.nonce);
        }
    }
}

/// The shop's host name, as it's shown: `shop.example.com`.
fn site_host(site_url: &str) -> String {
    url::Url::parse(site_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_else(|| site_url.to_owned())
}

/// `GET /connect/{platform}`: asks whether the shop is already a store,
/// then shows the form for the answer. Plain links between the screens, so
/// it works without JavaScript.
pub fn platform_page(chrome: &PageChrome, data: &PlatformConnectViewModel) -> Markup {
    let shop = site_host(&data.site_url);
    let body = html! {
        div class="wrap" {
            h1 { "Connect your WooCommerce shop" }
            p { "The WooCommerce plugin on " strong { (shop) } " asked to connect to your account." }
            @if let Some(unavailable) = &data.unavailable {
                p class="error" role="alert" { (unavailable) }
            } @else {
                @if let Some(error) = &data.error {
                    p class="error" role="alert" { (error) }
                }
                @match data.mode {
                    ConnectMode::Ask => {
                        h2 { "Is this shop already a store in Monokulo?" }
                        div class="answer-grid" {
                            a class="btn" href=(data.link(Some("new"))) {
                                strong { "No, it's a new store" }
                                span class="hint" { "Most shops. Next you pick the wallet that gets paid." }
                            }
                            a class="btn" href=(data.link(Some("existing"))) {
                                strong { "Yes, add it to an existing store" }
                                span class="hint" { "It shares that store's orders, settings and wallet." }
                            }
                        }
                    }
                    ConnectMode::New => {
                        h2 { "New store" }
                        @if data.wallets.is_empty() {
                            (needs_a_wallet(&data.link(Some("new"))))
                        } @else {
                            form method="post" action=(format!("/connect/{}", data.platform)) {
                                (data.hidden_fields())
                                input type="hidden" name="mode" value="new";
                                (wallet_select(&data.wallets, data.selected_wallet.as_deref()))
                                (add_wallet_links(&data.link(Some("new"))))
                                label {
                                    "Base currency"
                                    (currency_select(&data.currency_options))
                                    span class="field-help" { "What custom confirmation thresholds are set in." }
                                }
                                div class="form-actions" {
                                    button type="submit" class="btn-primary" { "Connect shop" }
                                    @if !data.existing_stores.is_empty() {
                                        a class="btn" href=(data.link(None)) { "Back" }
                                    }
                                }
                            }
                        }
                    }
                    ConnectMode::Existing => {
                        h2 { "Existing store" }
                        form method="post" action=(format!("/connect/{}", data.platform)) {
                            (data.hidden_fields())
                            input type="hidden" name="mode" value="existing";
                            label {
                                "Store"
                                select name="connection_id" required {
                                    option value="" disabled selected { "Choose a store…" }
                                    @for store in &data.existing_stores {
                                        option value=(store.connection_id) { (store.display_name) " (" (store.platform) ")" }
                                    }
                                }
                            }
                            div class="form-actions" {
                                button type="submit" class="btn-primary" { "Connect shop" }
                                a class="btn" href=(data.link(None)) { "Back" }
                            }
                        }
                    }
                }
                hr class="rule";
                p { a href=(data.site_url) { "Cancel and go back to " (shop) } }
            }
        }
    };
    layout(chrome, "Connect Monero payments - Monokulo", body)
}

pub fn new_store_picker_page(chrome: &PageChrome) -> Markup {
    let body = html! {
        div class="wrap" {
            nav class="context-nav" aria-label="Breadcrumb" { a href="/dashboard" { "Dashboard" } }
            h1 { "Add a store" }
            p { "Choose the setup that matches your storefront." }
            div class="pick-grid" {
                div class="pick-card" {
                    h3 { "WooCommerce" }
                    p {
                        "Running WordPress + WooCommerce? Install the plugin and click Connect. You pick which "
                        "wallet gets paid when you connect."
                    }
                    a class="btn btn-primary" href="/dashboard/stores/new/woocommerce" { "Set up WooCommerce" }
                }
                div class="pick-card" {
                    h3 { "Custom store" }
                    p {
                        "Your own website, the in-person POS, or anything else that uses the API. Pick a wallet "
                        "and you're connected."
                    }
                    a class="btn" href="/dashboard/connect" { "Set up a custom store" }
                }
            }
        }
    };
    layout(chrome, "Add a store - Monokulo", body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{EngineWalletId, UserId, WalletId, WalletOrigin, WalletRow};

    fn chrome() -> PageChrome {
        PageChrome::from_user(None, "")
    }

    fn wallet(id: &str, name: &str) -> WalletSummary {
        WalletSummary {
            wallet: WalletRow {
                id: WalletId::new(id),
                user_id: UserId::new("u"),
                name: name.to_owned(),
                network: "mainnet".to_owned(),
                primary_address: "48xQ7aaaaaaaaaaaaaaaaaaaaav3Rk".to_owned(),
                engine_wallet_id: EngineWalletId::new("wl_1"),
                origin: WalletOrigin::Imported,
                backup: None,
                created_at: 0,
            },
            store_count: 0,
        }
    }

    fn default_connect_data() -> ConnectViewModel {
        ConnectViewModel {
            error: None,
            public_key: None,
            connection_id: None,
            public_url: None,
            site_url: String::new(),
            currency_options: vec![],
            wallets: vec![wallet("w_1", "Copper Heron")],
            selected_wallet: None,
        }
    }

    #[test]
    fn the_custom_store_form_picks_a_wallet_instead_of_asking_for_keys() {
        let html = page(&chrome(), &default_connect_data()).into_string();
        assert!(html.contains("<form"));
        assert!(html.contains(r#"name="wallet_id""#));
        assert!(
            !html.contains("view_key_hex"),
            "keys belong to the wallet now"
        );
    }

    #[test]
    fn one_wallet_is_picked_for_you_and_several_are_not() {
        let one = page(&chrome(), &default_connect_data()).into_string();
        assert!(one.contains(r#"value="w_1" selected"#), "{one}");
        let several = page(
            &chrome(),
            &ConnectViewModel {
                wallets: vec![wallet("w_1", "Copper Heron"), wallet("w_2", "Café till")],
                ..default_connect_data()
            },
        )
        .into_string();
        assert!(several.contains("Choose a wallet…"));
        assert!(!several.contains(r#"value="w_1" selected"#));
        assert!(!several.contains(r#"value="w_2" selected"#));
    }

    #[test]
    fn with_no_wallet_the_custom_store_page_sends_you_to_set_one_up() {
        let html = page(
            &chrome(),
            &ConnectViewModel {
                wallets: vec![],
                ..default_connect_data()
            },
        )
        .into_string();
        assert!(!html.contains(r#"id="connect-form""#));
        assert!(html.contains("/dashboard/wallets/setup?next=%2Fdashboard%2Fconnect"));
    }

    #[test]
    fn the_custom_store_form_shows_its_error_and_keeps_the_site_url() {
        let data = ConnectViewModel {
            error: Some("Choose one of your wallets.".to_string()),
            site_url: "https://shop.example.com".to_string(),
            ..default_connect_data()
        };
        let html = page(&chrome(), &data).into_string();
        assert!(html.contains("Choose one of your wallets."));
        assert!(html.contains(r#"value="https://shop.example.com""#));
    }

    #[test]
    fn connect_page_shows_the_public_key_instead_of_the_form_on_success() {
        let data = ConnectViewModel {
            public_key: Some("pk_deadbeef".to_string()),
            connection_id: Some("conn_1".to_string()),
            public_url: Some("https://pay.example.com".to_string()),
            ..default_connect_data()
        };
        let html = page(&chrome(), &data).into_string();
        assert!(html.contains(r#"href="/dashboard/stores/conn_1""#));
        assert!(html.contains("pk_deadbeef"));
        assert!(!html.contains("id=\"connect-form\""));
        assert!(html.contains("https://pay.example.com/static/monokulo-client.js"));
        assert!(html.contains("POST https://pay.example.com/pay/pk_deadbeef/orders"));
    }

    fn default_platform_data() -> PlatformConnectViewModel {
        PlatformConnectViewModel {
            platform: "woocommerce".to_string(),
            site_url: "https://shop.example.com".to_string(),
            return_url: "https://shop.example.com/settings".to_string(),
            nonce: "nonce-abc".to_string(),
            error: None,
            mode: ConnectMode::Ask,
            currency_options: vec![],
            wallets: vec![wallet("w_1", "Copper Heron"), wallet("w_2", "Café till")],
            selected_wallet: None,
            existing_stores: vec![ExistingStoreOption {
                connection_id: crate::db::ConnectionId::new("c_1"),
                display_name: "market.example.com".to_owned(),
                platform: "custom".to_owned(),
            }],
            unavailable: None,
        }
    }

    #[test]
    fn platform_page_shows_why_it_cannot_connect_and_no_form_when_unavailable() {
        let data = PlatformConnectViewModel {
            unavailable: Some("No public address yet.".to_string()),
            ..default_platform_data()
        };
        let html = platform_page(&chrome(), &data).into_string();
        assert!(html.contains("No public address yet."));
        assert!(!html.contains("<form"));
    }

    #[test]
    fn platform_page_asks_first_with_a_link_for_each_answer() {
        let html = platform_page(&chrome(), &default_platform_data()).into_string();
        assert!(html.contains("Is this shop already a store in Monokulo?"));
        assert!(html.contains("mode=new"));
        assert!(html.contains("mode=existing"));
        assert!(!html.contains("<form"), "the question is links, not a form");
    }

    #[test]
    fn the_new_store_answer_picks_a_wallet_and_carries_the_plugins_request() {
        let data = PlatformConnectViewModel {
            mode: ConnectMode::New,
            ..default_platform_data()
        };
        let html = platform_page(&chrome(), &data).into_string();
        assert!(html.contains(r#"action="/connect/woocommerce""#));
        assert!(html.contains(r#"name="return_url" value="https://shop.example.com/settings""#));
        assert!(html.contains(r#"name="nonce" value="nonce-abc""#));
        assert!(html.contains(r#"name="mode" value="new""#));
        assert!(
            html.contains("Choose a wallet…"),
            "two wallets: none picked for you"
        );
    }

    #[test]
    fn the_existing_store_answer_lists_the_stores() {
        let data = PlatformConnectViewModel {
            mode: ConnectMode::Existing,
            ..default_platform_data()
        };
        let html = platform_page(&chrome(), &data).into_string();
        assert!(html.contains(r#"name="mode" value="existing""#));
        assert!(html.contains("market.example.com"));
    }

    #[test]
    fn new_store_picker_links_to_both_flows() {
        let html = new_store_picker_page(&chrome()).into_string();
        assert!(html.contains(r#"href="/dashboard/stores/new/woocommerce""#));
        assert!(html.contains(r#"href="/dashboard/connect""#));
        assert!(html.contains("Custom store"));
    }
}
