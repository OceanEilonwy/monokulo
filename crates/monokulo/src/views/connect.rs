//! The WooCommerce plugin's connect flow (`/connect/{platform}`,
//! `http::connect`), where it isn't setup: connecting the plugin to the
//! merchant's store that already has the shop's site, or saying why it
//! can't connect. A shop with no store yet goes through setup (`/setup`,
//! `views::setup`), which ends back at the plugin.
//!
//! Also the key storage choice, shared by the forms that take a wallet's
//! keys.

use maud::{html, Markup};

use super::PageChrome;
use ui::{Choice, Select};

/// One key custody backend a new wallet's keys can go to (part 5).
pub struct CustodyChoice {
    pub backend: String,
    pub label: String,
    pub selected: bool,
}

/// The key storage choice, shown only when this instance offers more than
/// one backend.
pub(crate) fn custody_select(choices: &[CustodyChoice]) -> Markup {
    html! {
        @if choices.len() > 1 {
            label {
                "Key storage"
                (Select::new(
                    "key_custody_backend",
                    choices
                        .iter()
                        .map(|choice| Choice::new(&choice.backend, &choice.label).selected(choice.selected))
                        .collect::<Vec<_>>(),
                ))
                (ui::FieldNote::new(html! { "Where this wallet's view key is kept. You can move it later from a store's settings, "
                    "by entering the keys again." }))
            }
        }
    }
}

/// The plugin's request: what it was sent with, carried in the form that
/// gives it the store's key.
pub struct PluginRequestView {
    pub platform: String,
    pub site_url: String,
    pub return_url: String,
    pub nonce: String,
    /// The shop's host, as it's shown.
    pub host: String,
}

/// `GET /connect/{platform}` for a shop whose site one of the merchant's
/// stores already has: connect the plugin to that store.
pub fn existing_store_page(
    chrome: &PageChrome,
    request: &PluginRequestView,
    store_id: &str,
    store_name: &str,
) -> Markup {
    let header = ui::PageHeader::new("Connect your WooCommerce shop");
    let body = html! {
        p {
            "The WooCommerce plugin on " strong { (request.host) } " asked to connect. Your store "
            a href=(format!("/dashboard/stores/{store_id}")) { (store_name) } " already uses that site, so it "
            "connects to that store: the same orders, settings and wallet."
        }
        div class="step-foot" {
            (ui::LinkButton::new("Cancel", crate::stores::site_link(&request.host)))
            span class="spacer" {}
            form method="post" action=(format!("/connect/{}", request.platform)) class="inline-form" {
                input type="hidden" name="connection_id" value=(store_id);
                input type="hidden" name="site_url" value=(request.site_url);
                input type="hidden" name="return_url" value=(request.return_url);
                input type="hidden" name="nonce" value=(request.nonce);
                (ui::Button::new(format!("Connect to {store_name}"))
                    .variant(ui::ButtonVariant::Primary)
                    .kind(ui::ButtonType::Submit))
            }
        }
    };
    super::Page::new("Connect Monero payments - Monokulo", header).render(chrome, body)
}

/// `GET`/`POST /connect/{platform}` when the plugin can't connect: why, and
/// no form.
pub fn cannot_connect_page(chrome: &PageChrome, host: Option<&str>, why: &str) -> Markup {
    let header = ui::PageHeader::new("Connect your WooCommerce shop");
    let body = html! {
        @if let Some(host) = host {
            p { "The WooCommerce plugin on " strong { (host) } " asked to connect to your account." }
        }
        (ui::Alert::new(why, ui::Tone::Error))
        p { a href="/" { "Go to your dashboard" } }
    };
    super::Page::new("Connect Monero payments - Monokulo", header).render(chrome, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chrome() -> PageChrome {
        PageChrome::from_user(None, "")
    }

    fn request() -> PluginRequestView {
        PluginRequestView {
            platform: "woocommerce".into(),
            site_url: "https://shop.example.com".into(),
            return_url: "https://shop.example.com/settings".into(),
            nonce: "nonce-abc".into(),
            host: "shop.example.com".into(),
        }
    }

    #[test]
    fn a_shop_your_store_already_has_connects_to_it_with_one_button() {
        let html = existing_store_page(&chrome(), &request(), "c_1", "Corner shop").into_string();
        assert!(html.contains(r#"action="/connect/woocommerce""#));
        assert!(html.contains(r#"name="connection_id" value="c_1""#));
        assert!(html.contains(r#"name="return_url" value="https://shop.example.com/settings""#));
        assert!(html.contains(r#"name="nonce" value="nonce-abc""#));
        assert!(html.contains("Connect to Corner shop"));
        assert_eq!(html.matches(r#"data-variant="primary""#).count(), 1);
    }

    #[test]
    fn a_shop_that_cant_connect_says_why_and_shows_no_form() {
        let html = cannot_connect_page(
            &chrome(),
            Some("shop.example.com"),
            "No public address yet.",
        )
        .into_string();
        assert!(html.contains("No public address yet."));
        assert!(!html.contains("<form"));
    }
}
