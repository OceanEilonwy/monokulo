//! Composable, compile-time-checked HTML rendering (Maud), replacing the
//! old `handlebars`-based `TemplateEngine`/`.hbs` files.
//!
//! The thing that made adding a per-user theme awkward under the old setup
//! was structural, not cosmetic: every one of the ~20 old templates carried
//! its own literal `<html>`/`<head>`/`{{> nav}}` - there was no single place
//! that owned the page shell. [`layout`] and [`nav`] below are that place
//! now: every page in this module builds its own body content and hands it
//! to [`layout`], which owns `<!doctype html>` through `</html>`, the shared
//! `<head>` (the colours in `theme.css`, the components in `site.css`), and
//! the nav bar
//! (including the theme-toggle form) - so a per-user `data-theme` attribute,
//! or anything else every page needs, is a one-place change from here on.
//!
//! One submodule per page/page-group, mirroring `http/`'s own per-feature
//! split rather than one flat file.

use maud::{html, Markup, PreEscaped, DOCTYPE};
pub use shared::order_status::OrderStatus;

use crate::db::{Theme, UserRow};

pub mod admin;
pub mod auth;
pub mod challenge;
pub mod checkout;
pub mod connect;
pub mod create_order;
pub mod dashboard;
pub mod integration_help;
pub mod landing;
pub mod logs;
pub mod orders;
pub mod pos;
pub mod scaling;
pub mod status;
pub mod store_detail;
pub mod store_settings;
pub mod time;

/// Every colour, and the spacing, radius and type tokens: the only place a
/// colour is defined (`theme_tests`).
pub(crate) const THEME_CSS: &str = include_str!("theme.css");
/// The fonts and every shared component, painted with `THEME_CSS`'s roles.
pub(crate) const SITE_CSS: &str = include_str!("site.css");

/// Shared back links for store pages and pages nested under Orders.
pub fn store_breadcrumb(connection_id: &str, display_name: &str, include_orders: bool) -> Markup {
    html! {
        nav class="context-nav" aria-label="Breadcrumb" {
            a href=(format!("/dashboard/stores/{connection_id}")) title=(display_name) { (display_name) }
            @if include_orders {
                span class="breadcrumb-sep" aria-hidden="true" { "›" }
                a href=(format!("/dashboard/stores/{connection_id}/orders")) { "Orders" }
            }
        }
    }
}

/// Every value a page needs to render the parts every page shares (the
/// `<html data-theme>` attribute and the nav bar) - built once per request,
/// almost always via [`PageChrome::from_user`], and threaded through to
/// whichever `views::*::page(...)` function is actually rendering.
pub struct PageChrome {
    pub logged_in: bool,
    pub is_admin: bool,
    pub theme: Theme,
    /// The current request's own path (+ query string, where relevant) -
    /// carried as the theme-toggle form's hidden `next` field so toggling
    /// theme redirects back to the page it was toggled from, not a fixed
    /// default. Checked into a `SafePath` (`crate::http::dashboard`) before
    /// ever being used as a redirect target, same as the login
    /// flow's own `next` - never trusted at face value just because it
    /// came from this struct.
    pub current_path: String,
    /// The engine's last known health for the status indicator, `None`
    /// while not known yet. See `crate::http::status_page::known_health`.
    pub health: Option<Health>,
    /// Problems the signed-in merchant needs to know about on every page
    /// (task 3.7): their stores that can't be scanned right now. Shown under
    /// the nav on every page that has one, so never in the POS terminal.
    pub alerts: Vec<String>,
    /// The page loads `static/telemetry.js`. Always on pages that aren't
    /// about one store; on a store's pages, only when it has opted in to
    /// client logs (`crate::http::page_chrome`).
    pub browser_reports: bool,
    /// The zone this page shows times in (`views::time`): the signed-in
    /// user's, or UTC.
    pub clock: time::Clock,
}

