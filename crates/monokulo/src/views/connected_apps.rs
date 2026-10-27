//! Connected apps on a store's settings (`crate::apps`, `http::apps`): the
//! Apps card on the Apps tab, the page that shows a connect code,
//! the page that says whether it was used, and the public page a connect
//! code's link opens in a browser. None of them names an app: monokulo
//! keeps no list of apps.
//!
//! Connecting is three steps, shown as a strip under the page's title:
//! naming the device, scanning the code, connected. The code page asks
//! whether the code was used every couple of seconds (fixi, `fx-poll`)
//! and moves the strip on once it was; without JavaScript, Check it
//! connected loads the same answer as a page.

use maud::{html, Markup, PreEscaped};

use super::PageChrome;

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
            "/stores/{}/settings?tab=apps#card-apps",
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

/// Where connecting an app has got to: the strip's three steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Name,
    Scan,
    /// The code ran out before an app used it.
    RanOut,
    Connected,
}

/// The strip under the page's title. Its id lets the code's status swap a
/// newer one in (`data-fx-oob`).
fn steps(now: Step, oob: bool) -> Markup {
    use ui::WizardState::{Current, Done, Next};
    let step = |label: &str, detail: &str, state| ui::WizardStep {
        label: label.into(),
        detail: detail.into(),
        done: usize::from(state == Done),
        tasks: 1,
        state,
    };
    let steps = match now {
        Step::Name => vec![
            step("Name it", "Optional", Current),
            step("Scan", "Next", Next),
            step("Connected", "Next", Next),
        ],
        Step::Scan => vec![
            step("Name it", "Done", Done),
            step("Scan", "With the app", Current),
            step("Connected", "Next", Next),
        ],
        Step::RanOut => vec![
            step("Name it", "Done", Done),
            step("Scan", "The code ran out", Current),
            step("Connected", "Next", Next),
        ],
        Step::Connected => vec![
            step("Name it", "Done", Done),
            step("Scan", "Done", Done),
            step("Connected", "Done", Done),
        ],
    };
    html! {
        div id="connect-app-steps" data-fx-oob[oob] {
            (ui::WizardSteps::new("Connecting an app", steps))
        }
    }
}

fn page(chrome: &PageChrome, view: &AppsView, title: &str, now: Step, body: Markup) -> Markup {
    let header = ui::PageHeader::new(title.to_owned())
        .title_id("connect-app-title")
        .crumbs(super::store_crumbs(&view.store_id, &view.store_name, false))
        .progress(steps(now, false));
    super::Page::new(format!("{title} - {} - Monokulo", view.store_name), header)
        .class("settings-page connect-app")
        .render(chrome, body)
}

/// `GET /stores/{id}/settings/apps/connect`: what to call the device, and
/// how it will read under Apps, then a code.
pub fn connect_form_page(
    chrome: &PageChrome,
    view: &AppsView,
    device_name: &str,
    error: Option<&str>,
) -> Markup {
    let body = html! {
        form method="post" action=(view.connect_path()) {
            section id="card-new-app" class="settings-card" aria-labelledby="card-new-app-title" {
                header class="card-head" {
                    h3 id="card-new-app-title" { "New app" }
                    span class="card-meta" { "for " (view.store_name) }
                }
                div class="card-body" {
                    @if let Some(error) = error { (ui::Alert::new(error, ui::Tone::Error)) }
                    (ui::Setting::new("Device name", "device-name", html! {
                        (ui::Input::new("device_name").id("device-name").value(device_name)
                            .placeholder("Front counter").autocomplete("off")
                            .described_by("device-name-help").attr("maxlength", "40")
                            .attr("data-preview", "connect-app-preview"))
                    })
                    .chip(ui::Hint::new("(optional)").inline(true))
                    .help("How it's listed under Apps, like \u{201c}Front counter\u{201d}. Without one, the app names it.")
                    .help_id("device-name-help")
                    .layout(ui::SettingLayout::Stacked))
                    h4 { "Under Apps it will read" }
                    ul class="int-rows connect-app-preview" {
                        li {
                            span class="int-mark" aria-hidden="true" { (PreEscaped(ui::assets::icons::POS)) }
                            span {
                                span class="int-name" {
                                    "The app"
                                    span id="connect-app-preview" hidden[device_name.is_empty()] {
                                        " · " span { (device_name) }
                                    }
                                }
                                span class="int-meta" { "Named by the app itself, once it connects" }
                            }
                        }
                    }
                    (ui::Hint::new(html! {
                        "It gets a key of its own for \u{201c}" (view.store_name) "\u{201d}: it can make orders and follow "
                        "them, never change the store or its wallet. You can disconnect it here at any time."
                    }))
                }
            }
            (ui::DialogActions::new(ui::Slot::many([
                ui::LinkButton::new("Cancel", view.settings()).into(),
                ui::Button::new("Show the code").variant(ui::ButtonVariant::Primary).kind(ui::ButtonType::Submit).into(),
            ])))
        }
        (super::script("connect-app.js", super::Load::Defer))
    };
    page(chrome, view, "Connect an app", Step::Name, body)
}

