//! `GET`/`POST /dashboard/connect`, `/connect/{platform}`, and
//! `/dashboard/connections/new` - `http::dashboard::render_connect_form`/
//! `render_connect_success`, `http::connect::render_confirm_form`,
//! `http::home::new_store_picker`.

use maud::{html, Markup, PreEscaped};

use super::{layout, PageChrome};

/// Either `error` is set (re-rendering the form after the engine rejected
/// the request) or `public_key` is set (a successful connection, showing
/// the confirmation view instead of the form) - never both, and a plain
/// `GET` gets neither.
pub struct ConnectViewModel {
    pub error: Option<String>,
    pub public_key: Option<String>,
    /// The engine's real base URL - only ever populated (and only ever
    /// rendered) alongside `public_key`.
    pub endpoint: String,
    /// The submitted field values, echoed back into the re-rendered form on
    /// a validation error so a rejected submission doesn't throw away
    /// everything the merchant typed. All empty (network flags defaulting
    /// to mainnet) on a plain `GET`; never populated on success.
    pub site_url: String,
    pub view_key_hex: String,
    pub spend_pubkey_hex: String,
    pub allowed_origins: String,
    pub network_mainnet_selected: bool,
    pub network_stagenet_selected: bool,
    pub network_testnet_selected: bool,
    pub currency_options: Vec<crate::currencies::CurrencyOptionView>,
}

/// The same wallet-connection fields [`ConnectViewModel`] has, plus the
/// three values that must survive the round trip as hidden fields
/// (`return_url`/`nonce`) or be shown to the merchant (`site_url`), and
/// `platform` so the form's own `action` can post back to the same
/// `/connect/{platform}` path it was reached at.
pub struct PlatformConnectViewModel {
    pub platform: String,
    pub site_url: String,
    pub return_url: String,
    pub nonce: String,
    pub error: Option<String>,
    pub view_key_hex: String,
    pub spend_pubkey_hex: String,
    pub allowed_origins: String,
    pub network_mainnet_selected: bool,
    pub network_stagenet_selected: bool,
    pub network_testnet_selected: bool,
    pub currency_options: Vec<crate::currencies::CurrencyOptionView>,
    /// Every store this user already has connected (any platform) - lets
    /// the confirm screen offer "use an existing store" instead of always
    /// forcing a brand-new tenant to be provisioned.
    pub existing_stores: Vec<ExistingStoreOption>,
}

/// One entry in the "use an existing store" picker.
pub struct ExistingStoreOption {
    pub connection_id: String,
    pub display_name: String,
    pub platform: String,
}

