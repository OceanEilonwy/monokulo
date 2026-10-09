//! A store's website and the plugins connected to it, on its settings page
//! (`docs/design/user-testing/store-site-integrations.html`: A3, B1, C1).
//!
//! - The Store card's website is locked while a plugin is connected
//!   ("Set by WooCommerce · see Connections").
//! - The Connections card lists each plugin: its site, version, when it
//!   connected and its last order, with "Disconnect…"; past ones fold
//!   under "Before".
//! - Changing or removing the website asks first (B1): blocked while a
//!   plugin is connected, then what follows and the store's name typed.
//! - Disconnecting a plugin asks first (C1): blocked while an order from
//!   the shop can still be paid, then what follows (its key changes, its
//!   webhook is removed, the website unlocks) and the store's name typed.
//!
//! Each dialog is also a page, for a browser without JavaScript, drawn by
//! the same function. Handlers: `http::store_site`.

use maud::{html, Markup};

use super::{layout, PageChrome};

/// A plugin connected to the store, now or before.
pub struct IntegrationView {
    pub id: String,
    /// What it's called: "WooCommerce".
    pub name: String,
    /// The store's site when it connected.
    pub site: String,
    /// The plugin's version; empty when it never said.
    pub version: String,
    /// When it connected, in the viewer's time zone.
    pub connected: String,
    /// Its last order.
    pub last_order: Option<String>,
    /// When it was disconnected; `None` while it's active.
    pub until: Option<String>,
    /// The paid webhook it registered, removed when it's disconnected.
    pub webhook_url: Option<String>,
}

/// Orders that can still be paid, and until about when.
pub struct OpenOrders {
    pub count: u64,
    pub until: Option<String>,
}

/// What the website and connections parts of the settings page show.
pub struct SiteView {
    pub store_id: String,
    pub store_name: String,
    /// The store's site; empty when it has none.
    pub site: String,
    /// The plugin connected now: the site is locked.
    pub active: Option<IntegrationView>,
    /// Plugins connected before.
    pub past: Vec<IntegrationView>,
    /// The store's orders that can still be paid; `None` when the engine
    /// couldn't say.
    pub open_orders: Option<OpenOrders>,
    /// Those the connected plugin made; `None` when the engine couldn't say.
    pub plugin_open_orders: Option<OpenOrders>,
    /// The site is among the store's domains (waiting or verified).
    pub site_domain: bool,
}

impl SiteView {
    fn settings(&self) -> String {
        format!("/dashboard/stores/{}/settings", self.store_id)
    }

    pub fn website_path(&self) -> String {
        format!("{}/website", self.settings())
    }

    pub fn disconnect_path(&self, integration: &IntegrationView) -> String {
        format!(
            "{}/connections/{}/disconnect",
            self.settings(),
            integration.id
        )
    }
}

/// Changing the website to another, or removing it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebsiteChange {
    To(String),
    Remove,
}

fn lock_icon() -> Markup {
    html! {
        svg class="ico" viewBox="0 0 24 24" aria-hidden="true" focusable="false" {
            rect x="4" y="11" width="16" height="10" rx="2" {}
            path d="M8 11V7a4 4 0 0 1 8 0v4" {}
        }
    }
}

/// WooCommerce's mark: its speech bubble with "Woo" in it.
fn woo_mark() -> Markup {
    html! {
        span class="int-mark" aria-hidden="true" {
            svg viewBox="0 0 32 24" focusable="false" {
                path class="woo-bubble" d="M4 3h24a3 3 0 0 1 3 3v9a3 3 0 0 1-3 3H17l-5 4 1-4H4a3 3 0 0 1-3-3V6a3 3 0 0 1 3-3z" {}
                text class="woo-text" x="16" y="14.6" text-anchor="middle" { "Woo" }
            }
        }
    }
}