/// Where a code shown earlier is.
pub enum CodeState {
    /// Not used yet, with this many seconds left.
    Waiting {
        remaining: u64,
    },
    Connected(AppRow),
    /// Not used, and too old to be now.
    RanOut,
}

/// How long a code lasts, in seconds.
const CODE_SECONDS: u64 = crate::apps::CONNECT_TOKEN_TTL_SECONDS.unsigned_abs();

/// The card a code's page shows it on: named for the device, if it was.
fn code_card(device_name: &str, meta: &str, body: Markup) -> Markup {
    html! {
        section id="card-connect-code" class="settings-card" aria-labelledby="card-connect-code-title" {
            header class="card-head" {
                h3 id="card-connect-code-title" {
                    @if device_name.is_empty() { "New app" } @else { (device_name) }
                }
                span class="card-meta" id="connect-app-meta" { (meta) }
            }
            (body)
        }
    }
}

/// Whether the code was used: asked for again every couple of seconds while
/// it waits (fx-poll), which stops once an app has used it or it ran out.
/// Asked for by fixi, it also brings the strip and the card's meta up to
/// date (`oob`).
pub fn code_status(view: &AppsView, state: &CodeState, status_path: &str, oob: bool) -> Markup {
    let status_url = crate::base::url(status_path);
    html! {
        @match state {
            CodeState::Waiting { .. } => {
                div id="connect-app-status" class="connect-app-status" fx-action=(status_url) fx-trigger="fx:poll"
                    fx-swap="outerHTML" fx-poll="2000" fx-own-errors {
                    p class="connect-app-wait" role="status" {
                        span class="check-spinner" aria-hidden="true" {}
                        span { "Waiting for the app to scan the code\u{2026}" }
                    }
                    a class="mk-btn" data-size="compact" data-only="no-script" href=(status_url) { "Check it connected" }
                }
            }
            CodeState::Connected(app) => {
                div id="connect-app-status" class="connect-app-status" {
                    (ui::StatusBanner::new(format!("{} is connected.", app.title()), ui::Tone::Success)
                        .icon(html! { (PreEscaped(ui::assets::icons::CHECK)) }))
                    // As the Apps card lists it.
                    ul class="int-rows connect-app-preview" {
                        li {
                            (super::store_site::plugin_mark(&app.app))
                            span {
                                span class="int-name" { (app.title()) }
                                span class="int-meta" { (app.meta()) }
                            }
                        }
                    }
                }
                @if oob {
                    (steps(Step::Connected, true))
                    span class="card-meta" id="connect-app-meta" data-fx-oob { "connected" }
                    div id="connect-app-time" data-fx-oob {}
                    div id="connect-app-done" data-fx-oob { (done_actions(view)) }
                }
            }
            CodeState::RanOut => {
                div id="connect-app-status" class="connect-app-status" {
                    (ui::StatusBanner::new("No app used the code, and it has run out.", ui::Tone::Warning)
                        .icon(html! { (PreEscaped(ui::assets::icons::CLOCK)) })
                        .action(ui::LinkButton::new("Show a new code", view.connect_path())
                            .variant(ui::ButtonVariant::Primary).size(ui::ControlSize::Compact)))
                }
                @if oob {
                    (steps(Step::RanOut, true))
                    span class="card-meta" id="connect-app-meta" data-fx-oob { "ran out" }
                    div id="connect-app-time" data-fx-oob {}
                }
            }
        }
    }
}