impl PageChrome {
    /// The constructor almost every caller wants: `user` is whatever
    /// [`AuthedUser`](crate::http::AuthedUser)/[`AuthedAdmin`](crate::http::AuthedAdmin)
    /// already resolved, or `None` for an unauthenticated page (still real
    /// per-request state for `/`/`/status`, which show "log out" for a
    /// visitor who happens to have a session - see `views::landing`/
    /// `views::status`'s own callers).
    pub fn from_user(user: Option<&UserRow>, current_path: impl Into<String>) -> Self {
        match user {
            Some(u) => PageChrome {
                logged_in: true,
                is_admin: u.is_admin,
                theme: u.theme,
                current_path: current_path.into(),
                health: None,
                alerts: Vec::new(),
                browser_reports: true,
                clock: time::Clock::for_user(u),
            },
            None => PageChrome {
                logged_in: false,
                is_admin: false,
                theme: Theme::System,
                current_path: current_path.into(),
                health: None,
                alerts: Vec::new(),
                browser_reports: true,
                clock: time::Clock::utc(crate::now_unix()),
            },
        }
    }

    pub fn with_health(mut self, health: Option<Health>) -> Self {
        self.health = health;
        self
    }

    pub fn with_alerts(mut self, alerts: Vec<String>) -> Self {
        self.alerts = alerts;
        self
    }
}

fn theme_attr(theme: Theme) -> Option<&'static str> {
    match theme {
        Theme::System => None,
        Theme::Light => Some("light"),
        Theme::Dark => Some("dark"),
    }
}

/// The page shell every page in this module renders through: everything
/// from `<!doctype html>` to `</html>`, nav bar included. `title` is the
/// full `<title>` text (not auto-suffixed - some pages, e.g. the landing
/// page, deliberately have no " - Monokulo" suffix, so each caller states
/// its own title exactly).
const DEFAULT_VIEWPORT: &str = "width=device-width, initial-scale=1";

pub fn layout(chrome: &PageChrome, title: &str, body: Markup) -> Markup {
    page_shell(
        chrome,
        title,
        DEFAULT_VIEWPORT,
        None,
        Some(nav(chrome)),
        body,
    )
}

/// Same page shell, but with no nav bar - for a screen deliberately built
/// to have no site chrome at all (the POS terminal - see `views::pos`'s own
/// doc comment). Still carries the shared `<head>` and the per-user
/// `data-theme`, since a merchant who chose dark mode should get it here
/// too, even without a nav to toggle it from.
pub fn layout_bare(chrome: &PageChrome, title: &str, body: Markup) -> Markup {
    page_shell(chrome, title, DEFAULT_VIEWPORT, None, None, body)
}

/// Same as [`layout`], plus arbitrary extra `<head>` content (e.g. a
/// conditional `<meta http-equiv="refresh">`) rendered right after the
/// shared head partial - for the handful of pages that need one.
pub fn layout_with_head(
    chrome: &PageChrome,
    title: &str,
    extra_head: Markup,
    body: Markup,
) -> Markup {
    page_shell(
        chrome,
        title,
        DEFAULT_VIEWPORT,
        Some(extra_head),
        Some(nav(chrome)),
        body,
    )
}

/// [`layout_bare`] plus extra `<head>` content and a custom `viewport`
/// (the POS terminal wants `maximum-scale=1, viewport-fit=cover` - no
/// accidental pinch-zoom on a counter device, and safe-area insets around a
/// notch/home-indicator - see `views::pos`'s own doc comment).
pub fn layout_bare_with_head(
    chrome: &PageChrome,
    title: &str,
    viewport: &str,
    extra_head: Markup,
    body: Markup,
) -> Markup {
    page_shell(chrome, title, viewport, Some(extra_head), None, body)
}

