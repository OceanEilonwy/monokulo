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
    pub backend: engine_protocol::custody::CustodyBackend,
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
                        .map(|choice| Choice::new(choice.backend.as_str(), &choice.label).selected(choice.selected))
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
            a href=(crate::base::url(format!("/stores/{store_id}"))) { (store_name) } " already uses that site, so it "
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
            form method="post" action=(crate::base::url(format!("/plugins/connect/{}", request.plugin.slug()))) class="inline-form" {
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

/// A store offered for a shop whose site none of the merchant's stores has.
pub struct NearStoreView {
    pub id: String,
    pub name: String,
    /// Its site; `None` for a store with none.
    pub site: Option<String>,
    pub reason: crate::http::connect::NearReason,
}

/// `GET /plugins/connect/{platform}` when no store has the shop's site but
/// some of the merchant's are close: connect one of them (its site becomes
/// the shop's), or make a new store.
pub fn near_match_page(
    chrome: &PageChrome,
    request: &PluginRequestView,
    stores: &[NearStoreView],
) -> Markup {
    let header = ui::PageHeader::new(format!("Connect your {} shop", request.plugin));
    let new_store = {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query
            .append_pair("site_url", &request.site_url)
            .append_pair("return_url", &request.return_url)
            .append_pair("nonce", &request.nonce)
            .append_pair("new", "1");
        if let Some(name) = request.plugin.name() {
            query.append_pair("name", name.as_str());
        }
        crate::base::url(format!(
            "/plugins/connect/{}?{}",
            request.plugin.slug(),
            query.finish()
        ))
    };
    let body = html! {
        p {
            "The " (request.plugin) " plugin on " strong { (request.host) } " asked to connect, and none of your "
            "stores uses that site. Connect one of these, or make a new store."
        }
        ul class="int-rows" {
            @for store in stores {
                li {
                    span {
                        span class="int-name" { (store.name) }
                        span class="int-meta" {
                            @match (&store.site, store.reason) {
                                (Some(site), crate::http::connect::NearReason::WwwVariant) => { "Uses " (site) ", the same site with or without www." }
                                (Some(site), _) => { "Uses " (site) "; its verified domains cover " (request.host) "." }
                                (None, _) => { "Has no website yet." }
                            }
                            " Connecting sets its website to " (request.host) "."
                        }
                    }
                    form method="post" action=(crate::base::url(format!("/plugins/connect/{}", request.plugin.slug()))) class="inline-form" {
                        input type="hidden" name="connection_id" value=(store.id);
                        input type="hidden" name="adopt" value="1";
                        input type="hidden" name="site_url" value=(request.site_url);
                        input type="hidden" name="return_url" value=(request.return_url);
                        input type="hidden" name="nonce" value=(request.nonce);
                        @if let Some(name) = request.plugin.name() {
                            input type="hidden" name="name" value=(name.as_str());
                        }
                        (ui::Button::new("Connect")
                            .variant(ui::ButtonVariant::Primary)
                            .kind(ui::ButtonType::Submit))
                    }
                }
            }
        }
        div class="step-foot" {
            (ui::LinkButton::new("Cancel", request.host.link()))
            span class="spacer" {}
            (ui::LinkButton::new(html! { "Make a new store for " (request.host) }, new_store))
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
        p { a href=(crate::base::url("/")) { "Go to your dashboard" } }
    };
    super::Page::new("Connect Monero payments - Monokulo", header).render(chrome, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chrome() -> PageChrome {
        PageChrome::from_user(None, "", clock::Clock::System.now())
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
}
