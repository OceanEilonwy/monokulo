//! A store's plugins and its deletion, on its settings page
//! (`docs/design/user-testing/store-site-integrations.html`: A3, B1, C1).
//!
//! - The Shop plugins card, on the Integrations tab, lists each plugin: its site, version, when it
//!   connected and its last order, with "Disconnect…"; past ones fold
//!   under "Before".
//! - Deleting the store asks first: blocked while an order can still be
//!   paid, then what is lost and the store's name typed.
//! - Disconnecting a plugin asks first (C1): blocked while an order from
//!   the shop can still be paid, then what follows (its key changes, its
//!   webhook is removed) and the store's name typed.
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

/// What the integrations and deletion parts of the settings page show.
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
}

impl SiteView {
    fn settings(&self) -> String {
        crate::base::url(format!("/stores/{}/settings", self.store_id))
    }

    pub fn delete_path(&self) -> String {
        format!("{}/delete", self.settings())
    }

    pub fn disconnect_path(&self, integration: &IntegrationView) -> String {
        format!("{}/plugins/{}/disconnect", self.settings(), integration.id)
    }
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

/// The Shop plugins card, on the Integrations tab: each plugin connected to the store, with
/// Disconnect…; the ones before folded under "Before". An action card of
/// its own, outside the settings form.
pub fn integrations_card(view: &SiteView) -> Markup {
    let meta = if view.active.is_some() {
        "1 active"
    } else {
        "none"
    };
    html! {
        section id="card-integrations" class="settings-card" aria-labelledby="card-integrations-title" {
            header class="card-head" { h3 id="card-integrations-title" { "Shop plugins" } span class="card-meta" { (meta) } }
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
                    None => {
                        (ui::Hint::new(html! {
                            "No plugin is connected. "
                            a href=(super::docs_url("plugins/")) target="_blank" rel="noopener" { "Install the plugin for your shop" }
                            "; connecting it asks which of your stores it should use."
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
/// save; each also a page (`delete_page`, `disconnect_page`).
pub fn dialogs(view: &SiteView) -> Markup {
    html! {
        @if let Some(active) = &view.active {
            (ui::Dialog::new("disconnect-dialog", "disconnect-title", disconnect_content(view, active, None).in_dialog()))
        }
        (ui::Dialog::new("delete-dialog", "delete-title", delete_content(view, None).in_dialog()))
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

/// Deleting the store. Blocked while an order can still be paid (or the
/// engine can't say); then what is lost, and the store's name typed.
pub fn delete_content(view: &SiteView, error: Option<&str>) -> DialogContent {
    let title = html! { "Delete \u{201c}" (view.store_name) "\u{201d}?" };
    let back = view.settings();
    let checks = match &view.open_orders {
        None => vec![check(
            ui::CheckMark::No,
            html! { "Monokulo can't check the store's orders right now" },
            Some(html! { a href=(view.delete_path()) { "try again" } }),
        )],
        Some(open) if open.count > 0 => vec![check(
            ui::CheckMark::No,
            html! { "No order can still be paid" },
            Some(html! {
                (orders_word(open.count)) " can be paid"
                @if let Some(until) = &open.until { " until about " (until) }
            }),
        )],
        Some(_) => {
            let mut checks = vec![
                check(
                    ui::CheckMark::Ok,
                    html! { "No order can still be paid" },
                    None,
                ),
                check(
                    ui::CheckMark::Then,
                    html! { "Its orders, order history and webhooks are deleted" },
                    None,
                ),
                check(
                    ui::CheckMark::Then,
                    html! { "Its domains, apps and settings are deleted" },
                    None,
                ),
                check(
                    ui::CheckMark::Then,
                    html! { "Its checkout and payment links stop working" },
                    None,
                ),
            ];
            if let Some(active) = &view.active {
                checks.push(check(
                    ui::CheckMark::Then,
                    html! { (active.name) " on " (active.site) " stops taking Monero" },
                    None,
                ));
            }
            checks.push(check(
                ui::CheckMark::Then,
                html! { "Your wallet is not touched" },
                None,
            ));
            checks
        }
    };
    let ready = view.open_orders.as_ref().is_some_and(|o| o.count == 0);
    let delete = ui::Button::new("Delete store").variant(ui::ButtonVariant::Danger);
    let body = html! {
        p { "This can't be undone." }
        @if let Some(error) = error { (ui::Alert::new(error, ui::Tone::Error)) }
        (ui::Checks::new(checks))
        @if ready {
            form method="post" action=(view.delete_path()) {
                (confirm_field("delete-confirm", &view.store_name))
                (actions(&back, "Cancel", delete.kind(ui::ButtonType::Submit)))
            }
        } @else {
            (ui::Hint::new("Deleting becomes available once no order can be paid.")
                .attr("id", "delete-why"))
            (actions(&back, "Close", delete.disabled(true).attr("aria-describedby", "delete-why")))
        }
    };
    DialogContent {
        title_id: "delete-title".into(),
        title,
        aside: None,
        body,
    }
}

/// The General tab's Delete store card, an action card of its own.
pub fn delete_card(view: &SiteView) -> Markup {
    html! {
        section id="card-delete" class="settings-card" aria-labelledby="card-delete-title" {
            header class="card-head" { h3 id="card-delete-title" { "Delete store" } }
            div class="card-body" {
                p { "Deletes the store with its orders, order history and webhooks. Your wallet is not touched." }
                (ui::LinkButton::new("Delete store\u{2026}", view.delete_path())
                    .attr("data-opens-dialog", "delete-dialog"))
            }
        }
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

/// `GET /stores/{id}/settings/delete`: the delete dialog as a page.
pub fn delete_page(chrome: &PageChrome, view: &SiteView, error: Option<&str>) -> Markup {
    dialog_page(chrome, view, "Delete store", delete_content(view, error))
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
        }
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