fn page_shell(
    chrome: &PageChrome,
    title: &str,
    viewport: &str,
    extra_head: Option<Markup>,
    nav: Option<Markup>,
    body: Markup,
) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" data-theme=[theme_attr(chrome.theme)] {
            head {
                meta charset="utf-8";
                meta name="viewport" content=(viewport);
                title { (title) }
                link rel="icon" type="image/svg+xml" href="/static/favicon.svg";
                style { (PreEscaped(THEME_CSS)) (PreEscaped(SITE_CSS)) }
                // The trace of the request that rendered this page, so
                // browser reports join it (structured_logging.md 2.5).
                @if let Some(traceparent) = telemetry::trace::current_traceparent() {
                    meta name="traceparent" content=(traceparent);
                }
                // Browser problem reports on the site's own pages only: not
                // the challenge page or the POS app, which render without
                // nav, and on a store's pages only when it opted in. (The
                // checkout embed adds its own, also only when opted in.)
                @if nav.is_some() && chrome.browser_reports {
                    script src="/static/telemetry.js" defer {}
                }
                @if nav.is_some() {
                    // Partial updates and live streams (structured_logging.md
                    // part 4), in this order: the glue sets fixi's defaults.
                    script src="/static/fx-glue.js" defer {}
                    script src="/static/fixi.js" defer {}
                    script src="/static/ssexi.js" defer {}
                }
                @if let Some(extra_head) = extra_head {
                    (extra_head)
                }
            }
            body {
                @if let Some(nav) = nav {
                    (nav)
                    @if !chrome.alerts.is_empty() {
                        div class="wrap site-alerts" {
                            @for alert in &chrome.alerts {
                                p class="error" role="alert" { (alert) }
                            }
                        }
                    }
                }
                (body)
            }
        }
    }
}

/// The Monokulo mark, inline: a looking glass doubling as a monocle, watching
/// an eye with a Monero-orange pupil. Its lines are `currentColor`, so it
/// takes the text colour of whatever it sits on in either theme; the pupil
/// is `--brand-orange` (`.logo-pupil` in site.css). `static/logo.svg` is
/// the same drawing for other sites to link to.
pub fn logo_mark(size: u32, class: &str) -> Markup {
    html! {
        svg class=(format!("logo-mark {class}")) viewBox="0 0 64 64" width=(size) height=(size) aria-hidden="true" focusable="false" {
            line x1="19.7" y1="16.3" x2="15.5" y2="12.5" stroke="currentColor" stroke-width="2.25" {}
            circle cx="13.5" cy="10.5" r="2.25" fill="none" stroke="currentColor" stroke-width="2.25" {}
            line x1="39.3" y1="39.3" x2="56" y2="56" stroke="currentColor" stroke-width="6" stroke-linecap="round" {}
            circle cx="28" cy="28" r="16" fill="none" stroke="currentColor" stroke-width="5" {}
            path d="M19,28 Q28,20.5 37,28 Q28,35.5 19,28 Z" fill="none" stroke="currentColor" stroke-width="1.75" {}
            circle class="logo-pupil" cx="28" cy="28" r="5" stroke="currentColor" stroke-width="1" {}
            rect x="26.1" y="26.1" width="3.8" height="3.8" fill="currentColor" transform="rotate(45 28 28)" {}
        }
    }
}

/// What a page shows as an order's status: the engine's, or `Cancelled`
/// for a POS order cancelled before anything was paid (monokulo's own
/// record; the engine has no such status). Serialized by name, as the POS
/// app and the pages' `data-status` attributes read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayStatus {
    Order(OrderStatus),
    Cancelled,
}

impl From<OrderStatus> for DisplayStatus {
    fn from(status: OrderStatus) -> Self {
        DisplayStatus::Order(status)
    }
}

impl From<&OrderStatus> for DisplayStatus {
    fn from(status: &OrderStatus) -> Self {
        DisplayStatus::Order(*status)
    }
}

impl From<&DisplayStatus> for DisplayStatus {
    fn from(status: &DisplayStatus) -> Self {
        *status
    }
}

impl DisplayStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            DisplayStatus::Order(status) => status.as_str(),
            DisplayStatus::Cancelled => "cancelled",
        }
    }
}

