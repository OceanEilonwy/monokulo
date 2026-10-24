//! Connected apps on a store's settings (`crate::apps`, `http::apps`): the
//! Apps card on the Connections tab, the page that shows a connect code,
//! the page that says whether it was used, and the public page a connect
//! code's link opens in a browser. None of them names an app: monokulo
//! keeps no list of apps.

use maud::{html, Markup, PreEscaped};

use super::{DialogContent, PageChrome};

/// A store's connected apps, for its settings.
pub struct AppsView {
    pub store_id: String,
    pub store_name: String,
    pub apps: Vec<AppRow>,
}

/// One connected app.
#[derive(Clone)]
pub struct AppRow {
    pub id: String,
    /// What the app calls itself ("Monokular").
    pub app: String,
    pub device_name: String,
    pub platform: String,
    pub version: String,
    pub connected: String,
    pub last_seen: Option<String>,
}

impl AppsView {
    fn settings(&self) -> String {
        crate::base::url(format!(
            "/stores/{}/settings?tab=connections#card-apps",
            self.store_id
        ))
    }

    pub fn connect_path(&self) -> String {
        crate::base::url(format!("/stores/{}/settings/apps/connect", self.store_id))
    }

    fn disconnect_path(&self, app: &AppRow) -> String {
        crate::base::url(format!(
            "/stores/{}/settings/apps/{}/disconnect",
            self.store_id, app.id
        ))
    }
}

impl AppRow {
    fn title(&self) -> String {
        if self.device_name.is_empty() {
            self.app.clone()
        } else {
            format!("{} · {}", self.app, self.device_name)
        }
    }

    fn meta(&self) -> String {
        let mut parts = Vec::new();
        if !self.platform.is_empty() {
            parts.push(self.platform.clone());
        }
        if !self.version.is_empty() {
            parts.push(format!("version {}", self.version));
        }
        parts.push(format!("connected {}", self.connected));
        parts.push(match &self.last_seen {
            Some(at) => format!("last order {at}"),
            None => "no orders yet".to_owned(),
        });
        parts.join(" · ")
    }
}

/// The Apps card: each connected device with Disconnect, and Connect an
/// app. An action card of its own, outside the settings form.
pub fn apps_card(view: &AppsView) -> Markup {
    let meta = match view.apps.len() {
        0 => "none".to_owned(),
        1 => "1 device".to_owned(),
        n => format!("{n} devices"),
    };
    html! {
        section id="card-apps" class="settings-card" aria-labelledby="card-apps-title" {
            header class="card-head" { h3 id="card-apps-title" { "Apps" } span class="card-meta" { (meta) } }
            div class="card-body" {
                @if view.apps.is_empty() {
                    (ui::Hint::new(
                        "Devices such as a till app can take payments for this store with a key of their own. \
                         Connect one by scanning a code with the app."
                    ))
                } @else {
                    ul class="int-rows" {
                        @for app in &view.apps {
                            li {
                                (super::store_site::plugin_mark(&app.app))
                                span {
                                    span class="int-name" { (app.title()) }
                                    span class="int-meta" { (app.meta()) }
                                }
                                form method="post" action=(view.disconnect_path(app)) {
                                    (ui::Button::new("Disconnect").kind(ui::ButtonType::Submit)
                                        .attr("aria-label", format!("Disconnect {}", app.title())))
                                }
                            }
                        }
                    }
                }
                (ui::LinkButton::new("Connect an app", view.connect_path()))
            }
        }
    }
}

fn page(chrome: &PageChrome, view: &AppsView, title: &str, content: DialogContent) -> Markup {
    let crumbs = super::store_crumbs(&view.store_id, &view.store_name, false);
    let (header, body) = content.page(crumbs, ui::DialogSize::Confirm);
    super::Page::new(format!("{title} - {} - Monokulo", view.store_name), header)
        .class("settings-page")
        .render(chrome, body)
}

