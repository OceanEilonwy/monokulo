//! A shop plugin's connect flow (`/plugins/connect/{platform}`,
//! `http::connect`), where it isn't setup: connecting the plugin to the
//! merchant's store that already has the shop's site, or saying why it
//! can't connect. A shop with no store yet goes through setup (`/setup`,
//! `views::setup`), which ends back at the plugin.
//!
//! Also the key storage choice, shared by the forms that take a wallet's
//! keys.

use maud::{html, Markup};

use super::PageChrome;
use crate::plugins::Plugin;
use crate::stores::Site;
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
    pub plugin: Plugin,
    pub site_url: String,
    pub return_url: String,
    pub nonce: String,
    /// The shop's site.
    pub host: Site,
}

/// `GET /plugins/connect/{platform}` for a shop whose site one of the merchant's
/// stores already has: connect the plugin to that store. With an onion
/// address (`onion`), it says that customers shopping on the shop's own
/// onion address pay there.
pub fn existing_store_page(
    chrome: &PageChrome,
    request: &PluginRequestView,
    store_id: &str,
    store_name: &str,
    onion: Option<&str>,
) -> Markup {
    let header = ui::PageHeader::new(format!("Connect your {} shop", request.plugin));
    let body = html! {
        p {
            "The " (request.plugin) " plugin on " strong { (request.host) } " asked to connect. Your store "
            a href=(format!("/dashboard/stores/{store_id}")) { (store_name) } " already uses that site, so it "
            "connects to that store: the same orders, settings and wallet."
        }
        @if let Some(onion) = onion {
            (ui::Hint::new(html! {
                "Customers shopping on the shop's own .onion address pay at " code { (onion) } ", so they stay on Tor. "
                "Everyone else pays on the web address."
            }))
        }
        div class="step-foot" {
            (ui::LinkButton::new("Cancel", request.host.link()))
            span class="spacer" {}
            form method="post" action=(format!("/plugins/connect/{}", request.plugin.slug())) class="inline-form" {
                input type="hidden" name="connection_id" value=(store_id);
                input type="hidden" name="site_url" value=(request.site_url);
                input type="hidden" name="return_url" value=(request.return_url);
                input type="hidden" name="nonce" value=(request.nonce);
                @if let Some(name) = request.plugin.name() {
                    input type="hidden" name="name" value=(name.as_str());
                }
                (ui::Button::new(format!("Connect to {store_name}"))
                    .variant(ui::ButtonVariant::Primary)
                    .kind(ui::ButtonType::Submit))
            }
        }
    };
    super::Page::new("Connect Monero payments - Monokulo", header).render(chrome, body)
}

/// `GET`/`POST /plugins/connect/{platform}` when the plugin can't connect: why, and
/// no form.
pub fn cannot_connect_page(
    chrome: &PageChrome,
    plugin: &Plugin,
    host: Option<&Site>,
    why: &str,
) -> Markup {
    let header = ui::PageHeader::new(format!("Connect your {plugin} shop"));
    let body = html! {
        @if let Some(host) = host {
            p { "The " (plugin) " plugin on " strong { (host) } " asked to connect to your account." }
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
            plugin: acme_cart(),
            site_url: "https://shop.example.com".into(),
            return_url: "https://shop.example.com/settings".into(),
            nonce: "nonce-abc".into(),
            host: Site::parse("shop.example.com").unwrap(),
        }
    }

    fn acme_cart() -> Plugin {
        Plugin::parse("acme-cart", "Acme Cart").unwrap()
    }

    #[test]
    fn a_shop_your_store_already_has_connects_to_it_with_one_button() {
        let html =
            existing_store_page(&chrome(), &request(), "c_1", "Corner shop", None).into_string();
        assert!(
            html.contains("<h1>Connect your Acme Cart shop</h1>"),
            "{html}"
        );
        assert!(html.contains("The Acme Cart plugin on <strong>shop.example.com</strong>"));
        assert!(html.contains(r#"action="/plugins/connect/acme-cart""#));
        assert!(html.contains(r#"name="connection_id" value="c_1""#));
        assert!(html.contains(r#"name="return_url" value="https://shop.example.com/settings""#));
        assert!(html.contains(r#"name="nonce" value="nonce-abc""#));
        assert!(html.contains(r#"name="name" value="Acme Cart""#));
        assert!(html.contains("Connect to Corner shop"));
        assert_eq!(html.matches(r#"data-variant="primary""#).count(), 1);
        assert!(!html.contains(".onion"), "nothing about Tor without it");
        let onion = "http://2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion";
        let html = existing_store_page(&chrome(), &request(), "c_1", "Corner shop", Some(onion))
            .into_string();
        assert!(
            html.contains(onion) && html.contains("stay on Tor"),
            "{html}"
        );
    }

    /// A plugin's name is its own word for itself, shown as text: markup in
    /// it is escaped.
    #[test]
    fn a_plugins_name_is_shown_as_text() {
        let request = PluginRequestView {
            plugin: Plugin::parse("acme-cart", "<b>Shop</b>").unwrap(),
            ..request()
        };
        let html =
            existing_store_page(&chrome(), &request, "c_1", "Corner shop", None).into_string();
        assert!(
            html.contains("The &lt;b&gt;Shop&lt;/b&gt; plugin"),
            "{html}"
        );
        assert!(!html.contains("<b>Shop</b>"));
    }

    #[test]
    fn a_shop_that_cant_connect_says_why_and_shows_no_form() {
        let html = cannot_connect_page(
            &chrome(),
            &acme_cart(),
            Site::parse("shop.example.com").ok().as_ref(),
            "No public address yet.",
        )
        .into_string();
        assert!(html.contains("Connect your Acme Cart shop"));
        assert!(html.contains("No public address yet."));
        assert!(!html.contains("<form"));
    }
}
