//! A store's website and the plugins connected to it, on its settings page
//! (`docs/design/user-testing/store-site-integrations.html`: A3, B1, C1).
//!
//! - The Store card's website is locked while a plugin is connected
//!   ("Set by PrestaShop · see Connections").
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

use super::{DialogContent, PageChrome};

/// A plugin connected to the store, now or before.
pub struct IntegrationView {
    pub id: String,
    /// What it's called, as it named itself: "PrestaShop".
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
        crate::base::url(format!("/stores/{}/settings", self.store_id))
    }

    pub fn website_path(&self) -> String {
        format!("{}/website", self.settings())
    }

    pub fn disconnect_path(&self, integration: &IntegrationView) -> String {
        format!("{}/plugins/{}/disconnect", self.settings(), integration.id)
    }
}

/// Changing the website to another, or removing it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebsiteChange {
    To(String),
    Remove,
}

/// A plugin's mark: the first letter of its name on a tile. monokulo
/// knows no plugin, so it draws no plugin's logo.
pub(super) fn plugin_mark(name: &str) -> Markup {
    let initial: String = name
        .chars()
        .next()
        .into_iter()
        .flat_map(char::to_uppercase)
        .collect();
    html! {
        span class="int-mark" aria-hidden="true" { (initial) }
    }
}