fn done_actions(view: &AppsView) -> Markup {
    html! {
        (ui::DialogActions::new(ui::Slot::many([
            ui::LinkButton::new("Done", view.settings()).variant(ui::ButtonVariant::Primary).into(),
        ])))
    }
}

/// The code an app scans, or the same as a link to paste: shown once,
/// right after it was made (it can't be shown again).
pub fn code_page(
    chrome: &PageChrome,
    view: &AppsView,
    device_name: &str,
    link: &str,
    qr: &crate::qr::QrSvg,
    status_path: &str,
    onion: bool,
) -> Markup {
    let state = CodeState::Waiting {
        remaining: CODE_SECONDS,
    };
    let body = html! {
        (code_card(device_name, "waiting", html! {
            div class="card-body connect-app-code" {
                div class="connect-app-qr" { (PreEscaped(qr.as_str())) }
                div class="connect-app-text" {
                    p { "Open the app and scan this code. On this device, copy the link and paste it into the app instead." }
                    div id="connect-app-time" { (countdown(CODE_SECONDS)) }
                    (code_status(view, &state, status_path, false))
                    (ui::CopyValue::new(link).copy_label("Copy the connect link"))
                    (ui::Hint::new("Anyone with it can connect a device to this store, so don't share it."))
                    @if onion {
                        (ui::Hint::new("The code is on this Monokulo's onion address, as you are, so the app connects over Tor: it needs Tor running on the device (Orbot, for one) to use it."))
                    }
                }
            }
        }))
        div id="connect-app-done" {
            (ui::DialogActions::new(ui::Slot::many([ui::LinkButton::new("Done", view.settings()).into()])))
        }
    };
    page(chrome, view, "Scan with the app", Step::Scan, body)
}

/// The time left to scan: it works once, within it.
fn countdown(remaining: u64) -> Markup {
    let left = format!("{}m", remaining.div_ceil(60));
    html! {
        (ui::Countdown::new(CODE_SECONDS, remaining, left)
            .suffix("left to scan")
            .note("Works once")
            .toned())
    }
}

/// Whether a code shown earlier has been used, as a page: without
/// JavaScript, where Check it connected goes.
pub fn status_page(
    chrome: &PageChrome,
    view: &AppsView,
    device_name: &str,
    state: &CodeState,
    status_path: &str,
) -> Markup {
    let (now, meta) = match state {
        CodeState::Waiting { .. } => (Step::Scan, "waiting"),
        CodeState::Connected(_) => (Step::Connected, "connected"),
        CodeState::RanOut => (Step::RanOut, "ran out"),
    };
    let body = html! {
        (code_card(device_name, meta, html! {
            div class="card-body connect-app-text" {
                @if let CodeState::Waiting { remaining } = state {
                    p { "No app has used the code yet." }
                    div id="connect-app-time" { (countdown(*remaining)) }
                }
                (code_status(view, state, status_path, false))
            }
        }))
        div id="connect-app-done" {
            @if let CodeState::Connected(_) = state {
                (done_actions(view))
            } @else {
                (ui::DialogActions::new(ui::Slot::many([ui::LinkButton::new("Back to settings", view.settings()).into()])))
            }
        }
    };
    page(chrome, view, "Connect an app", now, body)
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