impl DisplayStatus {
    /// A status by its name, `cancelled` included, for tests.
    #[cfg(test)]
    pub fn named(name: &str) -> Self {
        match name {
            "cancelled" => DisplayStatus::Cancelled,
            other => DisplayStatus::Order(other.parse().expect("a known status")),
        }
    }
}

impl std::fmt::Display for DisplayStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl serde::Serialize for DisplayStatus {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// An order status's words and its `.state-*` colour class, and whether the
/// order can still change. The checkout, the POS and the dashboard show a
/// status with the same words and colours.
pub fn order_state(status: impl Into<DisplayStatus>) -> (&'static str, &'static str, bool) {
    use OrderStatus::*;
    match status.into() {
        DisplayStatus::Order(Pending) => ("Waiting for payment", "state-pending", false),
        DisplayStatus::Order(Unconfirmed) => {
            ("Payment seen, unconfirmed", "state-unconfirmed", false)
        }
        DisplayStatus::Order(Confirming) => ("Confirming", "state-confirming", false),
        DisplayStatus::Order(Partial) => ("Partial payment received", "state-partial", false),
        DisplayStatus::Order(Paid) => ("Paid", "state-paid", true),
        DisplayStatus::Order(Overpaid) => ("Overpaid", "state-overpaid", true),
        DisplayStatus::Order(Expired) => ("Expired", "state-expired", true),
        DisplayStatus::Cancelled => ("Cancelled", "state-cancelled", true),
    }
}

/// A status's words short enough for a phone's table column ("Waiting"
/// for "Waiting for payment"). The long words stay for screen readers.
pub fn order_state_short(status: impl Into<DisplayStatus>) -> &'static str {
    match status.into() {
        DisplayStatus::Order(OrderStatus::Pending) => "Waiting",
        DisplayStatus::Order(OrderStatus::Unconfirmed) => "Seen",
        DisplayStatus::Order(OrderStatus::Partial) => "Part paid",
        other => order_state(other).0,
    }
}

/// An order's status as a badge ([`order_state`]): the full words, and the
/// short ones that replace them on a narrow screen (`.label-short`).
pub fn state_badge(status: impl Into<DisplayStatus> + Copy) -> Markup {
    let (label, class, _) = order_state(status);
    let short = order_state_short(status);
    html! {
        span class=(format!("tag {class}")) {
            @if short == label { (label) } @else {
                span class="label-long" { (label) }
                span class="label-short" aria-hidden="true" { (short) }
            }
        }
    }
}

/// An order id as people read it: without the `order_` every id starts
/// with, cut in the middle when it doesn't fit (as the macOS Finder cuts a
/// long file name), so its start and its last characters both show. The
/// whole id stays in the page, for find-in-page, and in `title`.
pub fn order_id_short(order_id: &str) -> Markup {
    let id = order_id.strip_prefix("order_").unwrap_or(order_id);
    let split = id.char_indices().rev().nth(5).map_or(0, |(i, _)| i);
    html! {
        span class="mid-ellipsis" title=(order_id) {
            span class="mid-head" { (&id[..split]) }
            span class="mid-tail" { (&id[split..]) }
        }
    }
}

/// An amount for people to read: XMR without its trailing zeros
/// (`0.420000000000` is `0.42`), anything else as given (`12.50` stays).
pub fn display_amount(amount: &str, currency: &str) -> String {
    let amount = if currency == "XMR" {
        trim_xmr(amount)
    } else {
        amount
    };
    if currency.is_empty() {
        amount.to_string()
    } else {
        format!("{amount} {currency}")
    }
}

/// An exact XMR amount without trailing zeros (`0.001000000000` is `0.001`).
pub fn trim_xmr(amount: &str) -> &str {
    if amount.contains('.') {
        amount.trim_end_matches('0').trim_end_matches('.')
    } else {
        amount
    }
}