/// The Store card's website field: an ordinary field (adding one saves
/// with the settings form; changing or emptying a saved one asks first),
/// or locked while a plugin is connected.
pub fn website_field(view: &SiteView, shown: &str) -> Markup {
    match &view.active {
        Some(active) => html! {
            div class="setting-field" {
                div class="setting-label-row" { span class="setting-label" id="store-site-label" { "Your website" } }
                div class="locked-input" aria-labelledby="store-site-label" { (lock_icon()) (view.site) }
                p class="lock-note" {
                    "Set by " (active.name) " · " a href="#card-connections" { "see Connections" }
                }
            }
        },
        None => html! {
            (super::settings::Field::new("Your website (optional)", "store-site")
                .help(None, html! {
                    "Needed for the checkout on your pages or for the WooCommerce plugin, which finds this store by it. "
                    "Any page on the site works."
                    @if !view.site.is_empty() { " Changing or removing it asks first." }
                })
                .render(html! {
                    input type="text" id="store-site" name="store_site" value=(shown) inputmode="url" autocomplete="url"
                        placeholder="shop.example" data-site-saved=(view.site);
                }))
        },
    }
}

/// The Connections card: each plugin connected to the store, with
/// Disconnect…; the ones before folded under "Before". An action card of
/// its own, outside the settings form.
pub fn connections_card(view: &SiteView) -> Markup {
    let meta = if view.active.is_some() {
        "1 active"
    } else {
        "none"
    };
    html! {
        section id="card-connections" class="settings-card" aria-labelledby="card-connections-title" {
            header class="card-head" { h3 id="card-connections-title" { "Connections" } span class="card-meta" { (meta) } }
            div class="card-body" {
                @match &view.active {
                    Some(active) => {
                        ul class="int-rows" {
                            li {
                                (woo_mark())
                                span {
                                    span class="int-name" { (active.name) } " " span class="tag-active" { "active" }
                                    span class="int-meta" { (integration_meta(active)) }
                                }
                                a class="btn" href=(view.disconnect_path(active)) data-opens-dialog="disconnect-dialog" { "Disconnect…" }
                            }
                        }
                    }
                    None if view.site.is_empty() => {
                        p class="hint card-note" { "The WooCommerce plugin needs your website: add it above first." }
                    }
                    None => {
                        p class="hint card-note" {
                            "No plugin is connected. "
                            a href=(super::docs_url("woocommerce/")) target="_blank" rel="noopener" { "Install the WooCommerce plugin" }
                            "; it finds this store by " (view.site) "."
                        }
                    }
                }
                @if !view.past.is_empty() {
                    h4 { "Before" }
                    ul class="int-rows past" {
                        @for past in &view.past {
                            li {
                                (woo_mark())
                                span {
                                    span class="int-name" { (past.name) }
                                    span class="int-meta" {
                                        (past.site)
                                        @if !past.version.is_empty() { " · plugin " (past.version) }
                                        " · " (past.connected) " to " (past.until.as_deref().unwrap_or(""))
                                    }
                                }
                                span {}
                            }
                        }
                    }
                }
            }
        }
    }
}

fn integration_meta(i: &IntegrationView) -> String {
    let mut parts = vec![i.site.clone()];
    if !i.version.is_empty() {
        parts.push(format!("plugin {}", i.version));
    }
    parts.push(format!("connected {}", i.connected));
    parts.push(match &i.last_order {
        Some(at) => format!("last order {at}"),
        None => "no orders yet".to_owned(),
    });
    parts.join(" · ")
}

/// The dialogs the settings page holds, opened by its buttons and its
/// save; each also a page (`website_page`, `disconnect_page`).
pub fn dialogs(view: &SiteView) -> Markup {
    html! {
        @if !view.site.is_empty() {
            dialog id="website-dialog" class="settings-dialog confirm-dialog" aria-labelledby="website-title" {
                (website_content(view, &WebsiteChange::To(String::new()), None, true))
            }
            dialog id="website-remove-dialog" class="settings-dialog confirm-dialog" aria-labelledby="website-remove-title" {
                (website_content(view, &WebsiteChange::Remove, None, true))
            }
        }
        @if view.active.is_some() {
            dialog id="disconnect-dialog" class="settings-dialog confirm-dialog" aria-labelledby="disconnect-title" {
                (disconnect_content(view, None, true))
            }
        }
    }
}