/// `GET /stores/{id}/settings/apps/connect`: what to call the device, then
/// a code.
pub fn connect_form_page(chrome: &PageChrome, view: &AppsView, error: Option<&str>) -> Markup {
    let body = html! {
        p {
            "The app gets a key of its own for \u{201c}" (view.store_name) "\u{201d}: it can make orders and follow "
            "them, never change the store or its wallet. You can disconnect it here at any time."
        }
        @if let Some(error) = error { (ui::Alert::new(error, ui::Tone::Error)) }
        form method="post" action=(view.connect_path()) {
            (super::settings::Field::new("Device name (optional)", "device-name")
                .help(None, html! { "How it's listed under Apps, like \u{201c}Front counter\u{201d}. Without one, the app names it." })
                .render(html! {
                    (ui::Input::new("device_name").id("device-name").autocomplete("off").attr("maxlength", "40"))
                }))
            (ui::DialogActions::new(ui::Slot::many([
                ui::LinkButton::new("Cancel", view.settings()).into(),
                ui::Button::new("Show the code").variant(ui::ButtonVariant::Primary).kind(ui::ButtonType::Submit).into(),
            ])))
        }
    };
    page(
        chrome,
        view,
        "Connect an app",
        DialogContent {
            title_id: "connect-app-title".into(),
            title: html! { "Connect an app" },
            aside: None,
            body,
        },
    )
}

/// The code an app scans, or the same as a link to paste: shown once,
/// right after it was made (it can't be shown again).
pub fn code_page(
    chrome: &PageChrome,
    view: &AppsView,
    link: &str,
    qr: &crate::qr::QrSvg,
    status_path: &str,
) -> Markup {
    let body = html! {
        p { "Open the app and scan this code. On this device, copy the link and paste it into the app instead." }
        div class="connect-app-qr" { (PreEscaped(qr.as_str())) }
        (ui::CopyValue::new(link).copy_label("Copy the connect link"))
        (ui::Hint::new("It works once, for 10 minutes. Anyone with it can connect a device to this store, so don't share it."))
        (ui::DialogActions::new(ui::Slot::many([
            ui::LinkButton::new("Done", view.settings()).into(),
            ui::LinkButton::new("Check it connected", crate::base::url(status_path)).into(),
        ])))
    };
    page(
        chrome,
        view,
        "Connect an app",
        DialogContent {
            title_id: "connect-app-title".into(),
            title: html! { "Scan with the app" },
            aside: None,
            body,
        },
    )
}

/// Whether a code shown earlier has been used.
pub fn status_page(
    chrome: &PageChrome,
    view: &AppsView,
    connected: Option<&AppRow>,
    status_path: &str,
) -> Markup {
    let body = match connected {
        Some(app) => html! {
            (ui::Alert::new(html! { (app.title()) " is connected." }, ui::Tone::Success))
            (ui::DialogActions::new(ui::Slot::many([ui::LinkButton::new("Done", view.settings()).into()])))
        },
        None => html! {
            p { "No app has used the code yet. It works for 10 minutes after it was shown." }
            (ui::DialogActions::new(ui::Slot::many([
                ui::LinkButton::new("Back to settings", view.settings()).into(),
                ui::LinkButton::new("Refresh", crate::base::url(status_path)).into(),
            ])))
        },
    };
    page(
        chrome,
        view,
        "Connect an app",
        DialogContent {
            title_id: "connect-app-title".into(),
            title: html! { "Connect an app" },
            aside: None,
            body,
        },
    )
}

/// `GET /apps/connect`: where a connect link lands when it's opened in a
/// browser rather than an app. The code is in the link's `#` part, which
/// never reaches this server.
pub fn landing_page(chrome: &PageChrome) -> Markup {
    let header = ui::PageHeader::new(html! { "Connect an app" });
    let body = html! {
        (ui::DialogPage::new(html! {
            p { "This link connects an app to a store. Open it in the app instead of a browser:" }
            ul {
                li { "scan the code shown on the store's settings with the app, or" }
                li { "copy this page's address and paste it into the app." }
            }
            (ui::Hint::new("The code in the link works once, for 10 minutes. Don't share it."))
        }).size(ui::DialogSize::Confirm).render())
    };
    super::Page::new("Connect an app - Monokulo".to_owned(), header).render(chrome, body)
}