/// A link to the Logs page searching for `field = value` over everything
/// kept, for admins on the pages about that thing (an order, a store).
pub fn logs_link(chrome: &PageChrome, field: &str, value: &str, text: &str) -> Markup {
    let query = format!(
        "{field} = '{}'",
        value.replace('\\', "\\\\").replace('\'', "\\'")
    );
    let href = format!(
        "/dashboard/admin/logs?q={}&range=all",
        url::form_urlencoded::byte_serialize(query.as_bytes()).collect::<String>()
    );
    html! {
        @if chrome.is_admin {
            a class="logs-link" href=(href) { (text) }
        }
    }
}

/// The Reload button a point-in-time page shows instead of refreshing by
/// itself (structured_logging.md D4): without JavaScript these pages are a
/// snapshot, and a reader reloads when they want the latest. `class` is
/// `reload` so the page's own script can hide it once it streams updates.
pub fn reload_button(href: &str) -> Markup {
    html! { a class="btn btn-secondary reload" href=(href) { "Reload" } }
}

/// The engine's health as the status indicator shows it: green, yellow or
/// red (docs/engine_scaling.md section 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    /// Everything answers and scans.
    Ok,
    /// Everything works, but one block has taken over two minutes to scan.
    Slow,
    /// A node or the scan is failing, or the engine can't be reached.
    Problem,
}

impl Health {
    /// The name the summary endpoint and the dot's class use.
    pub fn as_str(self) -> &'static str {
        match self {
            Health::Ok => "ok",
            Health::Slow => "slow",
            Health::Problem => "error",
        }
    }
}

/// Polls `GET /status/summary` and updates the indicator in place. Starts
/// straight away only when the page was rendered without a known health;
/// skips polls while the tab is hidden.
const STATUS_INDICATOR_SCRIPT: &str = r#"(function () {
  var link = document.getElementById("status-indicator");
  if (!link) return;
  var dot = link.querySelector(".status-dot");
  var POLL_MS = 30000;
  function show(state, title) { dot.className = "status-dot status-dot-" + state; link.title = title; link.setAttribute("aria-label", "Status: " + title); }
  function poll() {
    if (document.hidden) { setTimeout(poll, POLL_MS); return; }
    fetch("/status/summary", { cache: "no-store" }).then(function (r) {
      if (!r.ok) throw new Error("status unavailable");
      return r.json();
    }).then(function (data) {
      if (data && data.state === "ok") show("ok", "all systems healthy");
      else if (data && data.state === "slow") show("slow", "scanning is slow - see the status page");
      else show("error", "an issue was detected - see the status page");
    }).catch(function () {
      show("unknown", "could not check status");
    }).finally(function () {
      setTimeout(poll, POLL_MS);
    });
  }
  setTimeout(poll, dot.classList.contains("status-dot-unknown") ? 0 : POLL_MS);
})();"#;

/// The status indicator: a dot linking to `/status`, rendered with the
/// engine's last known health so it is right without JavaScript (every
/// page load renders it afresh). Its script only adds live updates by
/// polling. It is only the glowing dot - the title and aria-label carry the
/// words. `class` is the link's own class, for where it sits (nav bar, POS
/// top bar).
pub fn status_indicator(health: Option<Health>, class: &str) -> Markup {
    let (state, title) = match health {
        Some(Health::Ok) => ("ok", "all systems healthy"),
        Some(Health::Slow) => ("slow", "scanning is slow - see the status page"),
        Some(Health::Problem) => ("error", "an issue was detected - see the status page"),
        None => ("unknown", "status not checked yet"),
    };
    html! {
        a href="/status" class=(class) id="status-indicator" title=(title) aria-label=(format!("Status: {title}")) {
            span class=(format!("status-dot status-dot-{state}")) {}
        }
        script { (PreEscaped(STATUS_INDICATOR_SCRIPT)) }
    }
}