fn heading(id: &str, title: Markup, in_dialog: bool) -> Markup {
    html! {
        @if in_dialog {
            h2 id=(id) class="dialog-title" {
                (title)
                button type="button" class="dialog-x" aria-label="Close" data-closes-dialog { "×" }
            }
        } @else {
            h1 id=(id) class="dialog-title" { (title) }
        }
    }
}

/// One row of a checklist: ✓ done, ✕ not yet with its fix beside it, or →
/// something that follows.
fn row(mark: Mark, what: Markup, fix: Option<Markup>) -> Markup {
    let (class, sign, hidden) = match mark {
        Mark::Ok => ("ok", "✓", "Done: "),
        Mark::No => ("no", "✕", "Not yet: "),
        Mark::Then => ("then", "→", ""),
    };
    html! {
        li class=(class) {
            span class="mark" aria-hidden="true" { (sign) }
            span {
                @if !hidden.is_empty() { span class="visually-hidden" { (hidden) } }
                (what)
                @if let Some(fix) = fix { span class="fix" { (fix) } }
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Mark {
    Ok,
    No,
    Then,
}

fn orders_word(n: u64) -> String {
    if n == 1 {
        "1 order".to_owned()
    } else {
        format!("{n} orders")
    }
}

fn confirm_field(id: &str, store_name: &str) -> Markup {
    html! {
        div class="setting-field" {
            div class="setting-label-row" {
                label class="setting-label" for=(id) { "Type " strong { "\u{201c}" (store_name) "\u{201d}" } " to confirm" }
            }
            input type="text" id=(id) name="confirm" autocomplete="off" spellcheck="false" required;
        }
    }
}

/// B1: changing the website to another, or removing it. Blocked while a
/// plugin is connected; then what follows, and the store's name typed.
pub fn website_content(
    view: &SiteView,
    change: &WebsiteChange,
    error: Option<&str>,
    in_dialog: bool,
) -> Markup {
    let remove = matches!(change, WebsiteChange::Remove);
    let id = if remove && in_dialog {
        "website-remove-title"
    } else {
        "website-title"
    };
    let verb = if remove { "Remove" } else { "Change" };
    let title = html! { (verb) " the website of \u{201c}" (view.store_name) "\u{201d}?" };
    let back = view.settings();
    let confirm_id = if remove {
        "website-remove-confirm"
    } else {
        "website-confirm"
    };
    html! {
        (heading(id, title, in_dialog))
        @if remove {
            p { "Its checkout runs on its website: without one, the store takes payments at the till only." }
        } @else {
            p { "Its checkout runs on its website, so changing it moves where the checkout works." }
        }
        @if let Some(error) = error { p class="error" role="alert" { (error) } }
        @match &view.active {
            Some(active) => {
                ul class="checks" {
                    (row(Mark::No, html! { "No plugin is connected to it" }, Some(html! {
                        (active.name) " on " (active.site) " is · "
                        a href=(view.disconnect_path(active)) data-opens-dialog=[in_dialog.then_some("disconnect-dialog")] { "disconnect it first" }
                    })))
                }
                p class="hint" id=(format!("{confirm_id}-why")) { (verb) "ing becomes available once the plugin is disconnected." }
                div class="dialog-actions" {
                    a class="btn" href=(back) data-closes-dialog { "Close" }
                    button type="button" class="btn-danger" disabled aria-describedby=(format!("{confirm_id}-why")) { (verb) " website" }
                }
            }
            None => {
                form method="post" action=(view.website_path()) {
                    @match change {
                        WebsiteChange::To(new) => {
                            div class="setting-field first" {
                                div class="setting-label-row" { label class="setting-label" for=(format!("{confirm_id}-site")) { "New website" } }
                                input type="text" id=(format!("{confirm_id}-site")) name="site" value=(new) inputmode="url" placeholder="shop.example" required;
                            }
                        }
                        WebsiteChange::Remove => { input type="hidden" name="remove" value="1"; }
                    }
                    ul class="checks" {
                        (row(Mark::Then, html! { "Checkouts embedded on " (view.site) " stop loading" }, None))
                        @if view.site_domain {
                            (row(Mark::Then, html! { "Its verified domain " (view.site) " is dropped" },
                                (!remove).then(|| html! { "the new one waits to be verified" })))
                        }
                        @if let Some(open) = view.open_orders.as_ref().filter(|o| o.count > 0) {
                            (row(Mark::Then, html! { (orders_word(open.count)) " started there can still be paid" }, Some(html! {
                                @if let Some(until) = &open.until { "until about " (until) "; " }
                                "Monokulo keeps watching them"
                            })))
                        }
                    }
                    (confirm_field(confirm_id, &view.store_name))
                    div class="dialog-actions" {
                        a class="btn" href=(back) data-closes-dialog { "Cancel" }
                        button type="submit" class="btn-danger" { (verb) " website" }
                    }
                }
            }
        }
    }
}

/// C1: disconnecting the connected plugin. Blocked while an order from the
/// shop can still be paid (or the engine can't say); then what follows,
/// and the store's name typed.
pub fn disconnect_content(view: &SiteView, error: Option<&str>, in_dialog: bool) -> Markup {
    let Some(active) = &view.active else {
        return html! {};
    };
    let title =
        html! { "Disconnect " (active.name) " from \u{201c}" (view.store_name) "\u{201d}?" };
    let back = view.settings();
    let ready = view
        .plugin_open_orders
        .as_ref()
        .is_some_and(|o| o.count == 0);
    html! {
        (heading("disconnect-title", title, in_dialog))
        p {
            "The store's secret key changes, so the plugin on " (active.site) " can't make orders: the shop stops taking "
            "Monero until you connect it again from the plugin."
        }
        @if let Some(error) = error { p class="error" role="alert" { (error) } }
        ul class="checks" {
            @match &view.plugin_open_orders {
                None => {
                    (row(Mark::No, html! { "Monokulo can't check its orders right now" }, Some(html! {
                        a href=(view.disconnect_path(active)) { "try again" }
                    })))
                }
                Some(open) if open.count > 0 => {
                    (row(Mark::No, html! { "No order from " (active.site) " can still be paid" }, Some(html! {
                        (open.count) " can be paid"
                        @if let Some(until) = &open.until { " until about " (until) }
                    })))
                }
                Some(_) => {
                    (row(Mark::Ok, html! { "No order from " (active.site) " can still be paid" }, None))
                    (row(Mark::Then, html! { "New Monero orders on " (active.site) " stop" }, None))
                    @if let Some(url) = &active.webhook_url {
                        (row(Mark::Then, html! { "The plugin's webhook is removed" }, Some(html! { (url) })))
                    }
                    (row(Mark::Then, html! { "The website unlocks" }, Some(html! { "change or remove it in the Store card" })))
                }
            }
        }
        @if ready {
            form method="post" action=(view.disconnect_path(active)) {
                (confirm_field("disconnect-confirm", &view.store_name))
                div class="dialog-actions" {
                    a class="btn" href=(back) data-closes-dialog { "Cancel" }
                    button type="submit" class="btn-danger" { "Disconnect " (active.name) }
                }
            }
        } @else {
            p class="hint" id="disconnect-why" { "Disconnecting becomes available once no order from the shop can be paid." }
            div class="dialog-actions" {
                a class="btn" href=(back) data-closes-dialog { "Close" }
                button type="button" class="btn-danger" disabled aria-describedby="disconnect-why" { "Disconnect " (active.name) }
            }
        }
    }
}

fn dialog_page(chrome: &PageChrome, view: &SiteView, title: &str, content: Markup) -> Markup {
    let body = html! {
        div class="wrap settings-page" {
            (super::store_breadcrumb(&view.store_id, &view.store_name, false))
            div class="confirm-dialog-page" { (content) }
        }
    };
    layout(
        chrome,
        &format!("{title} - {} - Monokulo", view.store_name),
        body,
    )
}

/// `GET /dashboard/stores/{id}/settings/website`: B1 as a page.
pub fn website_page(
    chrome: &PageChrome,
    view: &SiteView,
    change: &WebsiteChange,
    error: Option<&str>,
) -> Markup {
    let title = match change {
        WebsiteChange::To(_) => "Change website",
        WebsiteChange::Remove => "Remove website",
    };
    dialog_page(
        chrome,
        view,
        title,
        website_content(view, change, error, false),
    )
}

/// `GET /dashboard/stores/{id}/settings/connections/{integration}/disconnect`:
/// C1 as a page.
pub fn disconnect_page(chrome: &PageChrome, view: &SiteView, error: Option<&str>) -> Markup {
    dialog_page(
        chrome,
        view,
        "Disconnect",
        disconnect_content(view, error, false),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn woo(until: Option<&str>) -> IntegrationView {
        IntegrationView {
            id: "int_1".into(),
            name: "WooCommerce".into(),
            site: "shop.example".into(),
            version: "0.4.0".into(),
            connected: "3 Oct".into(),
            last_order: Some("today".into()),
            until: until.map(str::to_owned),
            webhook_url: Some("https://shop.example/?wc-api=monokulo".into()),
        }
    }

    fn view(active: bool, plugin_open: Option<u64>) -> SiteView {
        SiteView {
            store_id: "c_1".into(),
            store_name: "Bakery".into(),
            site: "shop.example".into(),
            active: active.then(|| woo(None)),
            past: vec![woo(Some("12 Sep"))],
            open_orders: Some(OpenOrders {
                count: 3,
                until: Some("14:20".into()),
            }),
            plugin_open_orders: plugin_open.map(|count| OpenOrders {
                count,
                until: Some("14:20".into()),
            }),
            site_domain: true,
        }
    }

    #[test]
    fn a_connected_plugin_locks_the_website_and_lists_in_connections() {
        let v = view(true, Some(0));
        let field = website_field(&v, "shop.example").into_string();
        assert!(
            field.contains(r#"<div class="locked-input" aria-labelledby="store-site-label">"#),
            "{field}"
        );
        assert!(
            field.contains(
                r##"Set by WooCommerce · <a href="#card-connections">see Connections</a>"##
            ),
            "{field}"
        );
        assert!(!field.contains("<input"), "nothing to type: {field}");
        let card = connections_card(&v).into_string();
        assert!(
            card.contains("shop.example · plugin 0.4.0 · connected 3 Oct · last order today"),
            "{card}"
        );
        assert!(card.contains(r#"<a class="btn" href="/dashboard/stores/c_1/settings/connections/int_1/disconnect" data-opens-dialog="disconnect-dialog">Disconnect…</a>"#), "{card}");
        assert!(
            card.contains("<h4>Before</h4>") && card.contains("3 Oct to 12 Sep"),
            "{card}"
        );
    }

    #[test]
    fn with_no_plugin_the_website_is_a_field_that_asks_before_changing() {
        let v = view(false, None);
        let field = website_field(&v, "shop.example").into_string();
        assert!(
            field.contains(r#"name="store_site" value="shop.example""#)
                && field.contains(r#"data-site-saved="shop.example""#),
            "{field}"
        );
        assert!(
            field.contains("Changing or removing it asks first."),
            "{field}"
        );
        let card = connections_card(&SiteView { past: vec![], ..v }).into_string();
        assert!(
            card.contains("No plugin is connected.")
                && card.contains("it finds this store by shop.example"),
            "{card}"
        );
        let none = connections_card(&SiteView {
            site: String::new(),
            past: vec![],
            ..view(false, None)
        })
        .into_string();
        assert!(
            none.contains("The WooCommerce plugin needs your website: add it above first."),
            "{none}"
        );
    }

    #[test]
    fn changing_the_website_is_blocked_while_a_plugin_is_connected() {
        let html = website_content(
            &view(true, Some(0)),
            &WebsiteChange::To("new.example".into()),
            None,
            true,
        )
        .into_string();
        assert!(
            html.contains("Change the website of \u{201c}Bakery\u{201d}?"),
            "{html}"
        );
        assert!(html.contains(r#"No plugin is connected to it<span class="fix">WooCommerce on shop.example is · <a href="/dashboard/stores/c_1/settings/connections/int_1/disconnect" data-opens-dialog="disconnect-dialog">disconnect it first</a>"#), "{html}");
        assert!(
            html.contains(r#"class="btn-danger" disabled"#) && !html.contains("<form"),
            "{html}"
        );
    }

    #[test]
    fn changing_the_website_lists_what_follows_then_asks_for_the_name() {
        let html = website_content(
            &view(false, None),
            &WebsiteChange::To("new.example".into()),
            None,
            false,
        )
        .into_string();
        assert!(
            html.starts_with(r#"<h1 id="website-title" class="dialog-title">"#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<form method="post" action="/dashboard/stores/c_1/settings/website">"#
            ),
            "{html}"
        );
        assert!(
            html.contains(r#"name="site" value="new.example""#),
            "{html}"
        );
        assert!(
            html.contains("Checkouts embedded on shop.example stop loading"),
            "{html}"
        );
        assert!(
            html.contains("Its verified domain shop.example is dropped"),
            "{html}"
        );
        assert!(html.contains(r#"3 orders started there can still be paid<span class="fix">until about 14:20; Monokulo keeps watching them</span>"#), "{html}");
        assert!(
            html.contains("Type <strong>\u{201c}Bakery\u{201d}</strong> to confirm"),
            "{html}"
        );
        assert!(
            html.contains(r#"<button type="submit" class="btn-danger">Change website</button>"#),
            "{html}"
        );
        let remove =
            website_content(&view(false, None), &WebsiteChange::Remove, None, true).into_string();
        assert!(
            remove.contains("Remove the website of")
                && remove.contains(r#"name="remove" value="1""#),
            "{remove}"
        );
        assert!(!remove.contains(r#"name="site""#), "{remove}");
    }

    #[test]
    fn disconnecting_is_blocked_while_an_order_from_the_shop_can_be_paid() {
        let html = disconnect_content(&view(true, Some(3)), None, true).into_string();
        assert!(html.contains(r#"No order from shop.example can still be paid<span class="fix">3 can be paid until about 14:20</span>"#), "{html}");
        assert!(html.contains(r#"<button type="button" class="btn-danger" disabled aria-describedby="disconnect-why">Disconnect WooCommerce</button>"#), "{html}");
        assert!(
            !html.contains("webhook") && !html.contains("<form"),
            "{html}"
        );
        let unknown = disconnect_content(&view(true, None), None, true).into_string();
        assert!(
            unknown.contains("Monokulo can't check its orders right now")
                && unknown.contains("disabled"),
            "{unknown}"
        );
    }

    #[test]
    fn disconnecting_says_the_webhook_goes_then_asks_for_the_name() {
        let html = disconnect_content(&view(true, Some(0)), None, false).into_string();
        assert!(html.contains("<h1 id=\"disconnect-title\" class=\"dialog-title\">Disconnect WooCommerce from \u{201c}Bakery\u{201d}?</h1>"), "{html}");
        assert!(html.contains(r#"<li class="ok">"#), "{html}");
        assert!(html.contains(r#"The plugin's webhook is removed<span class="fix">https://shop.example/?wc-api=monokulo</span>"#), "{html}");
        assert!(
            html.contains("New Monero orders on shop.example stop")
                && html.contains("The website unlocks"),
            "{html}"
        );
        assert!(html.contains(r#"<form method="post" action="/dashboard/stores/c_1/settings/connections/int_1/disconnect">"#), "{html}");
        assert!(
            html.contains(
                r#"<button type="submit" class="btn-danger">Disconnect WooCommerce</button>"#
            ),
            "{html}"
        );
    }
}