fn network_select(mainnet: bool, stagenet: bool, testnet: bool) -> Markup {
    html! {
        select name="network" {
            option value="mainnet" selected[mainnet] { "mainnet" }
            option value="stagenet" selected[stagenet] { "stagenet" }
            option value="testnet" selected[testnet] { "testnet" }
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

/// Sensible default: mirror `site_url`'s own origin into `allowed_origins`,
/// but only while the merchant hasn't typed anything there themselves - a
/// small, optional convenience, not a requirement (the field stays a plain
/// text input either way).
const AUTOFILL_ORIGIN_SCRIPT: &str = r#"(function () {
  var siteUrl = document.getElementById("site_url");
  var allowedOrigins = document.getElementById("allowed_origins");
  var touched = false;
  allowedOrigins.addEventListener("input", function () { touched = true; });
  siteUrl.addEventListener("blur", function () {
    if (touched || allowedOrigins.value) return;
    try {
      var origin = new URL(siteUrl.value).origin;
      allowedOrigins.value = origin;
    } catch (e) { /* not a valid URL yet - leave it alone */ }
  });
})();"#;

pub fn page(chrome: &PageChrome, data: &ConnectViewModel) -> Markup {
    let body = html! {
        div class="wrap" {
            @if let Some(public_key) = &data.public_key {
                a class="btn" href="/dashboard" { "← back to dashboard" }
                h1 { "Store connected" }
                p { "Your public key: " code { (public_key) } }
                p class="hint" {
                    "Keep this value handy. Your secret token is stored securely and is never shown "
                    "here - the control plane keeps it on your behalf for the calls it makes on your store's behalf."
                }
                (super::integration_help::fragment(public_key, &data.endpoint, false))
            } @else {
                h1 { "Connect your store (advanced)" }
                p class="hint" {
                    "This form provisions your store directly from a watch-only key pair - no plugin "
                    "needed. Only a " strong { "view key" } " and a " strong { "public spend key" } " are ever "
                    "collected: this system can watch for incoming payments, but can never move funds, because it "
                    "never sees a spend " em { "key" } ", only the public spend address material. If you'd rather use a "
                    "guided flow, see the " a href="/dashboard/connections/new" { "add-a-store picker" } "."
                }
                @if let Some(error) = &data.error {
                    p class="error" { (error) }
                }
                form method="post" action="/dashboard/connect" id="connect-form" {
                    label {
                        "Site URL"
                        input type="url" name="site_url" id="site_url" value=(data.site_url) placeholder="https://shop.example.com" required;
                        span class="field-help" {
                            "Your storefront's own URL - shown on your dashboard, and used below to "
                            "suggest a default allowed origin."
                        }
                    }
                    label {
                        "View key (hex)"
                        input type="text" name="view_key_hex" value=(data.view_key_hex) required pattern="[0-9a-fA-F]{64}" placeholder="64 hex characters";
                        span class="field-help" {
                            "The " em { "private view key" } " of a watch-only wallet - lets this "
                            "service detect incoming payments. This is not your spend key and cannot move funds by itself."
                        }
                    }
                    label {
                        "Spend public key (hex)"
                        input type="text" name="spend_pubkey_hex" value=(data.spend_pubkey_hex) required pattern="[0-9a-fA-F]{64}" placeholder="64 hex characters";
                        span class="field-help" {
                            "The " em { "public" } " half of your spend key pair (not the private spend "
                            "key - never enter that anywhere). Together with the view key above, this is everything needed to "
                            "watch a wallet without ever being able to spend from it."
                        }
                    }
                    label {
                        "Network"
                        (network_select(data.network_mainnet_selected, data.network_stagenet_selected, data.network_testnet_selected))
                        span class="field-help" { "Leave on " code { "mainnet" } " unless this is a test wallet." }
                    }
                    label {
                        "Base currency"
                        (currency_select(&data.currency_options))
                        span class="field-help" {
                            "What custom confirmation thresholds are denominated in, and what an "
                            "order's own currency is converted into to pick one. Choosing a currency here doesn't require any "
                            "exchange-rate provider to actually support it yet - that only matters once it's actually used to "
                            "price something."
                        }
                    }
                    label {
                        "Allowed origins (comma-separated)"
                        input type="text" name="allowed_origins" id="allowed_origins" value=(data.allowed_origins) placeholder="https://shop.example.com";
                        span class="field-help" {
                            "Which browser origins may create orders directly against your store's "
                            "public API. Defaults to your site URL above if left blank - only change this if orders will be "
                            "created from a different origin (e.g. a headless frontend)."
                        }
                    }
                    button type="submit" { "Connect" }
                }
                script { (PreEscaped(AUTOFILL_ORIGIN_SCRIPT)) }
            }
        }
    };
    layout(chrome, "Connect your store - Monokulo", body)
}

pub fn platform_page(chrome: &PageChrome, data: &PlatformConnectViewModel) -> Markup {
    let body = html! {
        div class="wrap" {
            h1 { "Connect Monero payments for " (data.site_url) }
            p class="hint" {
                "Your platform sent you here to finish connecting - only a watch-only view key and "
                "public spend key are collected below, never a spend key. Once confirmed you'll be sent straight "
                "back to " (data.site_url) "."
            }
            @if let Some(error) = &data.error {
                p class="error" { (error) }
            }

            @if !data.existing_stores.is_empty() {
                div class="box" {
                    h2 { "Use an existing store" }
                    p class="hint" {
                        "Already have a store connected (maybe set up through the advanced form, or a different "
                        "site)? Connect this one to it directly - no keys to paste in."
                    }
                    form method="post" action=(format!("/connect/{}", data.platform)) {
                        input type="hidden" name="site_url" value=(data.site_url);
                        input type="hidden" name="return_url" value=(data.return_url);
                        input type="hidden" name="nonce" value=(data.nonce);
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
                        button type="submit" { "Connect this store" }
                    }
                }

                hr class="rule";
                h2 { "Or create a new store" }
            }

            form method="post" action=(format!("/connect/{}", data.platform)) {
                input type="hidden" name="site_url" value=(data.site_url);
                input type="hidden" name="return_url" value=(data.return_url);
                input type="hidden" name="nonce" value=(data.nonce);
                input type="hidden" name="mode" value="new";
                label {
                    "View key (hex)"
                    input type="text" name="view_key_hex" value=(data.view_key_hex) required pattern="[0-9a-fA-F]{64}" placeholder="64 hex characters";
                    span class="field-help" { "The private view key of a watch-only wallet." }
                }
                label {
                    "Spend public key (hex)"
                    input type="text" name="spend_pubkey_hex" value=(data.spend_pubkey_hex) required pattern="[0-9a-fA-F]{64}" placeholder="64 hex characters";
                    span class="field-help" { "The public half of your spend key pair - never your private spend key." }
                }
                label {
                    "Network"
                    (network_select(data.network_mainnet_selected, data.network_stagenet_selected, data.network_testnet_selected))
                }
                label {
                    "Allowed origins (comma-separated)"
                    input type="text" name="allowed_origins" value=(data.allowed_origins);
                    span class="field-help" { "Leave blank to default to your site's own origin." }
                }
                label {
                    "Base currency"
                    (currency_select(&data.currency_options))
                    span class="field-help" { "What custom confirmation thresholds are denominated in." }
                }
                button type="submit" { "Create a new store" }
            }
        }
    };
    layout(chrome, "Connect Monero payments - Monokulo", body)
}

pub fn new_store_picker_page(chrome: &PageChrome) -> Markup {
    let body = html! {
        div class="wrap" {
            h1 { "Add a store" }
            p { "Choose the setup that matches your storefront." }
            div class="pick-grid" {
                div class="pick-card" {
                    h3 { "Simple → WooCommerce" }
                    p {
                        "Running WordPress + WooCommerce? Install the plugin and click Connect - no keys to paste in "
                        "by hand."
                    }
                    a class="btn" href="/dashboard/connections/new/woocommerce" { "Set up WooCommerce" }
                }
                div class="pick-card" {
                    h3 { "Custom (advanced)" }
                    p {
                        "Integrating something else, or want full control over the wallet material yourself? Paste in "
                        "your own watch-only key pair directly."
                    }
                    a class="btn btn-secondary" href="/dashboard/connect" { "Advanced setup" }
                }
            }
        }
    };
    layout(chrome, "Add a store - Monokulo", body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chrome() -> PageChrome {
        PageChrome::from_user(None, "")
    }

    fn default_connect_data() -> ConnectViewModel {
        ConnectViewModel {
            error: None,
            public_key: None,
            endpoint: String::new(),
            site_url: String::new(),
            view_key_hex: String::new(),
            spend_pubkey_hex: String::new(),
            allowed_origins: String::new(),
            network_mainnet_selected: true,
            network_stagenet_selected: false,
            network_testnet_selected: false,
            currency_options: vec![],
        }
    }

    #[test]
    fn connect_page_renders_the_form_with_no_error_or_public_key() {
        let html = page(&chrome(), &default_connect_data()).into_string();
        assert!(html.contains("<form"));
        assert!(html.contains("view_key_hex"));
    }

    #[test]
    fn connect_page_shows_the_error_when_present() {
        let data = ConnectViewModel { error: Some("bad view key hex".to_string()), ..default_connect_data() };
        let html = page(&chrome(), &data).into_string();
        assert!(html.contains("bad view key hex"));
        assert!(html.contains("<form"), "the form must still be present on error");
    }

    /// The actual bug being fixed: a rejected submission used to lose every
    /// field the merchant typed, forcing them to retype two long hex keys
    /// from scratch. `error` being set must not mean these are gone.
    #[test]
    fn connect_page_re_fills_every_submitted_field_when_re_rendered_after_a_rejected_submission() {
        let data = ConnectViewModel {
            error: Some("bad view key hex".to_string()),
            site_url: "https://shop.example.com".to_string(),
            view_key_hex: "0707070707070707070707070707070707070707070707070707070707070707".to_string(),
            spend_pubkey_hex: "deadbeef".to_string(),
            allowed_origins: "https://shop.example.com, https://admin.example.com".to_string(),
            network_mainnet_selected: false,
            network_stagenet_selected: true,
            ..default_connect_data()
        };
        let html = page(&chrome(), &data).into_string();
        assert!(html.contains(r#"value="https://shop.example.com""#), "expected site_url echoed back, got: {html}");
        assert!(
            html.contains(r#"value="0707070707070707070707070707070707070707070707070707070707070707""#),
            "expected view_key_hex echoed back, got: {html}"
        );
        assert!(html.contains(r#"value="deadbeef""#), "expected spend_pubkey_hex echoed back, got: {html}");
        assert!(
            html.contains(r#"value="https://shop.example.com, https://admin.example.com""#),
            "expected allowed_origins echoed back, got: {html}"
        );
        assert!(html.contains(r#"value="stagenet" selected"#), "expected the stagenet option marked selected, got: {html}");
        assert!(!html.contains(r#"value="mainnet" selected"#), "mainnet must not stay marked selected once stagenet was actually submitted, got: {html}");
    }

    #[test]
    fn connect_page_shows_the_public_key_instead_of_the_form_on_success() {
        let data = ConnectViewModel {
            public_key: Some("pk_deadbeef".to_string()),
            endpoint: "http://127.0.0.1:8080".to_string(),
            ..default_connect_data()
        };
        let html = page(&chrome(), &data).into_string();
        assert!(html.contains("pk_deadbeef"));
        assert!(!html.contains("<form"), "the confirmation view should not still show the form");
    }

    fn default_platform_data() -> PlatformConnectViewModel {
        PlatformConnectViewModel {
            platform: "woocommerce".to_string(),
            site_url: "https://shop.example.com".to_string(),
            return_url: "https://shop.example.com/settings".to_string(),
            nonce: "nonce-abc".to_string(),
            error: None,
            view_key_hex: String::new(),
            spend_pubkey_hex: String::new(),
            allowed_origins: String::new(),
            network_mainnet_selected: true,
            network_stagenet_selected: false,
            network_testnet_selected: false,
            currency_options: vec![],
            existing_stores: vec![],
        }
    }

    #[test]
    fn platform_page_renders_the_confirm_form() {
        let html = platform_page(&chrome(), &default_platform_data()).into_string();
        assert!(html.contains("https://shop.example.com"));
        assert!(html.contains(r#"action="/connect/woocommerce""#));
        assert!(html.contains(r#"name="return_url" value="https://shop.example.com/settings""#));
        assert!(html.contains(r#"name="nonce" value="nonce-abc""#));
        assert!(html.contains("view_key_hex"));
    }

    #[test]
    fn platform_page_shows_the_error_when_present() {
        let data = PlatformConnectViewModel { error: Some("bad view key hex".to_string()), ..default_platform_data() };
        let html = platform_page(&chrome(), &data).into_string();
        assert!(html.contains("bad view key hex"));
        assert!(html.contains("<form"), "the form must still be present on error");
    }

    #[test]
    fn platform_page_re_fills_every_submitted_field_when_re_rendered_after_a_rejected_submission() {
        let data = PlatformConnectViewModel {
            error: Some("bad view key hex".to_string()),
            view_key_hex: "0707070707070707070707070707070707070707070707070707070707070707".to_string(),
            spend_pubkey_hex: "deadbeef".to_string(),
            allowed_origins: "https://shop.example.com".to_string(),
            network_stagenet_selected: true,
            network_mainnet_selected: false,
            ..default_platform_data()
        };
        let html = platform_page(&chrome(), &data).into_string();
        assert!(
            html.contains(r#"value="0707070707070707070707070707070707070707070707070707070707070707""#),
            "expected view_key_hex echoed back, got: {html}"
        );
        assert!(html.contains(r#"value="deadbeef""#), "expected spend_pubkey_hex echoed back, got: {html}");
        assert!(
            html.contains(r#"name="allowed_origins" value="https://shop.example.com""#),
            "expected allowed_origins echoed back, got: {html}"
        );
        assert!(html.contains(r#"value="stagenet" selected"#), "expected the stagenet option marked selected, got: {html}");
    }

    #[test]
    fn new_store_picker_links_to_both_flows() {
        let html = new_store_picker_page(&chrome()).into_string();
        assert!(html.contains(r#"href="/dashboard/connections/new/woocommerce""#));
        assert!(html.contains(r#"href="/dashboard/connect""#));
    }
}