/// The site nav - brand, log-in-state links, the no-JS theme toggle (only
/// shown once logged in: there's no account to persist a preference
/// against otherwise, and an anonymous visitor already gets a real
/// `prefers-color-scheme` experience with no control needed), admin links,
/// and the status dot.
fn nav(chrome: &PageChrome) -> Markup {
    html! {
        nav class="site-nav" {
            div class="wrap site-nav-row" {
                a href="/" class="site-nav-brand" {
                    (logo_mark(24, "site-nav-logo"))
                    "Monokulo"
                }
                input type="checkbox" id="nav-toggle" class="nav-toggle-checkbox";
                label for="nav-toggle" class="nav-toggle-label" aria-label="Menu" { "☰" }
                div class="site-nav-links" {
                    a href="/dashboard" { "dashboard" }
                    @if chrome.is_admin {
                        a href="/dashboard/admin/settings" { "admin" }
                        a href="/dashboard/admin/invites" { "invites" }
                        a href="/dashboard/admin/logs" { "logs" }
                    }
                    @if chrome.logged_in {
                        form method="post" action="/dashboard/logout" class="nav-logout-form" {
                            button type="submit" class="nav-link-button" { "log out" }
                        }
                    } @else {
                        a href="/dashboard/login" { "log in" }
                        a href="/dashboard/signup" { "sign up" }
                    }
                    @if chrome.logged_in {
                        (theme_toggle(chrome))
                        // The zone every time on the page is in; changed on
                        // the dashboard.
                        a href="/dashboard#timezone" class="nav-tz-link"
                            title=(format!("Times are in {}{}. Change it on your dashboard.", chrome.clock.name(), if chrome.clock.is_automatic() { " (automatic)" } else { "" })) {
                            "tz: " (chrome.clock.short_label())
                        }
                    }
                    // Rightmost on every page.
                    (status_indicator(chrome.health, "nav-status-link"))
                }
            }
        }
    }
}

/// Three submit buttons select Light, System, or Dark directly without JS.
/// CSS previews each option on hover/focus and animates the indicator across
/// the form navigation in browsers that support view transitions.
pub fn theme_toggle(chrome: &PageChrome) -> Markup {
    html! {
        form method="post" action="/dashboard/theme" class="nav-theme-form" {
            input type="hidden" name="next" value=(chrome.current_path);
            div class=(format!("theme-toggle theme-toggle-{}", chrome.theme.as_str())) role="group" aria-label="Theme" {
                span class="theme-toggle-track" {
                    button type="submit" name="theme" value="system" class="theme-toggle-option theme-toggle-option-system" aria-label="System theme" aria-pressed=(chrome.theme == Theme::System) title="System theme" {
                        svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"
                            stroke-linejoin="round" aria-hidden="true" focusable="false" {
                            rect x="3" y="3" width="18" height="13" rx="2" {}
                            line x1="12" y1="16" x2="12" y2="21" {}
                            line x1="8" y1="21" x2="16" y2="21" {}
                        }
                    }
                    button type="submit" name="theme" value="light" class="theme-toggle-option theme-toggle-option-light" aria-label="Light theme" aria-pressed=(chrome.theme == Theme::Light) title="Light theme" {
                        svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"
                            stroke-linejoin="round" aria-hidden="true" focusable="false" {
                            circle cx="12" cy="12" r="4" {}
                            line x1="12" y1="2" x2="12" y2="4" {}
                            line x1="12" y1="20" x2="12" y2="22" {}
                            line x1="4.2" y1="4.2" x2="5.6" y2="5.6" {}
                            line x1="18.4" y1="18.4" x2="19.8" y2="19.8" {}
                            line x1="2" y1="12" x2="4" y2="12" {}
                            line x1="20" y1="12" x2="22" y2="12" {}
                            line x1="4.2" y1="19.8" x2="5.6" y2="18.4" {}
                            line x1="18.4" y1="5.6" x2="19.8" y2="4.2" {}
                        }
                    }
                    button type="submit" name="theme" value="dark" class="theme-toggle-option theme-toggle-option-dark" aria-label="Dark theme" aria-pressed=(chrome.theme == Theme::Dark) title="Dark theme" {
                        svg viewBox="0 0 24 24" fill="currentColor" stroke="none" aria-hidden="true" focusable="false" {
                            path d="M21 12.5A9 9 0 1 1 11.5 3 7 7 0 0 0 21 12.5Z" {}
                        }
                    }
                    span class="theme-toggle-thumb" {}
                }
            }
        }
    }
}