/// The Store card's website field: an ordinary field (adding one saves
/// with the settings form; changing or emptying a saved one asks first),
/// or locked while a plugin is connected.
pub fn website_field(view: &SiteView, shown: &str) -> Markup {
    match &view.active {
        Some(active) => html! {
            (ui::Setting::new("Your website", "store-site", html! {
                (ui::LockedValue::new(view.site.as_str()).id("store-site").described_by("store-site-lock"))
                (ui::FieldNote::new(html! {
                    "Set by " (active.name) " · " a href=(format!("{}?tab=connections#card-connections", view.settings())) { "see Connections" }
                }).id("store-site-lock"))
            }))
        },
        None => html! {
            (super::settings::Field::new("Your website (optional)", "store-site")
                .help(None, html! {
                    "Needed for the checkout on your pages or for a shop plugin, which finds this store by it. "
                    "Any page on the site works."
                    @if !view.site.is_empty() { " Changing or removing it asks first." }
                })
                .render(html! {
                    (ui::Input::new("store_site").id("store-site").value(shown).autocomplete("url")
                        .placeholder("shop.example").attr("inputmode", "url").attr("data-saved", view.site.as_str()))
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
                                (plugin_mark(&active.name))
                                span {
                                    span class="int-name" { (active.name) } " " span class="tag-active" { "active" }
                                    span class="int-meta" { (integration_meta(active)) }
                                }
                                (ui::LinkButton::new("Disconnect…", view.disconnect_path(active))
                                    .attr("data-opens-dialog", "disconnect-dialog"))
                            }
                        }
                    }
                    None if view.site.is_empty() => {
                        (ui::Hint::new("A shop plugin needs your website: add it above first."))
                    }
                    None => {
                        (ui::Hint::new(html! {
                            "No plugin is connected. "
                            a href=(super::docs_url("plugins/")) target="_blank" rel="noopener" { "Install the plugin for your shop" }
                            "; it finds this store by " (view.site) "."
                        }))
                    }
                }
                @if !view.past.is_empty() {
                    h4 { "Before" }
                    ul class="int-rows past" {
                        @for past in &view.past {
                            li {
                                (plugin_mark(&past.name))
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
            (ui::Dialog::new("website-dialog", "website-title",
                website_content(view, &WebsiteChange::To(String::new()), None, true).in_dialog()).confirms("store_site"))
            (ui::Dialog::new("website-remove-dialog", "website-remove-title",
                website_content(view, &WebsiteChange::Remove, None, true).in_dialog())
                .confirms("store_site").confirms_when(ui::ConfirmWhen::Emptied))
        }
        @if let Some(active) = &view.active {
            (ui::Dialog::new("disconnect-dialog", "disconnect-title", disconnect_content(view, active, None).in_dialog()))
        }
    }
}

/// One row of a checklist: ✓ done, ✕ not yet with its fix beside it, or →
/// something that follows.
fn check(mark: ui::CheckMark, what: Markup, fix: Option<Markup>) -> ui::Check {
    ui::Check {
        mark,
        what: what.into(),
        fix: fix.map(ui::Slot::from),
    }
}

/// A dialog's Cancel (or Close) back to the settings, then its action.
fn actions(back: &str, cancel: &str, action: ui::Button) -> ui::DialogActions {
    ui::DialogActions::new(ui::Slot::many([
        ui::LinkButton::new(cancel, back)
            .flag("data-closes-dialog")
            .into(),
        action.into(),
    ]))
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
            (ui::Input::new("confirm").id(id).autocomplete("off").attr("spellcheck", "false").required(true))
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
) -> DialogContent {
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
    let mut follows = vec![check(
        ui::CheckMark::Then,
        html! { "Checkouts embedded on " (view.site) " stop loading" },
        None,
    )];
    if view.site_domain {
        follows.push(check(
            ui::CheckMark::Then,
            html! { "Its verified domain " (view.site) " is dropped" },
            (!remove).then(|| html! { "the new one waits to be verified" }),
        ));
    }
    if let Some(open) = view.open_orders.as_ref().filter(|o| o.count > 0) {
        follows.push(check(
            ui::CheckMark::Then,
            html! { (orders_word(open.count)) " started there can still be paid" },
            Some(html! {
                @if let Some(until) = &open.until { "until about " (until) "; " }
                "Monokulo keeps watching them"
            }),
        ));
    }
    let body = html! {
        @if remove {
            p { "Its checkout runs on its website: without one, the store takes payments at the till only." }
        } @else {
            p { "Its checkout runs on its website, so changing it moves where the checkout works." }
        }
        @if let Some(error) = error { (ui::Alert::new(error, ui::Tone::Error)) }
        @match &view.active {
            Some(active) => {
                (ui::Checks::new(vec![check(ui::CheckMark::No, html! { "No plugin is connected to it" }, Some(html! {
                    (active.name) " on " (active.site) " is · "
                    a href=(view.disconnect_path(active)) data-opens-dialog=[in_dialog.then_some("disconnect-dialog")] { "disconnect it first" }
                }))]))
                (ui::Hint::new(format!("{verb}ing becomes available once the plugin is disconnected."))
                    .attr("id", format!("{confirm_id}-why")))
                (actions(&back, "Close", ui::Button::new(format!("{verb} website"))
                    .variant(ui::ButtonVariant::Danger)
                    .disabled(true)
                    .attr("aria-describedby", format!("{confirm_id}-why"))))
            }
            None => {
                form method="post" action=(view.website_path()) {
                    @match change {
                        WebsiteChange::To(new) => {
                            div class="setting-field first" {
                                div class="setting-label-row" { label class="setting-label" for=(format!("{confirm_id}-site")) { "New website" } }
                                (ui::Input::new("site")
                                    .id(format!("{confirm_id}-site"))
                                    .value(new.as_str())
                                    .placeholder("shop.example")
                                    .required(true)
                                    .attr("inputmode", "url")
                                    .flag("data-confirmed"))
                            }
                        }
                        WebsiteChange::Remove => { input type="hidden" name="remove" value="1"; }
                    }
                    (ui::Checks::new(follows))
                    (confirm_field(confirm_id, &view.store_name))
                    (actions(&back, "Cancel", ui::Button::new(format!("{verb} website"))
                        .variant(ui::ButtonVariant::Danger)
                        .kind(ui::ButtonType::Submit)))
                }
            }
        }
    };
    DialogContent {
        title_id: id.into(),
        title,
        aside: None,
        body,
    }
}

/// C1: disconnecting the connected plugin. Blocked while an order from the
/// shop can still be paid (or the engine can't say); then what follows,
/// and the store's name typed.
pub fn disconnect_content(
    view: &SiteView,
    active: &IntegrationView,
    error: Option<&str>,
) -> DialogContent {
    let title =
        html! { "Disconnect " (active.name) " from \u{201c}" (view.store_name) "\u{201d}?" };
    let back = view.settings();
    let ready = view
        .plugin_open_orders
        .as_ref()
        .is_some_and(|o| o.count == 0);
    let checks = match &view.plugin_open_orders {
        None => vec![check(
            ui::CheckMark::No,
            html! { "Monokulo can't check its orders right now" },
            Some(html! { a href=(view.disconnect_path(active)) { "try again" } }),
        )],
        Some(open) if open.count > 0 => vec![check(
            ui::CheckMark::No,
            html! { "No order from " (active.site) " can still be paid" },
            Some(html! {
                (open.count) " can be paid"
                @if let Some(until) = &open.until { " until about " (until) }
            }),
        )],
        Some(_) => {
            let mut checks = vec![
                check(
                    ui::CheckMark::Ok,
                    html! { "No order from " (active.site) " can still be paid" },
                    None,
                ),
                check(
                    ui::CheckMark::Then,
                    html! { "New Monero orders on " (active.site) " stop" },
                    None,
                ),
            ];
            if let Some(url) = &active.webhook_url {
                checks.push(check(
                    ui::CheckMark::Then,
                    html! { "The plugin's webhook is removed" },
                    Some(html! { (url) }),
                ));
            }
            checks.push(check(
                ui::CheckMark::Then,
                html! { "The website unlocks" },
                Some(html! { "change or remove it in the Store card" }),
            ));
            checks
        }
    };
    let disconnect =
        ui::Button::new(format!("Disconnect {}", active.name)).variant(ui::ButtonVariant::Danger);
    let body = html! {
        p {
            "The store's secret key changes, so the plugin on " (active.site) " can't make orders: the shop stops taking "
            "Monero until you connect it again from the plugin."
        }
        @if let Some(error) = error { (ui::Alert::new(error, ui::Tone::Error)) }
        (ui::Checks::new(checks))
        @if ready {
            form method="post" action=(view.disconnect_path(active)) {
                (confirm_field("disconnect-confirm", &view.store_name))
                (actions(&back, "Cancel", disconnect.kind(ui::ButtonType::Submit)))
            }
        } @else {
            (ui::Hint::new("Disconnecting becomes available once no order from the shop can be paid.")
                .attr("id", "disconnect-why"))
            (actions(&back, "Close", disconnect.disabled(true).attr("aria-describedby", "disconnect-why")))
        }
    };
    DialogContent {
        title_id: "disconnect-title".into(),
        title,
        aside: None,
        body,
    }
}

fn dialog_page(
    chrome: &PageChrome,
    view: &SiteView,
    title: &str,
    content: DialogContent,
) -> Markup {
    let crumbs = super::store_crumbs(&view.store_id, &view.store_name, false);
    let (header, body) = content.page(crumbs, ui::DialogSize::Confirm);
    super::Page::new(format!("{title} - {} - Monokulo", view.store_name), header)
        .class("settings-page")
        .render(chrome, body)
}

/// `GET /stores/{id}/settings/website`: B1 as a page.
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

/// `GET /stores/{id}/settings/plugins/{integration}/disconnect`:
/// C1 as a page.
pub fn disconnect_page(
    chrome: &PageChrome,
    view: &SiteView,
    active: &IntegrationView,
    error: Option<&str>,
) -> Markup {
    dialog_page(
        chrome,
        view,
        "Disconnect",
        disconnect_content(view, active, error),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin(until: Option<&str>) -> IntegrationView {
        IntegrationView {
            id: "int_1".into(),
            name: "Acme Cart".into(),
            site: "shop.example".into(),
            version: "0.4.0".into(),
            connected: "3 Oct".into(),
            last_order: Some("today".into()),
            until: until.map(str::to_owned),
            webhook_url: Some("https://shop.example/monokulo-webhook".into()),
        }
    }

    fn disconnect_html(view: &SiteView) -> String {
        disconnect_content(view, view.active.as_ref().unwrap(), None)
            .in_dialog()
            .into_string()
    }

    fn view(active: bool, plugin_open: Option<u64>) -> SiteView {
        SiteView {
            store_id: "c_1".into(),
            store_name: "Bakery".into(),
            site: "shop.example".into(),
            active: active.then(|| plugin(None)),
            past: vec![plugin(Some("12 Sep"))],
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
    fn changing_the_website_is_blocked_while_a_plugin_is_connected() {
        let html = website_content(
            &view(true, Some(0)),
            &WebsiteChange::To("new.example".into()),
            None,
            true,
        )
        .in_dialog()
        .into_string();
        assert!(
            html.contains("Change the website of \u{201c}Bakery\u{201d}?"),
            "{html}"
        );
        assert!(html.contains(r#"No plugin is connected to it<span class="fix">Acme Cart on shop.example is · <a href="/stores/c_1/settings/plugins/int_1/disconnect" data-opens-dialog="disconnect-dialog">disconnect it first</a>"#), "{html}");
        assert!(
            html.contains(r#"data-variant="danger" disabled"#) && !html.contains("<form"),
            "{html}"
        );
    }

    #[test]
    fn disconnecting_is_blocked_while_an_order_from_the_shop_can_be_paid() {
        let html = disconnect_html(&view(true, Some(3)));
        assert!(html.contains(r#"No order from shop.example can still be paid<span class="fix">3 can be paid until about 14:20</span>"#), "{html}");
        assert!(html.contains(r#"<button class="mk-btn" type="button" data-variant="danger" disabled aria-describedby="disconnect-why">Disconnect Acme Cart</button>"#), "{html}");
        assert!(
            !html.contains("webhook") && !html.contains("<form"),
            "{html}"
        );
        let unknown = disconnect_html(&view(true, None));
        assert!(
            unknown.contains("Monokulo can't check its orders right now")
                && unknown.contains("disabled"),
            "{unknown}"
        );
    }
}