#[cfg(test)]
mod theme_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_order_id_drops_its_prefix_and_keeps_its_last_six_characters_whole() {
        let html = order_id_short("order_a8723b2e45b0d44e").into_string();
        assert_eq!(
            html,
            r#"<span class="mid-ellipsis" title="order_a8723b2e45b0d44e"><span class="mid-head">a8723b2e45</span><span class="mid-tail">b0d44e</span></span>"#
        );
        assert!(order_id_short("abc")
            .into_string()
            .contains(r#"<span class="mid-head"></span><span class="mid-tail">abc</span>"#));
    }

    #[test]
    fn status_indicator_is_rendered_with_the_known_health_and_polls_only_as_an_enhancement() {
        let healthy = status_indicator(Some(Health::Ok), "nav-status-link").into_string();
        assert!(healthy.contains(r#"<a href="/status" class="nav-status-link" id="status-indicator" title="all systems healthy" aria-label="Status: all systems healthy"><span class="status-dot status-dot-ok"></span></a>"#), "got: {healthy}");
        assert!(healthy.contains("/status/summary"));

        let problem = status_indicator(Some(Health::Problem), "pos-status-link").into_string();
        let slow = status_indicator(Some(Health::Slow), "nav-status-link").into_string();
        assert!(
            slow.contains(r#"class="status-dot status-dot-slow""#),
            "{slow}"
        );
        assert!(slow.contains("scanning is slow"), "{slow}");
        assert!(
            problem.contains(r#"class="status-dot status-dot-error""#),
            "got: {problem}"
        );

        let unknown = status_indicator(None, "nav-status-link").into_string();
        assert!(
            unknown.contains(r#"class="status-dot status-dot-unknown""#),
            "got: {unknown}"
        );

        let chrome = PageChrome::from_user(None, "/").with_health(Some(Health::Ok));
        assert!(nav(&chrome)
            .into_string()
            .contains("status-dot status-dot-ok"));
    }

    #[test]
    fn logged_in_admin_nav_order_is_dashboard_admin_invites_logout_theme_status() {
        let chrome = PageChrome {
            logged_in: true,
            is_admin: true,
            theme: Theme::Dark,
            current_path: "/dashboard".to_string(),
            health: None,
            alerts: Vec::new(),
            browser_reports: true,
            clock: time::Clock::utc(0),
        };
        let html = nav(&chrome).into_string();

        let dashboard = html.find(r#"href="/dashboard""#).expect("dashboard link");
        let admin = html
            .find(r#"href="/dashboard/admin/settings""#)
            .expect("admin link");
        let invites = html
            .find(r#"href="/dashboard/admin/invites""#)
            .expect("invites link");
        let logout = html
            .find(r#"action="/dashboard/logout""#)
            .expect("logout form");
        let status = html.find(r#"href="/status""#).expect("status link");
        let theme = html
            .find(r#"action="/dashboard/theme""#)
            .expect("theme form");

        assert!(
            dashboard < admin,
            "dashboard must come before admin, got: {html}"
        );
        assert!(
            admin < invites,
            "admin must come before invites, got: {html}"
        );
        let logs = html
            .find(r#"href="/dashboard/admin/logs""#)
            .expect("logs link");
        assert!(
            invites < logs && logs < logout,
            "logs comes after invites, before log out, got: {html}"
        );
        assert!(
            logout < theme,
            "log out must come before the theme toggle, got: {html}"
        );
        assert!(
            theme < status,
            "the status indicator must be rightmost, after the theme toggle, got: {html}"
        );
    }

    #[test]
    fn logged_out_nav_has_no_admin_logout_or_theme_controls() {
        let chrome = PageChrome::from_user(None, "/dashboard");
        let html = nav(&chrome).into_string();
        assert!(html.contains(r#"href="/dashboard/login""#));
        assert!(html.contains(r#"href="/dashboard/signup""#));
        assert!(!html.contains("admin"));
        assert!(!html.contains(r#"action="/dashboard/logout""#));
        assert!(!html.contains("theme-toggle"));
    }

    #[test]
    fn theme_toggle_renders_a_slider_with_a_thumb_positioned_for_the_current_theme() {
        for (theme, class) in [
            (Theme::Light, "theme-toggle-light"),
            (Theme::System, "theme-toggle-system"),
            (Theme::Dark, "theme-toggle-dark"),
        ] {
            let chrome = PageChrome {
                logged_in: true,
                is_admin: false,
                theme,
                current_path: "/dashboard".to_string(),
                health: None,
                alerts: Vec::new(),
                browser_reports: true,
                clock: time::Clock::utc(0),
            };
            let html = nav(&chrome).into_string();
            assert!(
                html.contains(&class.to_string()),
                "expected {class} on the toggle for {theme:?}, got: {html}"
            );
            assert!(
                html.contains("theme-toggle-option-light")
                    && html.contains("theme-toggle-option-dark"),
                "expected both sun and moon options, got: {html}"
            );
            assert!(
                html.contains("theme-toggle-thumb"),
                "expected a thumb element, got: {html}"
            );
            // Each option submits its theme through the same no-JS form.
            assert!(html.contains(r#"<form method="post" action="/dashboard/theme""#));
            assert!(html.contains(r#"<input type="hidden" name="next" value="/dashboard">"#));
            for value in ["light", "system", "dark"] {
                assert!(html.contains(&format!(r#"name="theme" value="{value}""#)));
            }
            assert_eq!(
                html.matches("aria-pressed=\"true\"").count(),
                1,
                "only the current theme should be pressed"
            );
        }
    }

    #[test]
    fn theme_hover_moves_the_thumb_and_recolors_only_the_hovered_icon() {
        let css = include_str!("site.css");
        for option in ["light", "system", "dark"] {
            assert!(css.contains(&format!(
                ".theme-toggle-option-{option}:is(:hover, :focus-visible) ~ .theme-toggle-thumb"
            )));
        }
        assert!(css.contains(".theme-toggle:has(.theme-toggle-option:is(:hover, :focus-visible)) .theme-toggle-option { color: var(--muted); }"));
        assert!(css.contains(".theme-toggle:has(.theme-toggle-option:is(:hover, :focus-visible)) .theme-toggle-option:is(:hover, :focus-visible) { color: var(--accent-ink); }"));
    }

    #[test]
    fn logs_links_are_for_admins_and_quote_the_value() {
        let admin = PageChrome {
            logged_in: true,
            is_admin: true,
            theme: Theme::System,
            current_path: "/".into(),
            health: None,
            alerts: Vec::new(),
            browser_reports: true,
            clock: time::Clock::utc(0),
        };
        let html = logs_link(&admin, "order.id", "o'1", "Logs").into_string();
        assert_eq!(
            html,
            r#"<a class="logs-link" href="/dashboard/admin/logs?q=order.id+%3D+%27o%5C%271%27&amp;range=all">Logs</a>"#
        );
        let query = "order.id = 'o\\'1'";
        assert_eq!(
            telemetry::query::parse(query).unwrap().unwrap().to_string(),
            query,
            "the link's query parses back"
        );
        let merchant = PageChrome {
            is_admin: false,
            ..admin
        };
        assert_eq!(
            logs_link(&merchant, "order.id", "o1", "Logs").into_string(),
            ""
        );
    }
}
